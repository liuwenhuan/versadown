import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { QueueTask, TaskCommand, TaskStatus } from '../lib/types';
import { formatEta, formatSpeed } from '../lib/downloadEvents';

interface DownloadQueueProps {
  tasks: readonly QueueTask[];
  selectedTaskId: string | null;
  onSelect(id: string): void;
  onCommand(id: string, command: TaskCommand): void;
  onConfirm?(task: QueueTask): void;
}

const statusText: Record<TaskStatus, string> = {
  draft: '草稿', parsing: '解析中', awaiting_user: '等待确认', queued: '等待下载', downloading: '下载中',
  processing: '处理中', completed: '已完成', failed: '失败', cancelled: '已取消', paused: '已暂停'
};

function magnetName(value: string): string {
  try {
    const query = value.slice(value.indexOf('?') + 1);
    const params = new URLSearchParams(query);
    const displayName = params.get('dn')?.trim();
    if (displayName) return displayName;
    const hash = params.get('xt')?.split(':').pop();
    if (hash) return `磁力下载 ${hash.slice(0, 8)}`;
  } catch {
    // Fall through to the stable generic label for malformed legacy data.
  }
  return '磁力下载';
}

export function displayTaskName(task: QueueTask): string {
  const candidate = task.name?.trim();
  if (task.kind === 'magnet' && candidate.toLowerCase().startsWith('magnet:')) return magnetName(candidate);
  return candidate || '未命名下载任务';
}

function taskSubtitle(task: QueueTask): string {
  if (task.error) return task.error;
  if (task.statusMessage) return task.statusMessage;
  if (task.size) return task.size;
  const fileCount = task.children?.length ?? task.files?.length ?? 0;
  if (task.kind === 'magnet') return `磁力任务 · ${fileCount ? `${fileCount} 个文件` : '正在获取文件信息'}`;
  if (task.kind === 'torrent') return `BT 种子 · ${fileCount ? `${fileCount} 个文件` : '正在获取文件信息'}`;
  if (task.kind === 'folder') return `目录任务 · ${fileCount} 个文件`;
  if (task.kind === 'cloud') return '网盘下载任务';
  try { return task.source ? new URL(task.source).host : '本地下载任务'; } catch { return '本地下载任务'; }
}

function TaskKindIcon({ kind }: { kind: QueueTask['kind'] }) {
  if (kind === 'folder') return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M3.5 7.5h6l1.7 2h9.3v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.2l1.7 2h6.7a2.2 2.2 0 0 1 2.2 2.2v1.8" /></svg>;
  if (kind === 'magnet' || kind === 'torrent') return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M7 4v8a5 5 0 0 0 10 0V4" /><path d="M7 8h4M13 8h4M7 4h4M13 4h4" /></svg>;
  return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M12 3v12" /><path d="m7.5 11 4.5 4.5 4.5-4.5" /><path d="M5 20h14" /></svg>;
}

function ActionIcon({ action }: { action: 'pause' | 'resume' | 'cancel' }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24">{action === 'pause' ? <><path d="M8 5v14" /><path d="M16 5v14" /></> : action === 'resume' ? <path d="m9 6 9 6-9 6z" /> : <><path d="m7 7 10 10" /><path d="m17 7-10 10" /></>}</svg>;
}

interface ContextMenuState { task: QueueTask; x: number; y: number }

function contextCommands(task: QueueTask): Array<{ command: TaskCommand; label: string; danger?: boolean; separated?: boolean }> {
  const commands: Array<{ command: TaskCommand; label: string; danger?: boolean; separated?: boolean }> = [];
  if (task.status === 'downloading') commands.push({ command: 'pause', label: '暂停下载' });
  if (task.status === 'paused') commands.push({ command: 'resume', label: '继续下载' });
  if (task.status === 'failed' || task.status === 'cancelled') commands.push({ command: 'retry', label: '重新下载' });
  commands.push({ command: 'open_directory', label: '打开下载目录', separated: commands.length > 0 });
  commands.push({ command: 'details', label: '查看任务详情' });
  commands.push({ command: 'copy_link', label: '复制下载链接' });
  if (['downloading', 'paused', 'queued', 'awaiting_user'].includes(task.status)) commands.push({ command: 'cancel', label: '取消任务', danger: true, separated: true });
  if (['completed', 'failed', 'cancelled'].includes(task.status)) commands.push({ command: 'remove_record', label: '移除任务记录…', danger: true, separated: true });
  return commands;
}

