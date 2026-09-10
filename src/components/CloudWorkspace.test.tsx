import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { CloudWorkspace } from './CloudWorkspace';

type TestAccount = { id: string; displayName: string; provider: 'quark' | 'pikpak'; authMethod: 'web' | 'password' };

function createClient(accounts: Record<string, TestAccount | undefined> = {}) {
  return {
    beginCloudAuthorization: vi.fn().mockResolvedValue({ provider: 'quark', windowLabel: '登录', instructions: '完成登录' }),
    completeCloudAuthorization: vi.fn().mockImplementation((provider: 'quark' | 'pikpak') => Promise.resolve({ id: `${provider}-1`, displayName: `${provider}-alice`, provider, authMethod: 'web' as const })),
    loginCloudWithPassword: vi.fn().mockImplementation((provider: 'quark' | 'pikpak') => Promise.resolve({ id: `${provider}-1`, displayName: `${provider}-alice`, provider, authMethod: 'password' as const })),
    getCloudAccount: vi.fn().mockImplementation((provider: 'quark' | 'pikpak') => accounts[provider] ? Promise.resolve(accounts[provider]) : Promise.reject(Object.assign(new Error('未授权'), { code: 'unauthorized' }))),
    listCloudDirectory: vi.fn().mockImplementation((provider: string, parentId = '0') => Promise.resolve({ id: parentId, items: [{ id: `${provider}-file`, parentId, name: `${provider}-文件.txt`, kind: 'file', size: 10, modifiedAt: '刚刚' }] })),
    createCloudDirectory: vi.fn().mockResolvedValue(undefined), renameCloudFile: vi.fn().mockResolvedValue(undefined), moveCloudFiles: vi.fn().mockResolvedValue(undefined), copyCloudFiles: vi.fn().mockResolvedValue(undefined), trashCloudFiles: vi.fn().mockResolvedValue(undefined), deleteCloudFiles: vi.fn().mockResolvedValue(undefined), downloadCloudFile: vi.fn().mockResolvedValue({}), downloadCloudDirectory: vi.fn().mockResolvedValue({ queuedCount: 0, tasks: [], failures: [] }), logoutCloud: vi.fn().mockResolvedValue(undefined), pickAndUploadFiles: vi.fn().mockResolvedValue(undefined),
    getCloudAuthorizationCapabilities: vi.fn().mockImplementation((provider: string) => Promise.resolve({
      provider,
      web: true,
      password: provider === 'pikpak' || provider === 'baidu' || provider === 'tianyi',
      passwordLoginMode: provider === 'pikpak' || provider === 'baidu' || provider === 'tianyi'
        ? 'rustDirect'
        : provider === 'aliyun' || provider === 'xunlei' || provider === 'uc'
          ? 'officialWebView'
          : 'none'
    }))
  };
}

it('defaults to the cloud management tab and lists every supported provider there', async () => {
  const client = createClient();
  render(<CloudWorkspace client={client} />);

  expect(await screen.findByRole('tab', { name: '网盘管理' })).toHaveAttribute('aria-selected', 'true');
  expect(screen.getByRole('region', { name: '网盘管理' })).toBeVisible();
  expect(screen.queryByText('添加网盘')).not.toBeInTheDocument();
  expect(screen.queryByText(/成功连接后会生成独立的文件管理标签/)).not.toBeInTheDocument();
  expect(screen.getByRole('region', { name: '夸克账号授权' })).toBeVisible();
  expect(screen.getByRole('region', { name: 'PikPak账号授权' })).toBeVisible();
  expect(screen.queryByRole('tab', { name: '夸克网盘' })).not.toBeInTheDocument();
  expect(screen.queryByRole('tab', { name: 'PikPak网盘' })).not.toBeInTheDocument();
  expect(screen.queryByRole('tree')).not.toBeInTheDocument();
  await waitFor(() => expect(client.getCloudAccount).toHaveBeenCalledTimes(7));
});

it('renders authorization cards for every newly supported cloud service', async () => {
  const client = createClient();
  render(<CloudWorkspace client={client as never} />);

  for (const label of ['百度', '天翼', '阿里', '迅雷']) {
    expect(await screen.findByRole('region', { name: `${label}账号授权` })).toBeVisible();
  }
  await waitFor(() => expect(client.getCloudAccount).toHaveBeenCalledTimes(7));
});

