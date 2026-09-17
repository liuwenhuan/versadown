import { readFileSync } from 'node:fs';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { SettingsPanel } from './SettingsPanel';
import { defaultSettings } from '../lib/api';

const initial = {
  downloadDirectory: 'C:\\Users\\tester\\Downloads',
  launcherVisibility: 'always' as const,
  proxy: { enabled: false, protocol: 'http' as const, host: '', port: 7890, username: '', password: '' },
  ai: { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false },
  magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' },
  aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 4, btSeedRatio: 1.5, debugLogging: false, overallDownloadLimit: 0, p2pUploadLimit: 1048576 },
  deleteTransferredShareFilesAfterDownload: true,
  cloudUploadLimit: 0
};

it('changes the interface theme from common settings', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  expect(screen.getAllByRole('radio', { name: /主题/ })).toHaveLength(8);
  await user.click(screen.getByRole('radio', { name: '暖阳金主题' }));

  expect(document.documentElement).toHaveAttribute('data-theme', 'warm-gold');
  expect(screen.getByRole('radio', { name: '暖阳金主题' })).toHaveAttribute('aria-checked', 'true');
});

it('separates settings into common, network, and advanced workspaces', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  expect(screen.getByRole('tab', { name: '常用' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '网络' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '高级' })).toBeVisible();
  expect(screen.getByRole('textbox', { name: '下载目录' })).toBeVisible();

  await user.click(screen.getByRole('tab', { name: '网络' }));
  expect(screen.getByRole('checkbox', { name: '启用下载代理' })).toBeVisible();
  expect(screen.queryByRole('textbox', { name: '下载目录' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('tab', { name: '高级' }));
  expect(screen.queryByRole('checkbox', { name: '启用调试报告' })).not.toBeInTheDocument();
  expect(screen.queryByText('隐私规则')).not.toBeInTheDocument();
  expect(screen.getByRole('checkbox', { name: '打开日志调试' })).toBeVisible();
  expect(screen.getByRole('checkbox', { name: '开启隐私下载' })).toBeVisible();
  expect(screen.queryByRole('checkbox', { name: '启用磁链元数据服务' })).not.toBeInTheDocument();
});

it('keeps proxy, traffic caps, and aria2 configuration in separate network settings groups', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);
  await user.click(screen.getByRole('tab', { name: '网络' }));

  expect(screen.getByText('启用下载代理服务器')).toBeVisible();
  const rateLimitForm = screen.getByText('限速设置').closest('form');
  const aria2Form = screen.getByText('aria2 下载引擎').closest('form');
  const overallDownloadLimit = screen.getByRole('spinbutton', { name: '整体下载限速' });
  const p2pUploadLimit = screen.getByRole('spinbutton', { name: 'P2P 上传限速' });
  const cloudUploadLimit = screen.getByRole('spinbutton', { name: '网盘上传限速' });
  const rateLimitFieldset = overallDownloadLimit.closest('fieldset');

  expect(rateLimitForm).toContainElement(overallDownloadLimit);
  expect(rateLimitForm).toContainElement(p2pUploadLimit);
  expect(rateLimitForm).toContainElement(cloudUploadLimit);
  expect(p2pUploadLimit.closest('fieldset')).toBe(rateLimitFieldset);
  expect(cloudUploadLimit.closest('fieldset')).toBe(rateLimitFieldset);
  expect(screen.getByRole('button', { name: '保存限速设置' }).parentElement).toBe(rateLimitFieldset);
  expect(aria2Form).not.toContainElement(overallDownloadLimit);
  expect(aria2Form).not.toContainElement(p2pUploadLimit);
  expect(aria2Form).not.toContainElement(cloudUploadLimit);
});

it('keeps rate limits and their save control inline while accepting one decimal place', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    render(<SettingsPanel initial={initial} onChange={onChange} />);
    await user.click(screen.getByRole('tab', { name: '网络' }));

    const overallDownloadLimit = screen.getByRole('spinbutton', { name: '整体下载限速' });
    const p2pUploadLimit = screen.getByRole('spinbutton', { name: 'P2P 上传限速' });
    const cloudUploadLimit = screen.getByRole('spinbutton', { name: '网盘上传限速' });
    const rateLimitFieldset = overallDownloadLimit.closest('fieldset')!;
    expect(screen.getByRole('button', { name: '保存限速设置' }).parentElement).toBe(rateLimitFieldset);
    expect(getComputedStyle(rateLimitFieldset).gridTemplateColumns).toBe('repeat(3, minmax(0, 1fr)) auto');
    expect(overallDownloadLimit).toHaveAttribute('step', '0.1');
    expect(p2pUploadLimit.value).toBe('1.0');
    expect(cloudUploadLimit.value).toBe('0.0');

    await user.clear(overallDownloadLimit);
    await user.type(overallDownloadLimit, '2.56');
    expect(overallDownloadLimit.value).toBe('2.5');
    await user.click(screen.getByRole('button', { name: '保存限速设置' }));

    expect(onChange).toHaveBeenLastCalledWith({
      ...initial,
      aria2: { ...initial.aria2, overallDownloadLimit: 2_621_440 },
      cloudUploadLimit: 0
    });
  } finally {
    stylesheet.remove();
  }
});

