import { useEffect, useRef, useState } from 'react';
import { useTranslation } from '../i18n';

const deleteFilesPreferenceKey = 'remove-task-delete-files';

function savedDeleteFilesPreference(): boolean {
  try {
    return window.localStorage.getItem(deleteFilesPreferenceKey) === 'true';
  } catch {
    return false;
  }
}

function saveDeleteFilesPreference(value: boolean) {
  try {
    window.localStorage.setItem(deleteFilesPreferenceKey, String(value));
  } catch {
    // Keep the dialog usable if persistent browser storage is unavailable.
  }
}

export function RemoveTaskDialog({ taskNames, onConfirm, onClose }: { taskNames: readonly string[]; onConfirm(deleteFiles: boolean): void; onClose(): void }) {
  const { t } = useTranslation();
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [deleteFiles, setDeleteFiles] = useState(savedDeleteFilesPreference);
  const isBatch = taskNames.length > 1;
  const subject = isBatch ? t('removeTasks.count', { count: taskNames.length }) : `“${taskNames[0] ?? t('removeTasks.unnamed')}”`;
  useEffect(() => {
    cancelRef.current?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', closeForEscape);
    return () => document.removeEventListener('keydown', closeForEscape);
  }, [onClose]);
  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
    <section className="remove-task-dialog" role="dialog" aria-modal="true" aria-labelledby="remove-task-title">
      <div className="dialog-warning" aria-hidden="true">!</div>
      <div><h2 id="remove-task-title">{t('removeTasks.title')}</h2><p>{t('removeTasks.confirm', { subject })}</p><label className="delete-files-option"><input aria-label={t('removeTasks.deleteFiles')} type="checkbox" checked={deleteFiles} onChange={(event) => { setDeleteFiles(event.target.checked); saveDeleteFilesPreference(event.target.checked); }} /> <span>{t('removeTasks.deleteFiles')}</span></label><small>{t('removeTasks.description')}</small></div>
      <footer><button ref={cancelRef} onClick={onClose}>{t('dialog.cancel')}</button><button className="danger-button" onClick={() => onConfirm(deleteFiles)}>{t('removeTasks.title')}</button></footer>
    </section>
  </div>;
}