it('fails closed when UC capability lookup is unavailable', async () => {
  const client = createClient();
  client.getCloudAuthorizationCapabilities.mockImplementation((provider: string) => provider === 'uc'
    ? Promise.reject(new Error('能力查询暂不可用'))
    : Promise.resolve({ provider, web: true, password: provider === 'pikpak' || provider === 'baidu' || provider === 'tianyi', passwordLoginMode: provider === 'pikpak' || provider === 'baidu' || provider === 'tianyi' ? 'rustDirect' : provider === 'aliyun' || provider === 'xunlei' ? 'officialWebView' : 'none' }));
  render(<CloudWorkspace client={client as never} />);

  const ucAuthorization = await screen.findByRole('region', { name: 'UC账号授权' });
  expect(within(ucAuthorization).getByRole('button', { name: '重试' })).toBeEnabled();
  expect(within(ucAuthorization).queryByLabelText('UC账号')).not.toBeInTheDocument();
});

it('opens a recovered UC account at root 0', async () => {
  const user = userEvent.setup();
  const client = createClient({
    uc: { id: 'uc-1', displayName: 'uc-alice', provider: 'uc' as never, authMethod: 'web' }
  });
  render(<CloudWorkspace client={client as never} />);

  await user.click(await screen.findByRole('tab', { name: 'UC网盘' }));
  expect(await screen.findByRole('tree', { name: 'UC网盘文件' })).toBeVisible();
  expect(client.listCloudDirectory).toHaveBeenLastCalledWith('uc', '0');
});

it('ignores a recovered account when the provider does not match the request', async () => {
  const client = createClient();
  client.getCloudAccount.mockResolvedValue({ id: 'p-1', displayName: 'p-alice', provider: 'pikpak', authMethod: 'password' });
  render(<CloudWorkspace client={client as never} />);

  await waitFor(() => expect(client.getCloudAccount).toHaveBeenCalledTimes(7));
  expect(screen.getAllByRole('tab', { name: 'PikPak网盘' })).toHaveLength(1);
  expect(screen.queryByRole('tab', { name: '夸克网盘' })).not.toBeInTheDocument();
});

it('never renders logged-out controls before persisted account recovery finishes', async () => {
  let resolveQuark!: (account: TestAccount) => void;
  let resolvePikpak!: (account: TestAccount) => void;
  const quark = new Promise<TestAccount>((resolve) => { resolveQuark = resolve; });
  const pikpak = new Promise<TestAccount>((resolve) => { resolvePikpak = resolve; });
  const client = createClient();
  client.getCloudAccount.mockImplementation((provider: 'quark' | 'pikpak') => provider === 'quark' ? quark : pikpak);

  render(<CloudWorkspace client={client} />);

  expect(screen.getByRole('status', { name: '正在读取网盘账号状态' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '使用网页授权' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '登录PikPak账号' })).not.toBeInTheDocument();

  resolveQuark({ id: 'q-1', displayName: 'q-alice', provider: 'quark', authMethod: 'web' });
  resolvePikpak({ id: 'p-1', displayName: 'p-alice', provider: 'pikpak', authMethod: 'password' });

  expect(await screen.findByRole('tab', { name: '夸克网盘' })).toBeVisible();
  expect(screen.getByRole('tab', { name: 'PikPak网盘' })).toBeVisible();
  expect(screen.queryByRole('status', { name: '正在读取网盘账号状态' })).not.toBeInTheDocument();
});

it('adds a provider tab after password login but keeps account controls in management', async () => {
  const user = userEvent.setup();
  const client = createClient();
  render(<CloudWorkspace client={client} />);

  await user.type(await screen.findByLabelText('PikPak账号'), 'alice');
  await user.type(screen.getByLabelText('PikPak密码'), 'secret');
  await user.click(screen.getByRole('button', { name: '登录PikPak账号' }));

  expect(await screen.findByRole('tab', { name: 'PikPak网盘' })).toHaveAttribute('aria-selected', 'false');
  expect(screen.getByRole('tab', { name: '网盘管理' })).toHaveAttribute('aria-selected', 'true');
  expect(screen.getByRole('button', { name: '已经登录' })).toBeDisabled();
  expect(screen.getByRole('button', { name: '退出登录' })).toBeVisible();
  expect(screen.queryByRole('tree')).not.toBeInTheDocument();

  await user.click(screen.getByRole('tab', { name: 'PikPak网盘' }));
  expect(await screen.findByRole('tree', { name: 'PikPak网盘文件' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '已经登录' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '退出登录' })).not.toBeInTheDocument();
});

