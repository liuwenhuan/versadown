import { useEffect, useRef } from 'react';

export function RemoveTaskDialog({ taskName, onConfirm, onClose }: { taskName: string; onConfirm(): void; onClose(): void }) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancelRef.current?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', closeForEscape);
    return () => document.removeEventListener('keydown', closeForEscape);
  }, [onClose]);
  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <section className="remove-task-dialog" role="dialog" aria-modal="true" aria-labelledby="remove-task-title">
      <div className="dialog-warning" aria-hidden="true">!</div>
      <div><h2 id="remove-task-title">移除任务记录</h2><p>确定从列表中移除“{taskName}”吗？</p><small>只会移除任务记录，不会删除已经下载的文件。</small></div>
      <footer><button ref={cancelRef} onClick={onClose}>取消</button><button className="danger-button" onClick={onConfirm}>移除记录</button></footer>
    </section>
  </div>;
}
