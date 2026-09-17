import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { readFileSync } from 'node:fs';
import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
  getGlobalStat: vi.fn(),
  getSettings: vi.fn(),
  scanDownloadInputs: vi.fn(),
  updateSettings: vi.fn(),
  getManagerVisible: vi.fn(),
  getLauncherPopupPlacement: vi.fn(),
  onGlobalStat: undefined as undefined | ((stat: { download_speed: number; num_active: number }) => void)
}));

vi.mock('../lib/api', () => ({
  isTauriRuntime: () => true,
  getDownloadGlobalStat: state.getGlobalStat,
  getSettings: state.getSettings,
  scanDownloadInputs: state.scanDownloadInputs,
  updateSettings: state.updateSettings
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => undefined) }));
vi.mock('../lib/launcherWindow', () => ({
  anchorLauncherToWorkArea: vi.fn().mockResolvedValue(undefined), openLauncherManager: vi.fn().mockResolvedValue(undefined), sendLauncherInput: vi.fn().mockResolvedValue(undefined),
  setLauncherCompactShape: vi.fn().mockResolvedValue(undefined), setLauncherCompact: vi.fn().mockResolvedValue(undefined), setLauncherExpanded: vi.fn().mockResolvedValue(undefined), setLauncherContextMenuVisible: vi.fn().mockResolvedValue(undefined),
  getLauncherPopupPlacement: state.getLauncherPopupPlacement,
  launcherDimensions: { compact: { width: 64, height: 64 }, menu: { width: 304, height: 392 }, expanded: { width: 410, height: 364 } },
  setLauncherWindowVisible: vi.fn().mockResolvedValue(undefined), runLauncherMenuAction: vi.fn().mockResolvedValue(undefined), getLauncherManagerVisible: state.getManagerVisible, showLauncherMenu: vi.fn().mockResolvedValue(undefined), startLauncherDragging: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('../lib/downloadEvents', () => ({
  formatSpeed: (value: number | undefined) => value === 524_288 ? '512 KB/s' : `${value ?? '未知'} B/s`,
  subscribeToDownloadEvents: vi.fn().mockImplementation(({ onGlobalStat }) => {
    state.onGlobalStat = onGlobalStat;
    return Promise.resolve(() => undefined);
  })
}));

import { LauncherWindow } from './LauncherWindow';
import { getLauncherPopupPlacement, setLauncherCompact, setLauncherContextMenuVisible, setLauncherExpanded, setLauncherWindowVisible, startLauncherDragging } from '../lib/launcherWindow';

it('keeps the floating-display menu heading to one row after its helper copy is removed', () => {
  const styles = readFileSync('src/styles.css', 'utf8');

  expect(styles).toContain('.launcher-context-menu>header button{grid-column:2;grid-row:1;');
});

it.each(['clear-blue', 'mint', 'warm-gold', 'lavender', 'coral', 'deep-ocean', 'night-violet'])('keeps the launcher viewport transparent with the %s theme', (theme) => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  const themeCanvas = document.createElement('style');
  themeCanvas.textContent = `:root[data-theme="${theme}"]{background:rgb(1,2,3)!important}`;
  document.head.appendChild(themeCanvas);
  document.head.appendChild(stylesheet);
  document.documentElement.classList.add('launcher-window');
  document.documentElement.dataset.theme = theme;

  try {
    expect(getComputedStyle(document.documentElement).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(getComputedStyle(document.body).backgroundColor).toBe('rgba(0, 0, 0, 0)');
  } finally {
    document.documentElement.classList.remove('launcher-window');
    delete document.documentElement.dataset.theme;
    themeCanvas.remove();
    stylesheet.remove();
  }
});

beforeEach(() => {
  vi.clearAllMocks();
  state.onGlobalStat = undefined;
  state.getGlobalStat.mockReset();
  state.getSettings.mockReset();
  state.scanDownloadInputs.mockReset();
  state.updateSettings.mockReset();
  state.getManagerVisible.mockReset();
  state.getLauncherPopupPlacement.mockReset();
  state.getSettings.mockResolvedValue({
    downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always',
    proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' },
    magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' },
    aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 4, btSeedRatio: 1.5 }
  });
  state.getManagerVisible.mockResolvedValue(true);
  state.getLauncherPopupPlacement.mockResolvedValue('above');
});

it('requests an initial global-stat snapshot after subscribing so startup events cannot be lost', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 2 });

  render(<LauncherWindow />);

  await waitFor(() => expect(setLauncherCompact).toHaveBeenCalled());
  expect(state.getGlobalStat).toHaveBeenCalledOnce();
  expect(screen.queryByLabelText('下载速度 512 KB/s')).not.toBeInTheDocument();
});

