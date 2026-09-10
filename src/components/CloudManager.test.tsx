import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { readFileSync } from 'node:fs';
import { vi } from 'vitest';
import { CloudManager } from './CloudManager';
import { cloudRecycleBinDirectoryId, type CloudProvider } from '../lib/cloudTypes';

function createCloudFake(overrides: Record<string, unknown> = {}) {
  return {
    beginCloudAuthorization: vi.fn().mockImplementation((provider: CloudProvider) => Promise.resolve({ provider, windowLabel: '网盘登录', instructions: '请在安全窗口中完成登录。' })),
    completeCloudAuthorization: vi.fn().mockImplementation((provider: CloudProvider) => Promise.resolve({ id: 'account-1', displayName: 'alice', provider, authMethod: 'web' as const })),
    loginCloudWithPassword: vi.fn().mockImplementation((provider: CloudProvider) => Promise.resolve({ id: 'account-1', displayName: 'alice', provider, authMethod: 'password' as const })),
    getCloudAccount: vi.fn().mockRejectedValue(Object.assign(new Error('未授权'), { code: 'unauthorized' })),
    listCloudDirectory: vi.fn().mockImplementation((_provider: string, parentId = '0') => Promise.resolve(parentId === 'folder-1'
      ? { id: 'folder-1', items: [{ id: 'child-1', parentId: 'folder-1', name: '说明.txt', kind: 'file', size: 1024, modifiedAt: '2025-06-07 08:09:10' }] }
      : parentId === 'folder-2'
        ? { id: 'folder-2', items: [{ id: 'nested-folder', parentId: 'folder-2', name: '深层目录', kind: 'folder', modifiedAt: '2025-06-07 08:09:10' }] }
      : { id: '0', items: [{ id: 'folder-1', parentId: '0', name: '归档', kind: 'folder', modifiedAt: '2025-06-07 08:09:10' }, { id: 'file-1', parentId: '0', name: '报告.pdf', kind: 'file', size: 2_000_000, modifiedAt: '2025-06-07 08:09:10' }] })),
    createCloudDirectory: vi.fn().mockResolvedValue(undefined),
    pickAndUploadFiles: vi.fn().mockResolvedValue(undefined),
    downloadCloudFile: vi.fn().mockResolvedValue('https://download.example/file'),
    downloadCloudDirectory: vi.fn().mockResolvedValue({ queuedCount: 0, tasks: [], failures: [] }),
    renameCloudFile: vi.fn().mockResolvedValue(undefined),
    moveCloudFiles: vi.fn().mockResolvedValue(undefined),
    copyCloudFiles: vi.fn().mockResolvedValue(undefined),
    trashCloudFiles: vi.fn().mockResolvedValue(undefined),
    deleteCloudFiles: vi.fn().mockResolvedValue(undefined),
    restoreCloudFiles: vi.fn().mockResolvedValue(undefined),
    logoutCloud: vi.fn().mockResolvedValue(undefined),
    getCloudAuthorizationCapabilities: vi.fn().mockImplementation((provider: string) => Promise.resolve({
      provider,
      web: true,
      password: provider === 'pikpak' || provider === 'baidu' || provider === 'tianyi',
      passwordLoginMode: provider === 'pikpak' || provider === 'baidu' || provider === 'tianyi'
        ? 'rustDirect'
        : provider === 'aliyun' || provider === 'xunlei' || provider === 'uc'
          ? 'officialWebView'
          : 'none'
    })),
    ...overrides
  };
}

it('keeps the unauthenticated provider card focused on the available sign-in action', async () => {
  render(<CloudManager provider="quark" client={createCloudFake()} />);

  expect(screen.getByRole('region', { name: '夸克账号授权' })).toBeVisible();
  expect(screen.queryByText('自有网盘 · 本地会话')).not.toBeInTheDocument();
  expect(screen.queryByText(/授权完成前不会显示或加载任何文件/)).not.toBeInTheDocument();
  expect(screen.queryByText(/夸克未提供可用的账号密码开放登录接口/)).not.toBeInTheDocument();
  expect(screen.queryByLabelText('夸克账号')).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '登录夸克账号' })).not.toBeInTheDocument();
  expect(await screen.findByRole('button', { name: '使用网页授权' })).toBeVisible();
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
  expect(screen.queryByRole('tree', { name: '夸克网盘文件' })).not.toBeInTheDocument();
  expect(screen.queryByText('下载示例.txt')).not.toBeInTheDocument();
});

it.each([
  ['quark', '夸克', false], ['pikpak', 'PikPak', true], ['baidu', '百度', true], ['tianyi', '天翼', true],
  ['aliyun', '阿里', false], ['xunlei', '迅雷', false], ['uc', 'UC', false]
] as const)('keeps the %s login card concise while prioritizing its supported login method', async (provider, label, supportsPasswordLogin) => {
  const client = createCloudFake({ getCloudAuthorizationCapabilities: vi.fn().mockReturnValue(new Promise<never>(() => undefined)) });
  render(<CloudManager provider={provider} client={client} authorizationOnly />);

  expect(await screen.findByRole('button', { name: '使用网页授权' })).toBeVisible();
  expect(screen.queryByLabelText(`${label}账号`)).not.toBeInTheDocument();
  expect(screen.queryByLabelText(`${label}密码`)).not.toBeInTheDocument();
  expect(screen.queryByText('账号和密码仅加密保存在本机，用于登录失效后的自动恢复。')).not.toBeInTheDocument();
  expect(screen.queryByText('请在官方网页中使用账号密码登录；应用不会收集或保存密码。')).not.toBeInTheDocument();
  if (supportsPasswordLogin) {
    expect(screen.getByLabelText('账号')).toBeVisible();
    expect(screen.getByLabelText('密码')).toBeVisible();
    expect(screen.getByRole('button', { name: '登录' })).toHaveClass('primary');
  } else {
    expect(screen.queryByLabelText('账号')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('密码')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '登录' })).not.toBeInTheDocument();
  }
  expect(client.getCloudAuthorizationCapabilities).not.toHaveBeenCalled();
});

it.each([
  ['aliyun', '阿里'],
  ['xunlei', '迅雷'],
  ['uc', 'UC']
] as const)('keeps the account and offers official WebView recovery for %s', async (provider, label) => {
  const client = createCloudFake({ listCloudDirectory: vi.fn().mockRejectedValue({ code: 'web_auth_required', message: 'verification required' }) });
  render(<CloudManager provider={provider} client={client} initialAccounts={[
    { id: `${provider}-account`, displayName: 'alice', provider, authMethod: 'web' }
  ]} restoreOnMount={false} />);

  expect(await screen.findByRole('button', { name: '重新网页授权' })).toBeVisible();
  expect(screen.getByRole('tab', { name: `alice · ${label}` })).toBeVisible();
});

