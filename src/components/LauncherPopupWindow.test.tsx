import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
  popupStateListener: undefined as undefined | ((event: { payload: { mode: 'input' | 'menu'; placement: 'above' | 'below' } }) => void),
  hideLauncherPopup: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockImplementation(async (event: string, listener: typeof state.popupStateListener) => {
    if (event === 'launcher-popup-state') state.popupStateListener = listener;
    return () => { if (event === 'launcher-popup-state') state.popupStateListener = undefined; };
  }),
}));
vi.mock('../lib/api', () => ({
  isTauriRuntime: () => true,
  getDownloadGlobalStat: vi.fn().mockResolvedValue({ download_speed: 0, num_active: 0 }),
  getSettings: vi.fn().mockResolvedValue({ launcherVisibility: 'always' }),
  scanDownloadInputs: vi.fn(),
  updateSettings: vi.fn(),
}));
vi.mock('../lib/launcherWindow', () => ({
  hideLauncherPopup: state.hideLauncherPopup,
  getLauncherManagerVisible: vi.fn().mockResolvedValue(true),
  openLauncherManager: vi.fn().mockResolvedValue(undefined),
  runLauncherMenuAction: vi.fn().mockResolvedValue(undefined),
  sendLauncherInput: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../lib/downloadEvents', () => ({
  formatSpeed: () => '0 B/s',
  subscribeToDownloadEvents: vi.fn().mockResolvedValue(() => undefined),
}));

import { LauncherPopupWindow } from './LauncherPopupWindow';

beforeEach(() => {
  vi.clearAllMocks();
  state.popupStateListener = undefined;
  state.hideLauncherPopup.mockResolvedValue(undefined);
});

it('renders input content when the native popup state requests input mode', async () => {
  render(<LauncherPopupWindow />);
  await waitFor(() => expect(state.popupStateListener).toBeDefined());
  await act(async () => state.popupStateListener?.({ payload: { mode: 'input', placement: 'above' } }));

  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
});

it('hides the input popup when its window loses focus', async () => {
  render(<LauncherPopupWindow />);
  await waitFor(() => expect(state.popupStateListener).toBeDefined());
  await act(async () => state.popupStateListener?.({ payload: { mode: 'input', placement: 'above' } }));

  fireEvent.blur(window);

  expect(state.hideLauncherPopup).toHaveBeenCalledOnce();
});

it('renders menu content and hides the separate popup when it closes', async () => {
  render(<LauncherPopupWindow />);
  await waitFor(() => expect(state.popupStateListener).toBeDefined());
  await act(async () => state.popupStateListener?.({ payload: { mode: 'menu', placement: 'below' } }));

  expect(screen.getByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
  fireEvent.keyDown(window, { key: 'Escape' });
  expect(state.hideLauncherPopup).toHaveBeenCalledOnce();
});
