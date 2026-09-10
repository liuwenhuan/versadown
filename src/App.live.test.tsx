import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { readFileSync } from 'node:fs';
import { beforeEach, expect, it, vi } from 'vitest';
import type { QueueTask } from './lib/types';
import { getThemeMode, setThemeMode } from './lib/theme';

const state = vi.hoisted(() => ({
  list: vi.fn(), parse: vi.fn(), onTask: undefined as undefined | ((task: QueueTask) => void),
  onFocus: undefined as undefined | ((event: { payload: boolean }) => void),
  listeners: new Map<string, (event: { payload: unknown }) => void>(), listen: vi.fn(),
  confirm: vi.fn(), confirmCloud: vi.fn(), resolveDuplicate: vi.fn(), openDirectory: vi.fn(), selectFiles: vi.fn(), cancelFile: vi.fn(), getEngineState: vi.fn(), retryEngine: vi.fn(), taskAction: vi.fn(),
  moveToRecycle: vi.fn(), purgeTasks: vi.fn(), clipboardWrite: vi.fn(),
  updateAiSettings: vi.fn(), listAiModels: vi.fn(), startDragging: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('./lib/api', () => ({
  isTauriRuntime: () => true, listPersistedTasks: state.list, parseDownload: state.parse,
  confirmDownload: state.confirm, confirmCloudShareDownload: state.confirmCloud, runTaskAction: state.taskAction, selectDownloadFiles: state.selectFiles, cancelDownloadFile: state.cancelFile,
  getDownloadEngineState: state.getEngineState, retryDownloadEngine: state.retryEngine,
  defaultSettings: { debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always', proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }, magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' }, aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 8, btSeedRatio: 1.5 } },
  getSettings: vi.fn().mockResolvedValue({ debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always', proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }, magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' }, aria2: { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 8, btSeedRatio: 1.5 } }),
  updateSettings: vi.fn(), updateAiSettings: state.updateAiSettings, listAiModels: state.listAiModels, openTaskDirectory: state.openDirectory,
  moveTaskToRecycle: state.moveToRecycle, purgeRecycledTasks: state.purgeTasks, removeTaskRecord: vi.fn(), resolveDuplicateDownload: state.resolveDuplicate
}));
vi.mock('./lib/downloadEvents', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./lib/downloadEvents')>();
  return { ...actual, subscribeToDownloadEvents: vi.fn().mockImplementation(({ onTask }) => {
    state.onTask = onTask;
    return Promise.resolve(() => undefined);
  }) };
});
vi.mock('@tauri-apps/api/event', () => ({ listen: state.listen }));
vi.mock('@tauri-apps/plugin-clipboard-manager', () => ({ writeText: state.clipboardWrite }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({
  onFocusChanged: vi.fn().mockImplementation((handler) => { state.onFocus = handler; return Promise.resolve(() => undefined); }),
  onResized: vi.fn().mockResolvedValue(() => undefined),
  startDragging: state.startDragging
}) }));
vi.mock('./lib/mainWindow', () => ({ mainWindowIsMaximized: vi.fn().mockResolvedValue(false), runMainWindowAction: vi.fn() }));

import { App } from './App';

const oldTask: QueueTask = { id: 'task-1', name: 'archive.iso', kind: 'file', status: 'downloading', progress: 10, total_length: 100, completed_length: 10 };
const liveTask: QueueTask = { ...oldTask, progress: 40, completed_length: 40 };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  state.list.mockReset();
  state.list.mockResolvedValue([]);
  state.parse.mockReset();
  state.confirm.mockReset();
  state.confirmCloud.mockReset();
  state.resolveDuplicate.mockReset();
  state.openDirectory.mockReset();
  state.selectFiles.mockReset();
  state.cancelFile.mockReset();
  state.getEngineState.mockReset();
  state.getEngineState.mockResolvedValue({ available: true, retrying: false });
  state.retryEngine.mockReset();
  state.taskAction.mockReset();
  state.moveToRecycle.mockReset();
  state.purgeTasks.mockReset();
  state.clipboardWrite.mockReset();
  state.updateAiSettings.mockReset();
  state.listAiModels.mockReset();
  state.startDragging.mockReset();
  state.startDragging.mockResolvedValue(undefined);
  state.onTask = undefined;
  state.onFocus = undefined;
  state.listeners.clear();
  state.listen.mockReset();
  state.listen.mockImplementation((name, handler) => {
    state.listeners.set(name, handler);
    return Promise.resolve(() => undefined);
  });
});

it('starts native dragging from the application icon and title', () => {
  render(<App />);

  fireEvent.mouseDown(document.querySelector('.brand')!, { button: 0 });

  expect(state.startDragging).toHaveBeenCalledOnce();
});

it('starts native dragging from blank title-bar space without stealing interactive controls', () => {
  render(<App />);
  const header = document.querySelector('.app-header')!;

  fireEvent.mouseDown(header, { button: 0 });
  expect(state.startDragging).toHaveBeenCalledOnce();

  fireEvent.mouseDown(screen.getByRole('textbox', { name: '搜索任务或粘贴下载链接' }), { button: 0 });
  fireEvent.mouseDown(screen.getByRole('button', { name: '新建下载' }), { button: 0 });
  fireEvent.mouseDown(screen.getByRole('button', { name: '最小化' }), { button: 0 });

  expect(state.startDragging).toHaveBeenCalledOnce();
});

it('uses regular font weight for task tabs and file titles', async () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    state.list.mockResolvedValueOnce([oldTask]);
    render(<App />);

    const taskTitle = await screen.findByText('archive.iso');
    const downloadingTab = screen.getByRole('tab', { name: /下载中/ });

    expect(getComputedStyle(taskTitle).fontWeight).toBe('400');
    expect(getComputedStyle(downloadingTab).fontWeight).toBe('400');
  } finally {
    stylesheet.remove();
  }
});

