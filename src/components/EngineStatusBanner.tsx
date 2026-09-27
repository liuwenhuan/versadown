import type { DownloadEngineState } from '../lib/types';
import { useTranslation } from '../i18n';

export function EngineStatusToast({ state, onRetry }: {
  state: DownloadEngineState;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  if (state.available) return null;
  const retrying = state.retrying;
  return (
    <section className={`task-toast engine-status-toast ${retrying ? 'engine-starting' : 'error'}`} role="status" aria-live="polite">
      <div>
        <b>{retrying ? t('engine.starting') : t('engine.unavailable')}</b>
        <span>{state.error ?? (retrying ? t('engine.mainWindowAvailable') : t('engine.retryHint'))}</span>
      </div>
      <button
        type="button"
        disabled={retrying}
        aria-label={retrying ? t('engine.retrying') : t('engine.retry')}
        onClick={onRetry}
      >
        {retrying ? t('engine.retryingShort') : t('engine.restart')}
      </button>
    </section>
  );
}