it('adds a Quark file tab after web authorization without moving authorization actions into it', async () => {
  const user = userEvent.setup();
  const client = createClient();
  render(<CloudWorkspace client={client} />);

  await user.click((await screen.findAllByRole('button', { name: '使用网页授权' }))[0]);
  await user.click(screen.getByRole('button', { name: '我已完成授权' }));

  expect(await screen.findByRole('tab', { name: '夸克网盘' })).toHaveAttribute('aria-selected', 'false');
  expect(screen.getByRole('button', { name: '已经完成授权' })).toBeDisabled();
  expect(screen.getByRole('button', { name: '退出授权' })).toBeVisible();

  await user.click(screen.getByRole('tab', { name: '夸克网盘' }));
  expect(await screen.findByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
  expect(screen.queryByText('已经完成授权')).not.toBeInTheDocument();
  expect(screen.queryByText('退出授权')).not.toBeInTheDocument();
});

it('automatically adds the Aliyun workspace tab after web authorization completes', async () => {
  const user = userEvent.setup();
  const client = createClient();
  render(<CloudWorkspace client={client as never} />);

  const aliyunAuthorization = await screen.findByRole('region', { name: '阿里账号授权' });
  await user.click(within(aliyunAuthorization).getByRole('button', { name: '在官方网页登录（可使用账号密码）' }));

  expect(await screen.findByRole('tab', { name: '阿里网盘' }, { timeout: 1_500 })).toHaveAttribute('aria-selected', 'false');
  expect(client.completeCloudAuthorization).toHaveBeenCalledWith('aliyun');
});

it('recovers accounts into tabs while still opening on cloud management', async () => {
  const client = createClient({ quark: { id: 'q-1', displayName: 'q-alice', provider: 'quark', authMethod: 'web' } });
  render(<CloudWorkspace client={client} />);

  expect(await screen.findByRole('tab', { name: '夸克网盘' })).toHaveAttribute('aria-selected', 'false');
  expect(screen.getByRole('tab', { name: '网盘管理' })).toHaveAttribute('aria-selected', 'true');
  expect(screen.queryByRole('tree')).not.toBeInTheDocument();
  expect(client.listCloudDirectory).not.toHaveBeenCalled();
  await waitFor(() => expect(client.getCloudAccount).toHaveBeenCalledTimes(7));
});

it('shows only authorized provider tabs and switches their real directory views', async () => {
  const user = userEvent.setup();
  const client = createClient({ quark: { id: 'q-1', displayName: 'q-alice', provider: 'quark', authMethod: 'web' }, pikpak: { id: 'p-1', displayName: 'p-alice', provider: 'pikpak', authMethod: 'password' } });
  render(<CloudWorkspace client={client} />);

  await screen.findByRole('tab', { name: '夸克网盘' });
  expect(screen.getByRole('tab', { name: 'PikPak网盘' })).toBeVisible();
  await user.click(screen.getByRole('tab', { name: '夸克网盘' }));
  expect(await screen.findByText('quark-文件.txt')).toBeVisible();
  await user.click(screen.getByRole('tab', { name: 'PikPak网盘' }));
  expect(await screen.findByText('pikpak-文件.txt')).toBeVisible();
});

it('logs out from management, removes the provider tab, and stays on management', async () => {
  const user = userEvent.setup();
  const client = createClient({ quark: { id: 'q-1', displayName: 'q-alice', provider: 'quark', authMethod: 'web' } });
  render(<CloudWorkspace client={client} />);

  await screen.findByRole('tab', { name: '夸克网盘' });
  await user.click(screen.getByRole('button', { name: '退出授权' }));
  await user.click(screen.getByRole('button', { name: '确认退出授权' }));

  await waitFor(() => expect(screen.queryByRole('tab', { name: '夸克网盘' })).not.toBeInTheDocument());
  expect(screen.getByRole('tab', { name: '网盘管理' })).toHaveAttribute('aria-selected', 'true');
  expect(screen.getByRole('region', { name: '夸克账号授权' })).toBeVisible();
  expect(screen.queryByRole('tree')).not.toBeInTheDocument();
});