it('keeps the application title bold as the typography exception', async () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    render(<App />);

    const title = await screen.findByRole('heading', { name: '万能下载' });
    expect(getComputedStyle(title).fontWeight).toBe('700');
  } finally {
    stylesheet.remove();
  }
});

it('shows an unavailable engine and clears it after a successful manual retry', async () => {
  const user = userEvent.setup();
  state.list.mockResolvedValueOnce([]);
  state.getEngineState.mockResolvedValueOnce({
    available: false, retrying: false, error: '下载引擎不可用：找不到 aria2'
  });
  state.retryEngine.mockResolvedValueOnce({ available: true, retrying: false });

  render(<App />);

  expect(await screen.findByText('下载引擎不可用')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '重试下载引擎' }));
  await waitFor(() => expect(screen.queryByText('下载引擎不可用')).not.toBeInTheDocument());
  expect(state.retryEngine).toHaveBeenCalledOnce();
});

it('keeps a live snapshot when the initial list returns an older copy', async () => {
  const initial = deferred<QueueTask[]>();
  state.list.mockReturnValueOnce(initial.promise);
  render(<App />);
  await waitFor(() => expect(state.onTask).toBeDefined());
  act(() => { state.onTask?.(liveTask); });
  await act(async () => { initial.resolve([oldTask]); });

  expect(screen.getByRole('progressbar', { name: 'archive.iso 下载进度' })).toHaveAttribute('aria-valuenow', '40');
  expect(within(screen.getByRole('region', { name: '下载队列' })).getAllByText('archive.iso')).toHaveLength(1);
});

it('requests active tasks for the first paint before loading terminal tabs in the background', async () => {
  const terminal = deferred<QueueTask[]>();
  const activeTask: QueueTask = { ...oldTask, id: 'active-startup', name: 'active-startup.iso' };
  const completedTask: QueueTask = { ...oldTask, id: 'completed-startup', name: 'completed-startup.iso', status: 'completed', progress: 100 };
  state.list.mockImplementation((scope?: string) => {
    if (scope === 'active') return Promise.resolve([activeTask]);
    if (scope === 'terminal') return terminal.promise;
    return Promise.resolve([activeTask, completedTask]);
  });
  const user = userEvent.setup();

  render(<App />);

  expect(await screen.findByText('active-startup.iso')).toBeVisible();
  expect(state.list.mock.calls[0]).toEqual(['active']);
  expect(screen.queryByText('completed-startup.iso')).not.toBeInTheDocument();

  await act(async () => { terminal.resolve([completedTask]); });
  await user.click(screen.getByRole('tab', { name: /已完成/ }));

  expect(await screen.findByText('completed-startup.iso')).toBeVisible();
  expect(state.list.mock.calls.map(([scope]) => scope)).toEqual(['active', 'terminal']);
});

it('parses each non-empty line from the first new-download tab and then opens downloading tasks', async () => {
  const user = userEvent.setup();
  const firstTask: QueueTask = { id: 'batch-1', name: 'first.zip', kind: 'file', status: 'downloading', progress: 0 };
  const secondTask: QueueTask = { id: 'batch-2', name: 'second.mp4', kind: 'file', status: 'queued', progress: 0 };
  state.parse
    .mockResolvedValueOnce({ task: firstTask })
    .mockResolvedValueOnce({ task: secondTask });

  render(<App />);

  const tabs = screen.getAllByRole('tab');
  expect(tabs[0]).toHaveAccessibleName('新建下载');
  await user.click(tabs[0]);
  await user.type(screen.getByRole('textbox', { name: '下载链接' }), 'https://example.test/first.zip{enter}{enter}https://example.test/second.mp4');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  await waitFor(() => expect(state.parse.mock.calls).toEqual([
    ['https://example.test/first.zip', undefined],
    ['https://example.test/second.mp4', undefined],
  ]));
  expect(screen.getByRole('tab', { name: /下载中/ })).toHaveAttribute('aria-selected', 'true');
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByTitle('first.zip')).toBeVisible();
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByTitle('second.mp4')).toBeVisible();
});

it('opens downloading tasks after confirming a download parsed from the new-download tab', async () => {
  const user = userEvent.setup();
  state.parse.mockResolvedValueOnce({
    confirmation: {
      taskId: 'torrent-batch', title: 'collection', requiresConfirmation: true,
      resources: [{ id: 'file-1', name: 'movie.mkv', size: '1 GB', selected: true }], processing: [],
    },
  });
  state.confirm.mockResolvedValueOnce({ id: 'torrent-batch', name: 'collection', kind: 'torrent', status: 'downloading', progress: 0 });

  render(<App />);
  await user.click(screen.getByRole('tab', { name: '新建下载' }));
  await user.type(screen.getByRole('textbox', { name: '下载链接' }), 'magnet:?xt=urn:btih:abc');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(await screen.findByRole('dialog', { name: '确认下载内容' })).toBeVisible();
  expect(screen.getByRole('tab', { name: '新建下载' })).toHaveAttribute('aria-selected', 'true');
  await user.click(screen.getByRole('button', { name: /立即下载 1/ }));

  await waitFor(() => expect(screen.getByRole('tab', { name: /下载中/ })).toHaveAttribute('aria-selected', 'true'));
});

it('uses the dark launcher graphic and vector icons in the primary navigation', async () => {
  const previousTheme = getThemeMode();
  setThemeMode('dark');
  const app = render(<App />);

  try {
    await screen.findByRole('heading', { name: '万能下载' });
    const brandMark = document.querySelector('.brand-mark');
    expect(brandMark).toHaveAttribute('data-icon-variant', 'dark');
    expect(brandMark?.querySelector('img')).toHaveAttribute('src', expect.stringContaining('dark_mint'));

    const navigation = screen.getByRole('navigation', { name: '主导航' });
    expect(navigation.querySelectorAll('.nav-icon svg')).toHaveLength(3);
    expect(navigation).not.toHaveTextContent('↓');
    expect(navigation).not.toHaveTextContent('☁');
    expect(navigation).not.toHaveTextContent('⚙');
  } finally {
    app.unmount();
    setThemeMode(previousTheme);
  }
});

