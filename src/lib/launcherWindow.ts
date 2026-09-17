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

export const launcherCanvas = {
  width: 410,
  height: 720,
  popupSlotHeight: 328,
  iconSize: 64,
  iconTop: 328,
  margin: 24,
} as const;

const launcherMargin = 24;
let launcherResizeQueue: Promise<void> = Promise.resolve();

type WorkArea = { x: number; y: number; width: number; height: number };
type Rect = { x: number; y: number; width: number; height: number };
type NativeWindowSize = { width: number; height: number };
export type LauncherPopupPlacement = 'above' | 'below';

export function launcherCanvasPosition(workArea: WorkArea, scaleFactor: number) {
  const canvasWidth = Math.round(launcherCanvas.width * scaleFactor);
  const iconBottom = Math.round(
    (launcherCanvas.iconTop + launcherCanvas.iconSize) * scaleFactor,
  );
  const margin = Math.round(launcherCanvas.margin * scaleFactor);
  return {
    x: workArea.x + workArea.width - canvasWidth - margin,
    y: workArea.y + workArea.height - iconBottom - margin,
  };
}

export function launcherIconScreenRect(
  windowPosition: { x: number; y: number },
  scaleFactor: number,
): Rect {
  return {
    x: windowPosition.x + Math.round((launcherCanvas.width - launcherCanvas.iconSize) * scaleFactor),
    y: windowPosition.y + Math.round(launcherCanvas.iconTop * scaleFactor),
    width: Math.round(launcherCanvas.iconSize * scaleFactor),
    height: Math.round(launcherCanvas.iconSize * scaleFactor),
  };
}

export function launcherPopupPlacementFromIcon(
  icon: Rect,
  popupHeight: number,
  workArea: WorkArea,
): LauncherPopupPlacement {
  return icon.y - popupHeight < workArea.y ? 'below' : 'above';
}

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
  workArea: WorkArea,
  placement: LauncherPopupPlacement = 'above'
) {
  const desiredX = position.x + currentSize.width - nextSize.width;
  const desiredY = placement === 'below'
    ? position.y
    : position.y + currentSize.height - nextSize.height;
  return {
    x: Math.min(Math.max(desiredX, workArea.x), workArea.x + workArea.width - nextSize.width),
    y: Math.min(Math.max(desiredY, workArea.y), workArea.y + workArea.height - nextSize.height)
  };
}

export function launcherPopupPlacement(
  position: { x: number; y: number },
  currentSize: NativeWindowSize,
  nextSize: NativeWindowSize,
  workArea: WorkArea
): LauncherPopupPlacement {
  const popupWouldStartAboveWorkArea = position.y + currentSize.height - nextSize.height < workArea.y;
  return nextSize.height > currentSize.height && popupWouldStartAboveWorkArea ? 'below' : 'above';
}

export function isLauncherWindow() {
  return isTauriRuntime() && getCurrentWindow().label === LAUNCHER_WINDOW_LABEL;
}

export function launcherCompactSize() {
  return launcherDimensions.compact;
}

async function resizeLauncherNow(
  size: NativeWindowSize,
  useCompactShape: boolean,
  logicalPositionOffset: number,
  requestedPlacement?: LauncherPopupPlacement
): Promise<LauncherPopupPlacement> {
  const fallbackPlacement = requestedPlacement ?? 'above';
  if (!isTauriRuntime()) return fallbackPlacement;
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
    const placement = requestedPlacement ?? launcherPopupPlacement(
      position,
      currentSize,
      nextSize,
      { x: workArea.position.x, y: workArea.position.y, width: workArea.size.width, height: workArea.size.height }
    );
    const resizedPosition = launcherResizedPosition(
      position,
      currentSize,
      nextSize,
      { x: workArea.position.x, y: workArea.position.y, width: workArea.size.width, height: workArea.size.height },
      placement
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
    return placement;
  }
  if (!useCompactShape) await setLauncherCompactShape(false);
  await launcher.setSize(new LogicalSize(size.width, size.height));
  if (useCompactShape) await setLauncherCompactShape(true);
  return fallbackPlacement;
}

function resizeLauncher(
  size: NativeWindowSize,
  useCompactShape: boolean,
  logicalPositionOffset = 0,
  requestedPlacement?: LauncherPopupPlacement
) {
  const operation = launcherResizeQueue.then(() => resizeLauncherNow(size, useCompactShape, logicalPositionOffset, requestedPlacement));
  launcherResizeQueue = operation.then(() => undefined, () => undefined);
  return operation;
}

export async function getLauncherPopupPlacement(size: NativeWindowSize): Promise<LauncherPopupPlacement> {
  if (!isTauriRuntime()) return 'above';
  const launcher = getCurrentWindow();
  const [position, currentSize, monitor] = await Promise.all([launcher.outerPosition(), launcher.outerSize(), currentMonitor()]);
  if (!monitor) return 'above';
  const nextSize = {
    width: Math.round(size.width * monitor.scaleFactor),
    height: Math.round(size.height * monitor.scaleFactor)
  };
  return launcherPopupPlacement(
    position,
    currentSize,
    nextSize,
    { x: monitor.workArea.position.x, y: monitor.workArea.position.y, width: monitor.workArea.size.width, height: monitor.workArea.size.height }
  );
}

export async function setLauncherExpanded(expanded: boolean, placement?: LauncherPopupPlacement) {
  return resizeLauncher(
    expanded ? launcherDimensions.expanded : launcherCompactSize(),
    !expanded,
    0,
    placement
  );
}

export async function setLauncherCompact() {
  await resizeLauncher(launcherCompactSize(), true);
}

export async function setLauncherContextMenuVisible(visible: boolean, placement?: LauncherPopupPlacement) {
  return resizeLauncher(
    visible ? launcherDimensions.menu : launcherCompactSize(),
    !visible,
    0,
    placement
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
