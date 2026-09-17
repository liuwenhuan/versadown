import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
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
    setPosition: vi.fn().mockResolvedValue(undefined),
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
  getCurrentWindow: vi.fn(() => ({ label: 'launcher', outerPosition: native.launcherPosition, outerSize: native.launcherSize, setPosition: native.setPosition })),
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

it('does not expose native resize or visual clipping helpers to popup code', () => {
  const source = readFileSync(resolve('src/lib/launcherWindow.ts'), 'utf8');
  expect(source).not.toContain('set_launcher_bounds');
  expect(source).not.toContain('set_launcher_compact_shape');
  expect(source).not.toContain('resizeLauncher');
});

it('installs the compact icon input region without changing native bounds', async () => {
  await launcherWindow.setLauncherInputRegions([{ x: 346, y: 328, width: 64, height: 64 }]);
  expect(native.invoke).toHaveBeenCalledWith('set_launcher_input_regions', {
    regions: [{ x: 346, y: 328, width: 64, height: 64 }],
  });
  expect(native.invoke).not.toHaveBeenCalledWith('set_launcher_bounds', expect.anything());
});

it('rejects malformed logical input regions before invoking the native adapter', async () => {
  await expect(launcherWindow.setLauncherInputRegions([
    { x: 346, y: 328, width: Number.NaN, height: 64 },
  ])).rejects.toThrow('悬浮窗输入区域无效');
  expect(native.invoke).not.toHaveBeenCalledWith('set_launcher_input_regions', expect.anything());
});

it('positions the fixed canvas without consulting a variable native window size', async () => {
  native.currentMonitor.mockResolvedValueOnce({
    scaleFactor: 1.5,
    workArea: { position: { x: 100, y: 50 }, size: { width: 1920, height: 1080 } },
  });
  native.launcherSize.mockResolvedValueOnce({ width: 64, height: 64 });
  await launcherWindow.anchorLauncherToWorkArea();
  expect(native.setPosition).toHaveBeenCalledWith({ x: 1369, y: 506 });
  expect(native.launcherSize).not.toHaveBeenCalled();
});

it.each([
  [{ x: 100, y: -400 }, 'below'],
  [{ x: 100, y: 100 }, 'above'],
] as const)('chooses placement from the invariant icon at %j', async (position, expected) => {
  native.launcherPosition.mockResolvedValueOnce(position);
  native.currentMonitor.mockResolvedValueOnce({
    scaleFactor: 1.5,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 } },
  });
  await expect(launcherWindow.getLauncherPopupPlacement()).resolves.toBe(expected);
  expect(native.launcherSize).not.toHaveBeenCalled();
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

it('moves the fixed canvas above the work area when a captured icon drag reaches the desktop top', () => {
  expect(launcherWindow.launcherPointerDragPosition(
    {
      windowPosition: { x: 1369, y: 506 },
      pointerPosition: { x: 378, y: 360 },
      scaleFactor: 1.5,
    },
    { x: 378, y: -292 },
  )).toEqual({ x: 1369, y: -472 });
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
