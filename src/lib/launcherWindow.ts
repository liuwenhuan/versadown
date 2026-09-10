import { emitTo } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { currentMonitor, getCurrentWindow, LogicalSize, PhysicalPosition, Window } from '@tauri-apps/api/window';
import { isTauriRuntime } from './api';

export const LAUNCHER_WINDOW_LABEL = 'launcher';
export const launcherDimensions = {
  compact: { width: 64, height: 64 },
  menu: { width: 304, height: 392 },
  expanded: { width: 410, height: 364 }
} as const;

const launcherMargin = 24;
let launcherResizeQueue: Promise<void> = Promise.resolve();

type WorkArea = { x: number; y: number; width: number; height: number };
type NativeWindowSize = { width: number; height: number };

export function launcherCornerPosition(workArea: WorkArea, windowSize: NativeWindowSize, scaleFactor: number) {
  const margin = launcherMargin * scaleFactor;
  return {
    x: Math.round(workArea.x + workArea.width - windowSize.width - margin),
    y: Math.round(workArea.y + workArea.height - windowSize.height - margin)
  };
}

export function launcherResizedPosition(
  position: { x: number; y: number },
  currentSize: NativeWindowSize,
  nextSize: NativeWindowSize,
  workArea: WorkArea
) {
  const desiredX = position.x + currentSize.width - nextSize.width;
  const desiredY = position.y + currentSize.height - nextSize.height;
  return {
    x: Math.min(Math.max(desiredX, workArea.x), workArea.x + workArea.width - nextSize.width),
    y: Math.min(Math.max(desiredY, workArea.y), workArea.y + workArea.height - nextSize.height)
  };
}

export function isLauncherWindow() {
  return isTauriRuntime() && getCurrentWindow().label === LAUNCHER_WINDOW_LABEL;
}

export function launcherCompactSize() {
  return launcherDimensions.compact;
}

async function resizeLauncherNow(size: NativeWindowSize, useCompactShape: boolean, logicalPositionOffset: number) {
  if (!isTauriRuntime()) return;
  const launcher = getCurrentWindow();
  const [position, currentSize, monitor] = await Promise.all([launcher.outerPosition(), launcher.outerSize(), currentMonitor()]);
  if (monitor) {
    const workArea = monitor.workArea;
    const scaleFactor = monitor.scaleFactor;
    const nextSize = {
      width: Math.round(size.width * scaleFactor),
      height: Math.round(size.height * scaleFactor)
    };
    const sizeChanged = currentSize.width !== nextSize.width || currentSize.height !== nextSize.height;
    const offset = sizeChanged ? Math.round(logicalPositionOffset * scaleFactor) : 0;
    const resizedPosition = launcherResizedPosition(
      position,
      currentSize,
      nextSize,
      { x: workArea.position.x, y: workArea.position.y, width: workArea.size.width, height: workArea.size.height }
    );
    const nextPosition = {
      x: Math.min(Math.max(resizedPosition.x + offset, workArea.position.x), workArea.position.x + workArea.size.width - nextSize.width),
      y: Math.min(Math.max(resizedPosition.y + offset, workArea.position.y), workArea.position.y + workArea.size.height - nextSize.height)
    };
    await invoke('set_launcher_bounds', {
      x: nextPosition.x,
      y: nextPosition.y,
      width: nextSize.width,
      height: nextSize.height,
      compact: useCompactShape,
      live: false
    });
    return;
  }
  if (!useCompactShape) await setLauncherCompactShape(false);
  await launcher.setSize(new LogicalSize(size.width, size.height));
  if (useCompactShape) await setLauncherCompactShape(true);
}

function resizeLauncher(size: NativeWindowSize, useCompactShape: boolean, logicalPositionOffset = 0) {
  const operation = launcherResizeQueue.then(() => resizeLauncherNow(size, useCompactShape, logicalPositionOffset));
  launcherResizeQueue = operation.catch(() => undefined);
  return operation;
}

export async function setLauncherExpanded(expanded: boolean) {
  await resizeLauncher(
    expanded ? launcherDimensions.expanded : launcherCompactSize(),
    !expanded
  );
}

export async function setLauncherCompact() {
  await resizeLauncher(launcherCompactSize(), true);
}

export async function setLauncherContextMenuVisible(visible: boolean) {
  await resizeLauncher(
    visible ? launcherDimensions.menu : launcherCompactSize(),
    !visible
  );
}

export async function setLauncherWindowVisible(visible: boolean) {
  if (!isTauriRuntime()) return;
  await invoke('set_launcher_window_visible', { visible });
}

export async function anchorLauncherToWorkArea() {
  if (!isTauriRuntime()) return;
  const launcher = getCurrentWindow();
  const monitor = await currentMonitor();
  if (!monitor) return;
  const [windowSize, workArea] = await Promise.all([launcher.outerSize(), Promise.resolve(monitor.workArea)]);
  const corner = launcherCornerPosition(
    { x: workArea.position.x, y: workArea.position.y, width: workArea.size.width, height: workArea.size.height },
    { width: windowSize.width, height: windowSize.height },
    monitor.scaleFactor
  );
  await launcher.setPosition(new PhysicalPosition(corner.x, corner.y));
}

export async function openLauncherManager() {
  if (!isTauriRuntime()) return;
  const mainWindow = await Window.getByLabel('main');
  await mainWindow?.unminimize();
  await mainWindow?.show();
  await mainWindow?.setFocus();
}

export async function sendLauncherInput(input: string) {
  if (!isTauriRuntime()) return;
  await openLauncherManager();
  await emitTo('main', 'launcher-parse', input);
}

export async function setLauncherCompactShape(compact: boolean) {
  if (!isTauriRuntime()) return;
  await invoke('set_launcher_compact_shape', { compact });
}

export async function startLauncherDragging() {
  if (!isTauriRuntime()) return;
  await invoke('start_launcher_dragging');
}

export async function showLauncherMenu() {
  if (!isTauriRuntime()) return;
  await invoke('show_launcher_menu');
}

export async function runLauncherMenuAction(action: string) {
  if (!isTauriRuntime()) return;
  await invoke('run_launcher_menu_action', { action });
}

export async function getLauncherManagerVisible(): Promise<boolean> {
  if (!isTauriRuntime()) return true;
  return invoke<boolean>('launcher_manager_visible');
}
