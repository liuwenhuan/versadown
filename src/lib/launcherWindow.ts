import { emitTo } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalSize, Window } from '@tauri-apps/api/window';
import { isTauriRuntime } from './api';

export const LAUNCHER_WINDOW_LABEL = 'launcher';
export const launcherDimensions = {
  compact: { width: 48, height: 48 },
  expanded: { width: 470, height: 300 }
} as const;

export function isLauncherWindow() {
  return isTauriRuntime() && getCurrentWindow().label === LAUNCHER_WINDOW_LABEL;
}

export async function setLauncherExpanded(expanded: boolean) {
  if (!isTauriRuntime()) return;
  const size = expanded ? launcherDimensions.expanded : launcherDimensions.compact;
  await getCurrentWindow().setSize(new LogicalSize(size.width, size.height));
}

export async function sendLauncherInput(input: string) {
  if (!isTauriRuntime()) return;
  const mainWindow = await Window.getByLabel('main');
  await mainWindow?.show();
  await mainWindow?.setFocus();
  await emitTo('main', 'launcher-parse', input);
}
