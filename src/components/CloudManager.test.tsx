import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { CloudManager } from './CloudManager';

it('shows an app-managed folder list rather than a provider website', () => {
  render(<CloudManager provider="quark" />);
  expect(screen.getByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
  expect(screen.queryByText('pan.quark.cn')).not.toBeInTheDocument();
});

it('opens an authorization boundary before a cloud account is connected', async () => {
  const user = userEvent.setup();
  render(<CloudManager provider="pikpak" />);
  await user.click(screen.getByRole('button', { name: '连接 PikPak 账号' }));
  expect(screen.getByRole('status')).toHaveTextContent('需要你在授权窗口中完成登录');
});
