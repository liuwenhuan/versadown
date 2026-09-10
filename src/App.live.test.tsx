import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import type { QueueTask } from './lib/types';

const state = vi.hoisted(() => ({
  list: vi.fn(), parse: vi.fn(), onTask: undefined as undefined | ((task: QueueTask) => void),
  onFocus: undefined as undefined | ((event: { payload: boolean }) => void),
  listeners: new Map<string, (event: { payload: unknown }) => void>(), listen: vi.fn(),
  confirm: vi.fn(), confirmCloud: vi.fn(), resolveDuplicate: vi.fn(), openDirectory: vi.fn(), selectFiles: vi.fn(), getEngineState: vi.fn(), retryEngine: vi.fn(), taskAction: vi.fn(),
  updateAiSettings: vi.fn(), listAiModels: vi.fn()
}));

vi.mock('./lib/api', () => ({
  isTauriRuntime: () => true, listPersistedTasks: state.list, parseDownload: state.parse,
  confirmDownload: state.confirm, confirmCloudShareDownload: state.confirmCloud, runTaskAction: state.taskAction, selectDownloadFiles: state.selectFiles,
  getDownloadEngineState: state.getEngineState, retryDownloadEngine: state.retryEngine,
  defaultSettings: { debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always', proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }, magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' } },
  getSettings: vi.fn().mockResolvedValue({ debugReports: false, downloadDirectory: 'C:\\Downloads', launcherVisibility: 'always', proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' }, magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' } }),
  updateSettings: vi.fn(), updateAiSettings: state.updateAiSettings, listAiModels: state.listAiModels, openTaskDirectory: state.openDirectory, removeTaskRecord: vi.fn(), resolveDuplicateDownload: state.resolveDuplicate
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
  state.confirmCloud.mockResolvedValueOnce([
    { task: { id: 'queued-1', name: 'first.bin', kind: 'cloud', status: 'downloading', progress: 0 } },
    { duplicate: { task: { id: 'duplicate-1', name: 'first-existing.bin', kind: 'cloud', status: 'completed', progress: 100 }, state: 'completed_files_present' } },
    { task: { id: 'queued-2', name: 'second.bin', kind: 'cloud', status: 'downloading', progress: 0 } },
    { duplicate: { task: { id: 'duplicate-2', name: 'second-existing.bin', kind: 'cloud', status: 'downloading', progress: 20 }, state: 'existing_active' } }
  ]);
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
  const firstSubmission = deferred<never[]>();
  const secondSubmission = deferred<never[]>();
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

  await act(async () => { firstSubmission.resolve([]); });
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('分享 B');

  await act(async () => { secondSubmission.reject(new Error('B 下载失败')); });
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('分享 B');
});

it('does not show a rejected response from a closed share generation over a newer confirmation', async () => {
  const firstSubmission = deferred<never[]>();
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
  const submission = deferred<never[]>();
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
  const firstSubmission = deferred<never[]>();
  const secondSubmission = deferred<never[]>();
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

  await act(async () => { firstSubmission.resolve([]); });

  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toHaveTextContent('同一分享');
  expect(screen.getByRole('checkbox', { name: '选择 first.jpg' })).not.toBeChecked();
  expect(screen.getByRole('checkbox', { name: '选择 second.jpg' })).toBeChecked();
  await act(async () => { secondSubmission.resolve([]); });
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
  state.confirmCloud.mockResolvedValueOnce([
    { duplicate: { task: { id: 'duplicate-1', name: 'first-existing.bin', kind: 'cloud', status: 'paused', progress: 40 }, state: 'resume_available' } },
    { duplicate: { task: { id: 'duplicate-2', name: 'second-existing.bin', kind: 'cloud', status: 'downloading', progress: 20 }, state: 'existing_active' } }
  ]);
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
