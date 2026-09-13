import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { DownloadQueue } from './DownloadQueue';

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
