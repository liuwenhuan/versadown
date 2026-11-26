import type { QueueTask } from './types';

export type NetworkRetryCommand = 'retry_direct' | 'retry_proxy';

/** Cloud downloads always follow the global proxy settings. */
export function isCloudSourceTask(task: Pick<QueueTask, 'kind' | 'source'>): boolean {
  return task.kind === 'cloud' || Boolean(task.source?.startsWith('cloud://') || task.source?.startsWith('cloud-share://'));
}

// aria2 exit codes: 2 timeout, 6 network problem, 19 name resolution failed.
const UNREACHABLE_ARIA2_CODES = new Set(['2', '6', '19']);
const UNREACHABLE_VIDEO_ERRORS = new Set(['video.error.network', 'video.error.geoRestricted']);

function failedToReach(task: QueueTask): boolean {
  return UNREACHABLE_VIDEO_ERRORS.has(task.error ?? '') || UNREACHABLE_ARIA2_CODES.has(task.aria_error_code ?? '');
}

/** The one-click network switch a failed task offers, if any. */
export function networkRetryCommand(task: QueueTask, manualProxyAvailable: boolean): NetworkRetryCommand | null {
  if (task.status !== 'failed' || isCloudSourceTask(task)) return null;
  if (task.error === 'video.error.accessDeniedProxy') return 'retry_direct';
  if (task.network === 'direct' && manualProxyAvailable && failedToReach(task)) return 'retry_proxy';
  return null;
}