it('asks to repeat a mutation after the session was automatically restored', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({
    pickAndUploadFiles: vi.fn().mockRejectedValue({
      code: 'session_renewed_retry_operation',
      message: 'this provider response must not be shown'
    })
  });
  render(<CloudManager provider="baidu" client={client} initialAccounts={[
    { id: 'account-1', displayName: 'alice', provider: 'baidu', authMethod: 'password' }
  ]} restoreOnMount={false} />);

  await screen.findByRole('tree', { name: '百度网盘文件' });
  await user.click(screen.getByRole('button', { name: '上传' }));

  expect(await screen.findByRole('alert')).toHaveTextContent('已自动恢复登录，请重新执行本次操作');
  expect(screen.getByRole('alert')).not.toHaveTextContent('this provider response must not be shown');
});

it('rejects a web authorization account returned for another provider', async () => {
  const user = userEvent.setup();
  const onAccountAuthorized = vi.fn();
  const client = createCloudFake({ completeCloudAuthorization: vi.fn().mockResolvedValue({ id: 'p-1', displayName: 'p-alice', provider: 'pikpak', authMethod: 'web' }) });
  render(<CloudManager provider="quark" client={client} authorizationOnly onAccountAuthorized={onAccountAuthorized} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));

  expect(await screen.findByRole('status')).toHaveTextContent('授权返回的服务商不匹配');
  expect(onAccountAuthorized).not.toHaveBeenCalled();
  expect(screen.queryByRole('button', { name: '已经完成授权' })).not.toBeInTheDocument();
});

it('recovers an existing account on mount before loading its root directory', async () => {
  const client = createCloudFake({ getCloudAccount: vi.fn().mockResolvedValue({ id: 'saved-1', displayName: 'saved-alice', provider: 'quark', authMethod: 'web' }) });
  render(<CloudManager provider="quark" client={client} />);

  expect(await screen.findByRole('tab', { name: 'saved-alice · 夸克' })).toHaveAttribute('aria-selected', 'true');
  expect(client.listCloudDirectory).toHaveBeenCalledWith('quark', '0');
  expect(await screen.findByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
});

it.each([
  ['baidu', '百度', '/'],
  ['tianyi', '天翼', '-11'],
  ['aliyun', '阿里', 'root'],
  ['xunlei', '迅雷', '0']
] as const)('uses the native root identifier for %s', async (provider, label, root) => {
  const client = createCloudFake({
    getCloudAccount: vi.fn().mockResolvedValue({ id: `${provider}-account`, displayName: 'alice', provider, authMethod: 'web' })
  });
  render(<CloudManager provider={provider} client={client} />);

  expect(await screen.findByRole('tree', { name: `${label}网盘文件` })).toBeVisible();
  expect(client.listCloudDirectory).toHaveBeenCalledWith(provider, root);
});

it('switches an account tab and reloads that account view from the root directory', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[
    { id: 'first', displayName: 'first-alice', provider: 'quark', authMethod: 'web' },
    { id: 'second', displayName: 'second-alice', provider: 'quark', authMethod: 'web' }
  ]} />);

  await screen.findByRole('tree', { name: '夸克网盘文件' });
  await user.click(screen.getByRole('tab', { name: 'second-alice · 夸克' }));
  expect(screen.getByRole('tab', { name: 'second-alice · 夸克' })).toHaveAttribute('aria-selected', 'true');
  expect(await screen.findByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
  expect(client.listCloudDirectory).toHaveBeenLastCalledWith('quark', '0');
});

it('automatically completes web authorization and loads the authorized account', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  expect(await screen.findByRole('tab', { name: 'alice · 夸克' }, { timeout: 1500 })).toHaveAttribute('aria-selected', 'true');
  expect(client.completeCloudAuthorization).toHaveBeenCalledWith('quark');
  expect(screen.getByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
});

it('keeps web authorization polling serial while a completion request is still pending', async () => {
  vi.useFakeTimers();
  try {
    let resolveCompletion: ((account: { id: string; displayName: string; provider: 'quark'; authMethod: 'web' }) => void) | undefined;
    const completion = new Promise<{ id: string; displayName: string; provider: 'quark'; authMethod: 'web' }>((resolve) => { resolveCompletion = resolve; });
    const client = createCloudFake({ completeCloudAuthorization: vi.fn().mockReturnValue(completion) });
    render(<CloudManager provider="quark" client={client} />);

    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByRole('button', { name: '使用网页授权' }));
    await act(async () => { await Promise.resolve(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(800); });
    expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(3_000); });
    expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(1);

    await act(async () => { resolveCompletion?.({ id: 'account-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }); await Promise.resolve(); });
  } finally {
    vi.useRealTimers();
  }
});

it('keeps polling after a transient pre-login response and completes when login becomes available', async () => {
  vi.useFakeTimers();
  try {
    const client = createCloudFake({ completeCloudAuthorization: vi.fn()
      .mockRejectedValueOnce({ code: 'provider_error', message: '服务端暂未返回登录账号' })
      .mockResolvedValue({ id: 'account-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }) });
    render(<CloudManager provider="quark" client={client} />);

    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByRole('button', { name: '使用网页授权' }));
    await act(async () => { await Promise.resolve(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(800); });
    expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(1);

    await act(async () => { await vi.advanceTimersByTimeAsync(1_800); });
    expect(screen.getByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
    expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(2);
  } finally {
    vi.useRealTimers();
  }
});

it('reports a successful authorization to an authorization-center host without rendering a file list', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  const onAccountAuthorized = vi.fn();
  render(<CloudManager provider="quark" client={client} authorizationOnly onAccountAuthorized={onAccountAuthorized} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));

  expect(onAccountAuthorized).toHaveBeenCalledWith({ id: 'account-1', displayName: 'alice', provider: 'quark', authMethod: 'web' });
  expect(screen.queryByRole('tree', { name: '夸克网盘文件' })).not.toBeInTheDocument();
});

it('renders an authorized account-management card without loading its directory', async () => {
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} authorizationOnly initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await act(async () => { await Promise.resolve(); });

  expect(screen.getByRole('heading', { name: '夸克网盘 · alice' })).toBeVisible();
  expect(document.querySelector('.cloud-account-summary')).not.toBeInTheDocument();
  expect(screen.queryByText(/官方网页授权.*凭据仅保存在本机/)).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: '已经完成授权' })).toBeDisabled();
  expect(screen.getByRole('button', { name: '退出授权' })).toBeVisible();
  expect(screen.queryByRole('tree')).not.toBeInTheDocument();
  expect(client.listCloudDirectory).not.toHaveBeenCalled();
});

it('can hide session actions in a file-only manager', async () => {
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} showSessionActions={false} />);

  expect(await screen.findByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '已经完成授权' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '退出授权' })).not.toBeInTheDocument();
});

