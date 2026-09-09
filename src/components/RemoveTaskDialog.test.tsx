import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { RemoveTaskDialog } from './RemoveTaskDialog';

it('asks whether downloaded files should be deleted before permanent removal', async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  render(<RemoveTaskDialog taskNames={['finished.iso']} onConfirm={onConfirm} onClose={() => {}} />);

  expect(screen.getByRole('dialog', { name: '彻底删除任务' })).toHaveTextContent('同时删除下载文件');
  await user.click(screen.getByRole('checkbox', { name: '同时删除下载文件' }));
  await user.click(screen.getByRole('button', { name: '彻底删除' }));
  expect(onConfirm).toHaveBeenCalledWith(true);
});
