import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
  getGlobalStat: vi.fn(),
  getSettings: vi.fn(),
  showLauncherPopup: vi.fn(),
  onGlobalStat: undefined as undefined | ((stat: { download_speed: number; num_active: number }) => void),
}));

vi.mock('../lib/api', () => ({
  isTauriRuntime: () => true,
  getDownloadGlobalStat: state.getGlobalStat,
  getSettings: state.getSettings,
  scanDownloadInputs: vi.fn(),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => undefined) }));
vi.mock('../lib/launcherWindow', () => ({
  anchorLauncherToWorkArea: vi.fn().mockResolvedValue(undefined),
  sendLauncherInput: vi.fn().mockResolvedValue(undefined),
  setLauncherWindowVisible: vi.fn().mockResolvedValue(undefined),
  showLauncherPopup: state.showLauncherPopup,
  startLauncherDragging: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../lib/downloadEvents', () => ({
  subscribeToDownloadEvents: vi.fn().mockImplementation(({ onGlobalStat }) => {
    state.onGlobalStat = onGlobalStat;
    return Promise.resolve(() => undefined);
  }),
}));

import { LauncherWindow } from './LauncherWindow';
import { setLauncherWindowVisible, showLauncherPopup, startLauncherDragging } from '../lib/launcherWindow';

beforeEach(() => {
  vi.clearAllMocks();
  state.onGlobalStat = undefined;
  state.getGlobalStat.mockResolvedValue({ download_speed: 524_288, num_active: 1 });
  state.getSettings.mockResolvedValue({ launcherVisibility: 'always' });
  state.showLauncherPopup.mockResolvedValue(undefined);
});

it.each(['clear-blue', 'mint', 'warm-gold', 'lavender', 'coral', 'deep-ocean', 'night-violet'])('keeps the fixed launcher viewport transparent with the %s theme', (theme) => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  document.documentElement.classList.add('launcher-window');
  document.documentElement.dataset.theme = theme;

  try {
    expect(getComputedStyle(document.documentElement).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(getComputedStyle(document.body).backgroundColor).toBe('rgba(0, 0, 0, 0)');
  } finally {
    document.documentElement.classList.remove('launcher-window');
    delete document.documentElement.dataset.theme;
    stylesheet.remove();
  }
});

it('opens the dedicated popup for each launcher click without changing the icon viewport', async () => {
  render(<LauncherWindow />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.click(trigger);
  await waitFor(() => expect(showLauncherPopup).toHaveBeenCalledWith('input'));
  fireEvent.contextMenu(trigger);
  await waitFor(() => expect(showLauncherPopup).toHaveBeenCalledWith('menu'));

  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menu', { name: '悬浮球菜单' })).not.toBeInTheDocument();
});

it('keeps the launcher visible for active tasks in download-only mode', async () => {
  state.getSettings.mockResolvedValueOnce({ launcherVisibility: 'downloading' });
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 1 });
  render(<LauncherWindow />);

  await waitFor(() => expect(setLauncherWindowVisible).toHaveBeenLastCalledWith(true));
});

it('starts native dragging from the fixed launcher icon', async () => {
  render(<LauncherWindow />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  await act(async () => {
    fireEvent.mouseDown(trigger, { button: 0, clientX: 20, clientY: 24 });
    fireEvent.mouseMove(trigger, { buttons: 1, clientX: 28, clientY: 31 });
  });

  expect(startLauncherDragging).toHaveBeenCalledOnce();
});
