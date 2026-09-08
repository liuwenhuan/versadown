import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { EngineStatusBanner } from './EngineStatusBanner';

it('explains that the engine is offline and lets the user retry it', async () => {
  const retry = vi.fn();
  render(<EngineStatusBanner state={{ available: false, retrying: false, error: '下载引擎不可用：aria2 启动失败' }} onRetry={retry} />);

  expect(screen.getByRole('status')).toHaveTextContent('下载引擎不可用');
  await userEvent.click(screen.getByRole('button', { name: '重试下载引擎' }));
  expect(retry).toHaveBeenCalledOnce();
});

it('disables duplicate retries while startup is in progress', () => {
  render(<EngineStatusBanner state={{ available: false, retrying: true }} onRetry={() => undefined} />);
  expect(screen.getByRole('button', { name: '正在重试下载引擎' })).toBeDisabled();
});
