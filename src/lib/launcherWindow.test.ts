import { beforeEach, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => {
  const calls: string[] = [];
  return {
    calls,
    invoke: vi.fn(),
    emitTo: vi.fn(async () => { calls.push('emit'); }),
    getByLabel: vi.fn(),
    currentMonitor: vi.fn(),
    launcherPosition: vi.fn(),
    launcherSize: vi.fn(),
    mainWindow: {
      unminimize: vi.fn(async () => { calls.push('unminimize'); }),
      show: vi.fn(async () => { calls.push('show'); }),
      setFocus: vi.fn(async () => { calls.push('focus'); })
    }
  };
});

vi.mock('./api', () => ({ isTauriRuntime: () => true }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ emitTo: native.emitTo }));
vi.mock('@tauri-apps/api/window', () => ({
  currentMonitor: native.currentMonitor,
  getCurrentWindow: vi.fn(() => ({ label: 'launcher', outerPosition: native.launcherPosition, outerSize: native.launcherSize })),
  LogicalSize: class LogicalSize {
    constructor(public width: number, public height: number) {}
  },
  PhysicalPosition: class PhysicalPosition {
    constructor(public x: number, public y: number) {}
  },
  Window: { getByLabel: native.getByLabel }
}));

import * as launcherWindow from './launcherWindow';

beforeEach(() => {
  native.calls.length = 0;
  vi.clearAllMocks();
  native.getByLabel.mockResolvedValue(native.mainWindow);
  native.currentMonitor.mockReset();
  native.launcherPosition.mockReset();
  native.launcherSize.mockReset();
});

it('anchors the complete native window inside the monitor work area even when Windows enforces a wider size', () => {
  const position = (launcherWindow as unknown as {
    launcherCornerPosition(workArea: { x: number; y: number; width: number; height: number }, windowSize: { width: number; height: number }, scaleFactor: number): { x: number; y: number };
  }).launcherCornerPosition({ x: 100, y: 50, width: 1920, height: 1080 }, { width: 214, height: 86 }, 1.75);

  expect(position).toEqual({ x: 1764, y: 1002 });
});

it('resizes around the launcher bottom-right corner and clamps it to the work area', () => {
  const resized = (launcherWindow as unknown as {
    launcherResizedPosition(
      position: { x: number; y: number },
      currentSize: { width: number; height: number },
      nextSize: { width: number; height: number },
      workArea: { x: number; y: number; width: number; height: number }
    ): { x: number; y: number };
  }).launcherResizedPosition(
    { x: 1000, y: 800 },
    { width: 236, height: 98 },
    { width: 822, height: 525 },
    { x: 0, y: 0, width: 2560, height: 1516 }
  );

  expect(resized).toEqual({ x: 414, y: 373 });
});

it('keeps the launcher fixed and opens its popup below when there is no room above it', () => {
  const helpers = launcherWindow as unknown as {
    launcherPopupPlacement(
      position: { x: number; y: number },
      currentSize: { width: number; height: number },
      nextSize: { width: number; height: number },
      workArea: { x: number; y: number; width: number; height: number }
    ): 'above' | 'below';
    launcherResizedPosition(
      position: { x: number; y: number },
      currentSize: { width: number; height: number },
      nextSize: { width: number; height: number },
      workArea: { x: number; y: number; width: number; height: number },
      placement: 'above' | 'below'
    ): { x: number; y: number };
  };
  const position = { x: 1000, y: 12 };
  const currentSize = { width: 64, height: 64 };
  const nextSize = { width: 304, height: 392 };
  const workArea = { x: 0, y: 0, width: 1920, height: 1080 };

  expect(helpers.launcherPopupPlacement(position, currentSize, nextSize, workArea)).toBe('below');
  expect(helpers.launcherResizedPosition(position, currentSize, nextSize, workArea, 'below')).toEqual({ x: 760, y: 12 });
});

it('uses the hexagon as the complete compact launcher viewport', () => {
  expect(launcherWindow.launcherCompactSize()).toEqual({ width: 64, height: 64 });
});

it('requests the clipped native compact shape instead of a transparent rectangle', async () => {
  native.launcherPosition.mockResolvedValueOnce({ x: 1000, y: 800 });
  native.launcherSize.mockResolvedValueOnce({ width: 64, height: 64 });
  native.currentMonitor.mockResolvedValueOnce({
    scaleFactor: 1,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 } }
  });

  await launcherWindow.setLauncherCompact();

  expect(native.invoke).toHaveBeenCalledWith('set_launcher_bounds', {
    x: 1000,
    y: 800,
    width: 64,
    height: 64,
    compact: true,
    live: false
  });
});

it('reserves vertical space for the quick-add panel above the persistent launcher', () => {
  expect(launcherWindow.launcherDimensions.expanded).toEqual({ width: 410, height: 364 });
});

it('reserves a shadow-safe viewport for the launcher context menu', () => {
  expect(launcherWindow.launcherDimensions.menu).toEqual({ width: 304, height: 392 });
});

it('uses the native visibility command so background smoke runs cannot surface a launcher', async () => {
  await launcherWindow.setLauncherWindowVisible(false);

  expect(native.invoke).toHaveBeenCalledWith('set_launcher_window_visible', { visible: false });
});

it('opens the dedicated launcher popup without resizing or reshaping the fixed icon window', async () => {
  await (launcherWindow as unknown as {
    showLauncherPopup(mode: 'input' | 'menu'): Promise<void>;
  }).showLauncherPopup('menu');

  expect(native.invoke).toHaveBeenCalledWith('show_launcher_popup', { mode: 'menu' });
  expect(native.invoke).not.toHaveBeenCalledWith('set_launcher_bounds', expect.anything());
  expect(native.invoke).not.toHaveBeenCalledWith('set_launcher_compact_shape', expect.anything());
});

it('reveals the main task list without forwarding a parse request', async () => {
  await launcherWindow.openLauncherManager();

  expect(native.getByLabel).toHaveBeenCalledWith('main');
  expect(native.calls).toEqual(['unminimize', 'show', 'focus']);
  expect(native.emitTo).not.toHaveBeenCalled();
});

it('restores and reveals the main window before forwarding launcher input', async () => {
  await launcherWindow.sendLauncherInput('https://example.test/file.zip');

  expect(native.getByLabel).toHaveBeenCalledWith('main');
  expect(native.calls).toEqual(['unminimize', 'show', 'focus', 'emit']);
  expect(native.emitTo).toHaveBeenCalledWith(
    'main',
    'launcher-parse',
    'https://example.test/file.zip'
  );
});
