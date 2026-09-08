import { useState } from 'react';
import { QueueTask, TaskStatus } from '../lib/types';
import { formatBytes, formatSpeed, hasKnownTotal, transferProgress } from '../lib/downloadEvents';

interface DownloadQueueProps {
  tasks: readonly QueueTask[];
  selectedTaskId: string | null;
  onSelect(id: string): void;
  onAction(id: string, action: 'pause' | 'resume' | 'cancel' | 'retry' | 'select' | 'skip'): void;
  onConfirm(task: QueueTask): void;
}

const statusText: Record<TaskStatus, string> = {
  draft: '草稿', parsing: '解析中', awaiting_user: '等待确认', queued: '等待下载', downloading: '下载中',
  processing: '处理中', completed: '已完成', failed: '失败', cancelled: '已取消', paused: '已暂停'
};

export function DownloadQueue({ tasks, selectedTaskId, onSelect, onAction, onConfirm }: DownloadQueueProps) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  return (
    <section className="download-queue" aria-label="下载队列">
      <div className="queue-heading"><span>任务</span><span>进度</span><span>状态</span></div>
      {tasks.length === 0 ? <p className="queue-empty">还没有下载任务</p> : tasks.map((task) => {
        const isExpanded = expanded[task.id] ?? false;
        const taskChildren = task.children?.length ? task.children : task.files?.map((file) => ({
          id: `${task.id}-file-${file.index}`, name: file.path, kind: 'file' as const, status: task.status,
          progress: file.length > 0 ? Math.min(100, Math.floor(file.completed_length * 100 / file.length)) : 0,
          file_index: file.index, selected: file.selected, size: `${formatBytes(file.completed_length)} / ${formatBytes(file.length)}`
        }));
        const hasChildren = Boolean(taskChildren?.length);
        const isBitTorrent = task.kind === 'magnet' || task.kind === 'torrent';
        const awaitsBitTorrentConfirmation = isBitTorrent && task.status === 'awaiting_user';
        return <div className="task-group" key={task.id}>
          <article className={`task-row ${selectedTaskId === task.id ? 'selected' : ''}`} onClick={() => onSelect(task.id)}>
            <div className="task-name">
              {hasChildren && <button className="expand-button" aria-label={`${isExpanded ? '收起' : '展开'} ${task.name}`} onClick={(event) => { event.stopPropagation(); setExpanded((current) => ({ ...current, [task.id]: !isExpanded })); }}>{isExpanded ? '⌄' : '›'}</button>}
              <span className={`kind-icon ${task.kind}`}>{task.kind === 'folder' ? '□' : '↓'}</span>
              <span><b>{task.name}</b><small>{task.size ?? task.source ?? '本地任务'}</small></span>
            </div>
            <div className={`task-progress${hasKnownTotal(task) ? '' : ' unknown-total'}`}><div>{hasKnownTotal(task) && <i style={{ width: `${task.progress}%` }} />}</div><span>{hasKnownTotal(task) ? `${task.progress}% · ${transferProgress(task)}` : transferProgress(task)} {task.download_speed !== undefined ? `· ${formatSpeed(task.download_speed)}` : task.speed && `· ${task.speed}`}</span></div>
            <div className="task-actions"><em className={task.status}>{statusText[task.status]}</em>
              {awaitsBitTorrentConfirmation && <button aria-label={`确认下载 ${task.name}`} onClick={(event) => { event.stopPropagation(); onConfirm(task); }}>确认下载</button>}
              {(task.status === 'downloading' || (task.status === 'queued' && task.aria_gid)) && <button aria-label={`暂停 ${task.name}`} onClick={(event) => { event.stopPropagation(); onAction(task.id, 'pause'); }}>Ⅱ</button>}
              {task.status === 'paused' && <button aria-label={`继续 ${task.name}`} onClick={(event) => { event.stopPropagation(); onAction(task.id, 'resume'); }}>▶</button>}
              {(task.status === 'failed' || task.status === 'cancelled') && <button aria-label={`重试 ${task.name}`} onClick={(event) => { event.stopPropagation(); onAction(task.id, 'retry'); }}>重试</button>}
              {task.status !== 'completed' && <button aria-label={`取消 ${task.name}`} onClick={(event) => { event.stopPropagation(); onAction(task.id, 'cancel'); }}>×</button>}
            </div>
          </article>
          {hasChildren && isExpanded && <div className="children">{taskChildren!.map((child) => <article className="child-row" key={child.id}>
            <span>{child.name}</span><span>{child.size ?? `${child.progress}%`}</span><em>{isBitTorrent ? (child.selected ? '已选择' : '已跳过') : statusText[child.status]}</em>
            {!awaitsBitTorrentConfirmation && (isBitTorrent ? <button aria-label={`${child.selected ? '跳过此文件' : '下载此文件'} ${child.name}`} onClick={() => onAction(child.id, child.selected ? 'skip' : 'select')}>{child.selected ? '跳过' : '下载'}</button> : child.status === 'downloading' && <button aria-label={`暂停 ${child.name}`} onClick={() => onAction(child.id, 'pause')}>Ⅱ</button>)}
          </article>)}</div>}
        </div>;
      })}
    </section>
  );
}
