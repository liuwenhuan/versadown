import { fireEvent, render, screen } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { vi } from 'vitest';
import userEvent from '@testing-library/user-event';
import { DownloadQueue, taskContextMenuPosition } from './DownloadQueue';

const folderTask = {
  id: 'folder-1', name: 'Season 1', kind: 'folder', status: 'downloading', progress: 42,
  children: [{ id: 'file-1', name: 'episode-01.mkv', kind: 'file', status: 'downloading', progress: 50, size: '8 MB / 161.2 MB' }]
} as const;

it('places multi-file controls and child progress in stable aligned columns', async () => {
  const user = userEvent.setup();
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const { container } = render(<DownloadQueue tasks={[folderTask]} selectedTaskId={null} onSelect={() => {}} onCommand={() => {}} />);
    await user.click(screen.getByRole('button', { name: '展开 Season 1' }));

    const expandSlot = container.querySelector('.expand-slot');
    const childRow = container.querySelector('.child-row');
    const childMarker = container.querySelector('.child-name i');
    const childProgress = container.querySelector('.child-progress');
    const childProgressValue = container.querySelector('.child-progress-value');
    const childTransferSize = container.querySelector('.child-transfer-size');
    const childAction = container.querySelector('.child-actions button');

    expect(getComputedStyle(expandSlot!).gridArea).toBe('auto');
    expect(getComputedStyle(expandSlot!).left).toBe('0px');
    expect(getComputedStyle(expandSlot!).width).toBe('22px');
    expect(getComputedStyle(childRow!).paddingLeft).toBe('21px');
    expect(getComputedStyle(childRow!).paddingTop).toBe('6px');
    expect(getComputedStyle(childRow!).paddingBottom).toBe('6px');
    expect(getComputedStyle(childMarker!).width).toBe('48px');
    expect(getComputedStyle(childRow!).gridTemplateColumns).toBe('minmax(0,1fr) minmax(140px,220px) 44px 120px 56px');
    expect(getComputedStyle(childRow!).paddingRight).toBe('24px');
    expect(getComputedStyle(childProgress!).justifySelf).toBe('end');
    expect(childProgress!.querySelector('small')).toBeNull();
    expect(childProgressValue).toHaveTextContent('50%');
    expect(childTransferSize).toHaveTextContent('8 MB / 161.2 MB');
    expect(getComputedStyle(childProgressValue!).textAlign).toBe('right');
    expect(getComputedStyle(childTransferSize!).textAlign).toBe('right');
    expect(getComputedStyle(childAction!).width).toBe('26px');
    expect(getComputedStyle(childAction!).height).toBe('26px');
  } finally {
    stylesheet.remove();
  }
});

it('positions a bottom task menu fully inside the viewport', () => {
  expect(taskContextMenuPosition({
    x: 980,
    y: 386,
    menuWidth: 224,
    menuHeight: 344,
    viewportWidth: 1560,
    viewportHeight: 460,
  })).toEqual({ left: 980, top: 108 });
});

it('keeps a folder as one row until the user expands it', async () => {
  const user = userEvent.setup();
  const onCommand = vi.fn();
  render(<DownloadQueue tasks={[folderTask]} selectedTaskId={null} onSelect={() => {}} onCommand={onCommand} />);
  expect(screen.queryByText('episode-01.mkv')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
  expect(screen.getByText('episode-01.mkv')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '暂停 episode-01.mkv' }));
  expect(onCommand).toHaveBeenCalledWith('file-1', 'pause');
});

