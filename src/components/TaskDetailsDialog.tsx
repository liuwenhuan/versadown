import { useEffect, useRef } from 'react';
import { QueueTask } from '../lib/types';
import { TaskDetails } from './TaskDetails';
import { useTranslation } from '../i18n';

interface TaskDetailsDialogProps {
  task: QueueTask;
  onClose(): void;
}

export function TaskDetailsDialog({ task, onClose }: TaskDetailsDialogProps) {
  const { t } = useTranslation();
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    closeRef.current?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', closeForEscape);
    return () => document.removeEventListener('keydown', closeForEscape);
  }, [onClose]);

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
    <section className="task-details-dialog" role="dialog" aria-modal="true" aria-label={t('details.title')}>
      <header><h2>{t('details.title')}</h2><button ref={closeRef} className="dialog-close" aria-label={t('details.close')} onClick={onClose}>×</button></header>
      <div className="task-details-dialog-content"><TaskDetails task={task} /></div>
    </section>
  </div>;
}
