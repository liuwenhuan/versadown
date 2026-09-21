import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { vi } from 'vitest';

const theme = vi.hoisted(() => ({
  resolved: 'light' as 'light' | 'dark',
  listeners: new Set<() => void>(),
}));

vi.mock('../lib/theme', () => ({
  getResolvedTheme: () => theme.resolved,
  subscribeTheme: (listener: () => void) => {
    theme.listeners.add(listener);
    return () => theme.listeners.delete(listener);
  },
}));

import { FloatingLauncher } from './FloatingLauncher';

it('keeps a single left click compact and does not render a new-task dialog', () => {
  const onActivateTaskList = vi.fn();
  render(<FloatingLauncher onActivateTaskList={onActivateTaskList} />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-size', '64');
  expect(trigger.querySelector('svg.launcher-mark')).toHaveAttribute('viewBox', '175 123 674 778');
  fireEvent.click(trigger);

  expect(onActivateTaskList).not.toHaveBeenCalled();
  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
});

it('activates the task list only after a left double click', () => {
  const onActivateTaskList = vi.fn();
  render(<FloatingLauncher onActivateTaskList={onActivateTaskList} />);

  fireEvent.doubleClick(screen.getByRole('button', { name: '打开下载输入' }));

  expect(onActivateTaskList).toHaveBeenCalledOnce();
  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
});

it('opens a normal new task only from a valid link embedded in dropped text', async () => {
  const onScan = vi.fn().mockResolvedValue([
    { input: 'not a link', valid: false, message: '不支持的下载内容' },
    { input: 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567', valid: true, kind: 'magnet' },
  ]);
  const onDroppedInput = vi.fn().mockResolvedValue(undefined);
  render(<FloatingLauncher onScan={onScan} onDroppedInput={onDroppedInput} />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  fireEvent.drop(trigger, {
    dataTransfer: {
      files: [],
      getData: (type: string) => type === 'text/plain'
        ? '复制内容：magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567'
        : '',
    },
  });

  expect(onScan).toHaveBeenCalledWith('复制内容：magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567');
  await waitFor(() => expect(onDroppedInput).toHaveBeenCalledWith('magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567'));
});

it('stages a dropped torrent before opening the normal new-task dialog', async () => {
  const onStageDroppedTorrent = vi.fn().mockResolvedValue('C:\\AppData\\dropped-torrents\\fixture.torrent');
  const onDroppedInput = vi.fn().mockResolvedValue(undefined);
  render(<FloatingLauncher onDroppedInput={onDroppedInput} onStageDroppedTorrent={onStageDroppedTorrent} />);

  fireEvent.drop(screen.getByRole('button', { name: '打开下载输入' }), {
    dataTransfer: { files: [{ name: 'fixture.torrent' }], getData: () => '' },
  });

  await waitFor(() => expect(onStageDroppedTorrent).toHaveBeenCalledWith(expect.objectContaining({ name: 'fixture.torrent' })));
  expect(onDroppedInput).toHaveBeenCalledWith('C:\\AppData\\dropped-torrents\\fixture.torrent');
});

it('starts dragging after pointer movement without activating the task list', () => {
  const onDragStart = vi.fn();
  const onDragMove = vi.fn();
  const onActivateTaskList = vi.fn();
  render(<FloatingLauncher onActivateTaskList={onActivateTaskList} onDragStart={onDragStart} onDragMove={onDragMove} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.mouseDown(trigger, { button: 0, clientX: 10, clientY: 10, screenX: 100, screenY: 200 });
  fireEvent.mouseMove(trigger, { buttons: 1, clientX: 2, clientY: 6, screenX: 108, screenY: 206 });
  fireEvent.click(trigger);

  expect(onDragStart).toHaveBeenCalledWith({ x: 100, y: 200 }, { x: 10, y: 10 });
  expect(onDragMove).toHaveBeenCalledWith({ x: 108, y: 206 });
  expect(onActivateTaskList).not.toHaveBeenCalled();
});

it('opens the launcher context menu on right click without a new-task dialog', () => {
  const onContextMenu = vi.fn();
  render(<FloatingLauncher onContextMenu={onContextMenu} />);

  fireEvent.contextMenu(screen.getByRole('button', { name: '打开下载输入' }));

  expect(onContextMenu).toHaveBeenCalledOnce();
  expect(screen.queryByRole('dialog', { name: '新建下载' })).not.toBeInTheDocument();
});

it('uses the themed inline SVG graphic in dark theme', () => {
  theme.resolved = 'dark';
  render(<FloatingLauncher />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-icon-variant', 'dark');
  expect(trigger.querySelector('svg.launcher-mark')).toBeInTheDocument();
  expect(trigger.querySelector('.launcher-top-start')).toBeInTheDocument();
});

it('switches the launcher graphic when the theme changes', () => {
  theme.resolved = 'light';
  render(<FloatingLauncher />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-icon-variant', 'light');
  act(() => {
    theme.resolved = 'dark';
    theme.listeners.forEach((listener) => listener());
  });
  expect(trigger).toHaveAttribute('data-icon-variant', 'dark');
});

it('renders the themed inline SVG artwork for the light launcher', () => {
  theme.resolved = 'light';
  render(<FloatingLauncher />);

  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger.querySelector('svg.launcher-mark')).toBeInTheDocument();
  expect(trigger.querySelector('img.launcher-mark')).not.toBeInTheDocument();
  expect(trigger.querySelector('.launcher-logo-left')).toBeInTheDocument();
});
