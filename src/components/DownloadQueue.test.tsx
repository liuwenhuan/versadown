import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { DownloadQueue } from './DownloadQueue';

const folderTask = {
  id: 'folder-1', name: 'Season 1', kind: 'folder', status: 'downloading', progress: 42,
  children: [{ id: 'file-1', name: 'episode-01.mkv', kind: 'file', status: 'downloading', progress: 50 }]
} as const;

it('keeps a folder as one row until the user expands it', async () => {
  const user = userEvent.setup();
  render(<DownloadQueue tasks={[folderTask]} selectedTaskId={null} onSelect={() => {}} onAction={() => {}} onConfirm={() => {}} />);
  expect(screen.queryByText('episode-01.mkv')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
  expect(screen.getByText('episode-01.mkv')).toBeVisible();
});

it('uses select and skip actions for files sharing a BitTorrent transfer', async () => {
  const user = userEvent.setup();
  const onAction = vi.fn();
  render(<DownloadQueue tasks={[{
    ...folderTask,
    kind: 'torrent',
    children: [{ ...folderTask.children[0], selected: false }]
  }]} selectedTaskId={null} onSelect={() => {}} onAction={onAction} onConfirm={() => {}} />);

  await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
  await user.click(screen.getByRole('button', { name: '下载此文件 episode-01.mkv' }));

  expect(onAction).toHaveBeenCalledWith('file-1', 'select');
  expect(screen.queryByRole('button', { name: '暂停 episode-01.mkv' })).not.toBeInTheDocument();
});

it('requires confirmation before an awaiting BitTorrent task can change file selection', async () => {
  const user = userEvent.setup();
  const onAction = vi.fn();
  const onConfirm = vi.fn();
  const task = {
    ...folderTask,
    kind: 'magnet' as const,
    status: 'awaiting_user' as const,
    files: [{ index: 1, path: 'episode-01.mkv', length: 1024, completed_length: 0, selected: true }]
  };
  render(<DownloadQueue tasks={[task]} selectedTaskId={null} onSelect={() => {}} onAction={onAction} onConfirm={onConfirm} />);

  await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
  expect(screen.queryByRole('button', { name: '跳过此文件 episode-01.mkv' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '下载此文件 episode-01.mkv' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '暂停 episode-01.mkv' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '确认下载 Season 1' }));

  expect(onAction).not.toHaveBeenCalled();
  expect(onConfirm).toHaveBeenCalledWith(task);
});

it('can pause a queued parent task once aria2 has assigned its GID', async () => {
  const user = userEvent.setup();
  const onAction = vi.fn();
  const task = {
    id: 'queued-with-gid',
    name: 'waiting.iso',
    kind: 'file' as const,
    status: 'queued' as const,
    progress: 0,
    aria_gid: '0123456789abcdef'
  };
  render(<DownloadQueue tasks={[task]} selectedTaskId={null} onSelect={() => {}} onAction={onAction} onConfirm={() => {}} />);

  await user.click(screen.getByRole('button', { name: '暂停 waiting.iso' }));

  expect(onAction).toHaveBeenCalledWith('queued-with-gid', 'pause');
});