it('shows the tab-scoped action before total speed only for its active download tab', async () => {
  state.list.mockResolvedValueOnce([
    { id: 'active-task', name: 'active.zip', kind: 'file', status: 'downloading', progress: 25 },
    { id: 'recycled-task', name: 'removed.zip', kind: 'file', status: 'recycled', progress: 100 }
  ]);
  const user = userEvent.setup();
  render(<App />);

  const speed = await screen.findByLabelText('当前总下载速度');
  const activeAction = screen.getByRole('button', { name: /全部暂停/ });
  expect(activeAction).toHaveClass('bulk-pause');
  expect(activeAction.compareDocumentPosition(speed) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  expect(screen.queryByRole('button', { name: '清空' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('tab', { name: /回收站/ }));

  const recycleAction = screen.getByRole('button', { name: '清空' });
  expect(recycleAction).toHaveClass('bulk-pause');
  expect(recycleAction.compareDocumentPosition(speed) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  expect(screen.queryByRole('button', { name: /全部暂停/ })).not.toBeInTheDocument();
});

it('sorts only the active task tab from the menu beside total speed', async () => {
  state.list.mockResolvedValueOnce([
    { id: 'active-old', name: 'alpha.zip', kind: 'file', status: 'downloading', progress: 80, total_length: 800, created_at: 100 },
    { id: 'active-new', name: 'zeta.zip', kind: 'file', status: 'downloading', progress: 20, total_length: 200, created_at: 200 },
    { id: 'done-old', name: 'beta.zip', kind: 'file', status: 'completed', progress: 100, total_length: 400, created_at: 100 },
    { id: 'done-new', name: 'delta.zip', kind: 'file', status: 'completed', progress: 100, total_length: 300, created_at: 300 },
  ]);
  const user = userEvent.setup();
  render(<App />);

  const queue = await screen.findByRole('region', { name: '下载队列' });
  const visibleNames = () => Array.from(queue.querySelectorAll<HTMLElement>('.task-copy b')).map((item) => item.textContent);
  await waitFor(() => expect(visibleNames()).toEqual(['zeta.zip', 'alpha.zip']));

  await user.click(screen.getByRole('button', { name: '排序任务' }));
  expect(screen.getByRole('menu', { name: '任务排序' })).toBeVisible();
  await user.click(screen.getByRole('menuitemradio', { name: '按文件名称从 A 到 Z' }));
  expect(visibleNames()).toEqual(['alpha.zip', 'zeta.zip']);

  await user.click(screen.getByRole('button', { name: '排序任务' }));
  await user.click(screen.getByRole('menuitemradio', { name: '按当前进度从低到高' }));
  expect(visibleNames()).toEqual(['zeta.zip', 'alpha.zip']);

  await user.click(screen.getByRole('button', { name: '排序任务' }));
  await user.click(screen.getByRole('menuitemradio', { name: '按文件大小从大到小' }));
  expect(visibleNames()).toEqual(['alpha.zip', 'zeta.zip']);

  await user.click(screen.getByRole('tab', { name: /已完成/ }));
  expect(visibleNames()).toEqual(['delta.zip', 'beta.zip']);
});

it('shows all start for paused tasks and resumes every paused task from the active tab', async () => {
  const pausedTask: QueueTask = { ...oldTask, id: 'paused-task', status: 'paused' };
  const downloadingTask: QueueTask = { ...oldTask, id: 'downloading-task', status: 'downloading' };
  state.list.mockResolvedValueOnce([pausedTask, downloadingTask]);
  state.taskAction.mockResolvedValueOnce({ task: { ...pausedTask, status: 'downloading' } });
  const user = userEvent.setup();
  render(<App />);

  await user.click(await screen.findByRole('button', { name: /全部开始/ }));

  await waitFor(() => expect(state.taskAction).toHaveBeenCalledWith('paused-task', 'resume'));
  expect(state.taskAction).not.toHaveBeenCalledWith('downloading-task', 'resume');
});

it('copies all selected task links to the clipboard on separate lines', async () => {
  const first: QueueTask = { ...oldTask, id: 'copy-1', name: 'first.zip', source: 'https://example.com/first.zip' };
  const second: QueueTask = { ...oldTask, id: 'copy-2', name: 'second.zip', source: 'magnet:?xt=urn:btih:SECOND' };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'active' ? [first, second] : []));
  state.clipboardWrite.mockResolvedValue(undefined);
  render(<App />);

  fireEvent.click(await screen.findByText('first.zip'));
  fireEvent.click(screen.getByText('second.zip'), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByText('first.zip'));
  fireEvent.click(screen.getByRole('menuitem', { name: '复制下载链接' }));

  await waitFor(() => expect(state.clipboardWrite).toHaveBeenCalledOnce());
  const copiedLines = String(state.clipboardWrite.mock.calls[0][0]).split('\n');
  expect(copiedLines).toHaveLength(2);
  expect(copiedLines).toEqual(expect.arrayContaining(['https://example.com/first.zip', 'magnet:?xt=urn:btih:SECOND']));
});

it('starts every selected paused task from the multi-selection menu', async () => {
  const first: QueueTask = { ...oldTask, id: 'resume-1', name: 'first-paused.zip', status: 'paused' };
  const second: QueueTask = { ...oldTask, id: 'resume-2', name: 'second-paused.zip', status: 'paused' };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'active' ? [first, second] : []));
  state.taskAction.mockImplementation((id: string) => Promise.resolve({ task: { ...(id === first.id ? first : second), status: 'downloading' } }));
  render(<App />);

  fireEvent.click(await screen.findByText('first-paused.zip'));
  fireEvent.click(screen.getByText('second-paused.zip'), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByText('first-paused.zip'));
  fireEvent.click(screen.getByRole('menuitem', { name: '开始下载' }));

  await waitFor(() => {
    expect(state.taskAction).toHaveBeenCalledWith('resume-1', 'resume');
    expect(state.taskAction).toHaveBeenCalledWith('resume-2', 'resume');
  });
});

