import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { SettingsPanel } from './SettingsPanel';

const initial = {
  debugReports: false,
  downloadDirectory: 'C:\\Users\\tester\\Downloads',
  proxy: { enabled: false, protocol: 'http' as const, host: '', port: 7890, username: '', password: '' }
};

it('keeps detailed report collection off by default', () => {
  render(<SettingsPanel initial={initial} onChange={() => {}} />);
  expect(screen.getByRole('checkbox', { name: '启用调试报告' })).not.toBeChecked();
});

it('saves a custom directory for subsequent downloads', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);

  const directory = screen.getByRole('textbox', { name: '下载目录' });
  await user.clear(directory);
  await user.type(directory, 'D:\\Downloads\\Games');
  await user.click(screen.getByRole('button', { name: '保存下载目录' }));

  expect(onChange).toHaveBeenLastCalledWith({ ...initial, downloadDirectory: 'D:\\Downloads\\Games' });
});

it('fills the download directory from the native folder picker', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} browseDirectory={async () => 'E:\\Library'} />);

  await user.click(screen.getByRole('button', { name: '浏览下载目录' }));

  expect(screen.getByRole('textbox', { name: '下载目录' })).toHaveValue('E:\\Library');
});

it('allows users to explicitly enable debug reports in beta', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);
  await user.click(screen.getByRole('checkbox', { name: '启用调试报告' }));
  expect(onChange).toHaveBeenCalledWith({ ...initial, debugReports: true });
});

it('saves an authenticated HTTPS proxy for aria2 downloads', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);

  await user.click(screen.getByRole('checkbox', { name: '启用下载代理' }));
  await user.selectOptions(screen.getByRole('combobox', { name: '代理协议' }), 'https');
  await user.clear(screen.getByRole('textbox', { name: '代理服务器' }));
  await user.type(screen.getByRole('textbox', { name: '代理服务器' }), '127.0.0.1');
  await user.clear(screen.getByRole('spinbutton', { name: '代理端口' }));
  await user.type(screen.getByRole('spinbutton', { name: '代理端口' }), '1080');
  await user.type(screen.getByRole('textbox', { name: '代理用户名' }), 'alice');
  await user.type(screen.getByLabelText('代理密码'), 'secret');
  await user.click(screen.getByRole('button', { name: '保存代理设置' }));

  expect(onChange).toHaveBeenLastCalledWith({
    debugReports: false,
    downloadDirectory: 'C:\\Users\\tester\\Downloads',
    proxy: { enabled: true, protocol: 'https', host: '127.0.0.1', port: 1080, username: 'alice', password: 'secret' }
  });
});