it('logs out only after confirmation and returns to the authorization boundary', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));
  await screen.findByRole('tree', { name: '夸克网盘文件' });
  await user.click(screen.getByRole('button', { name: '退出授权' }));
  expect(screen.getByRole('alertdialog', { name: '确认退出授权' })).toBeVisible();
  expect(client.logoutCloud).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: '确认退出授权' }));
  expect(client.logoutCloud).toHaveBeenCalledWith('quark');
  expect(await screen.findByRole('region', { name: '夸克账号授权' })).toBeVisible();
  expect(screen.queryByRole('tree', { name: '夸克网盘文件' })).not.toBeInTheDocument();
});

it('adds the authorized account tab and drives the injected directory operations', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));

  expect(await screen.findByRole('tab', { name: 'alice · 夸克' })).toHaveAttribute('aria-selected', 'true');
  expect(screen.getByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
  expect(screen.getByText('报告.pdf')).toBeVisible();

  await user.dblClick(screen.getByText('归档'));
  expect(await screen.findByText('说明.txt')).toBeVisible();
  expect(screen.getByRole('navigation', { name: '当前目录' })).toHaveTextContent('根目录');
  expect(client.listCloudDirectory).toHaveBeenLastCalledWith('quark', 'folder-1');

  await user.click(screen.getByRole('button', { name: '新建目录' }));
  await user.type(screen.getByLabelText('目录名称'), '新资料');
  await user.click(screen.getByRole('button', { name: '确认新建目录' }));
  expect(client.createCloudDirectory).toHaveBeenCalledWith('quark', 'folder-1', '新资料');
});

it('uses real checkboxes for row selection and supports select all', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  const { container } = render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('报告.pdf');
  const folderCheckbox = screen.getByRole('checkbox', { name: '选择 归档' });
  const fileCheckbox = screen.getByRole('checkbox', { name: '选择 报告.pdf' });
  expect(folderCheckbox).not.toBeChecked();
  await user.click(screen.getByText('归档'));
  expect(folderCheckbox).toBeChecked();

  await user.click(screen.getByRole('checkbox', { name: '全选当前目录' }));
  expect(folderCheckbox).toBeChecked();
  expect(fileCheckbox).toBeChecked();
  const selectionHeader = screen.getByText('已选择 2 项');
  expect(selectionHeader.closest('.cloud-columns')).not.toBeNull();
  expect(screen.queryByText('名称')).not.toBeInTheDocument();
  expect(container.querySelector('.cloud-selection-summary')).not.toBeInTheDocument();
});

it('keeps file controls on one line and preserves full breadcrumb names in tooltips', async () => {
  const user = userEvent.setup();
  render(<CloudManager provider="quark" client={createCloudFake()} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('归档');
  expect(screen.getByLabelText('文件操作')).toHaveClass('cloud-toolbar--single-line');

  await user.dblClick(screen.getByText('归档'));
  const navigation = screen.getByRole('navigation', { name: '当前目录' });
  expect(within(navigation).getByRole('button', { name: '归档' })).toHaveAttribute('title', '归档');
});

it('keeps only download and delete visible in the selected-file header', async () => {
  const user = userEvent.setup();
  render(<CloudManager provider="quark" client={createCloudFake()} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('报告.pdf');
  await user.click(screen.getByRole('checkbox', { name: '选择 报告.pdf' }));

  const toolbar = screen.getByRole('toolbar', { name: '已选择 1 项的文件操作' });
  expect(within(toolbar).getByRole('button', { name: '下载' })).toBeVisible();
  expect(within(toolbar).getByRole('button', { name: '删除' })).toBeVisible();
  expect(within(toolbar).getByRole('button', { name: '更多文件操作' })).toBeVisible();
  expect(within(toolbar).queryByRole('button', { name: '重命名' })).not.toBeInTheDocument();
  expect(within(toolbar).queryByRole('button', { name: '移动' })).not.toBeInTheDocument();
  expect(within(toolbar).queryByRole('button', { name: '复制到' })).not.toBeInTheDocument();
  expect(within(toolbar).queryByRole('button', { name: '文件详情' })).not.toBeInTheDocument();

  await user.click(within(toolbar).getByRole('button', { name: '更多文件操作' }));
  const menu = screen.getByRole('menu', { name: '更多文件操作' });
  expect(within(menu).getByRole('menuitem', { name: '重命名' })).toBeVisible();
  expect(within(menu).getByRole('menuitem', { name: '移动' })).toBeVisible();
  expect(within(menu).getByRole('menuitem', { name: '复制到' })).toBeVisible();
  expect(within(menu).getByRole('menuitem', { name: '文件详情' })).toBeVisible();
});

it('formats large cloud files in practical size units', async () => {
  const client = createCloudFake({ listCloudDirectory: vi.fn().mockResolvedValue({ id: '0', items: [{ id: 'large-file', parentId: '0', name: '镜像文件.iso', kind: 'file', size: 3 * 1024 * 1024 * 1024 }] }) });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('镜像文件.iso');
  expect(screen.getByText('3.0 GB')).toBeVisible();
});

it('opens folders by double click without rendering an enter button', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('归档');
  expect(screen.queryByRole('button', { name: '进入 归档' })).not.toBeInTheDocument();
  await user.dblClick(screen.getByText('归档'));
  expect(await screen.findByText('说明.txt')).toBeVisible();
  expect(client.listCloudDirectory).toHaveBeenLastCalledWith('quark', 'folder-1');
});

it('shows normal file commands in the row context menu and opens a folder from it', async () => {
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('归档');
  fireEvent.contextMenu(screen.getByText('归档'), { clientX: 300, clientY: 240 });
  const menu = screen.getByRole('menu', { name: '归档 文件菜单' });
  expect(screen.getByRole('menuitem', { name: '打开' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '下载' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '重命名' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '移动' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '复制到' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '文件详情' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '删除' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '分享' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '移入超级保险箱' })).not.toBeInTheDocument();
  expect(menu).toBeVisible();

  fireEvent.click(screen.getByRole('menuitem', { name: '打开' }));
  expect(await screen.findByText('说明.txt')).toBeVisible();
});

it('uses smaller cloud-card typography while preserving the signed-in account name size', async () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const client = createCloudFake();
    render(<div className="cloud-provider-auths"><CloudManager provider="quark" client={client} authorizationOnly initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} /></div>);

    await act(async () => { await Promise.resolve(); });
    const heading = screen.getByRole('heading', { name: '夸克网盘 · alice' });
    const accountName = heading.querySelector<HTMLElement>('.cloud-account-name')!;
    const authorizationState = screen.getByRole('button', { name: '已经完成授权' });
    const logout = screen.getByRole('button', { name: '退出授权' });

    expect(getComputedStyle(heading).fontSize).toBe('18px');
    expect(getComputedStyle(authorizationState).fontSize).toBe('13px');
    expect(getComputedStyle(logout).fontSize).toBe('13px');
    expect(getComputedStyle(accountName).fontSize).toBe('13px');
  } finally {
    stylesheet.remove();
  }
});

