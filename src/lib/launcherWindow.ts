import { emitTo } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { currentMonitor, getCurrentWindow, PhysicalPosition, Window } from '@tauri-apps/api/window';
import { isTauriRuntime } from './api';

export const LAUNCHER_WINDOW_LABEL = 'launcher';
export const launcherCanvas = {
  width: 410,
  height: 720,
  popupSlotHeight: 328,
  iconSize: 64,
  iconTop: 328,
  margin: 24,
} as const;

type WorkArea = { x: number; y: number; width: number; height: number };
type Rect = { x: number; y: number; width: number; height: number };
export type LauncherPopupPlacement = 'above' | 'below';
export type LauncherInputRegion = { x: number; y: number; width: number; height: number };
export type LauncherPointerPosition = { x: number; y: number };
export type LauncherPointerDragSession = {
  windowPosition: { x: number; y: number };
  pointerPosition: LauncherPointerPosition;
  scaleFactor: number;
};

export const launcherIconRegion: LauncherInputRegion = {
  x: launcherCanvas.width - launcherCanvas.iconSize,
  y: launcherCanvas.iconTop,
  width: launcherCanvas.iconSize,
  height: launcherCanvas.iconSize,
};

export async function setLauncherInputRegions(regions: LauncherInputRegion[]) {
  if (!isTauriRuntime()) return;
  if (regions.length === 0 || regions.some((region) => (
    !Number.isFinite(region.x)
    || !Number.isFinite(region.y)
    || !Number.isFinite(region.width)
    || !Number.isFinite(region.height)
    || region.width <= 0
    || region.height <= 0
  ))) {
    throw new Error('悬浮窗输入区域无效');
  }
  await invoke('set_launcher_input_regions', { regions });
}

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

// Dragging the full fixed canvas with the native titlebar loop makes Windows
// constrain the canvas itself to the work area.  The icon lives 328 CSS px
// inside that canvas, so it can never reach the top edge.  Keep tracking the
// captured icon pointer instead, allowing the transparent canvas to extend
// above the work area while the visible icon remains under the pointer.
export function launcherPointerDragPosition(
  session: LauncherPointerDragSession,
  pointerPosition: LauncherPointerPosition,
) {
  return {
    x: session.windowPosition.x + Math.round((pointerPosition.x - session.pointerPosition.x) * session.scaleFactor),
    y: session.windowPosition.y + Math.round((pointerPosition.y - session.pointerPosition.y) * session.scaleFactor),
  };
}

export async function beginLauncherPointerDrag(
  pointerPosition: LauncherPointerPosition,
): Promise<LauncherPointerDragSession> {
  const launcher = getCurrentWindow();
  const [windowPosition, monitor] = await Promise.all([launcher.outerPosition(), currentMonitor()]);
  return {
    windowPosition,
    pointerPosition,
    scaleFactor: monitor?.scaleFactor ?? 1,
  };
}

export async function moveLauncherPointerDrag(
  session: LauncherPointerDragSession,
  pointerPosition: LauncherPointerPosition,
) {
  const position = launcherPointerDragPosition(session, pointerPosition);
  await getCurrentWindow().setPosition(new PhysicalPosition(position.x, position.y));
}

export function launcherPopupPlacementFromIcon(
  icon: Rect,
  popupHeight: number,
  workArea: WorkArea,
): LauncherPopupPlacement {
  return icon.y - popupHeight < workArea.y ? 'below' : 'above';
}

export function isLauncherWindow() {
  return isTauriRuntime() && getCurrentWindow().label === LAUNCHER_WINDOW_LABEL;
}

export async function getLauncherPopupPlacement(): Promise<LauncherPopupPlacement> {
  if (!isTauriRuntime()) return 'above';
  const launcher = getCurrentWindow();
  const [position, monitor] = await Promise.all([launcher.outerPosition(), currentMonitor()]);
  if (!monitor) return 'above';
  return launcherPopupPlacementFromIcon(
    launcherIconScreenRect(position, monitor.scaleFactor),
    Math.round(launcherCanvas.popupSlotHeight * monitor.scaleFactor),
    { x: monitor.workArea.position.x, y: monitor.workArea.position.y, width: monitor.workArea.size.width, height: monitor.workArea.size.height }
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
  const workArea = monitor.workArea;
  const corner = launcherCanvasPosition(
    { x: workArea.position.x, y: workArea.position.y, width: workArea.size.width, height: workArea.size.height },
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
