import { fireEvent, render, screen, within } from '@testing-library/react';
import { vi } from 'vitest';
import { DownloadQueue } from './DownloadQueue';
import type { QueueTask } from '../lib/types';

const proxied: QueueTask = { id: 'video', name: 'clip.mp4', kind: 'web_video', status: 'failed', progress: 0, network: 'follow', error: 'video.error.accessDeniedProxy', source: 'https://www.bilibili.com/video/BV1' };
const directTimeout: QueueTask = { id: 'file', name: 'archive.zip', kind: 'file', status: 'failed', progress: 0, network: 'direct', aria_error_code: '2', error: 'timeout', source: 'https://example.test/archive.zip' };

function renderQueue(tasks: QueueTask[], manualProxyAvailable = true) {
  const onCommand = vi.fn();
  render(<DownloadQueue tasks={tasks} privacyEnabled={false} selectedTaskId={null} onSelect={() => {}} onCommand={onCommand} manualProxyAvailable={manualProxyAvailable} />);
  return onCommand;
}

it('offers a direct retry for a proxy rejection, on the row and in the context menu', () => {
  const onCommand = renderQueue([proxied]);
  fireEvent.click(screen.getByRole('button', { name: '直连重试 clip.mp4' }));
  expect(onCommand).toHaveBeenLastCalledWith('video', 'retry_direct');

  fireEvent.contextMenu(screen.getByText('clip.mp4'));
  fireEvent.click(within(screen.getByRole('menu')).getByRole('menuitem', { name: '直连重试' }));
  expect(onCommand).toHaveBeenLastCalledWith('video', 'retry_direct');
  expect(screen.queryByRole('button', { name: /用代理重试/ })).not.toBeInTheDocument();
});

it('offers a proxy retry only for unreachable direct tasks while a manual proxy exists', () => {
  const onCommand = renderQueue([
    directTimeout,
    { ...directTimeout, id: 'geo', name: 'geo.mp4', kind: 'web_video', aria_error_code: undefined, error: 'video.error.geoRestricted' },
    { ...directTimeout, id: 'follow', name: 'follow.zip', network: 'follow' },
    { ...directTimeout, id: 'denied', name: 'denied.zip', aria_error_code: '22' },
  ]);
  fireEvent.click(screen.getByRole('button', { name: '用代理重试 archive.zip' }));
  expect(onCommand).toHaveBeenLastCalledWith('file', 'retry_proxy');
  expect(screen.getByRole('button', { name: '用代理重试 geo.mp4' })).toBeVisible();
  expect(screen.queryByRole('button', { name: '用代理重试 follow.zip' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '用代理重试 denied.zip' })).not.toBeInTheDocument();

  fireEvent.contextMenu(screen.getByText('archive.zip'));
  fireEvent.click(within(screen.getByRole('menu')).getByRole('menuitem', { name: '用代理重试' }));
  expect(onCommand).toHaveBeenLastCalledWith('file', 'retry_proxy');
});

it('never offers a proxy retry without a manual proxy address', () => {
  renderQueue([directTimeout], false);
  expect(screen.queryByRole('button', { name: /用代理重试/ })).not.toBeInTheDocument();
  fireEvent.contextMenu(screen.getByText('archive.zip'));
  expect(within(screen.getByRole('menu')).queryByRole('menuitem', { name: '用代理重试' })).not.toBeInTheDocument();
});

it('keeps cloud downloads and unfinished tasks on their usual actions', () => {
  renderQueue([
    { ...proxied, id: 'cloud', name: 'cloud.mp4', kind: 'cloud', source: 'cloud://baidu/file' },
    { ...proxied, id: 'running', name: 'running.mp4', status: 'downloading' },
  ]);
  expect(screen.queryByRole('button', { name: /直连重试/ })).not.toBeInTheDocument();
});
