import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { FloatingLauncher } from './FloatingLauncher';

it('expands the 48px launcher and labels its action 开始解析下载', async () => {
  const user = userEvent.setup();
  render(<FloatingLauncher onParse={vi.fn()} activeCount={0} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-size', '48');
  await user.click(trigger);
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeVisible();
});

it('notifies its host when the compact launcher expands and collapses', async () => {
  const user = userEvent.setup();
  const onExpandedChange = vi.fn();
  // @ts-expect-error The window-size callback is the behavior under test.
  render(<FloatingLauncher onParse={vi.fn()} activeCount={0} onExpandedChange={onExpandedChange} />);

  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  expect(onExpandedChange).toHaveBeenLastCalledWith(true);

  await user.click(screen.getByRole('button', { name: '收起下载输入' }));
  expect(onExpandedChange).toHaveBeenLastCalledWith(false);
});

it('submits a pasted input to the parser', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn();
  render(<FloatingLauncher onParse={onParse} activeCount={1} />);
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'https://example.com/file.zip');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(onParse).toHaveBeenCalledWith('https://example.com/file.zip');
});

it('starts dragging after pointer movement without opening the input panel', () => {
  const onDragStart = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} activeCount={0} onDragStart={onDragStart} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.mouseDown(trigger, { button: 0, clientX: 10, clientY: 10 });
  fireEvent.mouseMove(trigger, { buttons: 1, clientX: 18, clientY: 16 });
  fireEvent.click(trigger);

  expect(onDragStart).toHaveBeenCalledOnce();
  expect(screen.queryByRole('button', { name: '开始解析下载' })).not.toBeInTheDocument();
});

it('opens the launcher context menu on right click without expanding', () => {
  const onContextMenu = vi.fn();
  render(<FloatingLauncher onParse={vi.fn()} activeCount={0} onContextMenu={onContextMenu} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });

  fireEvent.contextMenu(trigger);

  expect(onContextMenu).toHaveBeenCalledOnce();
  expect(screen.queryByRole('button', { name: '开始解析下载' })).not.toBeInTheDocument();
});
