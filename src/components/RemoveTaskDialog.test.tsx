import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { RemoveTaskDialog } from './RemoveTaskDialog';

it('explains that removing a task keeps downloaded files before confirmation', async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(<RemoveTaskDialog taskName="finished.iso" onConfirm={onConfirm} onClose={() => {}} />);

  expect(screen.getByRole('dialog', { name: '移除任务记录' })).toHaveTextContent('不会删除已经下载的文件');
  await user.click(screen.getByRole('button', { name: '移除记录' }));
  expect(onConfirm).toHaveBeenCalledOnce();
});
