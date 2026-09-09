import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import type { QueueTask } from './lib/types';

const state = vi.hoisted(() => ({
  list: vi.fn(), parse: vi.fn(), onTask: undefined as undefined | ((task: QueueTask) => void),
  onFocus: undefined as undefined | ((event: { payload: boolean }) => void),
  listeners: new Map<string, (event: { payload: unknown }) => void>(), listen: vi.fn(),
  confirm: vi.fn(), selectFiles: vi.fn(), getEngineState: vi.fn(), retryEngine: vi.fn(), taskAction: vi.fn(),
  updateAiSettings: vi.fn(), listAiModels: vi.fn()
}));

vi.mock('./lib/api', () => ({
  isTauriRuntime: () => true, listPersistedTasks: state.list, parseDownload: state.parse,
  confirmDownload: state.confirm, runTaskAction: state.taskAction, selectDownloadFiles: state.selectFiles,
  getDownloadEngineState: state.getEngineState, retryDownloadEngine: state.retryEngine,
  defaultSettings: { debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always', proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }, magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' } },
  getSettings: vi.fn().mockResolvedValue({ debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always', proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }, magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' } }),
  updateSettings: vi.fn(), updateAiSettings: state.updateAiSettings, listAiModels: state.listAiModels, openTaskDirectory: vi.fn(), removeTaskRecord: vi.fn(), resolveDuplicateDownload: vi.fn()
}));
vi.mock('./lib/downloadEvents', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./lib/downloadEvents')>();
  return { ...actual, subscribeToDownloadEvents: vi.fn().mockImplementation(({ onTask }) => {
    state.onTask = onTask;
    return Promise.resolve(() => undefined);
  }) };
});
vi.mock('@tauri-apps/api/event', () => ({ listen: state.listen }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({
  onFocusChanged: vi.fn().mockImplementation((handler) => { state.onFocus = handler; return Promise.resolve(() => undefined); }),
  onResized: vi.fn().mockResolvedValue(() => undefined)
}) }));
vi.mock('./lib/mainWindow', () => ({ mainWindowIsMaximized: vi.fn().mockResolvedValue(false), runMainWindowAction: vi.fn() }));

import { App } from './App';

const oldTask: QueueTask = { id: 'task-1', name: 'archive.iso', kind: 'file', status: 'downloading', progress: 10, total_length: 100, completed_length: 10 };
const liveTask: QueueTask = { ...oldTask, progress: 40, completed_length: 40 };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

beforeEach(() => {
  state.list.mockReset();
  state.list.mockResolvedValue([]);
  state.parse.mockReset();
  state.confirm.mockReset();
  state.selectFiles.mockReset();
  state.getEngineState.mockReset();
  state.getEngineState.mockResolvedValue({ available: true, retrying: false });
  state.retryEngine.mockReset();
  state.taskAction.mockReset();
  state.updateAiSettings.mockReset();
  state.listAiModels.mockReset();
  state.onTask = undefined;
  state.onFocus = undefined;
  state.listeners.clear();
  state.listen.mockReset();
  state.listen.mockImplementation((name, handler) => {
    state.listeners.set(name, handler);
    return Promise.resolve(() => undefined);
  });
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

it('upserts a parsed task that already arrived through a live event', async () => {
  state.list.mockResolvedValueOnce([]);
  state.parse.mockResolvedValueOnce({ task: liveTask });
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  act(() => { state.onTask?.(liveTask); });
  await act(async () => { state.listeners.get('launcher-parse')?.({ payload: 'https://example.test/archive.iso' }); });

  expect(within(screen.getByRole('region', { name: '下载队列' })).getAllByText('archive.iso')).toHaveLength(1);
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

it('reconciles an existing task from a later focus snapshot without dropping a new event task', async () => {
  state.list
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce([{ ...liveTask, status: 'completed', progress: 100 }]);
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
  state.list.mockResolvedValueOnce([]).mockReturnValueOnce(focusSnapshot.promise);
  render(<App />);
  await waitFor(() => expect(state.onFocus).toBeDefined());
  act(() => { state.onFocus?.({ payload: true }); });
  await waitFor(() => expect(state.list).toHaveBeenCalledTimes(2));
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
