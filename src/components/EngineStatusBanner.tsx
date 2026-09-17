import type { DownloadEngineState } from '../lib/types';

export function EngineStatusToast({ state, onRetry }: {
  state: DownloadEngineState;
  onRetry: () => void;
}) {
  if (state.available) return null;
  const retrying = state.retrying;
  return (
    <section className={`task-toast engine-status-toast ${retrying ? 'engine-starting' : 'error'}`} role="status" aria-live="polite">
      <div>
        <b>{retrying ? '正在启动下载引擎…' : '下载引擎不可用'}</b>
        <span>{state.error ?? (retrying ? '主界面仍可使用，请稍候。' : '下载操作暂不可用，可手动重试。')}</span>
      </div>
      <button
        type="button"
        disabled={retrying}
        aria-label={retrying ? '正在重试下载引擎' : '重试下载引擎'}
        onClick={onRetry}
      >
        {retrying ? '重试中…' : '重新启动'}
      </button>
    </section>
  );
}
