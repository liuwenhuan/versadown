import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
  getGlobalStat: vi.fn(),
  getEngineState: vi.fn(),
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  getManagerVisible: vi.fn(),
  onGlobalStat: undefined as undefined | ((stat: { download_speed: number; num_active: number }) => void)
}));

vi.mock('../lib/api', () => ({
  isTauriRuntime: () => true,
  getDownloadGlobalStat: state.getGlobalStat,
  getDownloadEngineState: state.getEngineState,
  getSettings: state.getSettings,
  updateSettings: state.updateSettings
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => undefined) }));
vi.mock('../lib/launcherWindow', () => ({
  anchorLauncherToWorkArea: vi.fn().mockResolvedValue(undefined), sendLauncherInput: vi.fn().mockResolvedValue(undefined),
  setLauncherCompactShape: vi.fn().mockResolvedValue(undefined), setLauncherExpanded: vi.fn().mockResolvedValue(undefined), setLauncherLiveSpeedVisible: vi.fn().mockResolvedValue(undefined), setLauncherContextMenuVisible: vi.fn().mockResolvedValue(undefined),
  setLauncherWindowVisible: vi.fn().mockResolvedValue(undefined), runLauncherMenuAction: vi.fn().mockResolvedValue(undefined), getLauncherManagerVisible: state.getManagerVisible, showLauncherMenu: vi.fn().mockResolvedValue(undefined), startLauncherDragging: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('../lib/downloadEvents', () => ({
  formatCompactSpeed: (value: number) => `${Math.round(value / 1024)}K`,
  subscribeToDownloadEvents: vi.fn().mockImplementation(({ onGlobalStat }) => {
    state.onGlobalStat = onGlobalStat;
    return Promise.resolve(() => undefined);
  })
}));

import { LauncherWindow } from './LauncherWindow';
import { setLauncherContextMenuVisible, setLauncherLiveSpeedVisible, setLauncherWindowVisible, startLauncherDragging } from '../lib/launcherWindow';

beforeEach(() => {
  state.onGlobalStat = undefined;
  state.getGlobalStat.mockReset();
  state.getEngineState.mockReset();
  state.getSettings.mockReset();
  state.updateSettings.mockReset();
  state.getManagerVisible.mockReset();
  state.getSettings.mockResolvedValue({
    debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always',
    proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }
  });
  state.getManagerVisible.mockResolvedValue(true);
});

it('requests an initial global-stat snapshot after subscribing so startup events cannot be lost', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 2 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });

  render(<LauncherWindow />);

  expect(await screen.findByLabelText('下载速度 512 KB/s')).toHaveTextContent('512K');
  expect(state.getGlobalStat).toHaveBeenCalledOnce();
  expect(setLauncherLiveSpeedVisible).toHaveBeenLastCalledWith(true);
});

it('keeps a compact live-status indicator visible and moves the speed into the expanded panel', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 0 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  const user = userEvent.setup();
  render(<LauncherWindow />);
  await waitFor(() => expect(state.onGlobalStat).toBeDefined());
  await act(async () => { state.onGlobalStat?.({ download_speed: 524_288, num_active: 2 }); });

  expect(screen.getByLabelText('下载速度 512 KB/s')).toHaveTextContent('512K');
  expect(setLauncherLiveSpeedVisible).toHaveBeenLastCalledWith(true);
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(screen.getByText(/512 KB\/s/)).toBeVisible();
});

it('shows the zero-speed status when aria2 still has an active task', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  render(<LauncherWindow />);

  expect(await screen.findByLabelText('下载速度 0 KB/s')).toHaveTextContent('0K');
  expect(setLauncherLiveSpeedVisible).toHaveBeenLastCalledWith(true);
});

