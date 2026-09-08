import { getCurrentWindow } from '@tauri-apps/api/window';

export type MainWindowAction = 'minimize' | 'toggle-maximize' | 'hide';

export type WindowOperations = {
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  hide: () => Promise<void>;
  isMaximized: () => Promise<boolean>;
};

const tauriWindowOperations: WindowOperations = {
  minimize: () => getCurrentWindow().minimize(),
  toggleMaximize: () => getCurrentWindow().toggleMaximize(),
  hide: () => getCurrentWindow().hide(),
  isMaximized: () => getCurrentWindow().isMaximized()
};

export async function runMainWindowAction(
  action: MainWindowAction,
  operations: WindowOperations = tauriWindowOperations,
): Promise<void> {
  if (action === 'minimize') return operations.minimize();
  if (action === 'toggle-maximize') return operations.toggleMaximize();
  return operations.hide();
}

export async function mainWindowIsMaximized(
  operations: WindowOperations = tauriWindowOperations,
): Promise<boolean> {
  return operations.isMaximized();
}
