import { emitTo } from '@tauri-apps/api/event';
import { currentMonitor, getCurrentWindow, LogicalSize, PhysicalPosition, Window } from '@tauri-apps/api/window';
import { isTauriRuntime } from './api';

export const LAUNCHER_WINDOW_LABEL = 'launcher';
export const launcherDimensions = {
  compact: { width: 48, height: 48 },
  expanded: { width: 470, height: 300 }
} as const;

const launcherMargin = 24;

type WorkArea = { x: number; y: number; width: number; height: number };

export function launcherCornerPosition(workArea: WorkArea, scaleFactor: number, expanded = false) {
  const size = expanded ? launcherDimensions.expanded : launcherDimensions.compact;
  const occupiedWidth = (size.width + launcherMargin) * scaleFactor;
  const occupiedHeight = (size.height + launcherMargin) * scaleFactor;
  return {
    x: Math.round(workArea.x + workArea.width - occupiedWidth),
    y: Math.round(workArea.y + workArea.height - occupiedHeight)
  };
}

export function isLauncherWindow() {
  return isTauriRuntime() && getCurrentWindow().label === LAUNCHER_WINDOW_LABEL;
}

export async function setLauncherExpanded(expanded: boolean) {
  if (!isTauriRuntime()) return;
  const size = expanded ? launcherDimensions.expanded : launcherDimensions.compact;
  const launcher = getCurrentWindow();
  await launcher.setSize(new LogicalSize(size.width, size.height));
  await anchorLauncherToWorkArea(expanded);
}

export async function anchorLauncherToWorkArea(expanded = false) {
  if (!isTauriRuntime()) return;
  const launcher = getCurrentWindow();
  const monitor = await currentMonitor();
  if (!monitor) return;
  const { position, size } = monitor.workArea;
  const corner = launcherCornerPosition({ x: position.x, y: position.y, width: size.width, height: size.height }, monitor.scaleFactor, expanded);
  await launcher.setPosition(new PhysicalPosition(corner.x, corner.y));
}

export async function sendLauncherInput(input: string) {
  if (!isTauriRuntime()) return;
  const mainWindow = await Window.getByLabel('main');
  await mainWindow?.show();
  await mainWindow?.setFocus();
  await emitTo('main', 'launcher-parse', input);
}