it('deletes every selected active task from the multi-selection menu', async () => {
  const first: QueueTask = { ...oldTask, id: 'delete-1', name: 'first-delete.zip' };
  const second: QueueTask = { ...oldTask, id: 'delete-2', name: 'second-delete.zip' };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'active' ? [first, second] : []));
  state.taskAction.mockImplementation((id: string) => Promise.resolve({ task: { ...(id === first.id ? first : second), status: 'cancelled' } }));
  render(<App />);

  fireEvent.click(await screen.findByText('first-delete.zip'));
  fireEvent.click(screen.getByText('second-delete.zip'), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByText('first-delete.zip'));
  fireEvent.click(screen.getByRole('menuitem', { name: '删除' }));

  await waitFor(() => {
    expect(state.taskAction).toHaveBeenCalledWith('delete-1', 'cancel');
    expect(state.taskAction).toHaveBeenCalledWith('delete-2', 'cancel');
  });
});

it('pauses every selected running task from the multi-selection menu', async () => {
  const first: QueueTask = { ...oldTask, id: 'pause-1', name: 'first-running.zip' };
  const second: QueueTask = { ...oldTask, id: 'pause-2', name: 'second-running.zip' };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'active' ? [first, second] : []));
  state.taskAction.mockImplementation((id: string) => Promise.resolve({ task: { ...(id === first.id ? first : second), status: 'paused' } }));
  render(<App />);

  fireEvent.click(await screen.findByText('first-running.zip'));
  fireEvent.click(screen.getByText('second-running.zip'), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByText('first-running.zip'));
  fireEvent.click(screen.getByRole('menuitem', { name: '暂停下载' }));

  await waitFor(() => {
    expect(state.taskAction).toHaveBeenCalledWith('pause-1', 'pause');
    expect(state.taskAction).toHaveBeenCalledWith('pause-2', 'pause');
  });
});

it('moves every selected completed task to the recycle bin', async () => {
  const first: QueueTask = { ...oldTask, id: 'done-delete-1', name: 'first-done.zip', status: 'completed', progress: 100 };
  const second: QueueTask = { ...oldTask, id: 'done-delete-2', name: 'second-done.zip', status: 'completed', progress: 100 };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'terminal' ? [first, second] : []));
  state.moveToRecycle.mockImplementation((id: string) => Promise.resolve({ ...(id === first.id ? first : second), status: 'recycled' }));
  render(<App />);

  fireEvent.click(await screen.findByRole('tab', { name: /已完成/ }));
  fireEvent.click(await screen.findByText('first-done.zip'));
  fireEvent.click(screen.getByText('second-done.zip'), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByText('first-done.zip'));
  fireEvent.click(screen.getByRole('menuitem', { name: '删除' }));

  await waitFor(() => {
    expect(state.moveToRecycle).toHaveBeenCalledWith('done-delete-1');
    expect(state.moveToRecycle).toHaveBeenCalledWith('done-delete-2');
  });
});

it('opens one permanent-delete confirmation for all selected recycled tasks', async () => {
  const first: QueueTask = { ...oldTask, id: 'purge-1', name: 'first-old.zip', status: 'recycled', progress: 100 };
  const second: QueueTask = { ...oldTask, id: 'purge-2', name: 'second-old.zip', status: 'cancelled', progress: 100 };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'terminal' ? [first, second] : []));
  state.purgeTasks.mockResolvedValue(undefined);
  render(<App />);

  fireEvent.click(await screen.findByRole('tab', { name: /回收站/ }));
  fireEvent.click(await screen.findByText('first-old.zip'));
  fireEvent.click(screen.getByText('second-old.zip'), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByText('first-old.zip'));
  fireEvent.click(screen.getByRole('menuitem', { name: '删除' }));

  expect(screen.getByRole('dialog', { name: '彻底删除任务' })).toHaveTextContent('2 个任务');
  fireEvent.click(screen.getByRole('button', { name: '彻底删除' }));
  await waitFor(() => expect(state.purgeTasks).toHaveBeenCalledWith(expect.arrayContaining(['purge-1', 'purge-2']), false));
});

it('clears cleanup retry only after the typed task action result reports success', async () => {
  const cleanupTask: QueueTask = {
    id: 'cleanup-task', name: 'cleanup.bin', kind: 'cloud', status: 'completed', progress: 100,
    source: 'cloud-share://baidu/share/entry', cleanup_pending: true, error: 'opaque provider failure',
  };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'active' ? [] : [cleanupTask]));
  state.taskAction.mockResolvedValueOnce({
    task: { ...cleanupTask, cleanup_pending: false, error: undefined },
  });
  render(<App />);
  fireEvent.click(await screen.findByRole('tab', { name: /已完成/ }));

  fireEvent.click(await screen.findByRole('button', { name: '重试网盘清理 cleanup.bin' }));

  await waitFor(() => expect(state.taskAction).toHaveBeenCalledWith('cleanup-task', 'retry_cleanup'));
  await waitFor(() => expect(screen.queryByRole('button', { name: '重试网盘清理 cleanup.bin' })).not.toBeInTheDocument());
});

