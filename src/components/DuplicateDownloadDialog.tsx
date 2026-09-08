import { useEffect, useRef } from 'react';
import type { DuplicateDownloadState, QueueTask } from '../lib/types';

interface DuplicateDownloadDialogProps {
  task: QueueTask;
  state: DuplicateDownloadState;
  onOpenDirectory(): void;
  onResolve(action: 'resume' | 'overwrite'): void;
  onRestartMissing(): void;
  onClose(): void;
}

const content = {
  existing_active: {
    text: '这个内容已经在下载队列中，不能重复创建任务。',
    action: '查看现有任务',
  },
  resume_available: {
    text: '检测到上次未完成的下载，已有文件可以续传。',
    action: '继续下载',
  },
  completed_files_present: {
    text: '这个内容已经下载完成，下载文件仍然存在。',
    action: '打开所在目录',
  },
  completed_files_missing: {
    text: '已经下载过，但下载文件不存在，将重新进行下载。',
    action: '重新下载',
  },
  overwrite_required: {
    text: '无法安全续传。继续会覆盖之前的下载内容。',
    action: '覆盖并重新下载',
  },
} satisfies Record<DuplicateDownloadState, { text: string; action: string }>;

export function DuplicateDownloadDialog({ task, state, onOpenDirectory, onResolve, onRestartMissing, onClose }: DuplicateDownloadDialogProps) {
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

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <section className="duplicate-download-dialog" role="dialog" aria-modal="true" aria-labelledby="duplicate-download-title">
      <div className="dialog-warning" aria-hidden="true">!</div>
      <div>
        <h2 id="duplicate-download-title">检测到重复下载</h2>
        <p>“{task.name}”</p>
        <small>{details.text}</small>
      </div>
      <footer>
        <button ref={closeRef} onClick={onClose}>取消</button>
        <button className={state === 'overwrite_required' ? 'danger-button' : 'primary'} onClick={performAction}>{details.action}</button>
      </footer>
    </section>
  </div>;
}