it('keeps helper descriptions limited to privacy and aria2 diagnostics', async () => {
  const user = userEvent.setup();
  const { container } = render(<SettingsPanel initial={initial} onChange={() => {}} />);

  expect(container.querySelectorAll('.settings-content small')).toHaveLength(0);
  await user.click(screen.getByRole('tab', { name: '网络' }));
  expect(container.querySelectorAll('.settings-content small')).toHaveLength(0);
  await user.click(screen.getByRole('tab', { name: '高级' }));
  const descriptions = Array.from(container.querySelectorAll('.settings-content small'));
  expect(descriptions).toHaveLength(2);
  expect(descriptions[0]).toHaveTextContent('保存 aria2 引擎和本地 RPC 调试日志');
  expect(descriptions[1]).toHaveTextContent('私密任务只会在本次应用会话解锁后显示；下载文件本身不会加密。');
});

it('saves a custom directory on blur without a separate save button', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);

  const directory = screen.getByRole('textbox', { name: '下载目录' });
  await user.clear(directory);
  await user.type(directory, 'D:\\Downloads\\Games');
  await user.tab();

  expect(onChange).toHaveBeenLastCalledWith({ ...initial, downloadDirectory: 'D:\\Downloads\\Games' });
  expect(screen.queryByRole('button', { name: '保存下载目录' })).not.toBeInTheDocument();
});

it('applies the download directory immediately after native folder selection', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} browseDirectory={async () => 'E:\\Library'} />);

  await user.click(screen.getByRole('button', { name: '浏览下载目录' }));

  expect(screen.getByRole('textbox', { name: '下载目录' })).toHaveValue('E:\\Library');
  expect(onChange).toHaveBeenLastCalledWith({ ...initial, downloadDirectory: 'E:\\Library' });
});

it('offers an OpenAI-compatible AI analysis configuration in advanced settings', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));

  expect(screen.getByRole('checkbox', { name: '启用 AI 智能分析' })).toBeVisible();
  expect(screen.getByRole('textbox', { name: 'AI 服务地址' })).toBeVisible();
  expect(screen.getByLabelText('AI API Key')).toHaveAttribute('type', 'password');
  expect(screen.getByRole('combobox', { name: 'AI 模型' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '获取可用模型' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '保存 AI 配置' })).not.toBeInTheDocument();
});

it('refreshes models when its selector is opened and saves the complete AI configuration on selection', async () => {
  const user = userEvent.setup();
  const loadAiModels = vi.fn().mockResolvedValue(['gpt-4.1-mini', 'gpt-5-mini']);
  const saveAiSettings = vi.fn().mockResolvedValue({ enabled: true, baseUrl: 'https://gateway.example.test/v1', model: 'gpt-5-mini', apiKeyConfigured: true });
  render(<SettingsPanel initial={initial} onChange={() => {}} loadAiModels={loadAiModels} saveAiSettings={saveAiSettings} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.click(screen.getByRole('checkbox', { name: '启用 AI 智能分析' }));
  await user.type(screen.getByRole('textbox', { name: 'AI 服务地址' }), 'https://gateway.example.test/v1');
  await user.type(screen.getByLabelText('AI API Key'), 'sk-local-secret');
  const model = screen.getByRole('combobox', { name: 'AI 模型' });
  fireEvent.pointerDown(model);
  expect(await screen.findByRole('option', { name: 'gpt-5-mini' })).toBeVisible();
  expect(loadAiModels).toHaveBeenCalledWith({ enabled: true, baseUrl: 'https://gateway.example.test/v1', model: '', apiKey: 'sk-local-secret' });

  await user.selectOptions(model, 'gpt-5-mini');
  expect(saveAiSettings).toHaveBeenCalledWith({ enabled: true, baseUrl: 'https://gateway.example.test/v1', model: 'gpt-5-mini', apiKey: 'sk-local-secret' });
});

it('shows the returned model-list error when the selector cannot load models', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} loadAiModels={vi.fn().mockRejectedValue(new Error('AI 服务返回 HTTP 401：invalid api key'))} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.type(screen.getByRole('textbox', { name: 'AI 服务地址' }), 'https://gateway.example.test/v1');
  await user.type(screen.getByLabelText('AI API Key'), 'bad-key');
  fireEvent.pointerDown(screen.getByRole('combobox', { name: 'AI 模型' }));

  expect(await screen.findByRole('status')).toHaveTextContent('无法获取模型列表，可能是服务地址或 API Key 不正确：AI 服务返回 HTTP 401：invalid api key');
});

