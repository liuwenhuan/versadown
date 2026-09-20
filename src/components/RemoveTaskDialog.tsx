import { useEffect, useRef, useState } from 'react';

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
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [deleteFiles, setDeleteFiles] = useState(savedDeleteFilesPreference);
  const isBatch = taskNames.length > 1;
  const subject = isBatch ? `${taskNames.length} 个任务` : `“${taskNames[0] ?? '未命名任务'}”`;
  useEffect(() => {
    cancelRef.current?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', closeForEscape);
    return () => document.removeEventListener('keydown', closeForEscape);
  }, [onClose]);
  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
    <section className="remove-task-dialog" role="dialog" aria-modal="true" aria-labelledby="remove-task-title">
      <div className="dialog-warning" aria-hidden="true">!</div>
      <div><h2 id="remove-task-title">彻底删除任务</h2><p>确定彻底删除{subject}吗？</p><label className="delete-files-option"><input aria-label="同时删除下载文件" type="checkbox" checked={deleteFiles} onChange={(event) => { setDeleteFiles(event.target.checked); saveDeleteFilesPreference(event.target.checked); }} /> <span>同时删除下载文件</span></label><small>任务将被永久删除；勾选后会一并删除记录中的本地下载文件。</small></div>
      <footer><button ref={cancelRef} onClick={onClose}>取消</button><button className="danger-button" onClick={() => onConfirm(deleteFiles)}>彻底删除</button></footer>
    </section>
  </div>;
}
