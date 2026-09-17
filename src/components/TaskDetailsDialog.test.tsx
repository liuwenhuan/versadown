import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { readFileSync } from 'node:fs';
import { afterEach, vi } from 'vitest';
import { TaskDetailsDialog } from './TaskDetailsDialog';

const task = {
  id: 'task-1',
  name: 'ubuntu.iso',
  source: 'https://releases.example/ubuntu.iso',
  kind: 'file',
  status: 'downloading',
  progress: 42,
} as const;

afterEach(cleanup);

it('shows task details in an independent dialog and closes from its explicit close control', async () => {
  const user = userEvent.setup();
  const onClose = vi.fn();
  render(<TaskDetailsDialog task={task} onClose={onClose} />);

  const dialog = screen.getByRole('dialog', { name: '任务详情' });
  expect(screen.getByRole('heading', { name: '任务详情' })).toBeVisible();
  expect(dialog.querySelector('.task-details-dialog-content')).not.toBeNull();
  expect(dialog).toHaveTextContent('ubuntu.iso');
  expect(dialog).toHaveTextContent('https://releases.example/ubuntu.iso');
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

it('uses fixed, independently scrollable file-list and expanded post-processing panels', () => {
  const styles = readFileSync('src/styles.css', 'utf8');

  expect(styles).toContain('.task-details-dialog{position:relative;display:grid;width:min(620px,calc(100vw - 36px));height:min(520px,calc(100vh - 36px));');
  expect(styles).toContain('.task-details-dialog-content{min-height:0;overflow:hidden;');
  expect(styles).toContain('.detail-file-list{min-height:0;overflow-y:auto;');
  expect(styles).toContain('.task-details .detail-file-list{height:176px;min-height:176px;');
  expect(styles).toContain('.task-details .task-post-processing-note.is-expanded .task-post-processing-content{height:150px;overflow-y:auto}');
  expect(styles).not.toContain('.task-details-dialog-content{min-height:0;overflow-y:auto;');
});
