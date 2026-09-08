import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import type { QueueTask } from './lib/types';

const state = vi.hoisted(() => ({
  list: vi.fn(), parse: vi.fn(), onTask: undefined as undefined | ((task: QueueTask) => void),
  onFocus: undefined as undefined | ((event: { payload: boolean }) => void),
  listeners: new Map<string, (event: { payload: unknown }) => void>(), listen: vi.fn()
}));

vi.mock('./lib/api', () => ({
  isTauriRuntime: () => true, listPersistedTasks: state.list, parseDownload: state.parse,
  confirmDownload: vi.fn(), runTaskAction: vi.fn(), selectDownloadFiles: vi.fn()
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
  state.parse.mockReset();
  state.onTask = undefined;
  state.onFocus = undefined;
  state.listeners.clear();
  state.listen.mockReset();
  state.listen.mockImplementation((name, handler) => {
    state.listeners.set(name, handler);
    return Promise.resolve(() => undefined);
  });
});

it('keeps a live snapshot when the initial list returns an older copy', async () => {
  const initial = deferred<QueueTask[]>();
  state.list.mockReturnValueOnce(initial.promise);
  render(<App />);
  await waitFor(() => expect(state.onTask).toBeDefined());
  act(() => { state.onTask?.(liveTask); });
  await act(async () => { initial.resolve([oldTask]); });

  expect(screen.getByText(/40%/)).toBeVisible();
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
  await user.click(screen.getByRole('checkbox'));

  act(() => { state.onTask?.({
    id: 'magnet-2', name: 'collection', kind: 'magnet', status: 'awaiting_user', progress: 0,
    files: [{ index: 1, path: 'renamed.mkv', length: 2048, completed_length: 1024, selected: true }]
  }); });

  expect(screen.getByText('renamed.mkv')).toBeVisible();
  expect(screen.getByRole('checkbox')).not.toBeChecked();
});

it('cleans up launcher listeners that resolve after App unmounts', async () => {
  const navigate = deferred<() => void>();
  const parse = deferred<() => void>();
  const stopNavigate = vi.fn();
  const stopParse = vi.fn();
  state.list.mockResolvedValueOnce([]);
  state.listen.mockImplementation((name) => name === 'launcher-navigate' ? navigate.promise : parse.promise);
  const { unmount } = render(<App />);

  unmount();
  await act(async () => {
    navigate.resolve(stopNavigate);
    parse.resolve(stopParse);
  });

  expect(stopNavigate).toHaveBeenCalledOnce();
  expect(stopParse).toHaveBeenCalledOnce();
});