it('keeps only the hexagon when downloads are active', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 2 });

  render(<LauncherWindow />);

  await waitFor(() => expect(setLauncherCompact).toHaveBeenLastCalledWith());
  expect(screen.queryByLabelText('下载速度 512 KB/s')).not.toBeInTheDocument();
});

it('shows the current total speed above the launcher menu commands', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 2 });
  render(<LauncherWindow />);

  fireEvent.contextMenu(screen.getByRole('button', { name: '打开下载输入' }));

  const summary = await screen.findByLabelText('当前整体下载速度');
  expect(summary).toHaveTextContent('512 KB/s');
  const firstCommand = screen.getByRole('menuitem', { name: '隐藏主界面' });
  expect(summary.compareDocumentPosition(firstCommand) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
});

it('keeps the compact launcher visible for an active task in download-only mode', async () => {
  state.getSettings.mockResolvedValueOnce({
    downloadDirectory: 'C:\\Downloads', launcherVisibility: 'downloading',
    proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' },
    magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' },
    aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 4, btSeedRatio: 1.5 }
  });
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 1 });
  render(<LauncherWindow />);

  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 25)); });
  expect(setLauncherWindowVisible).toHaveBeenLastCalledWith(true);
  expect(screen.queryByLabelText('下载速度 0 KB/s')).not.toBeInTheDocument();
});

it('opens the launcher context menu from the compact hexagon', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  render(<LauncherWindow />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  fireEvent.contextMenu(trigger);

  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
  expect(trigger.closest('main')).toHaveClass('launcher-menu-open');
});

it('waits for the native viewport before rendering the launcher context menu', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  let releaseNativeResize: (() => void) | undefined;
  vi.mocked(setLauncherContextMenuVisible).mockImplementationOnce(() => new Promise<void>((resolve) => {
    releaseNativeResize = resolve;
  }));
  render(<LauncherWindow />);

  fireEvent.contextMenu(screen.getByRole('button', { name: '打开下载输入' }));

  expect(screen.queryByRole('menu', { name: '悬浮球菜单' })).not.toBeInTheDocument();
  await waitFor(() => expect(setLauncherContextMenuVisible).toHaveBeenCalledWith(true, 'above'));
  await act(async () => { releaseNativeResize?.(); });
  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
});

it('keeps the compact launcher visible and screen-aligned while opening its menu viewport', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  let releaseNativeResize: (() => void) | undefined;
  vi.mocked(setLauncherContextMenuVisible).mockImplementationOnce(() => new Promise<void>((resolve) => {
    releaseNativeResize = resolve;
  }));
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);

  try {
    render(<LauncherWindow />);
    const trigger = screen.getByRole('button', { name: '打开下载输入' });
    const shell = trigger.closest('main')!;

    fireEvent.contextMenu(trigger);

    expect(shell).toHaveClass('launcher-viewport-transitioning');
    expect(trigger).toBeVisible();
    await waitFor(() => expect(setLauncherContextMenuVisible).toHaveBeenCalledWith(true, 'above'));
    await act(async () => { releaseNativeResize?.(); });

    expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
    const interactionTarget = screen.getByRole('button', { name: '打开下载输入' }).closest('.launcher-interaction-target')!;
    expect(getComputedStyle(interactionTarget).right).toBe('0px');
  } finally {
    stylesheet.remove();
  }
});

it('anchors a context menu below the launcher when the upper work area is insufficient', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getLauncherPopupPlacement.mockResolvedValueOnce('below');
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);

  try {
    render(<LauncherWindow />);
    const trigger = screen.getByRole('button', { name: '打开下载输入' });
    fireEvent.contextMenu(trigger);

    await waitFor(() => expect(getLauncherPopupPlacement).toHaveBeenCalled());
    await waitFor(() => expect(setLauncherContextMenuVisible).toHaveBeenCalledWith(true, 'below'));
    expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
    const shell = trigger.closest('main')!;
    expect(shell).toHaveClass('launcher-popup-below');
    const interactionTarget = trigger.closest('.launcher-interaction-target')!;
    expect(getComputedStyle(interactionTarget).top).toBe('0px');
    expect(getComputedStyle(interactionTarget).bottom).toBe('auto');
    expect(getComputedStyle(screen.getByRole('menu', { name: '悬浮球菜单' })).top).toBe('72px');
  } finally {
    stylesheet.remove();
  }
});