it('enables cleanup for application-created transfer files by default', () => {
  render(<SettingsPanel initial={defaultSettings} onChange={() => {}} />);

  expect(screen.getByLabelText('下载完成后自动删除转存文件')).toBeChecked();
  expect(screen.queryByText('仅删除本应用为下载创建的转存文件，不会删除您的其他网盘文件。')).not.toBeInTheDocument();
});

it('defaults new connections to four while preserving an explicit eight', async () => {
  const user = userEvent.setup();
  expect(defaultSettings.aria2.maxConnectionsPerServer).toBe(4);

  render(<SettingsPanel initial={{ ...defaultSettings, aria2: { ...defaultSettings.aria2, maxConnectionsPerServer: 8 } }} onChange={() => {}} />);
  await user.click(screen.getByRole('tab', { name: '网络' }));

  expect(screen.getByRole('spinbutton', { name: '单任务并发连接数' })).toHaveValue(8);
});

it('asks for and confirms a new password when privacy has no existing password', async () => {
  const user = userEvent.setup();
  const configurePrivacyDownload = vi.fn().mockResolvedValue({ ...initial, privacyDownloadEnabled: true, privacyPasswordConfigured: true });
  render(<SettingsPanel initial={initial} onChange={() => {}} configurePrivacyDownload={configurePrivacyDownload} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.click(screen.getByRole('checkbox', { name: '开启隐私下载' }));
  await user.type(screen.getByLabelText('设置新密码'), 'private-pass');
  await user.type(screen.getByLabelText('确认新密码'), 'private-pass');
  await user.click(screen.getByRole('button', { name: '保存密码' }));

  expect(configurePrivacyDownload).toHaveBeenCalledWith(true, undefined, 'private-pass');
  expect(await screen.findByRole('status')).toHaveTextContent('隐私下载已开启');
});

it('requires the old password before replacing an existing privacy password', async () => {
  const user = userEvent.setup();
  const configurePrivacyDownload = vi.fn().mockResolvedValue({ ...initial, privacyDownloadEnabled: true, privacyPasswordConfigured: true });
  render(<SettingsPanel initial={{ ...initial, privacyDownloadEnabled: true, privacyPasswordConfigured: true }} onChange={() => {}} configurePrivacyDownload={configurePrivacyDownload} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.type(screen.getByLabelText('输入旧密码'), 'old-pass');
  await user.type(screen.getByLabelText('设置新密码'), 'new-pass');
  await user.click(screen.getByRole('button', { name: '保存密码' }));

  expect(screen.queryByLabelText('确认新密码')).not.toBeInTheDocument();
  expect(configurePrivacyDownload).toHaveBeenCalledWith(true, 'old-pass', 'new-pass');
});

it('uses compact aligned controls and consistent cards across settings tabs', async () => {
  const user = userEvent.setup();
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    render(<SettingsPanel initial={{ ...initial, privacyDownloadEnabled: true, privacyPasswordConfigured: true }} onChange={() => {}} />);

    const cleanupToggle = screen.getByRole('checkbox', { name: '下载完成后自动删除转存文件' });
    expect(getComputedStyle(cleanupToggle).width).toBe('18px');

    const theme = screen.getByRole('region', { name: '界面主题' });
    const directory = screen.getByRole('textbox', { name: '下载目录' }).closest('form');
    expect(Number.parseFloat(getComputedStyle(directory!).marginTop)).toBeGreaterThan(0);
    expect(getComputedStyle(theme).borderRadius).toBe(getComputedStyle(directory!).borderRadius);

    await user.click(screen.getByRole('tab', { name: '网络' }));
    expect(getComputedStyle(screen.getByRole('checkbox', { name: '启用下载代理' })).width).toBe('18px');
    const proxyFieldset = screen.getByLabelText('代理密码').closest('fieldset');
    expect(screen.getByRole('button', { name: '保存代理设置' }).parentElement).toBe(proxyFieldset);
    expect(Array.from(proxyFieldset!.children).slice(-3)).toEqual([
      screen.getByRole('textbox', { name: '代理用户名' }).closest('label'),
      screen.getByLabelText('代理密码').closest('label'),
      screen.getByRole('button', { name: '保存代理设置' }),
    ]);
    const aria2Fieldset = screen.getByRole('spinbutton', { name: 'BT 分享率' }).closest('fieldset');
    expect(screen.getByRole('button', { name: '保存 aria2 设置' }).parentElement).toBe(aria2Fieldset);
    expect(getComputedStyle(aria2Fieldset!).gridTemplateColumns).toBe('repeat(3,minmax(0,1fr)) auto');
    expect(Array.from(aria2Fieldset!.children).slice(-2)).toEqual([
      screen.getByRole('spinbutton', { name: 'BT 分享率' }).closest('label'),
      screen.getByRole('button', { name: '保存 aria2 设置' }),
    ]);

    await user.click(screen.getByRole('tab', { name: '高级' }));
    const privacyToggle = screen.getByRole('checkbox', { name: '开启隐私下载' });
    expect(getComputedStyle(privacyToggle).width).toBe('18px');
    expect(getComputedStyle(privacyToggle).height).toBe('18px');
    expect(getComputedStyle(screen.getByText(/私密任务只会/)).whiteSpace).toBe('nowrap');
    const oldPassword = screen.getByLabelText('输入旧密码');
    const newPassword = screen.getByLabelText('设置新密码');
    const savePassword = screen.getByRole('button', { name: '保存密码' });
    expect(oldPassword.parentElement?.parentElement).toBe(newPassword.parentElement?.parentElement);
    expect(savePassword.parentElement).toBe(oldPassword.parentElement?.parentElement);
    expect(getComputedStyle(oldPassword).width).toBe(getComputedStyle(newPassword).width);
  } finally {
    stylesheet.remove();
  }
});

it('uses the shared settings form treatment for privacy password inputs', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={{ ...initial, privacyDownloadEnabled: true, privacyPasswordConfigured: true }} onChange={() => {}} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  expect(screen.getByLabelText('输入旧密码').closest('form')).toHaveClass('proxy-settings');
  expect(screen.getByLabelText('设置新密码').closest('form')).toHaveClass('proxy-settings');
});

