import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { TaskDetailsDialog } from './TaskDetailsDialog';

const task = {
  id: 'task-1',
  name: 'ubuntu.iso',
  source: 'https://releases.example/ubuntu.iso',
  kind: 'file',
  status: 'downloading',
  progress: 42,
} as const;

it('shows task details in an independent dialog and closes from its explicit close control', async () => {
  const user = userEvent.setup();
  const onClose = vi.fn();
  render(<TaskDetailsDialog task={task} onClose={onClose} />);

  expect(screen.getByRole('dialog', { name: '任务详情' })).toHaveTextContent('ubuntu.iso');
  expect(screen.getByRole('dialog', { name: '任务详情' })).toHaveTextContent('https://releases.example/ubuntu.iso');
  await user.click(screen.getByRole('button', { name: '关闭任务详情' }));
  expect(onClose).toHaveBeenCalledOnce();
});

it('keeps task details open when its backdrop is clicked', () => {
  const onClose = vi.fn();
  render(<TaskDetailsDialog task={task} onClose={onClose} />);

  fireEvent.mouseDown(screen.getByRole('presentation'));
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole('dialog', { name: '任务详情' })).toBeVisible();

  fireEvent.keyDown(document, { key: 'Escape' });
  expect(onClose).toHaveBeenCalledOnce();
});
