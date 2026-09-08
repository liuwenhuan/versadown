import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';

const launcherMocks = vi.hoisted(() => ({
  anchorLauncherToWorkArea: vi.fn(() => Promise.resolve()),
  sendLauncherInput: vi.fn(),
  setLauncherCompactShape: vi.fn(() => Promise.resolve()),
  setLauncherExpanded: vi.fn(() => Promise.resolve()),
  showLauncherMenu: vi.fn(() => Promise.resolve()),
  startLauncherDragging: vi.fn(() => Promise.resolve()),
}));

vi.mock('../lib/launcherWindow', () => launcherMocks);

import { LauncherWindow } from './LauncherWindow';

it('keeps the launcher dialog open and displays an error when sending input fails', async () => {
  const user = userEvent.setup();
  launcherMocks.sendLauncherInput.mockRejectedValueOnce(new Error('无法发送到主窗口'));
  render(<LauncherWindow />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'https://example.com/file.zip');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(await screen.findByRole('alert')).toHaveTextContent('无法发送到主窗口');
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
});