it('repositions a bottom file context menu using its rendered height', async () => {
  const innerWidth = vi.spyOn(window, 'innerWidth', 'get').mockReturnValue(900);
  const innerHeight = vi.spyOn(window, 'innerHeight', 'get').mockReturnValue(500);
  const bounds = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function () {
    if (this.classList.contains('cloud-context-menu')) {
      return { width: 210, height: 350, top: 0, right: 210, bottom: 350, left: 0, x: 0, y: 0, toJSON: () => ({}) };
    }
    return { width: 0, height: 0, top: 0, right: 0, bottom: 0, left: 0, x: 0, y: 0, toJSON: () => ({}) };
  });

  try {
    render(<CloudManager provider="quark" client={createCloudFake()} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

    await screen.findByText('报告.pdf');
    fireEvent.contextMenu(screen.getByText('报告.pdf'), { clientX: 300, clientY: 480 });

    await waitFor(() => expect(screen.getByRole('menu', { name: '报告.pdf 文件菜单' })).toHaveStyle({ top: '142px' }));
  } finally {
    bounds.mockRestore();
    innerHeight.mockRestore();
    innerWidth.mockRestore();
  }
});

it('copies selected files through a browsable destination picker', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('报告.pdf');
  fireEvent.contextMenu(screen.getByText('报告.pdf'), { clientX: 300, clientY: 240 });
  await user.click(screen.getByRole('menuitem', { name: '复制到' }));
  const picker = screen.getByRole('dialog', { name: '选择复制位置' });
  expect(picker).toBeVisible();
  await user.click(screen.getByRole('button', { name: '打开 归档' }));
  expect(screen.getByRole('navigation', { name: '目标目录' })).toHaveTextContent('根目录›归档');
  await user.click(screen.getByRole('button', { name: '返回根目录' }));
  await user.click(screen.getByRole('button', { name: '复制到此处' }));

  expect(client.copyCloudFiles).toHaveBeenCalledWith('quark', ['file-1'], '0');
  expect(await screen.findByRole('status')).toHaveTextContent('复制成功');
});

it('retries the destination directory that actually failed to load', async () => {
  const user = userEvent.setup();
  let folderAttempts = 0;
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockImplementation((_provider: string, parentId = '0') => {
      if (parentId === 'folder-1') {
        folderAttempts += 1;
        if (folderAttempts === 1) return Promise.reject(new Error('目录暂时不可用'));
        return Promise.resolve({ id: 'folder-1', items: [] });
      }
      return Promise.resolve({ id: '0', items: [{ id: 'folder-1', parentId: '0', name: '归档', kind: 'folder' }, { id: 'file-1', parentId: '0', name: '报告.pdf', kind: 'file' }] });
    })
  });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('报告.pdf');
  fireEvent.contextMenu(screen.getByText('报告.pdf'), { clientX: 300, clientY: 240 });
  await user.click(screen.getByRole('menuitem', { name: '复制到' }));
  await user.click(screen.getByRole('button', { name: '打开 归档' }));
  expect(await screen.findByRole('alert')).toHaveTextContent('目录暂时不可用');
  expect(screen.getByRole('navigation', { name: '目标目录' })).toHaveTextContent('根目录›归档');

  await user.click(screen.getByRole('button', { name: '重试' }));
  expect(await screen.findByText('此目录没有子文件夹')).toBeVisible();
  expect(client.listCloudDirectory).toHaveBeenLastCalledWith('quark', 'folder-1');
  await user.click(screen.getByRole('button', { name: '复制到此处' }));
  expect(client.copyCloudFiles).toHaveBeenCalledWith('quark', ['file-1'], 'folder-1');
});

it('closes the destination picker and requires official web authorization when its directory read needs verification', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({
    listCloudDirectory: vi.fn()
      .mockResolvedValueOnce({ id: '0', items: [{ id: 'folder-1', parentId: '0', name: '归档', kind: 'folder' }, { id: 'file-1', parentId: '0', name: '报告.pdf', kind: 'file' }] })
      .mockRejectedValueOnce({ code: 'web_auth_required', message: '需要网页验证' })
  });
  render(<CloudManager provider="xunlei" client={client} initialAccounts={[{ id: 'x-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('报告.pdf');
  fireEvent.contextMenu(screen.getByText('报告.pdf'), { clientX: 300, clientY: 240 });
  await user.click(screen.getByRole('menuitem', { name: '移动' }));

  expect(await screen.findByRole('button', { name: '重新网页授权' })).toBeVisible();
  expect(screen.queryByRole('dialog', { name: '选择移动位置' })).not.toBeInTheDocument();
  expect(screen.getByRole('tab', { name: 'alice · 迅雷' })).toHaveAttribute('aria-selected', 'true');
  expect(client.moveCloudFiles).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: '重新网页授权' }));
  expect(client.beginCloudAuthorization).toHaveBeenCalledWith('xunlei');
});

it('normalizes provider modified times in the list and preserves Tianyi canonical timestamps in details', async () => {
  const user = userEvent.setup();
  const timestamp = 1_735_787_045_000;
  const date = new Date(timestamp);
  const localTimestamp = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockResolvedValue({ id: '0', items: [
      { id: 'seconds', parentId: '0', name: 'seconds.txt', kind: 'file', modifiedAt: timestamp / 1_000 },
      { id: 'milliseconds', parentId: '0', name: 'milliseconds.txt', kind: 'file', modifiedAt: timestamp },
      { id: 'iso', parentId: '0', name: 'iso.txt', kind: 'file', modifiedAt: '2025-01-02T03:04:05.000Z' },
      { id: 'tianyi', parentId: '0', name: 'tianyi.txt', kind: 'file', modifiedAt: '2025-06-07 08:09:10' },
      { id: 'unknown', parentId: '0', name: 'unknown.txt', kind: 'file', modifiedAt: '昨天' },
      { id: 'missing', parentId: '0', name: 'missing.txt', kind: 'file' }
    ] })
  });
  render(<CloudManager provider="tianyi" client={client} initialAccounts={[{ id: 't-1', displayName: 'alice', provider: 'tianyi', authMethod: 'web' }]} restoreOnMount={false} />);

  const tree = await screen.findByRole('tree', { name: '天翼网盘文件' });
  expect(within(tree).getByText('seconds.txt').closest('[role="treeitem"]')).toHaveTextContent(localTimestamp);
  expect(within(tree).getByText('milliseconds.txt').closest('[role="treeitem"]')).toHaveTextContent(localTimestamp);
  expect(within(tree).getByText('iso.txt').closest('[role="treeitem"]')).toHaveTextContent(localTimestamp);
  expect(within(tree).getByText('tianyi.txt').closest('[role="treeitem"]')).toHaveTextContent('2025-06-07 08:09:10');
  expect(within(tree).getByText('unknown.txt').closest('[role="treeitem"]')).toHaveTextContent('—');
  expect(within(tree).getByText('missing.txt').closest('[role="treeitem"]')).toHaveTextContent('—');

  await user.click(within(tree).getByText('tianyi.txt'));
  await user.click(screen.getByRole('button', { name: '更多文件操作' }));
  await user.click(within(screen.getByRole('menu', { name: '更多文件操作' })).getByRole('menuitem', { name: '文件详情' }));
  expect(screen.getByRole('dialog', { name: '文件详情' })).toHaveTextContent('2025-06-07 08:09:10');
});

