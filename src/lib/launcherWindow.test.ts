import { beforeEach, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => {
  const calls: string[] = [];
  return {
    calls,
    emitTo: vi.fn(async () => { calls.push('emit'); }),
    getByLabel: vi.fn(),
    mainWindow: {
      unminimize: vi.fn(async () => { calls.push('unminimize'); }),
      show: vi.fn(async () => { calls.push('show'); }),
      setFocus: vi.fn(async () => { calls.push('focus'); })
    }
  };
});

vi.mock('./api', () => ({ isTauriRuntime: () => true }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ emitTo: native.emitTo }));
vi.mock('@tauri-apps/api/window', () => ({
  currentMonitor: vi.fn(),
  getCurrentWindow: vi.fn(() => ({ label: 'launcher' })),
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