it('keeps typed cleanup retry available when the task action fails', async () => {
  const cleanupTask: QueueTask = {
    id: 'cleanup-task', name: 'cleanup.bin', kind: 'cloud', status: 'completed', progress: 100,
    source: 'cloud-share://baidu/share/entry', cleanup_pending: true,
  };
  state.list.mockImplementation((scope?: string) => Promise.resolve(scope === 'active' ? [] : [cleanupTask]));
  state.taskAction.mockRejectedValueOnce(new Error('cleanup retry failed'));
  render(<App />);
  fireEvent.click(await screen.findByRole('tab', { name: /已完成/ }));

  fireEvent.click(await screen.findByRole('button', { name: '重试网盘清理 cleanup.bin' }));

  expect(await screen.findByRole('status')).toHaveTextContent('cleanup retry failed');
  expect(screen.getByRole('button', { name: '重试网盘清理 cleanup.bin' })).toBeVisible();
});

it('upserts a parsed task that already arrived through a live event', async () => {
  state.list.mockResolvedValueOnce([]);
  state.parse.mockResolvedValueOnce({ task: liveTask });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  act(() => { state.onTask?.(liveTask); });
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://example.test/archive.iso' }); });

  expect(within(screen.getByRole('region', { name: '下载队列' })).getAllByText('archive.iso')).toHaveLength(1);
});

it('keeps a protected share open for an access code before completing its normal parse result', async () => {
  const user = userEvent.setup();
  const shareLink = 'https://pan.baidu.com/s/protected-share';
  state.parse
    .mockResolvedValueOnce({ shareAccessCodeRequired: { provider: 'baidu', message: '请输入提取码' } })
    .mockResolvedValueOnce({ task: liveTask });
  render(<App />);

  await user.click(await screen.findByRole('button', { name: '新建下载' }));
  const downloadInput = screen.getByLabelText('下载内容');
  await user.type(downloadInput, shareLink);
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  const accessCode = await screen.findByLabelText('访问码');
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
  expect(downloadInput).toHaveValue(shareLink);
  expect(accessCode).toHaveFocus();
  expect(state.parse).toHaveBeenLastCalledWith(shareLink, undefined);

  await user.type(accessCode, '1234');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  await waitFor(() => expect(state.parse).toHaveBeenLastCalledWith(shareLink, '1234'));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument());
  expect(screen.getByText('archive.iso')).toBeVisible();
});

it('queues normal share results and presents duplicate results in FIFO order', async () => {
  state.parse.mockResolvedValueOnce({
    confirmation: {
      taskId: 'cloud-share://baidu/plan', title: '分享目录', requiresConfirmation: true,
      resources: [{ id: 'f1', name: 'first.bin', size: '1 B', selected: true }], processing: []
    }
  });
  state.confirmCloud.mockResolvedValueOnce({ outcomes: [
    { task: { id: 'queued-1', name: 'first.bin', kind: 'cloud', status: 'downloading', progress: 0 } },
    { duplicate: { task: { id: 'duplicate-1', name: 'first-existing.bin', kind: 'cloud', status: 'completed', progress: 100 }, state: 'completed_files_present' } },
    { task: { id: 'queued-2', name: 'second.bin', kind: 'cloud', status: 'downloading', progress: 0 } },
    { duplicate: { task: { id: 'duplicate-2', name: 'second-existing.bin', kind: 'cloud', status: 'downloading', progress: 20 }, state: 'existing_active' } }
  ], failures: [] });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());

  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://pan.baidu.com/s/share' }); });
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));

  const firstDuplicateDialog = await screen.findByRole('dialog', { name: '检测到重复下载' });
  expect(firstDuplicateDialog).toHaveTextContent('first-existing.bin');
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByTitle('first.bin')).toBeVisible();
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByTitle('second.bin')).toBeVisible();
  expect(firstDuplicateDialog).not.toHaveTextContent('second-existing.bin');

  const cancel = screen.getByRole('button', { name: '取消' });
  fireEvent.click(cancel);
  fireEvent.click(cancel);
  await waitFor(() => expect(screen.getByRole('dialog', { name: '检测到重复下载' })).toHaveTextContent('second-existing.bin'));
});

it('keeps only failed share entries selected while preserving earlier successful outcomes', async () => {
  state.parse.mockResolvedValueOnce({
    confirmation: {
      taskId: 'cloud-share://uc/partial-plan', title: 'UC 分享', requiresConfirmation: true,
      resources: [
        { id: 'ok', name: 'ok.jpg', size: '1 B', selected: true },
        { id: 'failed', name: 'failed.jpg', size: '2 B', selected: true },
      ], processing: []
    }
  });
  state.confirmCloud.mockResolvedValueOnce({
    outcomes: [{ task: { id: 'queued-ok', name: 'ok.jpg', kind: 'cloud', status: 'downloading', progress: 0 } }],
    failures: [{ selectedId: 'failed', code: 'web_auth_required', message: '网盘需要网页验证' }],
  });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/partial-plan' }); });

  fireEvent.click(screen.getByRole('button', { name: /立即下载 2/ }));

  expect(await screen.findByRole('status')).toHaveTextContent('网盘需要网页验证');
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByTitle('ok.jpg')).toBeVisible();
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toBeVisible();
  expect(screen.getByRole('checkbox', { name: '选择 ok.jpg' })).not.toBeChecked();
  expect(screen.getByRole('checkbox', { name: '选择 failed.jpg' })).toBeChecked();
});

it('keeps share confirmation open after one rejected submission and ignores concurrent clicks', async () => {
  const submission = deferred<never>();
  state.parse.mockResolvedValueOnce({
    confirmation: {
      taskId: 'cloud-share://uc/plan', title: 'UC 分享', requiresConfirmation: true,
      resources: [{ id: 'f1', name: 'photo.jpg', size: '1 B', selected: true }], processing: []
    }
  });
  state.confirmCloud.mockReturnValueOnce(submission.promise);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/share' }); });

  const submit = screen.getByRole('button', { name: /立即下载 1/ });
  fireEvent.click(submit);
  fireEvent.click(submit);
  expect(state.confirmCloud).toHaveBeenCalledTimes(1);

  await act(async () => { submission.reject(new Error('服务器拒绝下载')); });
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toBeVisible();
  expect(await screen.findByRole('status')).toHaveTextContent('服务器拒绝下载');
});