it('anchors quick-add below the launcher when the upper work area is insufficient', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  state.getLauncherPopupPlacement.mockResolvedValueOnce('below');
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);

  try {
    render(<LauncherWindow />);
    const trigger = screen.getByRole('button', { name: '打开下载输入' });
    fireEvent.click(trigger);

    await waitFor(() => expect(setLauncherExpanded).toHaveBeenCalledWith(true, 'below'));
    const dialog = await screen.findByRole('dialog', { name: '新建下载' });
    expect(trigger.closest('main')).toHaveClass('launcher-popup-below');
    expect(getComputedStyle(trigger.closest('.launcher-interaction-target')!).top).toBe('0px');
    expect(getComputedStyle(dialog).top).toBe('64px');
  } finally {
    stylesheet.remove();
  }
});

it('keeps the quick-add content hidden until its native viewport is ready', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  let releaseNativeResize: (() => void) | undefined;
  let hiddenWhenNativeResizeStarted = false;
  vi.mocked(setLauncherExpanded).mockImplementationOnce(() => new Promise<void>((resolve) => {
    const dialog = document.querySelector<HTMLElement>('[role="dialog"][aria-label="新建下载"]');
    hiddenWhenNativeResizeStarted = document.querySelector('.launcher-viewport-transitioning') !== null
      && Boolean(dialog && getComputedStyle(dialog).visibility === 'hidden');
    releaseNativeResize = resolve;
  }));
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);

  try {
    render(<LauncherWindow />);
    fireEvent.click(screen.getByRole('button', { name: '打开下载输入' }));

    await waitFor(() => expect(setLauncherExpanded).toHaveBeenCalledWith(true, 'above'));
    const dialog = document.querySelector<HTMLElement>('[role="dialog"][aria-label="新建下载"]')!;
    expect(dialog.closest('main')).toHaveClass('launcher-viewport-transitioning');
    expect(dialog).not.toBeVisible();
    expect(hiddenWhenNativeResizeStarted).toBe(true);
    await act(async () => { releaseNativeResize?.(); });
    expect(dialog).toBeVisible();
  } finally {
    stylesheet.remove();
  }
});

it('waits for native menu collapse before restoring the compact launcher canvas', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  render(<LauncherWindow />);

  fireEvent.contextMenu(screen.getByRole('button', { name: '打开下载输入' }));
  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();

  let releaseNativeResize: (() => void) | undefined;
  vi.mocked(setLauncherContextMenuVisible).mockImplementationOnce(() => new Promise<void>((resolve) => {
    releaseNativeResize = resolve;
  }));
  vi.mocked(setLauncherCompact).mockClear();
  fireEvent.keyDown(window, { key: 'Escape' });

  expect(screen.queryByRole('menu', { name: '悬浮球菜单' })).not.toBeInTheDocument();
  expect(setLauncherCompact).not.toHaveBeenCalled();
  await waitFor(() => expect(setLauncherContextMenuVisible).toHaveBeenLastCalledWith(false, 'above'));
  await act(async () => { releaseNativeResize?.(); });
  await waitFor(() => expect(setLauncherCompact).toHaveBeenCalled());
});

it('opens the download input when clicking the compact launcher', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  render(<LauncherWindow />);

  fireEvent.click(screen.getByRole('button', { name: '打开下载输入' }));

  expect(await screen.findByRole('dialog', { name: '新建下载' })).toBeVisible();
  await waitFor(() => expect(setLauncherExpanded).toHaveBeenCalledWith(true, 'above'));
});

it('starts native dragging from the compact launcher', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 1 });
  render(<LauncherWindow />);
  await waitFor(() => expect(state.onGlobalStat).toBeDefined());

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  await act(async () => {
    fireEvent.mouseDown(trigger, { button: 0, clientX: 20, clientY: 24 });
    fireEvent.mouseMove(trigger, { buttons: 1, clientX: 28, clientY: 31 });
  });

  expect(startLauncherDragging).toHaveBeenCalledOnce();
});

it('uses the main task-menu visual system and reflects the current main-window visibility', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 0 });
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
  expect(screen.queryByText('选择显示时机')).not.toBeInTheDocument();
  expect(screen.getByRole('menuitemradio', { name: '总是显示' })).toHaveAttribute('aria-checked', 'true');
  expect(screen.getByRole('menuitemradio', { name: '仅下载时显示' })).toBeVisible();
  expect(screen.getByRole('menuitemradio', { name: '关闭显示' })).toBeVisible();
});
