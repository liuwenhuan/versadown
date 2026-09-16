import { fireEvent, render, screen } from '@testing-library/react';
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

it('keeps permanent removal confirmation open when its backdrop is clicked', () => {
  const onClose = vi.fn();
  render(<RemoveTaskDialog taskNames={['finished.iso']} onConfirm={() => {}} onClose={onClose} />);

  fireEvent.mouseDown(screen.getByRole('presentation'));
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole('dialog', { name: '彻底删除任务' })).toBeVisible();
});

it('restores the last checked and unchecked delete-files preference', async () => {
  localStorage.clear();
  const user = userEvent.setup();
  try {
    const first = render(<RemoveTaskDialog taskNames={['first.iso']} onConfirm={() => {}} onClose={() => {}} />);
    const firstCheckbox = screen.getByRole('checkbox', { name: '同时删除下载文件' });
    expect(firstCheckbox).not.toBeChecked();
    await user.click(firstCheckbox);
    expect(firstCheckbox).toBeChecked();
    first.unmount();

    const second = render(<RemoveTaskDialog taskNames={['second.iso']} onConfirm={() => {}} onClose={() => {}} />);
    const secondCheckbox = screen.getByRole('checkbox', { name: '同时删除下载文件' });
    expect(secondCheckbox).toBeChecked();
    await user.click(secondCheckbox);
    second.unmount();

    render(<RemoveTaskDialog taskNames={['third.iso']} onConfirm={() => {}} onClose={() => {}} />);
    expect(screen.getByRole('checkbox', { name: '同时删除下载文件' })).not.toBeChecked();
  } finally {
    localStorage.clear();
  }
});
