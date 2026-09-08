import { fireEvent, render, screen } from '@testing-library/react';
import { vi } from 'vitest';
import userEvent from '@testing-library/user-event';
import { DownloadQueue } from './DownloadQueue';

const folderTask = {
  id: 'folder-1', name: 'Season 1', kind: 'folder', status: 'downloading', progress: 42,
  children: [{ id: 'file-1', name: 'episode-01.mkv', kind: 'file', status: 'downloading', progress: 50 }]
} as const;

it('keeps a folder as one row until the user expands it', async () => {
  const user = userEvent.setup();
  render(<DownloadQueue tasks={[folderTask]} selectedTaskId={null} onSelect={() => {}} onCommand={() => {}} />);
  expect(screen.queryByText('episode-01.mkv')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
  expect(screen.getByText('episode-01.mkv')).toBeVisible();
});

it('shows a readable magnet name and keeps the raw link out of the task row', () => {
  const source = 'magnet:?xt=urn:btih:46A00FD699618BC0319C45F7075C067D564A0819&dn=Lanterns.S01E04.1080p.HMAX.WEBRip.AAC5.1.10bits.x265-Rapta&tr=udp%3A%2F%2Ftracker.example';
  render(<DownloadQueue
    tasks={[{ id: 'magnet-1', name: source, source, kind: 'magnet', status: 'downloading', progress: 0, speed: '0 B/s', connections: 0 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByText('Lanterns.S01E04.1080p.HMAX.WEBRip.AAC5.1.10bits.x265-Rapta')).toBeVisible();
  expect(screen.queryByText(source)).not.toBeInTheDocument();
  expect(screen.getByText('磁力任务 · 正在获取文件信息')).toBeVisible();
});

it('shows the live magnet metadata stage instead of a generic zero-speed subtitle', () => {
  render(<DownloadQueue
    tasks={[{ id: 'magnet-stage', name: '磁力下载', source: 'magnet:?xt=urn:btih:0123456789ABCDEF', kind: 'magnet', status: 'downloading', progress: 0, speed: '0 B/s', connections: 0, statusMessage: '正在获取种子信息，暂无节点连接' }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByText('正在获取种子信息，暂无节点连接')).toBeVisible();
});

it('offers real state-aware commands when an active task is right-clicked', async () => {
  const user = userEvent.setup();
  const onCommand = vi.fn();
  render(<DownloadQueue
    tasks={[{ id: 'active-1', name: 'game.zip', source: 'https://example.com/game.zip', kind: 'file', status: 'downloading', progress: 25 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={onCommand}
  />);

  fireEvent.contextMenu(screen.getByText('game.zip'), { clientX: 280, clientY: 180 });
  expect(screen.getByRole('menu', { name: 'game.zip 任务菜单' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '暂停下载' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '取消任务' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '打开下载目录' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '重新下载' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('menuitem', { name: '暂停下载' }));
  expect(onCommand).toHaveBeenCalledWith('active-1', 'pause');
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
});

it('offers terminal-task cleanup without showing invalid transfer commands', () => {
  render(<DownloadQueue
    tasks={[{ id: 'done-1', name: 'finished.iso', source: 'https://example.com/finished.iso', kind: 'file', status: 'completed', progress: 100 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  fireEvent.contextMenu(screen.getByText('finished.iso'));
  expect(screen.getByRole('menuitem', { name: '打开下载目录' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '复制下载链接' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '移除任务记录…' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '暂停下载' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '取消任务' })).not.toBeInTheDocument();
});

it('dismisses the task menu with Escape', () => {
  render(<DownloadQueue
    tasks={[folderTask]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  fireEvent.contextMenu(screen.getByText('Season 1'));
  expect(screen.getByRole('menu')).toBeVisible();
  fireEvent.keyDown(document, { key: 'Escape' });
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
});

it('selects a task on ordinary click without emitting a task-details command', async () => {
  const user = userEvent.setup();
  const onSelect = vi.fn();
  const onCommand = vi.fn();
  render(<DownloadQueue
    tasks={[{ id: 'task-1', name: 'archive.zip', kind: 'file', status: 'queued', progress: 0 }]}
    selectedTaskId={null}
    onSelect={onSelect}
    onCommand={onCommand}
  />);

  await user.click(screen.getByText('archive.zip'));
  expect(onSelect).toHaveBeenCalledWith('task-1');
  expect(onCommand).not.toHaveBeenCalled();
});

it('emits details only after its right-click menu item is selected', async () => {
  const user = userEvent.setup();
  const onSelect = vi.fn();
  const onCommand = vi.fn();
  render(<DownloadQueue
    tasks={[{ id: 'task-1', name: 'archive.zip', kind: 'file', status: 'queued', progress: 0 }]}
    selectedTaskId={null}
    onSelect={onSelect}
    onCommand={onCommand}
  />);

  fireEvent.contextMenu(screen.getByText('archive.zip'));
  expect(onSelect).toHaveBeenCalledWith('task-1');
  expect(onCommand).not.toHaveBeenCalled();
  await user.click(screen.getByRole('menuitem', { name: '查看任务详情' }));
  expect(onCommand).toHaveBeenCalledTimes(1);
  expect(onCommand).toHaveBeenCalledWith('task-1', 'details');
});