it('does not expose expansion for a single-file torrent task', () => {
  render(<DownloadQueue
    tasks={[{
      id: 'single-file-torrent', name: 'movie.mkv', kind: 'torrent', status: 'downloading', progress: 25,
      files: [{ index: 1, path: 'D://Downloads/movie.mkv', length: 100, completed_length: 25, selected: true }],
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.queryByRole('button', { name: '展开 movie.mkv' })).not.toBeInTheDocument();
  expect(screen.queryByText('Downloads/movie.mkv')).not.toBeInTheDocument();
});

it('uses file operations instead of transfer status for expanded multi-file tasks and removes local path noise', async () => {
  const user = userEvent.setup();
  const onFileSelection = vi.fn();
  const onFileCancel = vi.fn();
  render(<DownloadQueue
    tasks={[{
      id: 'torrent-files', name: '演示合集', kind: 'torrent', status: 'downloading', progress: 25,
      files: [
        { index: 1, path: 'D://演示合集/episode-01.mkv', length: 1024 * 1024 * 1024, completed_length: 1536 * 1024, selected: true },
        { index: 2, path: 'D://演示合集/readme.txt', length: 10, completed_length: 0, selected: false },
      ],
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
    onFileSelection={onFileSelection}
    onFileCancel={onFileCancel}
  />);

  await user.click(screen.getByRole('button', { name: '展开 演示合集' }));

  expect(screen.getByText('episode-01.mkv')).toBeVisible();
  expect(screen.getByText('readme.txt')).toBeVisible();
  expect(screen.queryByText('D://演示合集/episode-01.mkv')).not.toBeInTheDocument();
  expect(screen.queryByText('下载中')).not.toBeInTheDocument();
  expect(screen.getByTitle('1.5 MB / 1 GB')).toBeVisible();
  expect(screen.queryByText('1572864 B / 1073741824 B')).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: '暂停 episode-01.mkv' }));
  expect(onFileSelection).toHaveBeenCalledWith('torrent-files', 1, false);
  await user.click(screen.getByRole('button', { name: '取消 episode-01.mkv 下载' }));
  expect(onFileCancel).toHaveBeenCalledWith('torrent-files', 1);
  expect(screen.getByRole('button', { name: '继续 readme.txt' })).toBeVisible();
});

it('shows a continue operation for selected files when their parent task is paused', async () => {
  const user = userEvent.setup();
  render(<DownloadQueue
    tasks={[{
      id: 'paused-torrent', name: '暂停合集', kind: 'torrent', status: 'paused', progress: 25,
      files: [
        { index: 1, path: 'D://暂停合集/episode.mkv', length: 100, completed_length: 25, selected: true },
        { index: 2, path: 'D://暂停合集/readme.txt', length: 10, completed_length: 0, selected: true },
      ],
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
    onFileSelection={() => {}}
  />);

  await user.click(screen.getByRole('button', { name: '展开 暂停合集' }));
  expect(screen.getByRole('button', { name: '继续 episode.mkv' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '暂停 episode.mkv' })).not.toBeInTheDocument();
});

it('keeps magnet metadata on the compact transfer-metrics row', () => {
  const source = 'magnet:?xt=urn:btih:46A00FD699618BC0319C45F7075C067D564A0819&dn=Lanterns.S01E04.1080p.HMAX.WEBRip.AAC5.1.10bits.x265-Rapta&tr=udp%3A%2F%2Ftracker.example';
  render(<DownloadQueue
    tasks={[{ id: 'magnet-1', name: source, source, kind: 'magnet', status: 'downloading', progress: 0, speed: '0 B/s', connections: 0 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByText('Lanterns.S01E04.1080p.HMAX.WEBRip.AAC5.1.10bits.x265-Rapta')).toBeVisible();
  expect(screen.queryByText(source)).not.toBeInTheDocument();
  const metadata = screen.getByText('磁力任务 · 正在获取文件信息');
  expect(metadata).toBeVisible();
  expect(metadata.closest('.transfer-metrics')).not.toBeNull();
  expect(metadata.closest('.task-copy')).toBeNull();
});

it('keeps the live magnet metadata stage in the transfer-metrics row', () => {
  render(<DownloadQueue
    tasks={[{ id: 'magnet-stage', name: '磁力下载', source: 'magnet:?xt=urn:btih:0123456789ABCDEF', kind: 'magnet', status: 'downloading', progress: 0, speed: '0 B/s', connections: 0, statusMessage: '正在获取种子信息，暂无节点连接' }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  const metadata = screen.getByText('正在获取种子信息，暂无节点连接');
  expect(metadata).toBeVisible();
  expect(metadata.closest('.transfer-metrics')).not.toBeNull();
});

it('shows transferred bytes, live speed and ETA without exposing peer-count noise', () => {
  render(<DownloadQueue
    tasks={[{
      id: 'transfer-1', name: 'movie.mkv', kind: 'file', status: 'downloading', progress: 25,
      completed_length: 1024 * 1024, total_length: 4 * 1024 * 1024,
      download_speed: 512 * 1024, eta_seconds: 61, connections: 99
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByText('1 MB / 4 MB')).toBeVisible();
  expect(screen.getByText('512 KB/s')).toBeVisible();
  expect(screen.getByText('剩余 1 分 1 秒')).toBeVisible();
  expect(screen.queryByText('99 个连接')).not.toBeInTheDocument();
});

it('replaces paused transfer estimates with a single paused label and no status pill', () => {
  render(<DownloadQueue
    tasks={[{
      id: 'paused-transfer', name: 'episode.mkv', kind: 'file', status: 'paused', progress: 42,
      completed_length: 42 * 1024 * 1024, total_length: 100 * 1024 * 1024,
      download_speed: 329_700, eta_seconds: 921,
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByText('已暂停')).toBeVisible();
  expect(screen.queryByText('剩余 15 分 21 秒')).not.toBeInTheDocument();
  expect(screen.queryByText('322.0 KB/s')).not.toBeInTheDocument();
  expect(screen.queryByText('失败')).not.toBeInTheDocument();
});

it('does not render a failure status pill in task rows', () => {
  render(<DownloadQueue
    tasks={[{ id: 'failed-transfer', name: 'archive.zip', kind: 'file', status: 'failed', progress: 30, error: '服务器拒绝连接' }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByText('服务器拒绝连接')).toBeVisible();
  expect(screen.queryByText('失败')).not.toBeInTheDocument();
});

it('renders a completed video task compactly and plays its video files', async () => {
  const user = userEvent.setup();
  const onCommand = vi.fn();
  render(<DownloadQueue
    tasks={[{
      id: 'completed-video', name: '纪录片合集', kind: 'torrent', status: 'completed', progress: 100,
      updated_at: 1_788_998_400,
      files: [
        { index: 1, path: 'D:/Downloads/documentary.mp4', length: 1_073_741_824, completed_length: 1_073_741_824, selected: true },
        { index: 2, path: 'D:/Downloads/notes.txt', length: 524_288, completed_length: 524_288, selected: true },
      ],
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={onCommand}
  />);

  expect(screen.getByText('完成于 2026-09-10 08:00')).toBeVisible();
  expect(screen.getByText('1.0 GB')).toBeVisible();
  expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
  expect(screen.queryByText('已完成')).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: '播放 纪录片合集' }));
  expect(onCommand).toHaveBeenCalledWith('completed-video', 'play_videos');
});

it('opens the download directory for a completed task without video files', async () => {
  const user = userEvent.setup();
  const onCommand = vi.fn();
  render(<DownloadQueue
    tasks={[{
      id: 'completed-archive', name: 'archive.zip', kind: 'file', status: 'completed', progress: 100,
      updated_at: 1_788_998_400,
      files: [{ index: 1, path: 'D:/Downloads/archive.zip', length: 1_572_864, completed_length: 1_572_864, selected: true }],
    }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={onCommand}
  />);

  expect(screen.getByText('1.5 MB')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '打开 archive.zip 所在目录' }));
  expect(onCommand).toHaveBeenCalledWith('completed-archive', 'open_directory');
});

it('offers cleanup retry only from the typed completed-task state', async () => {
  const user = userEvent.setup();
  const onCommand = vi.fn();
  render(<DownloadQueue
    tasks={[
      {
        id: 'cleanup-pending', name: 'pending.bin', kind: 'cloud', status: 'completed', progress: 100,
        source: 'cloud-share://baidu/share/entry', cleanup_pending: true, error: 'opaque provider failure',
      },
      {
        id: 'cleanup-done', name: 'done.bin', kind: 'cloud', status: 'completed', progress: 100,
        source: 'cloud-share://baidu/share/other', cleanup_pending: false,
        error: 'cleanup_pending：这段旧文案不能决定界面能力',
      },
    ]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={onCommand}
  />);

  await user.click(screen.getByRole('button', { name: '重试网盘清理 pending.bin' }));
  expect(onCommand).toHaveBeenLastCalledWith('cleanup-pending', 'retry_cleanup');
  expect(screen.queryByRole('button', { name: '重试网盘清理 done.bin' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: '更多操作 pending.bin' }));
  await user.click(screen.getByRole('menuitem', { name: '重试网盘清理' }));
  expect(onCommand).toHaveBeenLastCalledWith('cleanup-pending', 'retry_cleanup');
});

it('keeps destructive cancellation in the menu while the row exposes pause and more actions', async () => {
  const user = userEvent.setup();
  render(<DownloadQueue
    tasks={[{ id: 'active-actions', name: 'game.zip', kind: 'file', status: 'downloading', progress: 25 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  expect(screen.getByRole('button', { name: '暂停 game.zip' })).toBeVisible();
  expect(screen.getByRole('button', { name: '更多操作 game.zip' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '取消 game.zip' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '更多操作 game.zip' }));
  expect(screen.getByRole('menu', { name: 'game.zip 任务菜单' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '取消任务' })).toBeVisible();
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

it('keeps modifier-selected tasks together and exposes only batch-safe menu actions', async () => {
  const onSelect = vi.fn();
  const onBatchCommand = vi.fn();
  render(<DownloadQueue
    tasks={[
      { id: 'running-1', name: 'first.zip', source: 'https://example.com/first.zip', kind: 'file', status: 'downloading', progress: 25 },
      { id: 'paused-2', name: 'second.zip', source: 'https://example.com/second.zip', kind: 'file', status: 'paused', progress: 50 },
    ]}
    selectedTaskId={null}
    onSelect={onSelect}
    onCommand={() => {}}
    onBatchCommand={onBatchCommand}
  />);

  fireEvent.click(screen.getByText('first.zip'));
  fireEvent.click(screen.getByText('second.zip'), { ctrlKey: true });

  const firstRow = screen.getByText('first.zip').closest('.task-row');
  const secondRow = screen.getByText('second.zip').closest('.task-row');
  expect(firstRow).toHaveAttribute('aria-selected', 'true');
  expect(secondRow).toHaveAttribute('aria-selected', 'true');

  fireEvent.contextMenu(screen.getByText('first.zip'), { clientX: 280, clientY: 180 });
  expect(screen.getByRole('menu', { name: '已选择 2 个任务 任务菜单' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '开始下载' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '暂停下载' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '删除' })).toBeVisible();
  expect(screen.getByRole('menuitem', { name: '复制下载链接' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '打开下载目录' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '查看任务详情' })).not.toBeInTheDocument();

  fireEvent.click(screen.getByRole('menuitem', { name: '复制下载链接' }));
  expect(onBatchCommand).toHaveBeenCalledWith(['running-1', 'paused-2'], 'copy_link');
});

it('selects a contiguous task range with Shift-click', () => {
  render(<DownloadQueue
    tasks={[
      { id: 'range-1', name: 'one.zip', kind: 'file', status: 'queued', progress: 0 },
      { id: 'range-2', name: 'two.zip', kind: 'file', status: 'queued', progress: 0 },
      { id: 'range-3', name: 'three.zip', kind: 'file', status: 'queued', progress: 0 },
    ]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  fireEvent.click(screen.getByText('one.zip'));
  fireEvent.click(screen.getByText('three.zip'), { shiftKey: true });

  expect(document.querySelectorAll('.task-row[aria-selected="true"]')).toHaveLength(3);
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
  expect(screen.getByRole('menuitem', { name: '移入回收站' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '暂停下载' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '取消任务' })).not.toBeInTheDocument();
});

it('omits terminal status text from the more-menu header', () => {
  render(<DownloadQueue
    tasks={[{ id: 'done-menu-1', name: 'wallpaper 4.jpg', kind: 'file', status: 'completed', progress: 100 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  fireEvent.contextMenu(screen.getByText('wallpaper 4.jpg'));

  const menu = screen.getByRole('menu', { name: 'wallpaper 4.jpg 任务菜单' });
  expect(menu).toHaveTextContent('wallpaper 4.jpg');
  expect(menu).not.toHaveTextContent('已完成');
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

it('offers permanent deletion only for a task already in the recycle bin', () => {
  render(<DownloadQueue
    tasks={[{ id: 'recycled-1', name: 'old.iso', kind: 'file', status: 'recycled', progress: 100 }]}
    selectedTaskId={null}
    onSelect={() => {}}
    onCommand={() => {}}
  />);

  fireEvent.contextMenu(screen.getByText('old.iso'));
  expect(screen.getByRole('menuitem', { name: '彻底删除…' })).toBeVisible();
  expect(screen.queryByRole('menuitem', { name: '移入回收站' })).not.toBeInTheDocument();
  expect(screen.queryByRole('menuitem', { name: '重新下载' })).not.toBeInTheDocument();
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
