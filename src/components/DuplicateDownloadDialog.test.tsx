import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { DuplicateDownloadDialog } from './DuplicateDownloadDialog';

it('clearly explains that a completed record has no files and requires a new download', async () => {
  const user = userEvent.setup();
  const onRestartMissing = vi.fn();
  render(
    <DuplicateDownloadDialog
      task={{ id: 'task-1', name: 'fixture.mkv', kind: 'file', status: 'completed', progress: 100 }}
      state="completed_files_missing"
      onOpenDirectory={() => {}}
      onResolve={() => {}}
      onRestartMissing={onRestartMissing}
      onClose={() => {}}
    />,
  );

  expect(screen.getByRole('dialog', { name: '检测到重复下载' })).toHaveTextContent(
    '已经下载过，但下载文件不存在，将重新进行下载',
  );
  await user.click(screen.getByRole('button', { name: '重新下载' }));
  expect(onRestartMissing).toHaveBeenCalledOnce();
});

it('only exposes overwrite after explaining that a safe resume is unavailable', () => {
  render(
    <DuplicateDownloadDialog
      task={{ id: 'task-1', name: 'fixture.mkv', kind: 'file', status: 'failed', progress: 28 }}
      state="overwrite_required"
      onOpenDirectory={() => {}}
      onResolve={() => {}}
      onRestartMissing={() => {}}
      onClose={() => {}}
    />,
  );

  expect(screen.getByText(/无法安全续传/)).toBeVisible();
  expect(screen.getByRole('button', { name: '覆盖并重新下载' })).toBeVisible();
});
