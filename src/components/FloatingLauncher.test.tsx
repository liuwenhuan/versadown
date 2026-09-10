import { act, fireEvent, render, screen } from '@testing-library/react';
import { readFileSync } from 'node:fs';
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

it('starts dragging after pointer movement without opening the input panel', () => {
  const onDragStart = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} onDragStart={onDragStart} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.mouseDown(trigger, { button: 0, clientX: 10, clientY: 10 });
  fireEvent.mouseMove(trigger, { buttons: 1, clientX: 18, clientY: 16 });
  fireEvent.click(trigger);

  expect(onDragStart).toHaveBeenCalledOnce();
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
  expect(trigger.querySelector('img')).toHaveAttribute('src', expect.stringContaining('dark_mint'));
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

it('matches the live-speed strip to the active launcher graphic theme', () => {
  theme.resolved = 'light';
  render(<FloatingLauncher onParse={vi.fn()} liveSpeed={{ label: '下载速度 204 KB/s', value: '204K' }} />);

  const speed = screen.getByLabelText('下载速度 204 KB/s');
  expect(speed).toHaveAttribute('data-icon-variant', 'light');

  act(() => {
    theme.resolved = 'dark';
    theme.listeners.forEach((listener) => listener());
  });
  expect(speed).toHaveAttribute('data-icon-variant', 'dark');
});

it('renders the live launcher without rectangular chrome or seams around the hexagon', () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  document.documentElement.classList.add('launcher-window');
  try {
    render(<FloatingLauncher onParse={vi.fn()} liveSpeed={{ label: '下载速度 204 KB/s', value: '204K' }} />);

    const trigger = screen.getByRole('button', { name: '打开下载输入' });
    const image = trigger.querySelector('img');
    const speed = screen.getByLabelText('下载速度 204 KB/s');

    expect(getComputedStyle(document.body).transition).toBe('none');
    expect(getComputedStyle(trigger).appearance).toBe('none');
    expect(getComputedStyle(trigger).transition).toBe('none');
    expect(getComputedStyle(image!).filter).toBe('none');
    expect(getComputedStyle(image!).clipPath).not.toBe('none');
    expect(getComputedStyle(speed).borderTopWidth).toBe('0px');
    expect(getComputedStyle(speed).boxShadow).toBe('none');
    expect(getComputedStyle(speed).top).toBe('20px');
    expect(getComputedStyle(speed).left).toBe('54px');
    expect(getComputedStyle(speed).width).toBe('92px');
    expect(getComputedStyle(speed).height).toBe('25px');
    expect(getComputedStyle(speed).justifyContent).toBe('center');
    expect(speed).toHaveTextContent('204K/s');
    expect(speed).not.toHaveTextContent('↓');
  } finally {
    document.documentElement.classList.remove('launcher-window');
    stylesheet.remove();
  }
});
