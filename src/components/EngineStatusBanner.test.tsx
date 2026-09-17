import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { EngineStatusToast } from './EngineStatusBanner';

it('shows an unavailable engine in the shared lower-right toast and lets the user retry it', async () => {
  const retry = vi.fn();
  const { unmount } = render(<EngineStatusToast state={{ available: false, retrying: false, error: '下载引擎不可用：aria2 启动失败' }} onRetry={retry} />);

  expect(screen.getByRole('status')).toHaveClass('task-toast', 'engine-status-toast', 'error');
  expect(screen.getByRole('status')).toHaveTextContent('下载引擎不可用');
  await userEvent.click(screen.getByRole('button', { name: '重试下载引擎' }));
  expect(retry).toHaveBeenCalledOnce();
  unmount();
});

it('shows startup progress in the shared lower-right toast', () => {
  render(<EngineStatusToast state={{ available: false, retrying: true }} onRetry={() => undefined} />);
  expect(screen.getByRole('status')).toHaveClass('task-toast', 'engine-status-toast', 'engine-starting');
  expect(screen.getByRole('button', { name: '正在重试下载引擎' })).toBeDisabled();
});
