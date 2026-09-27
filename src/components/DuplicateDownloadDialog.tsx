import { useEffect, useRef } from 'react';
import type { DuplicateDownloadState, QueueTask } from '../lib/types';
import { t, useTranslation } from '../i18n';

interface DuplicateDownloadDialogProps {
  task: QueueTask;
  state: DuplicateDownloadState;
  onOpenDirectory(): void;
  onResolve(action: 'resume' | 'overwrite'): void;
  onRestartMissing(): void;
  onClose(): void;
}

const content: Record<DuplicateDownloadState, { text: string; action: string }> = {
  existing_active: {
    text: 'duplicate.active.text',
    action: 'duplicate.active.action',
  },
  resume_available: {
    text: 'duplicate.resume.text',
    action: 'duplicate.resume.action',
  },
  completed_files_present: {
    text: 'duplicate.present.text',
    action: 'duplicate.present.action',
  },
  completed_files_missing: {
    text: 'duplicate.missing.text',
    action: 'duplicate.missing.action',
  },
  overwrite_required: {
    text: 'duplicate.overwrite.text',
    action: 'duplicate.overwrite.action',
  },
} satisfies Record<DuplicateDownloadState, { text: string; action: string }>;

export function DuplicateDownloadDialog({ task, state, onOpenDirectory, onResolve, onRestartMissing, onClose }: DuplicateDownloadDialogProps) {
  const { t } = useTranslation();
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    closeRef.current?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', closeForEscape);
    return () => document.removeEventListener('keydown', closeForEscape);
  }, [onClose]);
  const details = content[state];
  const performAction = () => {
    if (state === 'existing_active') onClose();
    else if (state === 'completed_files_present') onOpenDirectory();
    else if (state === 'completed_files_missing') onRestartMissing();
    else onResolve(state === 'resume_available' ? 'resume' : 'overwrite');
  };

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
    <section className="duplicate-download-dialog" role="dialog" aria-modal="true" aria-labelledby="duplicate-download-title">
      <div className="dialog-warning" aria-hidden="true">!</div>
      <div>
        <h2 id="duplicate-download-title">{t('duplicate.title')}</h2>
        <p>“{task.name}”</p>
        <small>{t(details.text as Parameters<typeof t>[0])}</small>
      </div>
      <footer>
        <button ref={closeRef} onClick={onClose}>{t('dialog.cancel')}</button>
        <button className={state === 'overwrite_required' ? 'danger-button' : 'primary'} onClick={performAction}>{t(details.action as Parameters<typeof t>[0])}</button>
      </footer>
    </section>
  </div>;
}
