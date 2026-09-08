import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
  getGlobalStat: vi.fn(),
  getEngineState: vi.fn(),
  onGlobalStat: undefined as undefined | ((stat: { download_speed: number; num_active: number }) => void)
}));

vi.mock('../lib/api', () => ({
  isTauriRuntime: () => true,
  getDownloadGlobalStat: state.getGlobalStat,
  getDownloadEngineState: state.getEngineState
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => undefined) }));
vi.mock('../lib/launcherWindow', () => ({
  anchorLauncherToWorkArea: vi.fn().mockResolvedValue(undefined), sendLauncherInput: vi.fn(),
  setLauncherCompactShape: vi.fn(), setLauncherExpanded: vi.fn(), showLauncherMenu: vi.fn(), startLauncherDragging: vi.fn()
}));
vi.mock('../lib/downloadEvents', () => ({
  formatCompactSpeed: (value: number) => `${Math.round(value / 1024)}K`,
  subscribeToDownloadEvents: vi.fn().mockImplementation(({ onGlobalStat }) => {
    state.onGlobalStat = onGlobalStat;
    return Promise.resolve(() => undefined);
  })
}));

import { LauncherWindow } from './LauncherWindow';

beforeEach(() => {
  state.onGlobalStat = undefined;
  state.getGlobalStat.mockReset();
  state.getEngineState.mockReset();
});

it('requests an initial global-stat snapshot after subscribing so startup events cannot be lost', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 524_288, num_active: 2 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });

  render(<LauncherWindow />);

  expect(await screen.findByLabelText('下载速度 512 KB/s')).toHaveTextContent('512K');
  expect(state.getGlobalStat).toHaveBeenCalledOnce();
});

it('keeps a compact live-status indicator visible and moves the speed into the expanded panel', async () => {
  state.getGlobalStat.mockResolvedValueOnce({ download_speed: 0, num_active: 0 });
  state.getEngineState.mockResolvedValueOnce({ available: true, retrying: false });
  const user = userEvent.setup();
  render(<LauncherWindow />);
  await waitFor(() => expect(state.onGlobalStat).toBeDefined());
  await act(async () => { state.onGlobalStat?.({ download_speed: 524_288, num_active: 2 }); });

  expect(screen.getByLabelText('下载速度 512 KB/s')).toHaveTextContent('512K');
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(screen.getByText(/512 KB\/s/)).toBeVisible();
});
