import { useEffect, useRef } from 'react';
import { QueueTask } from '../lib/types';
import { TaskDetails } from './TaskDetails';

interface TaskDetailsDialogProps {
  task: QueueTask;
  onClose(): void;
}

export function TaskDetailsDialog({ task, onClose }: TaskDetailsDialogProps) {
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    closeRef.current?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', closeForEscape);
    return () => document.removeEventListener('keydown', closeForEscape);
  }, [onClose]);

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
    <section className="task-details-dialog" role="dialog" aria-modal="true" aria-label="任务详情">
      <button ref={closeRef} className="dialog-close" aria-label="关闭任务详情" onClick={onClose}>×</button>
      <TaskDetails task={task} />
    </section>
  </div>;
}
