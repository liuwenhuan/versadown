import { fireEvent, render, screen } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, vi } from 'vitest';
import { DuplicateDownloadDialog } from './DuplicateDownloadDialog';

let stylesheet: HTMLStyleElement;

beforeEach(() => {
  stylesheet = document.createElement('style');
  // jsdom does not apply the browser's :hover pseudo-state.  Use an
  // equivalent attribute only inside this integration-style CSS test so the
  // real cascade, including selector specificity, is still exercised.
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8').replaceAll(':hover', '[data-ui-hover]');
  document.head.appendChild(stylesheet);
});

afterEach(() => stylesheet.remove());

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

it('keeps duplicate-download resolution open when its backdrop is clicked', () => {
  const onClose = vi.fn();
  render(
    <DuplicateDownloadDialog
      task={{ id: 'task-1', name: 'fixture.mkv', kind: 'file', status: 'failed', progress: 28 }}
      state="overwrite_required"
      onOpenDirectory={() => {}}
      onResolve={() => {}}
      onRestartMissing={() => {}}
      onClose={onClose}
    />,
  );

  fireEvent.mouseDown(screen.getByRole('presentation'));
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole('dialog', { name: '检测到重复下载' })).toBeVisible();
});

it('keeps the existing-task primary action colored while hovered', () => {
  render(
    <DuplicateDownloadDialog
      task={{ id: 'task-1', name: 'fixture.mkv', kind: 'file', status: 'downloading', progress: 28 }}
      state="existing_active"
      onOpenDirectory={() => {}}
      onResolve={() => {}}
      onRestartMissing={() => {}}
      onClose={() => {}}
    />,
  );

  const action = screen.getByRole('button', { name: '查看现有任务' });
  action.setAttribute('data-ui-hover', '');
  expect(getComputedStyle(action).filter).toBe('brightness(.94)');
});

it('keeps the overwrite action colored while hovered', () => {
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

  const action = screen.getByRole('button', { name: '覆盖并重新下载' });
  action.setAttribute('data-ui-hover', '');
  expect(getComputedStyle(action).filter).toBe('brightness(.94)');
});
