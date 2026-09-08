import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { App } from './App';

const cloudApiMocks = vi.hoisted(() => ({
  beginCloudAuthorization: vi.fn(),
  completeCloudAuthorization: vi.fn(),
  loginCloudWithPassword: vi.fn(),
  getCloudAccount: vi.fn().mockRejectedValue({ code: 'not_authorized', message: '未授权' }),
  listCloudDirectory: vi.fn(),
  createCloudDirectory: vi.fn(),
  renameCloudFile: vi.fn(),
  moveCloudFiles: vi.fn(),
  trashCloudFiles: vi.fn(),
  downloadCloudFile: vi.fn(),
  logoutCloud: vi.fn(),
  pickAndUploadFiles: vi.fn()
}));

vi.mock('./lib/cloudApi', () => cloudApiMocks);

it('renders the compact download-manager layout without browser navigation controls', async () => {
  render(<App />);
  expect(screen.getByRole('heading', { name: '万能下载' })).toBeVisible();
  expect(screen.getByPlaceholderText('搜索任务或粘贴下载链接')).toBeVisible();
  expect(screen.getByRole('button', { name: '新建下载' })).toBeVisible();
  expect(screen.getByRole('tab', { name: /下载中/ })).toBeVisible();
  expect(screen.getByRole('tab', { name: /已完成/ })).toBeVisible();
  expect(screen.queryByRole('button', { name: '后退' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '前进' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '刷新' })).not.toBeInTheDocument();
  await waitFor(() => expect(document.querySelector('.cloud-workspace-recovering')).not.toBeInTheDocument());
});

it('leaves quick input to the independent native launcher window', async () => {
  render(<App />);
  expect(screen.queryByRole('button', { name: '打开下载输入' })).not.toBeInTheDocument();
  expect(screen.getByRole('region', { name: '下载队列' })).toBeVisible();
  await waitFor(() => expect(document.querySelector('.cloud-workspace-recovering')).not.toBeInTheDocument());
});

it('lets the user switch between automatic, light and dark themes', async () => {
  const user = userEvent.setup();
  render(<App />);

  expect(screen.getByRole('group', { name: '主题' })).toBeVisible();
  await user.click(screen.getByRole('button', { name: '浅色' }));
  expect(document.documentElement).toHaveAttribute('data-theme', 'light');
  expect(screen.getByRole('button', { name: '浅色' })).toHaveAttribute('aria-pressed', 'true');

  await user.click(screen.getByRole('button', { name: '深色' }));
  expect(document.documentElement).toHaveAttribute('data-theme', 'dark');
});

it('opens the same new-download dialog from the main toolbar', async () => {
  const user = userEvent.setup();
  render(<App />);

  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '新建下载' }));
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
});

it('opens the cloud management workspace and restores each provider once', async () => {
  const user = userEvent.setup();
  cloudApiMocks.getCloudAccount.mockClear();
  render(<App />);

  await user.click(screen.getByRole('button', { name: /我的网盘/ }));

  expect(await screen.findByRole('region', { name: '网盘管理' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '网盘管理' })).toHaveAttribute('aria-selected', 'true');
  expect(cloudApiMocks.getCloudAccount).toHaveBeenCalledTimes(2);
  expect(cloudApiMocks.getCloudAccount).toHaveBeenCalledWith('quark');
  expect(cloudApiMocks.getCloudAccount).toHaveBeenCalledWith('pikpak');
});

it('preloads cloud authorization before the user opens My Cloud', async () => {
  const user = userEvent.setup();
  cloudApiMocks.getCloudAccount.mockClear();
  cloudApiMocks.getCloudAccount.mockImplementation((provider: string) => provider === 'quark'
    ? Promise.resolve({ provider: 'quark', id: 'quark-account', displayName: '我的夸克', authMethod: 'web' })
    : Promise.reject({ code: 'not_authorized', message: '未授权' }));

  render(<App />);

  await waitFor(() => expect(cloudApiMocks.getCloudAccount).toHaveBeenCalledTimes(2));
  expect(screen.queryByRole('region', { name: '网盘管理' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: /我的网盘/ }));

  expect(screen.getByRole('region', { name: '网盘管理' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '夸克网盘' })).toBeVisible();
  expect(screen.getByRole('button', { name: '已经完成授权' })).toBeDisabled();
});

it('adds a native cloud download result to the visible download queue', async () => {
  const user = userEvent.setup();
  cloudApiMocks.getCloudAccount.mockImplementation((provider: string) => provider === 'quark'
    ? Promise.resolve({ provider: 'quark', id: 'quark-account', displayName: '我的夸克', authMethod: 'web' })
    : Promise.reject({ code: 'not_authorized', message: '未授权' }));
  cloudApiMocks.listCloudDirectory.mockResolvedValue({ id: '0', items: [{ id: 'cloud-file', parentId: '0', name: '网盘电影.mkv', kind: 'file', size: 1024 }] });
  cloudApiMocks.downloadCloudFile.mockResolvedValue({ id: 'cloud-task', source: 'cloud://quark/cloud-file', name: '网盘电影.mkv', kind: 'cloud', status: 'downloading', progress: 0 });
  render(<App />);

  await user.click(screen.getByRole('button', { name: /我的网盘/ }));
  await user.click(await screen.findByRole('tab', { name: '夸克网盘' }));
  await user.click(await screen.findByRole('treeitem', { name: /网盘电影\.mkv/ }));
  await user.click(screen.getByRole('button', { name: '下载', exact: true }));

  expect(await screen.findByText('已将「网盘电影.mkv」加入下载队列')).toBeVisible();
  await user.click(within(screen.getByRole('navigation', { name: '主导航' })).getByRole('button', { name: /下载/ }));
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByTitle('网盘电影.mkv')).toBeVisible();
});

it('shows task details only after the task context-menu command', async () => {
  const user = userEvent.setup();
  render(<App />);
  const search = screen.getByRole('textbox', { name: '搜索任务或粘贴下载链接' });

  await user.type(search, 'https://example.com/archive.zip{Enter}');
  expect(await screen.findByTitle('archive.zip')).toBeVisible();
  expect(screen.queryByRole('dialog', { name: '任务详情' })).not.toBeInTheDocument();

  await user.click(screen.getByTitle('archive.zip'));
  expect(screen.queryByRole('dialog', { name: '任务详情' })).not.toBeInTheDocument();

  fireEvent.contextMenu(screen.getByTitle('archive.zip'), { clientX: 320, clientY: 220 });
  await user.click(screen.getByRole('menuitem', { name: '查看任务详情' }));
  expect(screen.getByRole('dialog', { name: '任务详情' })).toBeVisible();
});