it('does not expose internal magnet metadata service settings', async () => {
  const user = userEvent.setup();
  render(<SettingsPanel initial={initial} onChange={() => {}} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  expect(screen.queryByRole('checkbox', { name: '启用磁链元数据服务' })).not.toBeInTheDocument();
  expect(screen.queryByRole('textbox', { name: '磁链元数据服务地址' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '保存磁链服务' })).not.toBeInTheDocument();
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
    downloadDirectory: 'C:\\Users\\tester\\Downloads',
    launcherVisibility: 'always',
    proxy: { enabled: true, protocol: 'https', host: '127.0.0.1', port: 1080, username: 'alice', password: 'secret' },
    ai: { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false },
    magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' },
    aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 4, btSeedRatio: 1.5, debugLogging: false, overallDownloadLimit: 0, p2pUploadLimit: 1048576 },
    deleteTransferredShareFilesAfterDownload: true,
    cloudUploadLimit: 0
  });
});

it('saves aria2 RPC and engine settings from the network settings', async () => {
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
      btSeedRatio: 1.5,
      debugLogging: false,
      overallDownloadLimit: 0,
      p2pUploadLimit: 1048576
    }
  });
});

it('saves aria2 debug logging immediately from advanced settings', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);

  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.click(screen.getByRole('checkbox', { name: '打开日志调试' }));

  expect(onChange).toHaveBeenLastCalledWith({
    ...initial,
    aria2: { ...initial.aria2, debugLogging: true }
  });
  expect(screen.queryByRole('button', { name: '保存日志设置' })).not.toBeInTheDocument();
});

it('saves the standalone rate limits in bytes per second', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={initial} onChange={onChange} />);
  await user.click(screen.getByRole('tab', { name: '网络' }));

  await user.clear(screen.getByRole('spinbutton', { name: '整体下载限速' }));
  await user.type(screen.getByRole('spinbutton', { name: '整体下载限速' }), '2.5');
  await user.clear(screen.getByRole('spinbutton', { name: 'P2P 上传限速' }));
  await user.type(screen.getByRole('spinbutton', { name: 'P2P 上传限速' }), '1.5');
  await user.clear(screen.getByRole('spinbutton', { name: '网盘上传限速' }));
  await user.type(screen.getByRole('spinbutton', { name: '网盘上传限速' }), '3');
  await user.click(screen.getByRole('button', { name: '保存限速设置' }));

  expect(onChange).toHaveBeenLastCalledWith({
    ...initial,
    aria2: {
      ...initial.aria2,
      overallDownloadLimit: 2_621_440,
      p2pUploadLimit: 1_572_864
    },
    cloudUploadLimit: 3_145_728
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