it('shows the zero-speed status for an active task in download-only mode', async () => {
  state.getSettings.mockResolvedValueOnce({
    debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'downloading',
    proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }
  });
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  render(<LauncherWindow />);

  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 25)); });
  expect(screen.getByLabelText('下载速度 0 KB/s')).toHaveTextContent('0K');
  expect(setLauncherWindowVisible).toHaveBeenLastCalledWith(true);
  expect(setLauncherLiveSpeedVisible).toHaveBeenLastCalledWith(true);
});

it('keeps live-speed status interactive for the launcher context menu', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  render(<LauncherWindow />);

  const speed = await screen.findByLabelText('下载速度 512 KB/s');
  fireEvent.contextMenu(speed);

  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
  expect(screen.getByLabelText('下载速度 512 KB/s')).toBeVisible();
  expect(speed.closest('main')).toHaveClass('launcher-speed-active', 'launcher-menu-open');
});

it('waits for the native viewport before rendering the launcher context menu', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  let releaseNativeResize: (() => void) | undefined;
  vi.mocked(setLauncherContextMenuVisible).mockImplementationOnce(() => new Promise<void>((resolve) => {
    releaseNativeResize = resolve;
  }));
  render(<LauncherWindow />);

  fireEvent.contextMenu(await screen.findByLabelText('下载速度 512 KB/s'));

  expect(screen.queryByRole('menu', { name: '悬浮球菜单' })).not.toBeInTheDocument();
  await act(async () => { releaseNativeResize?.(); });
  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
});

it('waits for native menu collapse before restoring the live launcher canvas', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  render(<LauncherWindow />);

  fireEvent.contextMenu(await screen.findByLabelText('下载速度 512 KB/s'));
  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();

  let releaseNativeResize: (() => void) | undefined;
  vi.mocked(setLauncherContextMenuVisible).mockImplementationOnce(() => new Promise<void>((resolve) => {
    releaseNativeResize = resolve;
  }));
  vi.mocked(setLauncherLiveSpeedVisible).mockClear();
  fireEvent.keyDown(window, { key: 'Escape' });

  expect(screen.queryByRole('menu', { name: '悬浮球菜单' })).not.toBeInTheDocument();
  expect(setLauncherLiveSpeedVisible).not.toHaveBeenCalled();
  await act(async () => { releaseNativeResize?.(); });
});

it('opens the download input when clicking the live-speed status area', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  render(<LauncherWindow />);

  fireEvent.click(await screen.findByLabelText('下载速度 512 KB/s'));

  expect(await screen.findByRole('dialog', { name: '新建下载' })).toBeVisible();
});

it('starts native dragging from the live-speed status area', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  render(<LauncherWindow />);

  const speed = await screen.findByLabelText('下载速度 512 KB/s');
  fireEvent.mouseDown(speed, { button: 0, clientX: 80, clientY: 24 });
  fireEvent.mouseMove(speed, { buttons: 1, clientX: 88, clientY: 31 });

  expect(startLauncherDragging).toHaveBeenCalledOnce();
});

it('uses the main task-menu visual system and reflects the current main-window visibility', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 0 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  state.getManagerVisible.mockResolvedValueOnce(false);
  const user = userEvent.setup();
  render(<LauncherWindow />);

  fireEvent.contextMenu(screen.getByRole('button', { name: '打开下载输入' }));

  const menu = await screen.findByRole('menu', { name: '悬浮球菜单' });
  expect(menu).toHaveClass('task-context-menu');
  expect(screen.getByRole('menuitem', { name: '显示主界面' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '悬浮球显示' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '本次隐藏悬浮球' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('menuitem', { name: '悬浮球显示' }));
  expect(await screen.findByRole('menu', { name: '悬浮球显示设置' })).toBeVisible();
  expect(screen.getByRole('menuitemradio', { name: '总是显示' })).toHaveAttribute('aria-checked', 'true');
  expect(screen.getByRole('menuitemradio', { name: '仅下载时显示' })).toBeVisible();
  expect(screen.getByRole('menuitemradio', { name: '关闭显示' })).toBeVisible();
});