it('shows concise file details in a separate dialog', async () => {
  const user = userEvent.setup();
  render(<CloudManager provider="quark" client={createCloudFake()} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('报告.pdf');
  await user.click(screen.getByText('报告.pdf'));
  const moreTrigger = screen.getByRole('button', { name: '更多文件操作' });
  await user.click(moreTrigger);
  const trigger = within(screen.getByRole('menu', { name: '更多文件操作' })).getByRole('menuitem', { name: '文件详情' });
  await user.click(trigger);

  const dialog = screen.getByRole('dialog', { name: '文件详情' });
  expect(dialog).toHaveTextContent('报告.pdf');
  expect(dialog).toHaveTextContent('1.9 MB');
  expect(dialog).toHaveTextContent('2025-06-07 08:09:10');
  expect(dialog).not.toHaveTextContent('file-1');
  expect(screen.getByRole('button', { name: '关闭文件详情' })).toHaveFocus();
  await user.tab();
  expect(screen.getByRole('button', { name: '关闭文件详情' })).toHaveFocus();
  await user.click(screen.getByRole('button', { name: '关闭文件详情' }));
  expect(screen.queryByRole('dialog', { name: '文件详情' })).not.toBeInTheDocument();
  expect(moreTrigger).toHaveFocus();
});

it('keeps multi-selection context actions valid for every selected item', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('归档');
  await user.click(screen.getByRole('checkbox', { name: '全选当前目录' }));
  fireEvent.contextMenu(screen.getByText('归档'), { clientX: 300, clientY: 240 });
  expect(screen.getByRole('menu', { name: '归档 文件菜单' })).toHaveTextContent('已选择 2 项');
  expect(screen.queryByRole('menuitem', { name: '打开' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '重命名' })).not.toBeInTheDocument();
  expect(screen.getByRole('menuitem', { name: '移动' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '删除' })).toBeVisible();
});

it('shows create, upload and refresh commands when right-clicking file-list whitespace', async () => {
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  const tree = await screen.findByRole('tree', { name: '夸克网盘文件' });
  fireEvent.click(screen.getByText('报告.pdf'));
  expect(screen.getByRole('checkbox', { name: '选择 报告.pdf' })).toBeChecked();
  fireEvent.contextMenu(tree, { clientX: 420, clientY: 320 });
  expect(screen.getByRole('checkbox', { name: '选择 报告.pdf' })).not.toBeChecked();
  expect(screen.getByRole('menu', { name: '文件区菜单' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '新建目录' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '上传文件' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '刷新' })).toBeVisible();
});

it('supports common file-manager keyboard shortcuts', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  const tree = await screen.findByRole('tree', { name: '夸克网盘文件' });
  tree.focus();
  fireEvent.keyDown(tree, { key: 'a', ctrlKey: true });
  expect(screen.getByRole('checkbox', { name: '选择 归档' })).toBeChecked();
  expect(screen.getByRole('checkbox', { name: '选择 报告.pdf' })).toBeChecked();

  await user.click(screen.getByText('报告.pdf'));
  fireEvent.keyDown(tree, { key: 'F2' });
  expect(screen.getByLabelText('新名称')).toHaveValue('报告.pdf');
  fireEvent.keyDown(tree, { key: 'Escape' });
  expect(screen.queryByLabelText('新名称')).not.toBeInTheDocument();
  fireEvent.keyDown(tree, { key: 'Delete' });
  expect(screen.getByRole('alertdialog', { name: '确认移入回收站' })).toBeVisible();
});

it('renders an explicit credential-expired state instead of a file list', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({ listCloudDirectory: vi.fn().mockRejectedValue(Object.assign(new Error('会话失效'), { code: 'credentials_expired' })) });
  render(<CloudManager provider="pikpak" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));
  expect(await screen.findByText('凭据已过期')).toBeVisible();
  expect(screen.getByRole('region', { name: 'PikPak账号授权' })).toBeVisible();
  expect(screen.getByRole('tab', { name: 'alice · PikPak' })).toBeVisible();
  expect(screen.queryByRole('tree', { name: 'PikPak网盘文件' })).not.toBeInTheDocument();
  expect(await screen.findByRole('button', { name: '使用网页授权' })).toBeVisible();
});

it('requires a recycle-bin confirmation before deleting selected files', async () => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="quark" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));
  await screen.findByText('报告.pdf');
  await user.click(screen.getByText('报告.pdf'));
  await user.click(screen.getByRole('button', { name: '删除' }));

  expect(screen.getByRole('alertdialog', { name: '确认移入回收站' })).toHaveTextContent('不会永久删除');
  expect(client.trashCloudFiles).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: '移入回收站' }));
  expect(client.trashCloudFiles).toHaveBeenCalledWith('quark', ['file-1']);
  expect(await screen.findByRole('status')).toHaveTextContent('删除成功');
});

it.each(['quark', 'uc', 'baidu', 'tianyi', 'aliyun', 'xunlei', 'pikpak'] as const)('shows a virtual recycle bin and recycle-only controls for %s', async (provider) => {
  const user = userEvent.setup();
  const recycleBinId = cloudRecycleBinDirectoryId[provider];
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockImplementation((_requestedProvider: CloudProvider, parentId = '0') => Promise.resolve(parentId === recycleBinId
      ? { id: recycleBinId, items: [{ id: 'deleted-file', parentId: recycleBinId, name: '已删除文件.txt', kind: 'file', size: 12 }] }
      : { id: parentId, items: [{ id: 'live-file', parentId, name: '仍在网盘的文件.txt', kind: 'file', size: 10 }] }))
  });
  render(<CloudManager provider={provider} client={client} initialAccounts={[{ id: `${provider}-1`, displayName: 'alice', provider, authMethod: 'web' }]} restoreOnMount={false} />);

  await user.dblClick(await screen.findByText('回收站'));
  await screen.findByText('已删除文件.txt');
  expect(screen.queryByRole('button', { name: '上传' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '新建目录' })).not.toBeInTheDocument();

  await user.click(screen.getByText('已删除文件.txt'));
  const toolbar = screen.getByRole('toolbar', { name: '已选择 1 项的文件操作' });
  expect(within(toolbar).getByRole('button', { name: '恢复' })).toBeVisible();
  expect(within(toolbar).getByRole('button', { name: '永久删除' })).toBeVisible();
  expect(within(toolbar).queryByRole('button', { name: '下载' })).not.toBeInTheDocument();
});

