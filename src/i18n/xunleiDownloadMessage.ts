import { t } from './index';

/** Native status and error keys are stored with the task and translated when displayed. */
export function xunleiDownloadMessage(message: string): string {
  switch (message) {
    case 'download.xunlei.native':
    case 'download.xunlei.accelerating':
    case 'download.xunlei.proxyCheckpoint':
    case 'download.xunlei.submitFailed':
      return t(message);
    default:
      return message;
  }
}