it('binds in-flight share confirmation to its plan without blocking or clearing a newer plan', async () => {
  const firstSubmission = deferred<{ outcomes: never[]; failures: never[] }>();
  const secondSubmission = deferred<{ outcomes: never[]; failures: never[] }>();
  state.parse
    .mockResolvedValueOnce({
      confirmation: {
        taskId: 'cloud-share://uc/plan-a', title: '分享 A', requiresConfirmation: true,
        resources: [{ id: 'a1', name: 'first.jpg', size: '1 B', selected: true }], processing: []
      }
    })
    .mockResolvedValueOnce({
      confirmation: {
        taskId: 'cloud-share://uc/plan-b', title: '分享 B', requiresConfirmation: true,
        resources: [{ id: 'b1', name: 'second.jpg', size: '1 B', selected: true }], processing: []
      }
    });
  state.confirmCloud
    .mockReturnValueOnce(firstSubmission.promise)
    .mockReturnValueOnce(secondSubmission.promise);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());

  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/plan-a' }); });
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));
  fireEvent.click(screen.getByRole('button', { name: '关闭确认窗口' }));
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/plan-b' }); });
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('分享 B');

  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));
  expect(state.confirmCloud).toHaveBeenCalledTimes(2);

  await act(async () => { firstSubmission.resolve({ outcomes: [], failures: [] }); });
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('分享 B');

  await act(async () => { secondSubmission.reject(new Error('B 下载失败')); });
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('分享 B');
});

it('does not show a rejected response from a closed share generation over a newer confirmation', async () => {
  const firstSubmission = deferred<{ outcomes: never[]; failures: never[] }>();
  state.parse
    .mockResolvedValueOnce({
      confirmation: {
        taskId: 'cloud-share://uc/plan-a', title: '分享 A', requiresConfirmation: true,
        resources: [{ id: 'a1', name: 'first.jpg', size: '1 B', selected: true }], processing: []
      }
    })
    .mockResolvedValueOnce({
      confirmation: {
        taskId: 'cloud-share://uc/plan-b', title: '分享 B', requiresConfirmation: true,
        resources: [{ id: 'b1', name: 'second.jpg', size: '1 B', selected: true }], processing: []
      }
    });
  state.confirmCloud.mockReturnValueOnce(firstSubmission.promise);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());

  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/plan-a' }); });
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));
  fireEvent.click(screen.getByRole('button', { name: '关闭确认窗口' }));
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/plan-b' }); });

  await act(async () => { firstSubmission.reject(new Error('A 已过期错误')); });

  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('分享 B');
  expect(screen.queryByText('A 已过期错误')).not.toBeInTheDocument();
});

it('does not show a rejected response after its share confirmation is closed', async () => {
  const submission = deferred<{ outcomes: never[]; failures: never[] }>();
  state.parse.mockResolvedValueOnce({
    confirmation: {
      taskId: 'cloud-share://uc/closed-plan', title: '已关闭分享', requiresConfirmation: true,
      resources: [{ id: 'f1', name: 'closed.jpg', size: '1 B', selected: true }], processing: []
    }
  });
  state.confirmCloud.mockReturnValueOnce(submission.promise);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());

  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/closed-plan' }); });
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));
  fireEvent.click(screen.getByRole('button', { name: '关闭确认窗口' }));
  await act(async () => { submission.reject(new Error('关闭后过期错误')); });

  expect(screen.queryByRole('dialog', { name: '确认下载内容' })).not.toBeInTheDocument();
  expect(screen.queryByText('关闭后过期错误')).not.toBeInTheDocument();
});

it('does not let an old response clear a reparsed confirmation for the same share plan', async () => {
  const firstSubmission = deferred<{ outcomes: never[]; failures: never[] }>();
  const secondSubmission = deferred<{ outcomes: never[]; failures: never[] }>();
  const confirmation = {
    taskId: 'cloud-share://uc/same-plan', title: '同一分享', requiresConfirmation: true,
    resources: [
      { id: 'f1', name: 'first.jpg', size: '1 B', selected: true },
      { id: 'f2', name: 'second.jpg', size: '1 B', selected: true },
    ],
    processing: []
  };
  state.parse
    .mockResolvedValueOnce({ confirmation })
    .mockResolvedValueOnce({ confirmation });
  state.confirmCloud
    .mockReturnValueOnce(firstSubmission.promise)
    .mockReturnValueOnce(secondSubmission.promise);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());

  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/same-plan' }); });
  fireEvent.click(screen.getByRole('checkbox', { name: '选择 second.jpg' }));
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));
  fireEvent.click(screen.getByRole('button', { name: '关闭确认窗口' }));

  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://drive.uc.cn/s/same-plan' }); });
  fireEvent.click(screen.getByRole('checkbox', { name: '选择 first.jpg' }));
  expect(screen.getByRole('checkbox', { name: '选择 second.jpg' })).toBeChecked();
  const secondSubmit = screen.getByRole('button', { name: /立即下载 1/ });
  fireEvent.click(secondSubmit);
  fireEvent.click(secondSubmit);
  expect(state.confirmCloud).toHaveBeenCalledTimes(2);
  expect(state.confirmCloud).toHaveBeenLastCalledWith('cloud-share://uc/same-plan', ['f2'], undefined);

  await act(async () => { firstSubmission.resolve({ outcomes: [], failures: [] }); });

  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('同一分享');
  expect(screen.getByRole('checkbox', { name: '选择 first.jpg' })).not.toBeChecked();
  expect(screen.getByRole('checkbox', { name: '选择 second.jpg' })).toBeChecked();
  await act(async () => { secondSubmission.resolve({ outcomes: [], failures: [] }); });
  expect(screen.queryByRole('dialog', { name: '确认下载内容' })).not.toBeInTheDocument();
});