it('restores and permanently deletes non-PikPak recycle records after confirmation', async () => {
  const user = userEvent.setup();
  const provider = 'quark' as const;
  const recycleBinId = cloudRecycleBinDirectoryId[provider];
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockImplementation((_requestedProvider: CloudProvider, parentId = '0') => Promise.resolve(parentId === recycleBinId
      ? { id: recycleBinId, items: [{ id: 'quark-recycle-record', parentId: recycleBinId, name: '已删除文件.txt', kind: 'file', size: 12 }] }
      : { id: parentId, items: [] }))
  });
  render(<CloudManager provider={provider} client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider, authMethod: 'web' }]} restoreOnMount={false} />);

  await user.dblClick(await screen.findByText('回收站'));
  await user.click(await screen.findByText('已删除文件.txt'));
  await user.click(screen.getByRole('button', { name: '恢复' }));
  expect(client.restoreCloudFiles).toHaveBeenCalledWith('quark', ['quark-recycle-record']);

  await screen.findByText('已删除文件.txt');
  await user.click(screen.getByText('已删除文件.txt'));
  await user.click(screen.getByRole('button', { name: '永久删除' }));
  const dialog = screen.getByRole('alertdialog', { name: '确认永久删除' });
  expect(dialog).toHaveTextContent('不可恢复');
  await user.click(within(dialog).getByRole('button', { name: '永久删除', exact: true }));
  expect(client.deleteCloudFiles).toHaveBeenCalledWith('quark', ['quark-recycle-record']);
  expect(client.trashCloudFiles).not.toHaveBeenCalled();
});

