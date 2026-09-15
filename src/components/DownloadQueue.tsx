import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { QueueTask, TaskCommand } from '../lib/types';
import { formatBytes, formatEta, formatSpeed, transferProgress } from '../lib/downloadEvents';

interface DownloadQueueProps {
  tasks: readonly QueueTask[];
  selectedTaskId: string | null;
  onSelect(id: string): void;
  onCommand(id: string, command: TaskCommand): void;
  onBatchCommand?(ids: readonly string[], command: BatchTaskCommand): void;
  onFileSelection?(taskId: string, fileIndex: number, selected: boolean): void;
  onFileCancel?(taskId: string, fileIndex: number): void;
  onConfirm?(task: QueueTask): void;
}

export type BatchTaskCommand = 'resume' | 'pause' | 'delete' | 'copy_link';

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

function childPathParts(value: string): string[] {
  const parts = value.trim().replace(/\\/g, '/').replace(/\/{2,}/g, '/').split('/').filter(Boolean);
  return /^[a-z]:$/i.test(parts[0] ?? '') ? parts.slice(1) : parts;
}

function sharedChildRoot(children: readonly QueueTask[]): string | undefined {
  const paths = children.map((child) => childPathParts(child.name));
  if (!paths.length || paths.some((parts) => parts.length < 2)) return undefined;
  const root = paths[0][0];
  return paths.every((parts) => parts[0] === root) ? root : undefined;
}

function displayChildName(child: QueueTask, root: string | undefined): string {
  const parts = childPathParts(child.name);
  const display = root && parts[0] === root ? parts.slice(1) : parts;
  return display.join('/') || child.name;
}

const playableMediaExtensions = new Set(['3gp', 'asf', 'avi', 'flv', 'm2ts', 'm4v', 'mkv', 'mov', 'mp4', 'mpeg', 'mpg', 'mts', 'ts', 'webm', 'wmv', 'aac', 'aiff', 'alac', 'ape', 'flac', 'm4a', 'mka', 'mp3', 'oga', 'ogg', 'opus', 'wav', 'wma']);
type AssociatedFileType = 'text' | 'image' | 'pdf' | 'office';

const associatedFileExtensions: Record<AssociatedFileType, ReadonlySet<string>> = {
  text: new Set(['cfg', 'conf', 'csv', 'css', 'htm', 'html', 'ini', 'json', 'log', 'md', 'markdown', 'rst', 'text', 'toml', 'tsv', 'txt', 'xml', 'yaml', 'yml']),
  image: new Set(['avif', 'bmp', 'gif', 'heic', 'ico', 'jpeg', 'jpg', 'png', 'svg', 'tif', 'tiff', 'webp']),
  pdf: new Set(['pdf']),
  office: new Set(['doc', 'docm', 'docx', 'dot', 'dotm', 'dotx', 'odp', 'ods', 'odt', 'pot', 'potm', 'potx', 'pps', 'ppsm', 'ppsx', 'ppt', 'pptm', 'pptx', 'rtf', 'wps', 'xls', 'xlsb', 'xlsm', 'xlsx', 'xlt', 'xltm', 'xltx']),
};

const associatedFileLabels: Record<AssociatedFileType, string> = {
  text: '打开文本',
  image: '打开图片',
  pdf: '打开 PDF',
  office: '打开 Office 文档',
};