it('ignores concurrent duplicate resolutions and advances to the next duplicate after success', async () => {
  const resolution = deferred<QueueTask>();
  state.parse.mockResolvedValueOnce({
    confirmation: {
      taskId: 'cloud-share://baidu/plan', title: '分享目录', requiresConfirmation: true,
      resources: [{ id: 'f1', name: 'first.bin', size: '1 B', selected: true }], processing: []
    }
  });
  state.confirmCloud.mockResolvedValueOnce({ outcomes: [
    { duplicate: { task: { id: 'duplicate-1', name: 'first-existing.bin', kind: 'cloud', status: 'paused', progress: 40 }, state: 'resume_available' } },
    { duplicate: { task: { id: 'duplicate-2', name: 'second-existing.bin', kind: 'cloud', status: 'downloading', progress: 20 }, state: 'existing_active' } }
  ], failures: [] });
  state.resolveDuplicate.mockReturnValueOnce(resolution.promise);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://pan.baidu.com/s/share' }); });
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));

  const resume = await screen.findByRole('button', { name: '继续下载' });
  fireEvent.click(resume);
  fireEvent.click(resume);
  expect(state.resolveDuplicate).toHaveBeenCalledTimes(1);

  await act(async () => { resolution.resolve({ id: 'duplicate-1', name: 'first-existing.bin', kind: 'cloud', status: 'downloading', progress: 40 }); });
  await waitFor(() => expect(screen.getByRole('dialog', { name: '检测到重复下载' })).toHaveTextContent('second-existing.bin'));
});

it('retrieves a compatible service model and persists the selected AI configuration', async () => {
  const user = userEvent.setup();
  state.listAiModels.mockResolvedValue(['gpt-4.1-mini', 'gpt-5-mini']);
  state.updateAiSettings.mockResolvedValue({ enabled: true, baseUrl: 'https://gateway.example.test/v1', model: 'gpt-5-mini', apiKeyConfigured: true });
  render(<App />);

  await user.click(screen.getByRole('button', { name: /设置/ }));
  await user.click(screen.getByRole('tab', { name: '高级' }));
  await user.click(screen.getByRole('checkbox', { name: '启用 AI 智能分析' }));
  await user.type(screen.getByRole('textbox', { name: 'AI 服务地址' }), 'https://gateway.example.test/v1');
  await user.type(screen.getByLabelText('AI API Key'), 'sk-local-secret');
  await user.click(screen.getByRole('button', { name: '获取可用模型' }));
  await screen.findByRole('option', { name: 'gpt-5-mini' });
  await user.selectOptions(screen.getByRole('combobox', { name: 'AI 模型' }), 'gpt-5-mini');
  await user.click(screen.getByRole('button', { name: '保存 AI 配置' }));

  expect(await screen.findByRole('status')).toHaveTextContent('AI 服务配置已保存。');
  expect(state.updateAiSettings).toHaveBeenCalledWith({
    enabled: true,
    baseUrl: 'https://gateway.example.test/v1',
    model: 'gpt-5-mini',
    apiKey: 'sk-local-secret'
  });
});

it('opens the normal new-download dialog from the floating-ball menu command', async () => {
  state.list.mockResolvedValueOnce([]);
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-command')).toBeDefined());

  act(() => { state.listeners.get('launcher-command')?.({ payload: 'new-download' }); });

  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
});

it('uses the existing native task action for floating-ball batch resume', async () => {
  const pausedTask: QueueTask = { ...oldTask, id: 'paused-task', status: 'paused' };
  state.list.mockResolvedValueOnce([pausedTask]);
  state.taskAction.mockResolvedValueOnce({ task: { ...pausedTask, status: 'downloading' } });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-command')).toBeDefined());
  await screen.findByText('archive.iso');

  act(() => { state.listeners.get('launcher-command')?.({ payload: 'resume-all' }); });

  await waitFor(() => expect(state.taskAction).toHaveBeenCalledWith('paused-task', 'resume'));
});

it('cancels one expanded torrent file without cancelling the parent task', async () => {
  const user = userEvent.setup();
  const task: QueueTask = {
    id: 'torrent-files', name: '合集', kind: 'torrent', status: 'downloading', progress: 25,
    files: [
      { index: 1, path: 'D://合集/episode.mkv', length: 100, completed_length: 25, selected: true },
      { index: 2, path: 'D://合集/readme.txt', length: 10, completed_length: 0, selected: true },
    ],
  };
  state.list.mockResolvedValueOnce([task]);
  state.cancelFile.mockResolvedValueOnce({
    ...task,
    files: [
      { ...task.files![0], selected: false },
      task.files![1],
    ],
  });
  render(<App />);

  await user.click(await screen.findByRole('button', { name: '展开 合集' }));
  await user.click(screen.getByRole('button', { name: '取消 episode.mkv 下载' }));

  await waitFor(() => expect(state.cancelFile).toHaveBeenCalledWith('torrent-files', 1));
  expect(state.taskAction).not.toHaveBeenCalledWith('torrent-files', 'cancel');
  expect(screen.getByRole('button', { name: '继续 episode.mkv' })).toBeVisible();
});

it('reconciles an existing task from a later focus snapshot without dropping a new event task', async () => {
  let terminalRequests = 0;
  state.list.mockImplementation((scope?: string) => {
    if (scope !== 'terminal') return Promise.resolve([]);
    terminalRequests += 1;
    return Promise.resolve(terminalRequests === 1 ? [] : [{ ...liveTask, status: 'completed', progress: 100 }]);
  });
  render(<App />);
  await waitFor(() => expect(state.onFocus).toBeDefined());
  act(() => {
    state.onTask?.(liveTask);
    state.onTask?.({ id: 'new-task', name: 'new.zip', kind: 'file', status: 'downloading', progress: 20 });
    state.onFocus?.({ payload: true });
  });

  await waitFor(() => expect(screen.getByText('已完成')).toBeVisible());
  expect(within(screen.getByRole('region', { name: '下载队列' })).getByText('new.zip')).toBeVisible();
});

