import { emitTo } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { currentMonitor, getCurrentWindow, PhysicalPosition, PhysicalSize, Window } from '@tauri-apps/api/window';
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

function launcherDisplayScaleFactor(nativeScaleFactor = 1) {
  // On fractional-scale X11 desktops WebKitGTK's CSS viewport follows
  // devicePixelRatio, while Tao can report an unscaled monitor factor. Use
  // the larger valid value so the fixed CSS canvas and native input shape
  // share one physical coordinate system.
  const webviewScaleFactor = Number.isFinite(window.devicePixelRatio) && window.devicePixelRatio > 0
    ? window.devicePixelRatio
    : 1;
  return Math.max(nativeScaleFactor, webviewScaleFactor, 1);
}

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
  await invoke('set_launcher_input_regions', {
    regions,
    scaleFactor: launcherDisplayScaleFactor(),
  });
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

// WebView implementations disagree on whether screenX/screenY are CSS or
// physical pixels. Compare the press location with the known physical icon
// location: a direct match is already physical (WebKitGTK/X11), otherwise a
// CSS-coordinate value needs the display scale to become physical.
export function launcherPointerCoordinateScaleFactor(
  windowPosition: { x: number; y: number },
  pointerPosition: LauncherPointerPosition,
  grabOffset: LauncherPointerPosition | undefined,
  displayScaleFactor: number,
) {
  const scaleFactor = Math.max(displayScaleFactor, 1);
  if (!grabOffset || scaleFactor === 1) return scaleFactor;
  const expectedPhysical = {
    x: windowPosition.x + Math.round(grabOffset.x * scaleFactor),
    y: windowPosition.y + Math.round(grabOffset.y * scaleFactor),
  };
  const directDistance = Math.abs(pointerPosition.x - expectedPhysical.x)
    + Math.abs(pointerPosition.y - expectedPhysical.y);
  const scaledDistance = Math.abs(pointerPosition.x * scaleFactor - expectedPhysical.x)
    + Math.abs(pointerPosition.y * scaleFactor - expectedPhysical.y);
  return directDistance <= scaledDistance ? 1 : scaleFactor;
}

export async function beginLauncherPointerDrag(
  pointerPosition: LauncherPointerPosition,
  grabOffset?: LauncherPointerPosition,
): Promise<LauncherPointerDragSession> {
  const launcher = getCurrentWindow();
  const [windowPosition, monitor] = await Promise.all([launcher.outerPosition(), currentMonitor()]);
  const displayScaleFactor = launcherDisplayScaleFactor(monitor?.scaleFactor);
  return {
    windowPosition,
    pointerPosition,
    scaleFactor: launcherPointerCoordinateScaleFactor(
      windowPosition,
      pointerPosition,
      grabOffset,
      displayScaleFactor,
    ),
  };
}

// Resolves after native mouse release on Windows; false selects the browser
// fallback on platforms without this adapter. No per-move IPC is needed.
export async function runNativeLauncherDrag(grabOffset?: LauncherPointerPosition): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  return invoke<boolean>('drag_launcher_window', { grabOffset: grabOffset ?? null });
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
    launcherIconScreenRect(position, launcherDisplayScaleFactor(monitor.scaleFactor)),
    Math.round(launcherCanvas.popupSlotHeight * launcherDisplayScaleFactor(monitor.scaleFactor)),
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
  const scaleFactor = launcherDisplayScaleFactor(monitor.scaleFactor);
  await launcher.setSize(new PhysicalSize(
    Math.round(launcherCanvas.width * scaleFactor),
    Math.round(launcherCanvas.height * scaleFactor),
  ));
  const corner = launcherCanvasPosition(
    { x: workArea.position.x, y: workArea.position.y, width: workArea.size.width, height: workArea.size.height },
    scaleFactor
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

/** Activates the manager and switches it to the all-tasks tab. */
export async function openLauncherAllTasks() {
  if (!isTauriRuntime()) return;
  await openLauncherManager();
  await emitTo('main', 'launcher-open-all-tasks');
}

/** Opens the manager's normal new-task dialog only for accepted dropped input. */
export async function openLauncherNewDownload(input: string) {
  if (!isTauriRuntime() || !input.trim()) return;
  await openLauncherManager();
  await emitTo('main', 'launcher-new-download', input);
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