function MenuIcon({ command }: { command: TaskCommand }) {
  const paths: Record<TaskCommand, React.ReactNode> = {
    pause: <><path d="M8 5v14" /><path d="M16 5v14" /></>, resume: <path d="m9 6 9 6-9 6z" />,
    cancel: <><circle cx="12" cy="12" r="8" /><path d="m9 9 6 6M15 9l-6 6" /></>, retry: <><path d="M4.5 10a8 8 0 1 1 1.8 6" /><path d="M4.5 5v5h5" /></>,
    open_directory: <><path d="M3.5 7.5h6l1.7 2h9.3v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.2l1.7 2h6.7" /></>,
    copy_link: <><rect x="8" y="8" width="11" height="11" rx="2" /><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" /></>,
    details: <><circle cx="12" cy="12" r="8" /><path d="M12 11v5M12 8h.01" /></>, remove_record: <><path d="M5 7h14M9 7V4h6v3M8 10v7M12 10v7M16 10v7M6.5 7l.8 13h9.4l.8-13" /></>,
  };
  return <svg aria-hidden="true" viewBox="0 0 24 24">{paths[command]}</svg>;
}

function TaskContextMenu({ state, onClose, onCommand }: { state: ContextMenuState; onClose(): void; onCommand(command: TaskCommand): void }) {
  const menuRef = useRef<HTMLDivElement>(null);
  const commands = contextCommands(state.task);
  const width = 224;
  const estimatedHeight = commands.length * 38 + 20;
  const left = Math.max(8, Math.min(state.x, window.innerWidth - width - 8));
  const top = Math.max(8, Math.min(state.y, window.innerHeight - estimatedHeight - 8));

  useEffect(() => {
    menuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
    const closeForPointer = (event: PointerEvent) => { if (!menuRef.current?.contains(event.target as Node)) onClose(); };
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    const close = () => onClose();
    document.addEventListener('pointerdown', closeForPointer);
    document.addEventListener('keydown', closeForEscape);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    window.addEventListener('scroll', close, true);
    return () => {
      document.removeEventListener('pointerdown', closeForPointer);
      document.removeEventListener('keydown', closeForEscape);
      window.removeEventListener('blur', close);
      window.removeEventListener('resize', close);
      window.removeEventListener('scroll', close, true);
    };
  }, [onClose]);

  function moveFocus(event: React.KeyboardEvent<HTMLDivElement>) {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
    if (!items.length) return;
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : event.key === 'ArrowDown' ? (current + 1) % items.length : (current - 1 + items.length) % items.length;
    items[next].focus();
  }

  return createPortal(<div ref={menuRef} className="task-context-menu" role="menu" aria-label={`${displayTaskName(state.task)} 任务菜单`} style={{ left, top }} onKeyDown={moveFocus}>
    <header><b title={displayTaskName(state.task)}>{displayTaskName(state.task)}</b><small>{statusText[state.task.status]}</small></header>
    {commands.map((item) => <div key={item.command} className={item.separated ? 'menu-separated' : undefined}><button role="menuitem" className={item.danger ? 'danger' : undefined} onClick={() => onCommand(item.command)}><MenuIcon command={item.command} /><span>{item.label}</span></button></div>)}
  </div>, document.body);
}

