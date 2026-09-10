import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { SettingsPanel } from './SettingsPanel';
import { defaultSettings } from '../lib/api';

const initial = {
  debugReports: false,
  downloadDirectory: 'C:\\Users\\tester\\Downloads',
  launcherVisibility: 'always' as const,
  proxy: { enabled: false, protocol: 'http' as const, host: '', port: 7890, username: '', password: '' },
  ai: { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false },
  magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' },
  aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 8, btSeedRatio: 1.5 },
  deleteTransferredShareFilesAfterDownload: true
};

it('keeps detailed report collection off by default', () => {
  render(<SettingsPanel initial={initial} onChange={() => {}} />);
  expect(screen.getByRole('tab', { name: '显示' })).toHaveAttribute('aria-selected', 'true');
  expect(screen.queryByRole('checkbox', { name: '启用调试报告' })).not.toBeInTheDocument();
});

it('changes the interface theme from display settings', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  await user.selectOptions(screen.getByRole('combobox', { name: '界面主题' }), 'dark');

  expect(document.documentElement).toHaveAttribute('data-theme', 'dark');
});

it('separates settings into display, network, and advanced workspaces', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  expect(screen.getByRole('tab', { name: '显示' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '网络' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '高级' })).toBeVisible();
  expect(screen.getByRole('textbox', { name: '下载目录' })).toBeVisible();

  await user.click(screen.getByRole('tab', { name: '网络' }));
  expect(screen.getByRole('checkbox', { name: '启用下载代理' })).toBeVisible();
  expect(screen.queryByRole('textbox', { name: '下载目录' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('tab', { name: '高级' }));
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
  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.click(screen.getByRole('checkbox', { name: '启用调试报告' }));
  expect(onChange).toHaveBeenCalledWith({ ...initial, debugReports: true });
});

it('offers an OpenAI-compatible AI analysis configuration in advanced settings', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));

  expect(screen.getByRole('checkbox', { name: '启用 AI 智能分析' })).toBeVisible();
  expect(screen.getByRole('textbox', { name: 'AI 服务地址' })).toBeVisible();
  expect(screen.getByLabelText('AI API Key')).toHaveAttribute('type', 'password');
  expect(screen.getByRole('button', { name: '获取可用模型' })).toBeVisible();
  expect(screen.getByRole('combobox', { name: 'AI 模型' })).toBeVisible();
});

it('enables cleanup for application-created transfer files by default', () => {
  render(<SettingsPanel initial={defaultSettings} onChange={() => {}} />);

  expect(screen.getByLabelText('下载完成后自动删除转存文件')).toBeChecked();
  expect(screen.getByText('仅删除本应用为下载创建的转存文件，不会删除您的其他网盘文件。')).toBeVisible();
});

it('saves the preferred magnet metadata service from advanced settings', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  const endpoint = screen.getByRole('textbox', { name: '磁链元数据服务地址' });
  await user.clear(endpoint);
  await user.type(endpoint, 'https://metadata.example.test');
  await user.click(screen.getByRole('button', { name: '保存磁链服务' }));

  expect(onChange).toHaveBeenLastCalledWith({
    ...initial,
    magnetMetadata: { enabled: true, baseUrl: 'https://metadata.example.test' }
  });
});

it('lets users restore a hidden floating ball from the main settings', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={{ ...initial, launcherVisibility: 'hidden' }} onChange={onChange} />);

  await user.selectOptions(screen.getByRole('combobox', { name: '悬浮球显示' }), 'always');

  expect(onChange).toHaveBeenLastCalledWith({ ...initial, launcherVisibility: 'always' });
});

it('saves an authenticated HTTPS proxy for aria2 downloads', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);
  await user.click(screen.getByRole('tab', { name: '网络' }));

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
    launcherVisibility: 'always',
    proxy: { enabled: true, protocol: 'https', host: '127.0.0.1', port: 1080, username: 'alice', password: 'secret' },
    ai: { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false },
    magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' },
    aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 8, btSeedRatio: 1.5 },
    deleteTransferredShareFilesAfterDownload: true
  });
});

it('saves aria2 RPC and transfer limits from the network settings', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);
  await user.click(screen.getByRole('tab', { name: '网络' }));

  await user.clear(screen.getByRole('spinbutton', { name: 'aria2 RPC 端口' }));
  await user.type(screen.getByRole('spinbutton', { name: 'aria2 RPC 端口' }), '16800');
  await user.type(screen.getByLabelText('aria2 RPC Token'), 'local-token');
  await user.clear(screen.getByRole('spinbutton', { name: '最大并行任务数' }));
  await user.type(screen.getByRole('spinbutton', { name: '最大并行任务数' }), '3');
  await user.clear(screen.getByRole('spinbutton', { name: '单任务并发连接数' }));
  await user.type(screen.getByRole('spinbutton', { name: '单任务并发连接数' }), '4');
  await user.clear(screen.getByRole('spinbutton', { name: 'BT 分享率' }));
  await user.type(screen.getByRole('spinbutton', { name: 'BT 分享率' }), '1.5');
  await user.click(screen.getByRole('button', { name: '保存 aria2 设置' }));

  expect(onChange).toHaveBeenLastCalledWith({
    ...initial,
    aria2: {
      rpcPort: 16800,
      rpcToken: 'local-token',
      maxConcurrentDownloads: 3,
      maxConnectionsPerServer: 4,
      btSeedRatio: 1.5
    }
  });
});

it('shows the native aria2 error instead of claiming a failed configuration was applied', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn().mockRejectedValue(new Error('aria2 RPC 端口 16800 已被占用'));
  render(<SettingsPanel initial={{ ...initial, aria2: { ...initial.aria2, rpcToken: 'local-token' } }} onChange={onChange} />);
  await user.click(screen.getByRole('tab', { name: '网络' }));
  await user.click(screen.getByRole('button', { name: '保存 aria2 设置' }));

  expect(await screen.findByRole('status')).toHaveTextContent('aria2 RPC 端口 16800 已被占用');
  expect(screen.queryByText('已保存并立即应用')).not.toBeInTheDocument();
});
