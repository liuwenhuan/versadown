import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { DownloadQueue } from './DownloadQueue';
import type { QueueTask, TaskStatus } from '../lib/types';

it('resumes the recovered directory root without issuing actions for completed descendants', async () => {
  const onCommand = vi.fn();
  render(<DownloadQueue selectedTaskId={null} onSelect={() => {}} onCommand={onCommand} tasks={[{
    id: 'recovered-root', name: 'Recovered directory', kind: 'folder', status: 'paused', progress: 40,
    children: [
      { id: 'done', name: 'done.txt', kind: 'file', status: 'completed', progress: 100 },
      { id: 'pending', name: 'pending.txt', kind: 'file', status: 'paused', progress: 0 },
    ],
  }]} />);
  await userEvent.setup().click(screen.getByRole('button', { name: /继续|恢复|开始/ }));
  expect(onCommand).toHaveBeenCalledTimes(1);
  expect(onCommand).toHaveBeenCalledWith('recovered-root', 'resume');
});

it('keeps one directory row with aggregate progress and speed as a child is resubmitted', () => {
  const task: QueueTask = {
    id: 'directory', name: 'Whole directory', kind: 'folder', status: 'downloading', progress: 45,
    completed_length: 900, total_length: 2000, download_speed: 128,
    children: [{ id: 'child', name: 'Preparing file', kind: 'file', status: 'parsing', progress: 0, download_speed: 0 }],
  };
  const props = { selectedTaskId: null, onSelect: vi.fn(), onCommand: vi.fn() };
  const { container, rerender } = render(<DownloadQueue {...props} tasks={[task]} />);
  const originalRow = container.querySelector('[data-task-id="directory"]');
  for (const status of ['parsing', 'queued', 'downloading'] as TaskStatus[]) {
    rerender(<DownloadQueue {...props} tasks={[{ ...task, children: [{ ...task.children![0], status }] }]} />);
    expect(container.querySelectorAll('.task-row')).toHaveLength(1);
    expect(container.querySelector('[data-task-id="directory"]')).toBe(originalRow);
    expect(screen.getByRole('progressbar', { name: 'Whole directory 下载进度' })).toHaveAttribute('aria-valuenow', '45');
    expect(screen.getByText('128 B/s')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '暂停 Whole directory' })).toBeInTheDocument();
  }
});
