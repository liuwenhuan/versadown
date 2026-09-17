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

it('uses one fixed canvas with an invariant hexagon anchor', () => {
  expect(launcherWindow.launcherCanvas).toEqual({
    width: 410,
    height: 720,
    popupSlotHeight: 328,
    iconSize: 64,
    iconTop: 328,
    margin: 24,
  });
});

it('anchors the invariant hexagon to the work-area bottom right', () => {
  expect(launcherWindow.launcherCanvasPosition(
    { x: 100, y: 50, width: 1920, height: 1080 },
    1.5,
  )).toEqual({ x: 1369, y: 506 });
});

it('maps the fixed hexagon to a physical screen rectangle', () => {
  expect(launcherWindow.launcherIconScreenRect({ x: 100, y: 50 }, 1.5)).toEqual({
    x: 619,
    y: 542,
    width: 96,
    height: 96,
  });
});

it('chooses the lower slot without moving the hexagon when upper space is insufficient', () => {
  const icon = { x: 850, y: 20, width: 64, height: 64 };
  expect(launcherWindow.launcherPopupPlacementFromIcon(
    icon,
    328,
    { x: 0, y: 0, width: 1000, height: 800 },
  )).toBe('below');
});

it('prefers the upper slot when the complete popup fits', () => {
  const icon = { x: 850, y: 500, width: 64, height: 64 };
  expect(launcherWindow.launcherPopupPlacementFromIcon(
    icon,
    328,
    { x: 0, y: 0, width: 1000, height: 800 },
  )).toBe('above');
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

it('uses the native visibility command so background smoke runs cannot surface a launcher', async () => {
  await launcherWindow.setLauncherWindowVisible(false);

  expect(native.invoke).toHaveBeenCalledWith('set_launcher_window_visible', { visible: false });
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