it('keeps a same-ID event update that arrives while a focus snapshot is pending', async () => {
  const focusSnapshot = deferred<QueueTask[]>();
  let activeRequests = 0;
  state.list.mockImplementation((scope?: string) => {
    if (scope !== 'active') return Promise.resolve([]);
    activeRequests += 1;
    return activeRequests === 1 ? Promise.resolve([]) : focusSnapshot.promise;
  });
  render(<App />);
  await waitFor(() => expect(state.onFocus).toBeDefined());
  act(() => { state.onFocus?.({ payload: true }); });
  await waitFor(() => expect(activeRequests).toBe(2));
  act(() => { state.onTask?.(liveTask); });
  await act(async () => { focusSnapshot.resolve([oldTask]); });

  expect(screen.getByRole('progressbar', { name: 'archive.iso 下载进度' })).toHaveAttribute('aria-valuenow', '40');
  expect(within(screen.getByRole('region', { name: '下载队列' })).getAllByText('archive.iso')).toHaveLength(1);
});

it('reopens awaiting magnet file confirmation and confirms only through the dialog', async () => {
  const user = userEvent.setup();
  const awaitingTask: QueueTask = {
    id: 'magnet-awaiting', name: 'collection', kind: 'magnet', status: 'awaiting_user', progress: 0,
    files: [{ index: 1, path: 'episode.mkv', length: 1024, completed_length: 0, selected: true }]
  };
  state.list.mockResolvedValueOnce([awaitingTask]);
  state.confirm.mockResolvedValueOnce({ ...awaitingTask, status: 'queued' });
  render(<App />);
  await screen.findByRole('button', { name: '确认下载 collection' });
  await user.click(screen.getByRole('button', { name: '确认下载 collection' }));

  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toBeVisible();
  expect(screen.getByText('episode.mkv')).toBeVisible();
  expect(state.selectFiles).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: /立即下载 1/ }));

  expect(state.confirm).toHaveBeenCalledWith('magnet-awaiting', ['1']);
  expect(state.selectFiles).not.toHaveBeenCalled();
});

it('fills a magnet confirmation dialog when its task files arrive later', async () => {
  state.list.mockResolvedValueOnce([]);
  state.parse.mockResolvedValueOnce({
    confirmation: { taskId: 'magnet-1', requiresConfirmation: true, resources: [], processing: [], isBitTorrent: true }
  });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  await waitFor(() => expect(state.onTask).toBeDefined());
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'magnet:?xt=urn:btih:abc' }); });
  expect(screen.getByText('正在获取文件信息…')).toBeVisible();

  act(() => { state.onTask?.({
    id: 'magnet-1', name: 'collection', kind: 'magnet', status: 'awaiting_user', progress: 0,
    files: [{ index: 1, path: 'episode.mkv', length: 1024, completed_length: 0, selected: true }]
  }); });

  expect(screen.getByText('episode.mkv')).toBeVisible();
  expect(screen.queryByText('正在获取文件信息…')).not.toBeInTheDocument();
});

it('polls a pending magnet confirmation when a metadata event is missed', async () => {
  state.list
    .mockResolvedValueOnce([])
    .mockResolvedValue([{
      id: 'magnet-poll', name: 'collection', kind: 'magnet', status: 'awaiting_user', progress: 0,
      files: [{ index: 1, path: 'episode.mkv', length: 1024, completed_length: 0, selected: true }]
    }]);
  state.parse.mockResolvedValueOnce({
    confirmation: { taskId: 'magnet-poll', requiresConfirmation: true, resources: [], processing: [], isBitTorrent: true }
  });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'magnet:?xt=urn:btih:poll' }); });

  await waitFor(() => expect(screen.getByText('episode.mkv')).toBeVisible(), { timeout: 1200 });
  expect(screen.queryByText('正在获取文件信息…')).not.toBeInTheDocument();
});

it('preserves a user deselection when later magnet metadata refreshes the file', async () => {
  const user = userEvent.setup();
  state.list.mockResolvedValueOnce([]);
  state.parse.mockResolvedValueOnce({
    confirmation: { taskId: 'magnet-2', requiresConfirmation: true, processing: [], isBitTorrent: true,
      resources: [{ id: '1', index: 1, name: 'pending.mkv', size: '未知', selected: true }] }
  });
  render(<App />);
  await waitFor(() => expect(state.onTask).toBeDefined());
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'magnet:?xt=urn:btih:def' }); });
  await user.click(screen.getByRole('checkbox', { name: '选择 pending.mkv' }));

  act(() => { state.onTask?.({
    id: 'magnet-2', name: 'collection', kind: 'magnet', status: 'awaiting_user', progress: 0,
    files: [{ index: 1, path: 'renamed.mkv', length: 2048, completed_length: 1024, selected: true }]
  }); });

  expect(screen.getByText('renamed.mkv')).toBeVisible();
  expect(screen.getByRole('checkbox', { name: '选择 renamed.mkv' })).not.toBeChecked();
});

it('cleans up launcher listeners that resolve after App unmounts', async () => {
  const navigate = deferred<() => void>();
  const parse = deferred<() => void>();
  const stopNavigate = vi.fn();
  const stopParse = vi.fn();
  state.list.mockResolvedValueOnce([]);
  state.listen.mockImplementation((name) => {
    if (name === 'launcher-navigate') return navigate.promise;
    if (name === 'launcher-parse') return parse.promise;
    return Promise.resolve(() => undefined);
  });
  const { unmount } = render(<App />);

  unmount();
  await act(async () => {
    navigate.resolve(stopNavigate);
    parse.resolve(stopParse);
  });

  expect(stopNavigate).toHaveBeenCalledOnce();
  expect(stopParse).toHaveBeenCalledOnce();
});