function hasPlayableMediaExtension(path: string): boolean {
  const extension = path.trim().split(/[?#]/, 1)[0].split('.').pop()?.toLowerCase();
  return extension !== undefined && playableMediaExtensions.has(extension);
}

function associatedFileType(path: string): AssociatedFileType | undefined {
  const basename = path.trim().replace(/\\/g, '/').split('/').at(-1);
  const extension = basename?.split('.').pop()?.toLowerCase();
  if (!extension) return undefined;
  return (Object.keys(associatedFileExtensions) as AssociatedFileType[]).find((type) => associatedFileExtensions[type].has(extension));
}

function taskAssociatedFileType(task: QueueTask): AssociatedFileType | undefined {
  if (task.kind === 'folder' || task.children?.length) return undefined;
  const files = task.files?.filter((file) => file.selected !== false) ?? [];
  return files.length === 1 ? associatedFileType(files[0].path) : undefined;
}

export function taskHasPlayableMedia(task: QueueTask): boolean {
  const files = task.files?.filter((file) => file.selected !== false) ?? [];
  if (files.length) return files.some((file) => hasPlayableMediaExtension(file.path));
  return task.kind !== 'folder' && hasPlayableMediaExtension(task.name);
}

export function completedTaskSize(task: QueueTask): string | undefined {
  const files = task.files?.filter((file) => file.selected !== false) ?? [];
  if (files.length) {
    const downloaded = files.reduce((total, file) => total + (file.completed_length > 0 ? file.completed_length : file.length), 0);
    return downloaded > 0 ? formatBytes(downloaded) : undefined;
  }
  const downloaded = task.completed_length && task.completed_length > 0
    ? task.completed_length
    : task.total_length && task.total_length > 0 ? task.total_length : undefined;
  return downloaded !== undefined ? formatBytes(downloaded) : undefined;
}

export function completedAt(completedAt: number | undefined): string | undefined {
  if (!Number.isFinite(completedAt) || !completedAt || completedAt <= 0) return undefined;
  const date = new Date(completedAt < 1_000_000_000_000 ? completedAt * 1000 : completedAt);
  if (Number.isNaN(date.getTime())) return undefined;
  const pad = (value: number) => String(value).padStart(2, '0');
  return `完成于 ${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function childFileSize(task: QueueTask): string {
  const selectedFiles = task.files?.filter((file) => file.selected !== false);
  const completed = task.completed_length ?? (selectedFiles?.length
    ? selectedFiles.reduce((total, file) => total + file.completed_length, 0) : undefined);
  const knownTotal = task.total_length && task.total_length > 0 ? task.total_length
    : selectedFiles?.reduce((total, file) => total + file.length, 0);
  if (task.status !== 'completed' && completed !== undefined) {
    return `${formatBytes(completed)} / ${knownTotal && knownTotal > 0 ? formatBytes(knownTotal) : '未知'}`;
  }
  if (typeof task.total_length === 'number' && task.total_length > 0) return formatBytes(task.total_length);
  const fileSize = task.files
    ?.filter((file) => file.selected !== false)
    .reduce((total, file) => total + Math.max(file.length, file.completed_length), 0);
  if (fileSize && fileSize > 0) return formatBytes(fileSize);
  if (task.status === 'completed' && task.completed_length && task.completed_length > 0) return formatBytes(task.completed_length);
  return task.size || '—';
}

function taskSubtitle(task: QueueTask): string | undefined {
  if (task.error) return task.error;
  if (task.statusMessage) return task.statusMessage;
  const fileCount = task.children?.length ?? task.files?.length ?? 0;
  if (task.kind === 'magnet') return `磁力任务 · ${fileCount ? `${fileCount} 个文件` : '正在获取文件信息'}`;
  if (task.kind === 'torrent') return `BT 种子 · ${fileCount ? `${fileCount} 个文件` : '正在获取文件信息'}`;
  if (task.kind === 'ed2k') return 'ED2K 下载';
  if (task.kind === 'ftp') return 'FTP 下载';
  if (task.kind === 'hls') return 'HLS 流媒体下载';
  if (task.kind === 'dash') return 'MPEG-DASH 流媒体下载';
  return undefined;
}

function taskPeerMetric(task: QueueTask): string | undefined {
  if (task.status !== 'downloading') return undefined;
  const connections = task.connections ?? 0;
  if (task.kind === 'magnet' || task.kind === 'torrent') {
    return '下载源 ' + connections + '/' + (task.discovered_nodes ?? 0);
  }
  return task.kind === 'ed2k' ? '已连接 ' + connections : undefined;
}

function TaskKindIcon({ kind }: { kind: QueueTask['kind'] }) {
  if (kind === 'folder') return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M3.5 7.5h6l1.7 2h9.3v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.2l1.7 2h6.7a2.2 2.2 0 0 1 2.2 2.2v1.8" /></svg>;
  if (kind === 'magnet' || kind === 'torrent') return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M7 4v8a5 5 0 0 0 10 0V4" /><path d="M7 8h4M13 8h4M7 4h4M13 4h4" /></svg>;
  if (kind === 'ed2k') return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M8.5 8.5 5 12l3.5 3.5M15.5 8.5 19 12l-3.5 3.5" /><path d="M10 17 14 7" /></svg>;
  if (kind === 'hls' || kind === 'dash') return <svg aria-hidden="true" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5" /><path d="m10 8 5 4-5 4z" /></svg>;
  return <svg aria-hidden="true" viewBox="0 0 24 24"><path d="M12 3v12" /><path d="m7.5 11 4.5 4.5 4.5-4.5" /><path d="M5 20h14" /></svg>;
}

function ActionIcon({ action }: { action: 'pause' | 'resume' | 'cancel' | 'retry' | 'reauthorize' | 'play' | 'folder' | AssociatedFileType | 'more' }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24">{action === 'pause'
    ? <><path d="M8 5v14" /><path d="M16 5v14" /></>
    : action === 'resume' ? <path d="m9 6 9 6-9 6z" />
      : action === 'cancel' ? <><circle cx="12" cy="12" r="8" /><path d="m9 9 6 6M15 9l-6 6" /></>
        : action === 'retry' || action === 'reauthorize' ? <><path d="M4.5 10a8 8 0 1 1 1.8 6" /><path d="M4.5 5v5h5" /></>
          : action === 'play' ? <><circle cx="12" cy="12" r="8.5" /><path d="m10 8 5 4-5 4z" /></>
            : action === 'folder' ? <><path d="M3.5 7.5h6l1.7 2h9.3v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.2l1.7 2h6.7" /></>
              : action === 'text' ? <><path d="M7 3.5h7l3 3V20.5H7z" /><path d="M14 3.5v3h3M9.5 11h5M9.5 14h5M9.5 17h3" /></>
                : action === 'image' ? <><rect x="3.5" y="5" width="17" height="14" rx="2" /><circle cx="9" cy="10" r="1.5" /><path d="m5.5 17 4.5-4 3.5 3 2.5-2 2.5 3" /></>
                  : action === 'pdf' ? <><path d="M7 3.5h7l3 3V20.5H7z" /><path d="M14 3.5v3h3M9.5 16.5h5M9.5 12h5" /></>
                    : action === 'office' ? <><path d="M7 3.5h7l3 3V20.5H7z" /><path d="M14 3.5v3h3M9.5 11h5M9.5 15h5M12 9v8" /></>
                      : <><circle cx="5" cy="12" r="1.2" /><circle cx="12" cy="12" r="1.2" /><circle cx="19" cy="12" r="1.2" /></>}</svg>;
}

type TaskMenuCommand = TaskCommand | 'delete';

interface ContextMenuState { tasks: readonly QueueTask[]; x: number; y: number }

interface TaskContextMenuPositionInput {
  x: number;
  y: number;
  menuWidth: number;
  menuHeight: number;
  viewportWidth: number;
  viewportHeight: number;
}

export function taskContextMenuPosition({ x, y, menuWidth, menuHeight, viewportWidth, viewportHeight }: TaskContextMenuPositionInput) {
  const inset = 8;
  return {
    left: Math.max(inset, Math.min(x, viewportWidth - menuWidth - inset)),
    top: Math.max(inset, Math.min(y, viewportHeight - menuHeight - inset)),
  };
}

function contextCommands(task: QueueTask): Array<{ command: TaskCommand; label: string; danger?: boolean; separated?: boolean }> {
  const commands: Array<{ command: TaskCommand; label: string; danger?: boolean; separated?: boolean }> = [];
  if (task.status === 'downloading') commands.push({ command: 'pause', label: '暂停下载' });
  if (task.status === 'paused') commands.push({ command: 'resume', label: '继续下载' });
  if (task.status === 'failed') commands.push(task.reauthorization_required
    ? { command: 'reauthorize', label: '重新授权' }
    : { command: 'retry', label: '重新下载' });
  if (task.status === 'completed' && task.cleanup_pending) commands.push({ command: 'retry_cleanup', label: '重试网盘清理' });
  commands.push({ command: 'open_directory', label: '打开下载目录', separated: commands.length > 0 });
  commands.push({ command: 'details', label: '查看任务详情' });
  if (task.source) commands.push({ command: 'copy_link', label: '复制下载链接' });
  if (['draft', 'parsing', 'awaiting_user', 'queued', 'downloading', 'processing', 'paused'].includes(task.status)) commands.push({ command: 'cancel', label: '取消任务', danger: true, separated: true });
  if (task.status === 'recycled' || task.status === 'cancelled') commands.push({ command: 'remove_record', label: '彻底删除…', danger: true, separated: true });
  if (['completed', 'failed'].includes(task.status)) commands.push({ command: 'move_to_recycle', label: '移入回收站', danger: true, separated: true });
  return commands;
}

function batchContextCommands(tasks: readonly QueueTask[]): Array<{ command: TaskMenuCommand; label: string; danger?: boolean; separated?: boolean }> {
  const commands: Array<{ command: TaskMenuCommand; label: string; danger?: boolean; separated?: boolean }> = [];
  if (tasks.some((task) => task.status === 'paused' || task.status === 'failed')) commands.push({ command: 'resume', label: '开始下载' });
  if (tasks.some((task) => task.status === 'downloading')) commands.push({ command: 'pause', label: '暂停下载' });
  if (tasks.some((task) => Boolean(task.source))) commands.push({ command: 'copy_link', label: '复制下载链接', separated: commands.length > 0 });
  commands.push({ command: 'delete', label: '删除', danger: true, separated: true });
  return commands;
}

function MenuIcon({ command }: { command: TaskMenuCommand }) {
  const deletePath = <><path d="M5 7h14M9 7V4h6v3M8 10v7M12 10v7M16 10v7M6.5 7l.8 13h9.4l.8-13" /></>;
  const paths: Record<TaskMenuCommand, React.ReactNode> = {
    pause: <><path d="M8 5v14" /><path d="M16 5v14" /></>, resume: <path d="m9 6 9 6-9 6z" />,
    cancel: <><circle cx="12" cy="12" r="8" /><path d="m9 9 6 6M15 9l-6 6" /></>, retry: <><path d="M4.5 10a8 8 0 1 1 1.8 6" /><path d="M4.5 5v5h5" /></>,
    reauthorize: <><path d="M4.5 10a8 8 0 1 1 1.8 6" /><path d="M4.5 5v5h5" /></>,
    retry_cleanup: <><path d="M4.5 10a8 8 0 1 1 1.8 6" /><path d="M4.5 5v5h5" /></>,
    open_directory: <><path d="M3.5 7.5h6l1.7 2h9.3v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.2l1.7 2h6.7" /></>,
    open_file: <><path d="M7 3.5h7l3 3V20.5H7z" /><path d="M14 3.5v3h3M9.5 12h5M9.5 16h5" /></>,
    play_videos: <><circle cx="12" cy="12" r="8.5" /><path d="m10 8 5 4-5 4z" /></>,
    copy_link: <><rect x="8" y="8" width="11" height="11" rx="2" /><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" /></>,
    details: <><circle cx="12" cy="12" r="8" /><path d="M12 11v5M12 8h.01" /></>,
    move_to_recycle: <><path d="M5 7h14M9 7V4h6v3M8 10v7M12 10v7M16 10v7M6.5 7l.8 13h9.4l.8-13" /><path d="M18.5 4.5 20 6" /></>,
    remove_record: <><path d="M5 7h14M9 7V4h6v3M8 10v7M12 10v7M16 10v7M6.5 7l.8 13h9.4l.8-13" /><path d="m7.5 7.5 9 9M16.5 7.5l-9 9" /></>,
    delete: deletePath,
  };
  return <svg aria-hidden="true" viewBox="0 0 24 24">{paths[command]}</svg>;
}

function TaskContextMenu({ state, onClose, onCommand }: { state: ContextMenuState; onClose(): void; onCommand(command: TaskMenuCommand): void }) {
  const menuRef = useRef<HTMLDivElement>(null);
  const isBatch = state.tasks.length > 1;
  const commands = isBatch ? batchContextCommands(state.tasks) : contextCommands(state.tasks[0]);
  const menuTitle = isBatch ? `已选择 ${state.tasks.length} 个任务` : displayTaskName(state.tasks[0]);
  const [menuSize, setMenuSize] = useState({ width: 224, height: commands.length * 38 + 64 });
  const { left, top } = taskContextMenuPosition({
    x: state.x,
    y: state.y,
    menuWidth: menuSize.width,
    menuHeight: menuSize.height,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
  });

  useLayoutEffect(() => {
    const rect = menuRef.current?.getBoundingClientRect();
    if (!rect?.width || !rect.height) return;
    setMenuSize((current) => current.width === rect.width && current.height === rect.height
      ? current
      : { width: rect.width, height: rect.height });
  }, [state.x, state.y, commands.length]);

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

  return createPortal(<div ref={menuRef} className="task-context-menu" role="menu" aria-label={`${menuTitle} 任务菜单`} style={{ left, top }} onKeyDown={moveFocus}>
    <header><b title={menuTitle}>{menuTitle}</b></header>
    {commands.map((item) => <div key={item.command} className={item.separated ? 'menu-separated' : undefined}><button role="menuitem" className={item.danger ? 'danger' : undefined} onClick={() => onCommand(item.command)}><MenuIcon command={item.command} /><span>{item.label}</span></button></div>)}
  </div>, document.body);
}

interface QueueTreeRowsProps {
  nodes: readonly QueueTask[];
  depth: number;
  expanded: Record<string, boolean>;
  onToggle(id: string): void;
  onCommand(id: string, command: TaskCommand): void;
  onFileSelection?: DownloadQueueProps['onFileSelection'];
  onFileCancel?: DownloadQueueProps['onFileCancel'];
  parentTaskId: string;
  isBtFileTree: boolean;
  completedRoot: boolean;
  childRoot?: string;
}

function QueueTreeRows({ nodes, depth, expanded, onToggle, onCommand, onFileSelection, onFileCancel, parentTaskId, isBtFileTree, completedRoot, childRoot }: QueueTreeRowsProps) {
  return <>{nodes.map((node) => {
    const nodeChildren = node.children ?? [];
    const hasChildren = nodeChildren.length > 0;
    const isFolder = node.kind === 'folder';
    const isExpanded = expanded[node.id] ?? false;
    const nodeName = isBtFileTree ? displayChildName(node, childRoot) : displayTaskName(node);
    const fileIsSelected = node.selected !== false;
    const selectionAction = node.status === 'paused' || !fileIsSelected ? 'resume' as const : 'pause' as const;
    const primaryAction = node.status === 'downloading' ? 'pause' as const
      : node.status === 'paused' ? 'resume' as const
        : node.status === 'failed' ? (node.reauthorization_required ? 'reauthorize' as const : 'retry' as const) : null;
    const isBtFile = isBtFileTree && node.file_index !== undefined;
    const canUseFileSelection = isBtFile && Boolean(onFileSelection) && ['downloading', 'paused', 'queued'].includes(node.status);
    const canUseTaskCommand = !isBtFile && !isFolder && Boolean(primaryAction);
    const canPlay = !isFolder && node.status === 'completed' && hasPlayableMediaExtension(node.name);
    const openableFileType = node.status === 'completed' ? taskAssociatedFileType(node) : undefined;
    const error = node.status === 'completed' ? undefined : node.error ?? (node.status === 'failed' ? node.statusMessage : undefined);
    const transferSize = childFileSize(node);
    const childSpeed = node.status === 'downloading'
      ? node.speed ?? (node.speedBytes !== undefined || node.download_speed !== undefined
        ? formatSpeed(node.speedBytes ?? node.download_speed) : undefined)
      : undefined;
    const isFileLeaf = !hasChildren && !isFolder;
    const completionTime = completedRoot && isFileLeaf && node.status === 'completed' ? completedAt(node.completed_at ?? node.completedAt) : undefined;
    const showsCompletionSummary = completedRoot && isFileLeaf && node.status === 'completed';
    return <div className="queue-tree-node" key={node.id}>
      <article className={`child-row queue-tree-row${hasChildren ? ' has-children' : ''}`} style={{ paddingLeft: `${21 + depth * 22}px` }}>
        <span className="child-name">
          {hasChildren && <button className="queue-tree-expand" aria-label={`${isExpanded ? '收起' : '展开'} ${nodeName}`} onClick={() => onToggle(node.id)}><svg aria-hidden="true" viewBox="0 0 20 20"><path d="m7 4 6 6-6 6" /></svg></button>}
          <b title={nodeName}>{nodeName}</b>
          {error && <small className="queue-tree-error" title={error}>{error}</small>}
        </span>
        {showsCompletionSummary
          ? <small className="child-completion-time" style={{ gridColumn: 'span 2' }}>{completionTime}</small>
          : <><span className="child-progress"><i aria-hidden="true"><b style={{ width: `${node.progress}%` }} /></i>{childSpeed !== undefined && <span className="child-speed" aria-label={`${nodeName} 下载速度`}>{childSpeed}</span>}</span><small className="child-progress-value">{node.progress}%</small></>}
        <small className="child-transfer-size" title={transferSize}>{transferSize}</small>
        {(canUseFileSelection || canUseTaskCommand || canPlay || openableFileType) && <span className="child-actions">
          {canPlay ? <button aria-label={`播放 ${nodeName}`} title="播放" onClick={(event) => { event.stopPropagation(); onCommand(node.id, 'play_videos'); }}><ActionIcon action="play" /></button> : <>
            {openableFileType ? <button aria-label={`${associatedFileLabels[openableFileType]} ${nodeName}`} title="使用系统默认应用打开" onClick={(event) => { event.stopPropagation(); onCommand(node.id, 'open_file'); }}><ActionIcon action={openableFileType} /></button> : <button aria-label={`${primaryAction === 'reauthorize' ? '重新授权' : primaryAction === 'retry' ? '重新下载' : selectionAction === 'pause' ? '暂停' : '继续'} ${nodeName}`} title={primaryAction === 'reauthorize' ? '重新授权' : primaryAction === 'retry' ? '重新下载' : `${selectionAction === 'pause' ? '暂停' : '继续'}此文件`} onClick={(event) => {
              event.stopPropagation();
              if (isBtFile) onFileSelection!(parentTaskId, node.file_index!, selectionAction === 'resume');
              else if (primaryAction) onCommand(node.id, primaryAction);
            }}><ActionIcon action={primaryAction === 'reauthorize' ? 'reauthorize' : primaryAction === 'retry' ? 'retry' : selectionAction} /></button>
            }
            {isBtFile && fileIsSelected && onFileCancel
              ? <button className="child-cancel" aria-label={`取消 ${nodeName} 下载`} title="取消此文件下载" onClick={(event) => { event.stopPropagation(); onFileCancel(parentTaskId, node.file_index!); }}><ActionIcon action="cancel" /></button>
              : !isBtFile && canUseTaskCommand && node.status !== 'failed'
                ? <button className="child-cancel" aria-label={`取消 ${nodeName} 下载`} title="取消此文件下载" onClick={(event) => { event.stopPropagation(); onCommand(node.id, 'cancel'); }}><ActionIcon action="cancel" /></button>
                : null}
          </>}
        </span>}
      </article>
      {hasChildren && isExpanded && <div className="queue-tree-children"><QueueTreeRows nodes={nodeChildren} depth={depth + 1} expanded={expanded} onToggle={onToggle} onCommand={onCommand} onFileSelection={onFileSelection} onFileCancel={onFileCancel} parentTaskId={parentTaskId} isBtFileTree={isBtFileTree} completedRoot={completedRoot} /></div>}
    </div>;
  })}</>;
}

export function DownloadQueue({ tasks, selectedTaskId, onSelect, onCommand, onBatchCommand, onFileSelection, onFileCancel, onConfirm }: DownloadQueueProps) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selectedTaskIds, setSelectedTaskIds] = useState<string[]>(() => selectedTaskId ? [selectedTaskId] : []);
  const selectionAnchor = useRef<string | null>(selectedTaskId);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const closeContextMenu = () => setContextMenu(null);
  useEffect(() => {
    const visibleIds = new Set(tasks.map((task) => task.id));
    setSelectedTaskIds((current) => {
      const visibleSelection = current.filter((id) => visibleIds.has(id));
      if (visibleSelection.length) return visibleSelection.length === current.length ? current : visibleSelection;
      if (selectedTaskId && visibleIds.has(selectedTaskId)) return [selectedTaskId];
      return current.length ? [] : current;
    });
  }, [selectedTaskId, tasks]);
  useEffect(() => {
    const selectAllTasks = (event: KeyboardEvent) => {
      if (event.defaultPrevented || !(event.ctrlKey || event.metaKey) || event.altKey || event.shiftKey
        || event.key.toLowerCase() !== 'a' || !tasks.length) return;
      const target = event.target instanceof Element ? event.target : document.activeElement;
      if (target?.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return;
      const modalOpen = [...document.querySelectorAll('[role="dialog"], [role="alertdialog"], [aria-modal="true"]')]
        .some(dialog => !dialog.closest('[hidden], [aria-hidden="true"]'));
      if (modalOpen) return;
      event.preventDefault();
      setSelectedTaskIds(tasks.map(task => task.id));
      selectionAnchor.current = tasks[0].id;
      setContextMenu(null);
      onSelect(tasks[0].id);
    };
    document.addEventListener('keydown', selectAllTasks);
    return () => document.removeEventListener('keydown', selectAllTasks);
  }, [tasks, onSelect]);
  function runCommand(command: TaskMenuCommand) {
    const targets = contextMenu?.tasks ?? [];
    closeContextMenu();
    if (targets.length > 1) {
      onBatchCommand?.(targets.map((task) => task.id), command as BatchTaskCommand);
      return;
    }
    if (targets[0] && command !== 'delete') onCommand(targets[0].id, command);
  }
  function selectTask(event: React.MouseEvent<HTMLElement>, task: QueueTask) {
    const currentIndex = tasks.findIndex((item) => item.id === task.id);
    let next: string[];
    if (event.shiftKey && selectionAnchor.current) {
      const anchorIndex = tasks.findIndex((item) => item.id === selectionAnchor.current);
      const start = Math.min(anchorIndex < 0 ? currentIndex : anchorIndex, currentIndex);
      const end = Math.max(anchorIndex < 0 ? currentIndex : anchorIndex, currentIndex);
      next = tasks.slice(start, end + 1).map((item) => item.id);
    } else if (event.ctrlKey || event.metaKey) {
      next = selectedTaskIds.includes(task.id)
        ? selectedTaskIds.filter((id) => id !== task.id)
        : [...selectedTaskIds, task.id];
      if (!next.length) next = [task.id];
      selectionAnchor.current = task.id;
    } else {
      next = [task.id];
      selectionAnchor.current = task.id;
    }
    setSelectedTaskIds(next);
    onSelect(next.at(-1) ?? task.id);
  }
  function openContextMenu(task: QueueTask, x: number, y: number) {
    const keepsSelection = selectedTaskIds.includes(task.id);
    const targets = keepsSelection ? tasks.filter((item) => selectedTaskIds.includes(item.id)) : [task];
    if (!keepsSelection) {
      setSelectedTaskIds([task.id]);
      selectionAnchor.current = task.id;
      onSelect(task.id);
    }
    setContextMenu({ tasks: targets, x, y });
  }
  return (
    <section className="download-queue" aria-label="下载队列" onContextMenu={(event) => { if (event.target === event.currentTarget) event.preventDefault(); }}>
      <div className="queue-heading"><span>任务</span><span>进度</span><span>状态</span></div>
      {tasks.length === 0 ? <p className="queue-empty">还没有下载任务</p> : tasks.map((task) => {
        const isExpanded = expanded[task.id] ?? false;
        const isCompleted = task.status === 'completed';
        const hasPersistentChildren = Boolean(task.children?.length);
        const isBtFileTree = !hasPersistentChildren && (task.kind === 'magnet' || task.kind === 'torrent') && Boolean(task.files?.length);
        const taskChildren = hasPersistentChildren ? task.children : isBtFileTree ? task.files!.map((file) => ({
          id: `${task.id}-file-${file.index}`,
          name: file.path,
          kind: 'file' as const,
          status: task.status,
          progress: file.length > 0 ? Math.min(100, Math.floor(file.completed_length * 100 / file.length)) : 0,
          size: file.length > 0 ? `${formatBytes(file.completed_length)} / ${formatBytes(file.length)}` : undefined,
          file_index: file.index,
          selected: file.selected,
        })) : undefined;
        const hasChildren = Boolean(taskChildren?.length)
          && (task.kind === 'folder' || taskChildren!.length > 1)
          && (!isCompleted || (task.kind === 'folder' && hasPersistentChildren));
        const childRoot = taskChildren ? sharedChildRoot(taskChildren) : undefined;
        const taskName = displayTaskName(task);
        const isPaused = task.status === 'paused';
        const speed = !isPaused ? task.speed ?? formatSpeed(task.speedBytes ?? task.download_speed) : undefined;
        const eta = !isPaused ? task.eta ?? (task.eta_seconds !== undefined ? formatEta(task.eta_seconds) : undefined) : undefined;
        const hasSpeed = !isPaused && (task.speed !== undefined || task.speedBytes !== undefined || task.download_speed !== undefined);
        const transferSize = task.total_length !== undefined || task.completed_length !== undefined ? transferProgress(task) : task.size;
        const subtitle = taskSubtitle(task);
        const peerMetric = taskPeerMetric(task);
        const primaryAction = task.status === 'downloading' ? 'pause' as const
          : task.status === 'paused' ? 'resume' as const
          : task.status === 'failed' ? (task.reauthorization_required ? 'reauthorize' as const : 'retry' as const) : null;
        const primaryLabel = primaryAction === 'pause' ? '暂停' : primaryAction === 'resume' ? '继续' : primaryAction === 'reauthorize' ? '重新授权' : '重新下载';
        const completedSize = isCompleted ? completedTaskSize(task) : undefined;
        const completionTime = isCompleted ? completedAt(task.completed_at ?? task.completedAt) : undefined;
        const completedHasPlayableMedia = isCompleted && taskHasPlayableMedia(task);
        const completedFileType = isCompleted ? taskAssociatedFileType(task) : undefined;
        return <div className={`task-group${isCompleted ? ' completed-task-group' : ''}${isExpanded ? ' is-expanded' : ''}`} key={task.id}>
          <article data-task-id={task.id} className={`task-row ${isCompleted ? 'completed-task' : ''} ${hasChildren ? 'has-children' : ''} ${selectedTaskIds.includes(task.id) ? 'selected' : ''}`} aria-selected={selectedTaskIds.includes(task.id)} onClick={(event) => selectTask(event, task)} onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); openContextMenu(task, event.clientX, event.clientY); }}>
            <div className="task-name">
              <span className="expand-slot">{hasChildren && <button className="expand-button" aria-label={`${isExpanded ? '收起' : '展开'} ${taskName}`} onClick={(event) => { event.stopPropagation(); setExpanded((current) => ({ ...current, [task.id]: !isExpanded })); }}><svg aria-hidden="true" viewBox="0 0 20 20"><path d="m7 4 6 6-6 6" /></svg></button>}</span>
              <span className={`kind-icon ${task.kind}`}><TaskKindIcon kind={task.kind} /></span>
              <span className="task-copy"><b title={taskName}>{taskName}</b>{peerMetric && <span className="task-peer-metric">{peerMetric}</span>}</span>
            </div>
            {isCompleted ? <div className="completed-summary">{completedSize && <strong>{completedSize}</strong>}{completionTime && <span>{completionTime}</span>}{task.p2p_sharing && <span>P2P 分享中</span>}</div> : <div className="task-progress"><div className="progress-track" role="progressbar" aria-label={`${taskName} 下载进度`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={task.progress}><i style={{ width: `${task.progress}%` }} /></div><div className="transfer-metrics">{transferSize && <strong>{transferSize}</strong>}{subtitle && <span className={`task-subtitle${task.error ? ' task-error' : ''}`} title={subtitle}>{subtitle}</span>}<span className="transfer-live">{isPaused ? <span className="transfer-paused">已暂停</span> : <>{eta && <span className="transfer-eta">剩余 {eta}</span>}{hasSpeed && <span className="transfer-speed">{speed}</span>}</>}</span></div></div>}
            <div className="task-actions">
              {isCompleted && task.cleanup_pending && <button className="completed-quick-action" aria-label={`重试网盘清理 ${taskName}`} title="重试网盘清理" onClick={(event) => { event.stopPropagation(); onCommand(task.id, 'retry_cleanup'); }}><ActionIcon action="retry" /></button>}
              {isCompleted && <button className="completed-quick-action" aria-label={completedHasPlayableMedia ? `播放 ${taskName}` : completedFileType ? `${associatedFileLabels[completedFileType]} ${taskName}` : `打开 ${taskName} 所在目录`} title={completedHasPlayableMedia ? '播放视频或音频' : completedFileType ? '使用系统默认应用打开' : '打开下载目录'} onClick={(event) => { event.stopPropagation(); onCommand(task.id, completedHasPlayableMedia ? 'play_videos' : completedFileType ? 'open_file' : 'open_directory'); }}><ActionIcon action={completedHasPlayableMedia ? 'play' : completedFileType ?? 'folder'} /></button>}
              {task.status === 'awaiting_user' && (task.kind === 'magnet' || task.kind === 'torrent') && onConfirm && <button aria-label={`确认下载 ${taskName}`} title="确认下载内容" onClick={(event) => { event.stopPropagation(); onConfirm(task); }}>确认</button>}
              {primaryAction && <button className={primaryAction === 'reauthorize' ? 'reauthorize-action' : undefined} aria-label={`${primaryLabel} ${taskName}`} title={primaryLabel} onClick={(event) => { event.stopPropagation(); onCommand(task.id, primaryAction); }}><ActionIcon action={primaryAction} />{primaryAction === 'reauthorize' && <span>重新授权</span>}</button>}
              <button className="task-more-button" aria-label={`更多操作 ${taskName}`} title="更多操作" onClick={(event) => { event.stopPropagation(); const rect = event.currentTarget.getBoundingClientRect(); openContextMenu(task, rect.left, rect.bottom + 6); }}><ActionIcon action="more" /></button>
            </div>
          </article>
          {hasChildren && isExpanded && <div className="children queue-tree"><QueueTreeRows nodes={taskChildren!} depth={0} expanded={expanded} onToggle={(id) => setExpanded((current) => ({ ...current, [id]: !current[id] }))} onCommand={onCommand} onFileSelection={onFileSelection} onFileCancel={onFileCancel} parentTaskId={task.id} isBtFileTree={isBtFileTree} completedRoot={isCompleted} childRoot={childRoot} /></div>}
        </div>;
      })}
      {contextMenu && <TaskContextMenu state={contextMenu} onClose={closeContextMenu} onCommand={runCommand} />}
    </section>
  );
}