it('limits PikPak recycle-bin items to restore and permanent deletion without folder navigation', async () => {
  const user = userEvent.setup();
  const recycleBinId = '__all_downloader_pikpak_recycle_bin__';
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockImplementation((_provider: string, parentId = '0') => Promise.resolve(parentId === recycleBinId
      ? { id: recycleBinId, items: [
        { id: 'deleted-file', parentId: recycleBinId, name: '已删除文件.txt', kind: 'file', size: 12 },
        { id: 'deleted-folder', parentId: recycleBinId, name: '已删除文件夹', kind: 'folder' }
      ] }
      : { id: '0', items: [
        { id: recycleBinId, parentId: '0', name: '回收站', kind: 'folder' },
        { id: 'live-file', parentId: '0', name: '仍在网盘的文件.txt', kind: 'file', size: 10 }
      ] }))
  });
  render(<CloudManager provider="pikpak" client={client} initialAccounts={[{ id: 'p-1', displayName: 'alice', provider: 'pikpak', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('回收站');
  fireEvent.doubleClick(screen.getByText('回收站'));
  const tree = await screen.findByRole('tree', { name: 'PikPak网盘文件' });
  await screen.findByText('已删除文件.txt');
  expect(screen.queryByRole('button', { name: '上传' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '新建目录' })).not.toBeInTheDocument();

  fireEvent.contextMenu(tree, { clientX: 260, clientY: 220 });
  expect(screen.queryByRole('menuitem', { name: '新建目录' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '上传文件' })).not.toBeInTheDocument();
  expect(screen.getByRole('menuitem', { name: '刷新' })).toBeVisible();

  const deletedFolder = screen.getByTitle('已删除文件夹');
  fireEvent.contextMenu(deletedFolder, { clientX: 300, clientY: 240 });
  expect(screen.queryByRole('menuitem', { name: '打开' })).not.toBeInTheDocument();
  expect(screen.getByRole('menuitem', { name: '恢复' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '永久删除' })).toBeVisible();
  client.listCloudDirectory.mockClear();
  fireEvent.doubleClick(deletedFolder);
  fireEvent.keyDown(tree, { key: 'Enter' });
  expect(client.listCloudDirectory).not.toHaveBeenCalled();

  await user.click(screen.getByText('已删除文件.txt'));
  await user.click(screen.getByRole('button', { name: '恢复' }));
  expect(client.restoreCloudFiles).toHaveBeenCalledWith('pikpak', ['deleted-file']);

  await screen.findByText('已删除文件.txt');
  await user.click(screen.getByText('已删除文件.txt'));
  await user.click(screen.getByRole('button', { name: '永久删除' }));
  const permanentDeleteDialog = screen.getByRole('alertdialog', { name: '确认永久删除' });
  expect(permanentDeleteDialog).toHaveTextContent('不可恢复');
  expect(client.deleteCloudFiles).not.toHaveBeenCalled();
  await user.click(within(permanentDeleteDialog).getByRole('button', { name: '永久删除', exact: true }));
  expect(client.deleteCloudFiles).toHaveBeenCalledWith('pikpak', ['deleted-file']);
  expect(client.trashCloudFiles).not.toHaveBeenCalled();
});

it('excludes the PikPak recycle-bin virtual folder from move destinations', async () => {
  const user = userEvent.setup();
  const recycleBinId = '__all_downloader_pikpak_recycle_bin__';
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockResolvedValue({ id: '0', items: [
      { id: recycleBinId, parentId: '0', name: '回收站', kind: 'folder' },
      { id: 'live-file', parentId: '0', name: '仍在网盘的文件.txt', kind: 'file', size: 10 }
    ] })
  });
  render(<CloudManager provider="pikpak" client={client} initialAccounts={[{ id: 'p-1', displayName: 'alice', provider: 'pikpak', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('仍在网盘的文件.txt');
  await user.click(screen.getByText('仍在网盘的文件.txt'));
  await user.click(screen.getByRole('button', { name: '更多文件操作' }));
  await user.click(screen.getByRole('menuitem', { name: '移动' }));

  const picker = await screen.findByRole('dialog', { name: '选择移动位置' });
  expect(picker).not.toHaveTextContent('回收站');
  expect(screen.queryByRole('button', { name: '打开 回收站' })).not.toBeInTheDocument();
});

it('treats the root PikPak recycle-bin folder as navigation-only', async () => {
  const user = userEvent.setup();
  const recycleBinId = '__all_downloader_pikpak_recycle_bin__';
  const client = createCloudFake({
    listCloudDirectory: vi.fn().mockImplementation((_provider: string, parentId = '0') => Promise.resolve(parentId === recycleBinId
      ? { id: recycleBinId, items: [] }
      : { id: '0', items: [{ id: recycleBinId, parentId: '0', name: '回收站', kind: 'folder' }] }))
  });
  render(<CloudManager provider="pikpak" client={client} initialAccounts={[{ id: 'p-1', displayName: 'alice', provider: 'pikpak', authMethod: 'web' }]} restoreOnMount={false} />);

  const recycleBin = await screen.findByText('回收站');
  await user.click(recycleBin);
  expect(screen.queryByRole('button', { name: '下载' })).not.toBeInTheDocument();
  fireEvent.keyDown(screen.getByRole('tree', { name: 'PikPak网盘文件' }), { key: 'Delete' });
  expect(screen.queryByRole('alertdialog', { name: '确认移入回收站' })).not.toBeInTheDocument();

  fireEvent.contextMenu(recycleBin, { clientX: 300, clientY: 240 });
  expect(screen.getByRole('menuitem', { name: '打开' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '下载' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '删除' })).not.toBeInTheDocument();
  fireEvent.doubleClick(recycleBin);
  await waitFor(() => expect(client.listCloudDirectory).toHaveBeenLastCalledWith('pikpak', recycleBinId));
});

it('queues a selected cloud folder through the directory download command and reports every queued task', async () => {
  const user = userEvent.setup();
  const onDownloadTask = vi.fn();
  const queuedTask = { id: 'folder-download-1', source: 'cloud://quark/child-1', name: '说明.txt', kind: 'cloud' as const, status: 'queued' as const, progress: 0 };
  const downloadCloudDirectory = vi.fn().mockResolvedValue({ queuedCount: 1, tasks: [{ task: queuedTask }], failures: [] });
  const client = createCloudFake({ downloadCloudDirectory });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} onDownloadTask={onDownloadTask} />);

  await screen.findByText('归档');
  await user.click(screen.getByRole('checkbox', { name: '选择 归档' }));
  await user.click(screen.getByRole('button', { name: '下载' }));

  await waitFor(() => expect(downloadCloudDirectory).toHaveBeenCalledWith('quark', 'folder-1', '归档'));
  expect(client.downloadCloudFile).not.toHaveBeenCalled();
  expect(onDownloadTask).toHaveBeenCalledWith({ task: queuedTask });
});

it('reports queued count and redacted per-file directory download failures instead of silently succeeding', async () => {
  const user = userEvent.setup();
  const queuedTask = { id: 'folder-download-1', source: 'cloud://quark/child-1', name: '说明.txt', kind: 'cloud' as const, status: 'queued' as const, progress: 0 };
  const client = createCloudFake({
    downloadCloudDirectory: vi.fn().mockResolvedValue({
      queuedCount: 1,
      tasks: [{ task: queuedTask }],
      failures: [{ relativePath: '归档/私密文件.txt', code: 'network_error', message: '网络或下载引擎错误' }]
    })
  });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByText('归档');
  await user.click(screen.getByRole('checkbox', { name: '选择 归档' }));
  await user.click(screen.getByRole('button', { name: '下载' }));

  expect(await screen.findByRole('alert')).toHaveTextContent('目录下载已入队 1 个文件；归档/私密文件.txt：网络或下载引擎错误');
});

it('keeps the current file list visible when a cloud operation fails', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({ downloadCloudFile: vi.fn().mockRejectedValue(new Error('网络断开')) });
  render(<CloudManager provider="quark" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));
  await screen.findByText('报告.pdf');
  await user.click(screen.getByText('报告.pdf'));
  await user.click(screen.getByRole('button', { name: '下载' }));

  expect(await screen.findByRole('alert')).toHaveTextContent('下载失败');
  expect(screen.getByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
  expect(screen.getByText('报告.pdf')).toBeVisible();
});

it.each([
  ['pikpak', 'PikPak'],
  ['xunlei', '迅雷']
] as const)('offers %s reauthorization after a download requires official web verification', async (provider, label) => {
  const user = userEvent.setup();
  const client = createCloudFake({
    downloadCloudFile: vi.fn().mockRejectedValue({
      code: 'web_auth_required',
      message: '此登录方式需要网页验证，请点击网页授权完成登录'
    })
  });
  render(<CloudManager provider={provider} client={client} initialAccounts={[
    { id: 'account-1', displayName: 'alice', provider, authMethod: 'web' }
  ]} restoreOnMount={false} />);

  await screen.findByRole('tree', { name: `${label}网盘文件` });
  await user.click(screen.getByText('报告.pdf'));
  await user.click(screen.getByRole('button', { name: '下载' }));

  expect(await screen.findByRole('button', { name: '重新网页授权' })).toBeVisible();
  await user.click(screen.getByRole('button', { name: '重新网页授权' }));
  expect(client.beginCloudAuthorization).toHaveBeenCalledWith(provider);
  expect(await screen.findByRole('tab', { name: `alice · ${label}` })).toHaveAttribute('aria-selected', 'true');
  await waitFor(() => expect(client.completeCloudAuthorization).toHaveBeenCalledWith(provider));
});

it.each(['provider_error', 'invalid_response', 'credentials_expired'])(
  'stops Xunlei authorization polling after %s until the user starts it again', async (code) => {
    vi.useFakeTimers();
    try {
      const client = createCloudFake({ completeCloudAuthorization: vi.fn()
        .mockRejectedValueOnce({ code, message: '迅雷授权验证失败，请重新授权。' })
        .mockResolvedValue({ id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' }) });
      const onAccountAuthorized = vi.fn();
      render(<CloudManager provider="xunlei" client={client} authorizationOnly onAccountAuthorized={onAccountAuthorized} />);

      await act(async () => { await Promise.resolve(); });
      fireEvent.click(screen.getByRole('button', { name: '使用网页授权' }));
      await act(async () => { await Promise.resolve(); });
      await act(async () => { await vi.advanceTimersByTimeAsync(800); });
      await act(async () => { await vi.advanceTimersByTimeAsync(5_400); });

      expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(1);
      expect(onAccountAuthorized).not.toHaveBeenCalled();
      expect(screen.getByRole('status')).toHaveTextContent('迅雷授权验证失败，请重新授权。');
      expect(screen.queryByRole('button', { name: '我已完成授权' })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole('button', { name: '使用网页授权' }));
      await act(async () => { await Promise.resolve(); });
      await act(async () => { await vi.advanceTimersByTimeAsync(800); });
      expect(client.beginCloudAuthorization).toHaveBeenCalledTimes(2);
      expect(onAccountAuthorized).toHaveBeenCalledWith({ id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' });
    } finally {
      vi.useRealTimers();
    }
  }
);

it('preserves the Xunlei account, directory and rejection when renewed authorization stops', async () => {
  vi.useFakeTimers();
  try {
    const client = createCloudFake({
      downloadCloudFile: vi.fn().mockRejectedValue({ code: 'web_auth_required', message: '需要网页验证' }),
      completeCloudAuthorization: vi.fn().mockRejectedValue({ code: 'provider_error', message: '迅雷拒绝了本次授权验证。' })
    });
    const onAccountLoggedOut = vi.fn();
    render(<CloudManager provider="xunlei" client={client} initialAccounts={[
      { id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' }
    ]} restoreOnMount={false} showSessionActions={false} onAccountLoggedOut={onAccountLoggedOut} />);
    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByText('报告.pdf'));
    fireEvent.click(screen.getByRole('button', { name: '下载' }));
    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByRole('button', { name: '重新网页授权' }));
    await act(async () => { await Promise.resolve(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(800); });
    await act(async () => { await vi.advanceTimersByTimeAsync(5_400); });

    expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('tab', { name: 'alice · 迅雷' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('报告.pdf')).toBeVisible();
    expect(screen.getByRole('alert')).toHaveTextContent('迅雷拒绝了本次授权验证。');
    expect(screen.getByRole('button', { name: '重新网页授权' })).toBeVisible();
    expect(client.logoutCloud).not.toHaveBeenCalled();
    expect(onAccountLoggedOut).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});

it.each(['web_auth_required', 'credentials_expired'])('recovers Xunlei initial directory %s with session actions hidden', async (code) => {
  const user = userEvent.setup();
  const client = createCloudFake({ listCloudDirectory: vi.fn()
    .mockRejectedValueOnce({ code, message: '网盘需要重新授权' })
    .mockResolvedValue({ id: '0', items: [{ id: 'file-2', parentId: '0', name: '恢复后的文件.txt', kind: 'file' }] }) });
  render(<CloudManager provider="xunlei" client={client} initialAccounts={[
    { id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' }
  ]} restoreOnMount={false} showSessionActions={false} />);

  await user.click(await screen.findByRole('button', { name: '重新网页授权' }));
  expect(client.beginCloudAuthorization).toHaveBeenCalledWith('xunlei');
  expect(screen.getByRole('tab', { name: 'alice · 迅雷' })).toHaveAttribute('aria-selected', 'true');
  expect(await screen.findByText('恢复后的文件.txt', {}, { timeout: 2_000 })).toBeVisible();
  expect(screen.queryByRole('button', { name: '重新网页授权' })).not.toBeInTheDocument();
});

it.each(['web_auth_required', 'credentials_expired'])('preserves the Xunlei directory after refresh returns %s', async (code) => {
  const user = userEvent.setup();
  const client = createCloudFake();
  render(<CloudManager provider="xunlei" client={client} initialAccounts={[
    { id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' }
  ]} restoreOnMount={false} showSessionActions={false} />);

  await screen.findByText('报告.pdf');
  client.listCloudDirectory.mockRejectedValueOnce({ code, message: '网盘需要重新授权' });
  await user.click(screen.getByRole('button', { name: '刷新' }));
  expect(await screen.findByRole('button', { name: '重新网页授权' })).toBeVisible();
  expect(screen.getByText('报告.pdf')).toBeVisible();
  expect(screen.getByRole('tree', { name: '迅雷网盘文件' })).toBeVisible();
  expect(screen.getByRole('alert')).toHaveTextContent('网页验证');
});

it.each(['web_auth_required', 'credentials_expired'])('keeps the Xunlei account and directory during pending renewal after %s', async (code) => {
  vi.useFakeTimers();
  try {
    const client = createCloudFake({
      downloadCloudFile: vi.fn().mockRejectedValue({ code, message: '网盘需要重新授权' }),
      completeCloudAuthorization: vi.fn()
        .mockRejectedValueOnce({ code: 'authorization_pending', message: '正在等待登录' })
        .mockResolvedValue({ id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' })
    });
    const onAccountLoggedOut = vi.fn();
    render(<CloudManager provider="xunlei" client={client} initialAccounts={[
      { id: 'account-1', displayName: 'alice', provider: 'xunlei', authMethod: 'web' }
    ]} restoreOnMount={false} showSessionActions={false} onAccountLoggedOut={onAccountLoggedOut} />);
    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByText('报告.pdf'));
    fireEvent.click(screen.getByRole('button', { name: '下载' }));
    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByRole('button', { name: '重新网页授权' }));
    await act(async () => { await Promise.resolve(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(800); });

    expect(screen.getByRole('tab', { name: 'alice · 迅雷' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('报告.pdf')).toBeVisible();
    expect(client.logoutCloud).not.toHaveBeenCalled();
    expect(onAccountLoggedOut).not.toHaveBeenCalled();

    await act(async () => { await vi.advanceTimersByTimeAsync(1_800); });
    expect(client.completeCloudAuthorization).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('button', { name: '重新网页授权' })).not.toBeInTheDocument();
    expect(screen.getByRole('tree', { name: '迅雷网盘文件' })).toBeVisible();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  } finally {
    vi.useRealTimers();
  }
});

it('surfaces the message when opening a web authorization window fails', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({ beginCloudAuthorization: vi.fn().mockRejectedValue({ code: 'authorization_unavailable', message: '无法打开授权窗口' }) });
  render(<CloudManager provider="pikpak" client={client} />);

  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));

  expect(await screen.findByRole('status')).toHaveTextContent('无法打开授权窗口');
});

it('prevents duplicate directory submissions while the first operation is pending', async () => {
  const user = userEvent.setup();
  let finish: (() => void) | undefined;
  const pending = new Promise<void>((resolve) => { finish = resolve; });
  const client = createCloudFake({ createCloudDirectory: vi.fn().mockReturnValue(pending) });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByRole('tree', { name: '夸克网盘文件' });
  await user.click(screen.getByRole('button', { name: '新建目录' }));
  await user.type(screen.getByLabelText('目录名称'), '资料');
  const submit = screen.getByRole('button', { name: '确认新建目录' });
  fireEvent.click(submit);
  fireEvent.click(submit);

  expect(client.createCloudDirectory).toHaveBeenCalledTimes(1);
  expect(submit).toBeDisabled();
  await act(async () => { finish?.(); await pending; });
  expect(await screen.findByRole('status')).toHaveTextContent('新建目录成功');
});

it('treats closing the upload picker as a cancellation instead of a success', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({ pickAndUploadFiles: vi.fn().mockResolvedValue([]) });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  await screen.findByRole('tree', { name: '夸克网盘文件' });
  await user.click(screen.getByRole('button', { name: '上传' }));
  expect(client.pickAndUploadFiles).toHaveBeenCalledWith('quark', '0');
  expect(screen.queryByText(/上传成功|已上传/)).not.toBeInTheDocument();
});

it('automatically completes a renewed web authorization after credentials expire', async () => {
  const user = userEvent.setup();
  const client = createCloudFake({ listCloudDirectory: vi.fn()
    .mockRejectedValueOnce(Object.assign(new Error('会话失效'), { code: 'credentials_expired' }))
    .mockResolvedValue({ id: '0', items: [{ id: 'file-1', parentId: '0', name: '恢复后的文件.txt', kind: 'file', size: 12 }] }) });
  render(<CloudManager provider="quark" client={client} initialAccounts={[{ id: 'q-1', displayName: 'alice', provider: 'quark', authMethod: 'web' }]} restoreOnMount={false} />);

  expect(await screen.findByText('凭据已过期')).toBeVisible();
  await user.click(await screen.findByRole('button', { name: '使用网页授权' }));

  expect(await screen.findByRole('tree', { name: '夸克网盘文件' }, { timeout: 2_000 })).toBeVisible();
  expect(client.completeCloudAuthorization).toHaveBeenCalledWith('quark');
});