export function DownloadQueue({ tasks, selectedTaskId, onSelect, onCommand, onConfirm }: DownloadQueueProps) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const closeContextMenu = () => setContextMenu(null);
  function runCommand(taskId: string, command: TaskCommand) { closeContextMenu(); onCommand(taskId, command); }
  return (
    <section className="download-queue" aria-label="下载队列" onContextMenu={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
      <div className="queue-heading"><span>任务</span><span>进度</span><span>状态</span></div>
      {tasks.length === 0 ? <p className="queue-empty">还没有下载任务</p> : tasks.map((task) => {
        const isExpanded = expanded[task.id] ?? false;
        const taskChildren = task.children?.length ? task.children : task.files?.map((file) => ({
          id: `${task.id}-file-${file.index}`,
          name: file.path,
          kind: 'file' as const,
          status: task.status,
          progress: file.length > 0 ? Math.min(100, Math.floor(file.completed_length * 100 / file.length)) : 0,
          size: file.length > 0 ? `${file.completed_length} B / ${file.length} B` : undefined,
          file_index: file.index,
          selected: file.selected,
        }));
        const hasChildren = Boolean(taskChildren?.length);
        const taskName = displayTaskName(task);
        const speed = task.speed ?? formatSpeed(task.speedBytes ?? task.download_speed);
        const eta = task.eta ?? (task.eta_seconds !== undefined ? formatEta(task.eta_seconds) : undefined);
        return <div className="task-group" key={task.id}>
          <article className={`task-row ${selectedTaskId === task.id ? 'selected' : ''}`} onClick={() => onSelect(task.id)} onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); onSelect(task.id); setContextMenu({ task, x: event.clientX, y: event.clientY }); }}>
            <div className="task-name">
              <span className="expand-slot">{hasChildren && <button className="expand-button" aria-label={`${isExpanded ? '收起' : '展开'} ${taskName}`} onClick={(event) => { event.stopPropagation(); setExpanded((current) => ({ ...current, [task.id]: !isExpanded })); }}><svg aria-hidden="true" viewBox="0 0 20 20"><path d="m7 4 6 6-6 6" /></svg></button>}</span>
              <span className={`kind-icon ${task.kind}`}><TaskKindIcon kind={task.kind} /></span>
              <span className="task-copy"><b title={taskName}>{taskName}</b><small className={task.error ? 'task-error' : ''} title={taskSubtitle(task)}>{taskSubtitle(task)}</small></span>
            </div>
            <div className="task-progress"><div className="progress-track" role="progressbar" aria-label={`${taskName} 下载进度`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={task.progress}><i style={{ width: `${task.progress}%` }} /></div><div className="transfer-metrics"><strong>{task.progress}%</strong><span>{speed}</span>{eta && <span>剩余 {eta}</span>}<span>{task.connections ?? 0} 个连接</span></div></div>
            <div className="task-actions"><em className={task.status}>{statusText[task.status]}</em>
              {task.status === 'awaiting_user' && (task.kind === 'magnet' || task.kind === 'torrent') && onConfirm && <button aria-label={`确认下载 ${taskName}`} title="确认下载内容" onClick={(event) => { event.stopPropagation(); onConfirm(task); }}>确认</button>}
              {task.status === 'downloading' && <button aria-label={`暂停 ${taskName}`} title="暂停" onClick={(event) => { event.stopPropagation(); onCommand(task.id, 'pause'); }}><ActionIcon action="pause" /></button>}
              {task.status === 'paused' && <button aria-label={`继续 ${taskName}`} title="继续" onClick={(event) => { event.stopPropagation(); onCommand(task.id, 'resume'); }}><ActionIcon action="resume" /></button>}
              {!['completed', 'cancelled'].includes(task.status) && <button aria-label={`取消 ${taskName}`} title="取消" onClick={(event) => { event.stopPropagation(); onCommand(task.id, 'cancel'); }}><ActionIcon action="cancel" /></button>}
            </div>
          </article>
          {hasChildren && isExpanded && <div className="children">{taskChildren!.map((child) => <article className="child-row" key={child.id}>
            <span className="child-name"><i aria-hidden="true">↳</i><b title={child.name}>{child.name}</b></span>
            <span className="child-progress"><i><b style={{ width: `${child.progress}%` }} /></i><small>{child.progress}%{child.size && ` · ${child.size}`}</small></span>
            <em className={child.status}>{statusText[child.status]}</em>
          </article>)}</div>}
        </div>;
      })}
      {contextMenu && <TaskContextMenu state={contextMenu} onClose={closeContextMenu} onCommand={(command) => runCommand(contextMenu.task.id, command)} />}
    </section>
  );
}
