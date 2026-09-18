import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';

const theme = vi.hoisted(() => ({
  resolved: 'light' as 'light' | 'dark',
  listeners: new Set<() => void>()
}));

vi.mock('../lib/theme', () => ({
  getResolvedTheme: () => theme.resolved,
  subscribeTheme: (listener: () => void) => {
    theme.listeners.add(listener);
    return () => theme.listeners.delete(listener);
  }
}));

import { FloatingLauncher } from './FloatingLauncher';

it('expands the 64px launcher and labels its action 开始解析下载', async () => {
  const user = userEvent.setup();
  render(<FloatingLauncher onParse={vi.fn()} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-size', '64');
  await user.click(trigger);
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeVisible();
});

it('notifies its host when the compact launcher expands and collapses', async () => {
  const user = userEvent.setup();
  const onExpandedChange = vi.fn();
  // @ts-expect-error The window-size callback is the behavior under test.
  render(<FloatingLauncher onParse={vi.fn()} onExpandedChange={onExpandedChange} />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(onExpandedChange).toHaveBeenLastCalledWith(true);

  await user.click(screen.getByRole('button', { name: '收起下载输入' }));
  expect(onExpandedChange).toHaveBeenLastCalledWith(false);
});

it('keeps the launcher available to close the quick-add panel with a second click', async () => {
  const user = userEvent.setup();
  const onExpandedChange = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} onExpandedChange={onExpandedChange} />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
  expect(screen.getByRole('button', { name: '打开下载输入' })).toBeVisible();

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
  expect(onExpandedChange).toHaveBeenLastCalledWith(false);
});

it('opens the task list from the quick-add panel', async () => {
  const user = userEvent.setup();
  const onOpenTaskList = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} onOpenTaskList={onOpenTaskList} />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.click(screen.getByRole('button', { name: '打开任务列表' }));

  expect(onOpenTaskList).toHaveBeenCalledOnce();
  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
});

it('collapses the expanded launcher with Escape', async () => {
  const user = userEvent.setup();
  render(<FloatingLauncher onParse={vi.fn()} />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
  await user.keyboard('{Escape}');
  expect(screen.getByRole('button', { name: '打开下载输入' })).toBeVisible();
});

it('submits a pasted input to the parser', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn();
  render(<FloatingLauncher onParse={onParse} />);
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'https://example.com/file.zip');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(onParse).toHaveBeenCalledWith('https://example.com/file.zip', undefined);
});

it('scans launcher quick-add input before dispatching it to the manager', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn().mockResolvedValue(undefined);
  const onScan = vi.fn().mockResolvedValue([
    { input: 'ftp://example.test/pub/archive.iso', valid: true, kind: 'ftp' }
  ]);
  render(<FloatingLauncher onParse={onParse} onScan={onScan} />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'ftp://example.test/pub/archive.iso');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(onScan).toHaveBeenCalledWith('ftp://example.test/pub/archive.iso');
  expect(onParse).not.toHaveBeenCalled();
  await user.click(screen.getByRole('button', { name: '确认并完成' }));
  expect(onParse).toHaveBeenCalledWith('ftp://example.test/pub/archive.iso', undefined);
});

it('opens a new task from a valid link embedded in dropped text', async () => {
  const onScan = vi.fn().mockResolvedValue([
    { input: 'not a link', valid: false, message: '不支持的下载内容' },
    { input: 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567', valid: true, kind: 'magnet' },
  ]);
  const onDroppedInput = vi.fn().mockResolvedValue(undefined);
  render(<FloatingLauncher onParse={vi.fn()} onScan={onScan} onDroppedInput={onDroppedInput} />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  fireEvent.drop(trigger, {
    dataTransfer: {
      files: [],
      getData: (type: string) => type === 'text/plain'
        ? '复制内容：magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567'
        : '',
    },
  });

  await screen.findByRole('button', { name: '打开下载输入' });
  expect(onScan).toHaveBeenCalledWith('复制内容：magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567');
  await waitFor(() => expect(onDroppedInput).toHaveBeenCalledWith('magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567'));
});

it('stages a dropped torrent before opening the new-task dialog', async () => {
  const onStageDroppedTorrent = vi.fn().mockResolvedValue('C:\\AppData\\dropped-torrents\\fixture.torrent');
  const onDroppedInput = vi.fn().mockResolvedValue(undefined);
  render(<FloatingLauncher onParse={vi.fn()} onDroppedInput={onDroppedInput} onStageDroppedTorrent={onStageDroppedTorrent} />);

  fireEvent.drop(screen.getByRole('button', { name: '打开下载输入' }), {
    dataTransfer: { files: [{ name: 'fixture.torrent' }], getData: () => '' },
  });

  await waitFor(() => expect(onStageDroppedTorrent).toHaveBeenCalledWith(expect.objectContaining({ name: 'fixture.torrent' })));
  expect(onDroppedInput).toHaveBeenCalledWith('C:\\AppData\\dropped-torrents\\fixture.torrent');
});

it('starts dragging after pointer movement without opening the input panel', () => {
  const onDragStart = vi.fn();
  const onDragMove = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} onDragStart={onDragStart} onDragMove={onDragMove} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.mouseDown(trigger, { button: 0, clientX: 10, clientY: 10, screenX: 100, screenY: 200 });
  // The window may already have moved beneath the cursor, so its local
  // coordinates can go backwards while screen coordinates keep increasing.
  fireEvent.mouseMove(trigger, { buttons: 1, clientX: 2, clientY: 6, screenX: 108, screenY: 206 });
  fireEvent.click(trigger);

  expect(onDragStart).toHaveBeenCalledWith({ x: 100, y: 200 }, { x: 10, y: 10 });
  expect(onDragMove).toHaveBeenCalledWith({ x: 108, y: 206 });
  expect(screen.queryByRole('button', { name: '开始解析下载' })).not.toBeInTheDocument();
});

it('opens the launcher context menu on right click without expanding', () => {
  const onContextMenu = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} onContextMenu={onContextMenu} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.contextMenu(trigger);

  expect(onContextMenu).toHaveBeenCalledOnce();
  expect(screen.queryByRole('button', { name: '开始解析下载' })).not.toBeInTheDocument();
});

it('uses the dark mint graphic in dark theme', () => {
  theme.resolved = 'dark';
  render(<FloatingLauncher onParse={vi.fn()} />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-icon-variant', 'dark');
  expect(trigger.querySelector('.launcher-mark-shell')).toHaveAttribute('fill', '#2e3239');
  expect(trigger.querySelector('.launcher-mark-chevron')).toHaveAttribute('fill', '#c4e3d1');
});

it('switches the launcher graphic when the theme changes', () => {
  theme.resolved = 'light';
  render(<FloatingLauncher onParse={vi.fn()} />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-icon-variant', 'light');
  act(() => {
    theme.resolved = 'dark';
    theme.listeners.forEach((listener) => listener());
  });
  expect(trigger).toHaveAttribute('data-icon-variant', 'dark');
});

it('renders vector launcher artwork without a raster matte or outline', () => {
  theme.resolved = 'light';
  render(<FloatingLauncher onParse={vi.fn()} />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  const shell = trigger.querySelector('.launcher-mark-shell');
  expect(trigger.querySelector('img')).not.toBeInTheDocument();
  expect(trigger.querySelector('svg.launcher-mark')).toBeInTheDocument();
  expect(shell).toHaveAttribute('fill', '#8bc2b3');
  expect(shell).toHaveAttribute('stroke', 'none');
});
