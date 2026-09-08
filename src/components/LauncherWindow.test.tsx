import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({ onGlobalStat: undefined as undefined | ((stat: { download_speed: number; num_active: number }) => void) }));

vi.mock('../lib/api', () => ({ isTauriRuntime: () => true }));
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

it('keeps a compact live-status indicator visible and moves the speed into the expanded panel', async () => {
  const user = userEvent.setup();
  render(<LauncherWindow />);
  await act(async () => { state.onGlobalStat?.({ download_speed: 524_288, num_active: 2 }); });

  expect(screen.getByLabelText('下载速度 512 KB/s')).toHaveTextContent('512K');
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(screen.getByText(/512 KB\/s/)).toBeVisible();
});
