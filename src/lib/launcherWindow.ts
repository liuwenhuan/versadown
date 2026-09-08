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
type NativeWindowSize = { width: number; height: number };

export function launcherCornerPosition(workArea: WorkArea, windowSize: NativeWindowSize, scaleFactor: number) {
  const margin = launcherMargin * scaleFactor;
  return {
    x: Math.round(workArea.x + workArea.width - windowSize.width - margin),
    y: Math.round(workArea.y + workArea.height - windowSize.height - margin)
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
  await anchorLauncherToWorkArea();
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

export async function sendLauncherInput(input: string) {
  if (!isTauriRuntime()) return;
  const mainWindow = await Window.getByLabel('main');
  await mainWindow?.show();
  await mainWindow?.setFocus();
  await emitTo('main', 'launcher-parse', input);
}
