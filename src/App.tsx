import { MouseEvent as ReactMouseEvent, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { BatchTaskCommand, displayTaskName, DownloadQueue } from './components/DownloadQueue';
import { NewDownloadDialog } from './components/NewDownloadDialog';
import { NewDownloadTab } from './components/NewDownloadTab';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { DuplicateDownloadDialog } from './components/DuplicateDownloadDialog';
import { RemoveTaskDialog } from './components/RemoveTaskDialog';
import { TaskDetailsDialog } from './components/TaskDetailsDialog';
import type { CloudManagerClient } from './components/CloudManager';
import { CloudWorkspace } from './components/CloudWorkspace';
import { SettingsPanel } from './components/SettingsPanel';
import { EngineStatusBanner } from './components/EngineStatusBanner';
import { cancelDownloadFile, confirmCloudShareDownload, confirmDownload, defaultSettings, getDownloadEngineState, getSettings, isTauriRuntime, listAiModels, listPersistedTasks, moveTaskToRecycle, openTaskDirectory, parseDownload, playTaskVideos, purgeRecycledTasks, resolveDuplicateDownload, retryDownloadEngine, runTaskAction, selectDownloadFiles, updateAiSettings, updateSettings } from './lib/api';
import * as cloudApi from './lib/cloudApi';
import { mergeTaskSnapshot, subscribeToDownloadEvents, upsertTask } from './lib/downloadEvents';
import { AiSettingsInput, AppSettings, DownloadEngineState, DuplicateDownload, ParseResult, QueueTask, TaskActionResult, TaskCommand } from './lib/types';
import { managerViewFromLauncherPayload, ManagerView } from './lib/managerNavigation';
import darkMintIcon from './assets/launcher/dark_mint_128.png';
import mintWhiteIcon from './assets/launcher/mint_white_128.png';
import { getResolvedTheme, subscribeTheme } from './lib/theme';
import type { CloudAuthorization } from './lib/cloudTypes';

type TaskListTab = 'active' | 'completed' | 'failed' | 'recycle';
type DownloadTab = 'new' | TaskListTab;
type TaskSortMode = 'created-desc' | 'created-asc' | 'progress-asc' | 'progress-desc' | 'name-asc' | 'name-desc' | 'size-asc' | 'size-desc';

const defaultTaskSort: TaskSortMode = 'created-desc';
const initialTaskSorts: Record<TaskListTab, TaskSortMode> = {
  active: defaultTaskSort,
  completed: defaultTaskSort,
  failed: defaultTaskSort,
  recycle: defaultTaskSort,
};

function taskSizeBytes(task: QueueTask): number | undefined {
  if (Number.isFinite(task.total_length)) return task.total_length;
  const selectedFiles = task.files?.filter((file) => file.selected !== false) ?? [];
  if (selectedFiles.length) return selectedFiles.reduce((total, file) => total + file.length, 0);
  const childSizes = task.children?.flatMap((child) => Number.isFinite(child.total_length) ? [child.total_length!] : []) ?? [];
  if (childSizes.length) return childSizes.reduce((total, size) => total + size, 0);
  return Number.isFinite(task.completed_length) ? task.completed_length : undefined;
}

function sortTasks(tasks: readonly QueueTask[], mode: TaskSortMode): QueueTask[] {
  const [field, direction] = mode.split('-') as ['created' | 'progress' | 'name' | 'size', 'asc' | 'desc'];
  return tasks.map((task, index) => ({ task, index })).sort((left, right) => {
    let comparison = 0;
    if (field === 'name') {
      comparison = displayTaskName(left.task).localeCompare(displayTaskName(right.task), 'zh-CN', { numeric: true, sensitivity: 'base' });
    } else {
      const leftValue = field === 'created' ? left.task.created_at || left.task.updated_at || undefined
        : field === 'progress' ? left.task.progress
          : taskSizeBytes(left.task);
      const rightValue = field === 'created' ? right.task.created_at || right.task.updated_at || undefined
        : field === 'progress' ? right.task.progress
          : taskSizeBytes(right.task);
      if (leftValue === undefined || rightValue === undefined) {
        if (leftValue === undefined && rightValue !== undefined) return 1;
        if (leftValue !== undefined && rightValue === undefined) return -1;
      } else {
        comparison = leftValue - rightValue;
      }
    }
    if (comparison === 0) return left.index - right.index;
    return direction === 'asc' ? comparison : -comparison;
  }).map(({ task }) => task);
}

const taskSortGroups: ReadonlyArray<{ label: string; options: ReadonlyArray<{ mode: TaskSortMode; label: string; ariaLabel: string }> }> = [
  { label: '按创建时间', options: [
    { mode: 'created-desc', label: '从新到旧', ariaLabel: '按创建时间从新到旧' },
    { mode: 'created-asc', label: '从旧到新', ariaLabel: '按创建时间从旧到新' },
  ] },
  { label: '按当前进度', options: [
    { mode: 'progress-asc', label: '从低到高', ariaLabel: '按当前进度从低到高' },
    { mode: 'progress-desc', label: '从高到低', ariaLabel: '按当前进度从高到低' },
  ] },
  { label: '按文件名称', options: [
    { mode: 'name-asc', label: '从 A 到 Z', ariaLabel: '按文件名称从 A 到 Z' },
    { mode: 'name-desc', label: '从 Z 到 A', ariaLabel: '按文件名称从 Z 到 A' },
  ] },
  { label: '按文件大小', options: [
    { mode: 'size-asc', label: '从小到大', ariaLabel: '按文件大小从小到大' },
    { mode: 'size-desc', label: '从大到小', ariaLabel: '按文件大小从大到小' },
  ] },
];

function TaskSortControl({ value, onChange }: { value: TaskSortMode; onChange(mode: TaskSortMode): void }) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const closeFromOutside = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeFromKeyboard = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', closeFromOutside);
    document.addEventListener('keydown', closeFromKeyboard);
    return () => {
      document.removeEventListener('pointerdown', closeFromOutside);
      document.removeEventListener('keydown', closeFromKeyboard);
    };
  }, [open]);
  return <div className="task-sort-control" ref={rootRef}>
    <button type="button" className="task-sort-trigger" aria-label="排序任务" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((current) => !current)}>
      <svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><path d="M10 6h9M10 12h6M10 18h3" /><path d="M5 5v14m-3-3 3 3 3-3" /></svg>
    </button>
    {open && <div className="task-sort-menu" role="menu" aria-label="任务排序">
      {taskSortGroups.map((group) => <section key={group.label}><p>{group.label}</p><div>{group.options.map((option) => <button type="button" key={option.mode} role="menuitemradio" aria-checked={value === option.mode} aria-label={option.ariaLabel} className={value === option.mode ? 'selected' : ''} onClick={() => { onChange(option.mode); setOpen(false); }}>{option.label}</button>)}</div></section>)}
    </div>}
  </div>;
}

const inactiveStatuses = new Set<QueueTask['status']>(['completed', 'failed', 'cancelled', 'recycled']);
const cloudClient: CloudManagerClient = cloudApi;

function isQueueTask(value: unknown): value is QueueTask {
  if (typeof value !== 'object' || value === null) return false;
  const task = value as Partial<QueueTask>;
  return typeof task.id === 'string' && typeof task.name === 'string' && typeof task.status === 'string' && typeof task.progress === 'number';
}

function findQueueTask(tasks: readonly QueueTask[], id: string): QueueTask | undefined {
  for (const task of tasks) {
    if (task.id === id) return task;
    const child = task.children && findQueueTask(task.children, id);
    if (child) return child;
  }
  return undefined;
}

function formatSpeed(bytes: number): string {
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s', 'TB/s'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return unit === 0 ? `${Math.round(value)} ${units[unit]}` : `${value.toFixed(1)} ${units[unit]}`;
}

async function runWindowAction(action: 'minimize' | 'maximize' | 'hide') {
  if (!isTauriRuntime()) return;
  try {
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    const window = getCurrentWindow();
    if (action === 'minimize') await window.minimize();
    if (action === 'maximize') await window.toggleMaximize();
    if (action === 'hide') await window.hide();
  } catch {
    // Window chrome is inert in browser previews and during shutdown.
  }
}

async function browseDownloadDirectory(currentDirectory: string): Promise<string | null> {
  if (!isTauriRuntime()) return null;
  const { open } = await import('@tauri-apps/plugin-dialog');
  const selected = await open({ directory: true, multiple: false, defaultPath: currentDirectory || undefined, title: '选择下载目录' });
  return typeof selected === 'string' ? selected : null;
}

function WindowIcon({ kind }: { kind: 'minimize' | 'maximize' | 'close' }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
    {kind === 'minimize' && <path d="M5 12h14" />}
    {kind === 'maximize' && <rect x="5" y="5" width="14" height="14" rx="1" />}
    {kind === 'close' && <><path d="m6.5 6.5 11 11" /><path d="m17.5 6.5-11 11" /></>}
  </svg>;
}

function SidebarToggleIcon({ collapsed }: { collapsed: boolean }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
    <rect x="4" y="4.5" width="16" height="15" rx="2" />
    <path d="M9 4.5v15" />
    {collapsed ? <path d="m12.5 9.5 3 2.5-3 2.5" /> : <path d="m15.5 9.5-3 2.5 3 2.5" />}
  </svg>;
}

function startDraggingFromTitleBar(event: ReactMouseEvent<HTMLElement>) {
  if (event.button !== 0 || !isTauriRuntime()) return;
  const target = event.target instanceof Element ? event.target : null;
  if (target?.closest('button, input, textarea, select, option, a, label, [role="button"], [contenteditable="true"], [data-window-no-drag]')) return;
  void getCurrentWindow().startDragging().catch(() => undefined);
}

function NavigationIcon({ kind }: { kind: 'tasks' | 'cloud' | 'settings' }) {
  const svgProps = {
    'aria-hidden': true,
    viewBox: '0 0 24 24',
    fill: 'none',
    stroke: 'currentColor',
    strokeWidth: 2.15,
    strokeLinecap: 'round' as const,
    strokeLinejoin: 'round' as const,
  };

  if (kind === 'tasks') {
    return <svg {...svgProps}><path d="M12 3.5v10.25" /><path d="m8.25 10.5 3.75 3.75 3.75-3.75" /><path d="M5 16.5h14v3H5z" /></svg>;
  }
  if (kind === 'cloud') {
    return <svg {...svgProps}><path d="M7.1 18.5h9.55a3.35 3.35 0 0 0 .48-6.66A5.7 5.7 0 0 0 6.42 10.2 4.2 4.2 0 0 0 7.1 18.5Z" /><path d="M8.5 20h7" /></svg>;
  }
  return <svg {...svgProps}><path d="m12 3.1 1.62 1.7 2.3-.27.92 2.12 2.1.98-.52 2.24 1.48 1.62-1.48 1.62.52 2.24-2.1.98-.92 2.12-2.3-.27L12 20.9l-1.62-1.7-2.3.27-.92-2.12-2.1-.98.52-2.24-1.48-1.62 1.48-1.62-.52-2.24 2.1-.98.92-2.12 2.3.27L12 3.1Z" /><circle cx="12" cy="12" r="2.8" /></svg>;
}

export function App() {
  const [tasks, setTasks] = useState<QueueTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ParseResult | null>(null);
  const [duplicateQueue, setDuplicateQueue] = useState<DuplicateDownload[]>([]);
  const [view, setView] = useState<ManagerView>('downloads');
  const [downloadTab, setDownloadTab] = useState<DownloadTab>('active');
  const [taskSorts, setTaskSorts] = useState<Record<TaskListTab, TaskSortMode>>(initialTaskSorts);
  const [searchInput, setSearchInput] = useState('');
  const [settings, setSettings] = useState<AppSettings>(defaultSettings);
  const [newDownloadOpen, setNewDownloadOpen] = useState(false);
  const [detailsTaskId, setDetailsTaskId] = useState<string | null>(null);
  const [pendingPurgeTaskIds, setPendingPurgeTaskIds] = useState<string[] | null>(null);
  const [notice, setNotice] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [taskReauthorization, setTaskReauthorization] = useState<CloudAuthorization | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const resolvedTheme = useSyncExternalStore(subscribeTheme, getResolvedTheme, getResolvedTheme);
  const activeTasks = useMemo(() => tasks.filter((task) => !inactiveStatuses.has(task.status)), [tasks]);
  const completedTasks = useMemo(() => tasks.filter((task) => task.status === 'completed'), [tasks]);
  const failedTasks = useMemo(() => tasks.filter((task) => task.status === 'failed'), [tasks]);
  const recycledTasks = useMemo(() => tasks.filter((task) => task.status === 'recycled' || task.status === 'cancelled'), [tasks]);
  const taskListTab: TaskListTab = downloadTab === 'new' ? 'active' : downloadTab;
  const unsortedVisibleTasks = taskListTab === 'active' ? activeTasks
    : taskListTab === 'completed' ? completedTasks
      : taskListTab === 'failed' ? failedTasks : recycledTasks;
  const visibleTasks = useMemo(() => sortTasks(unsortedVisibleTasks, taskSorts[taskListTab]), [taskListTab, taskSorts, unsortedVisibleTasks]);
  const detailsTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === detailsTaskId), [detailsTaskId, tasks]);
  const downloadingTasks = tasks.filter((task) => task.status === 'downloading');
  const pausedTasks = tasks.filter((task) => task.status === 'paused');
  const bulkTaskAction = pausedTasks.length > 0 ? 'resume' : downloadingTasks.length > 0 ? 'pause' : undefined;
  const currentSpeed = formatSpeed(downloadingTasks.reduce((total, task) => total + (task.speedBytes ?? task.download_speed ?? 0), 0));
  const pendingPurgeTasks = (pendingPurgeTaskIds ?? []).flatMap((id) => {
    const task = tasks.find((item) => item.id === id);
    return task ? [task] : [];
  });
  const [eventsReady, setEventsReady] = useState(false);
  const [engineState, setEngineState] = useState<DownloadEngineState>({ available: true, retrying: false });
  const taskEventEpoch = useRef(0);
  const taskEventVersions = useRef(new Map<string, number>());
  const engineEventEpoch = useRef(0);
  const tasksRef = useRef<QueueTask[]>(tasks);
  const confirmationGeneration = useRef(0);
  const newDownloadConfirmationResolver = useRef<((confirmed: boolean) => void) | null>(null);
  const shareConfirmationsInFlight = useRef(new Set<number>());
  const duplicateActionInFlight = useRef(false);
  const duplicateCloseInFlight = useRef(false);
  tasksRef.current = tasks;
  const duplicate = duplicateQueue[0] ?? null;

  function enqueueDuplicate(next: DuplicateDownload) {
    setDuplicateQueue((current) => [...current, next]);
  }

  function showConfirmation(next: ParseResult) {
    confirmationGeneration.current += 1;
    setConfirmation(next);
  }

  function dismissConfirmation() {
    confirmationGeneration.current += 1;
    setConfirmation(null);
    newDownloadConfirmationResolver.current?.(false);
    newDownloadConfirmationResolver.current = null;
  }

  function awaitNewDownloadConfirmation(): Promise<boolean> {
    newDownloadConfirmationResolver.current?.(false);
    return new Promise((resolve) => { newDownloadConfirmationResolver.current = resolve; });
  }

  function finishNewDownloadConfirmation(confirmed: boolean) {
    newDownloadConfirmationResolver.current?.(confirmed);
    newDownloadConfirmationResolver.current = null;
  }

  function dismissDuplicate(taskId: string) {
    if (duplicateActionInFlight.current || duplicateCloseInFlight.current) return;
    duplicateCloseInFlight.current = true;
    setDuplicateQueue((current) => current[0]?.task.id === taskId ? current.slice(1) : current);
    window.setTimeout(() => { duplicateCloseInFlight.current = false; }, 0);
  }

  function recordTaskEvent(task: QueueTask) {
    taskEventEpoch.current += 1;
    taskEventVersions.current.set(task.id, taskEventEpoch.current);
  }

  function refreshTaskSnapshot(disposed: () => boolean, scope?: 'active' | 'terminal') {
    const requestStartEpoch = taskEventEpoch.current;
    const snapshot = scope ? listPersistedTasks(scope) : listPersistedTasks();
    return snapshot.then((saved) => {
      if (disposed()) return;
      const protectedIds = new Set([...taskEventVersions.current]
        .flatMap(([id, epoch]) => epoch > requestStartEpoch ? [id] : []));
      setTasks((current) => mergeTaskSnapshot(current, saved, protectedIds));
    }).catch((error: unknown) => {
      if (!disposed()) setNotice({ kind: 'error', message: String(error) });
    });
  }

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 2600);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    if (!taskReauthorization) return;
    let cancelled = false;
    let timer: number | undefined;
    const poll = () => {
      timer = window.setTimeout(async () => {
        try {
          await cloudApi.completeCloudAuthorization(taskReauthorization.provider);
          if (!cancelled) {
            setTaskReauthorization(null);
            setNotice({ kind: 'success', message: '授权成功，正在恢复受影响的下载任务' });
          }
        } catch (error) {
          const code = typeof error === 'object' && error !== null && 'code' in error
            ? String((error as { code?: unknown }).code)
            : '';
          if (!cancelled && code !== 'authorization_pending') {
            setTaskReauthorization(null);
            setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
            return;
          }
        }
        if (!cancelled) poll();
      }, timer === undefined ? 500 : 1_000);
    };
    poll();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [taskReauthorization]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let terminalLoadTimer: number | undefined;
    subscribeToDownloadEvents({
      onTask: (task) => {
        if (disposed) return;
        recordTaskEvent(task);
        setTasks((current) => upsertTask(current, task));
        setConfirmation((current) => {
          if (!current || current.taskId !== task.id || !task.files?.length) return current;
          return {
            ...current,
            resources: task.files.map((file) => ({
              id: String(file.index), index: file.index, name: file.path,
              size: `${file.completed_length} B / ${file.length} B`,
              selected: current.resources.find((resource) => resource.index === file.index || resource.id === String(file.index))?.selected ?? file.selected
            }))
          };
        });
      },
      onGlobalStat: () => undefined
    }).then((stop) => {
      if (disposed) stop();
      else {
        unlisten = stop;
        setEventsReady(true);
        void refreshTaskSnapshot(() => disposed, 'active').then(() => {
          if (disposed) return;
          terminalLoadTimer = window.setTimeout(() => {
            void refreshTaskSnapshot(() => disposed, 'terminal');
          }, 0);
        });
      }
    }).catch((error: unknown) => {
      if (!disposed) setNotice({ kind: 'error', message: String(error) });
    });
    return () => {
      disposed = true;
      if (terminalLoadTimer !== undefined) window.clearTimeout(terminalLoadTimer);
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const pendingMagnet = confirmation?.isBitTorrent && confirmation.resources.length === 0;
    if (!pendingMagnet || !isTauriRuntime()) return;
    let disposed = false;
    const refreshMetadata = () => {
      void listPersistedTasks().then((saved) => {
        if (disposed) return;
        const task = saved.find((item) => item.id === confirmation.taskId);
        if (!task) return;
        recordTaskEvent(task);
        setTasks((current) => upsertTask(current, task));
        const files = task.files;
        if (!files?.length) return;
        setConfirmation((current) => {
          if (!current || current.taskId !== task.id) return current;
          return {
            ...current,
            resources: files.map((file) => ({
              id: String(file.index), index: file.index, name: file.path,
              size: `${file.completed_length} B / ${file.length} B`,
              selected: current.resources.find((resource) => resource.index === file.index || resource.id === String(file.index))?.selected ?? file.selected
            }))
          };
        });
      }).catch(() => undefined);
    };
    refreshMetadata();
    const timer = window.setInterval(refreshMetadata, 500);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [confirmation?.isBitTorrent, confirmation?.resources.length, confirmation?.taskId]);

  useEffect(() => {
    if (!isTauriRuntime() || !eventsReady) return;
    let disposed = false;
    let stopFocus: (() => void) | undefined;
    let terminalRefreshTimer: number | undefined;
    const refreshTasks = () => {
      void refreshTaskSnapshot(() => disposed, 'active').then(() => {
        if (disposed) return;
        if (terminalRefreshTimer !== undefined) window.clearTimeout(terminalRefreshTimer);
        terminalRefreshTimer = window.setTimeout(() => {
          void refreshTaskSnapshot(() => disposed, 'terminal');
        }, 0);
      });
    };
    getCurrentWindow().onFocusChanged(({ payload }) => { if (payload) refreshTasks(); })
      .then((stop) => { if (disposed) stop(); else stopFocus = stop; })
      .catch(() => undefined);
    return () => {
      disposed = true;
      if (terminalRefreshTimer !== undefined) window.clearTimeout(terminalRefreshTimer);
      stopFocus?.();
    };
  }, [eventsReady]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<DownloadEngineState>('download-engine-state-updated', (event) => {
      engineEventEpoch.current += 1;
      if (!disposed) setEngineState(event.payload);
    }).then((stop) => {
      if (disposed) { stop(); return; }
      unlisten = stop;
      const requestEpoch = engineEventEpoch.current;
      void getDownloadEngineState().then((state) => {
        if (!disposed && engineEventEpoch.current === requestEpoch) setEngineState(state);
      }).catch((error: unknown) => {
        if (!disposed && engineEventEpoch.current === requestEpoch) setEngineState({ available: false, retrying: false, error: String(error) });
      });
    }).catch((error: unknown) => {
      if (!disposed) setEngineState({ available: false, retrying: false, error: String(error) });
    });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    let active = true;
    getSettings().then((loaded) => { if (active) setSettings(loaded); }).catch(() => undefined);
    return () => { active = false; };
  }, []);

  function saveSettings(next: AppSettings): Promise<void> {
    const previous = settings;
    setSettings(next);
    return updateSettings(next).then(
      (saved) => { setSettings(saved); },
      (error) => {
        setSettings(previous);
        throw error;
      }
    );
  }

  async function saveAiConfiguration(configuration: AiSettingsInput) {
    const saved = await updateAiSettings(configuration);
    setSettings((current) => ({ ...current, ai: saved }));
    return saved;
  }

  async function runLauncherBatchAction(command: 'pause' | 'resume') {
    const targets = tasksRef.current.filter((task) => command === 'pause' ? task.status === 'downloading' : task.status === 'paused');
    if (!targets.length) return;
    const results = await Promise.allSettled(targets.map((task) => runTaskAction(task.id, command)));
    let failure: unknown;
    for (const result of results) {
      if (result.status === 'rejected') {
        failure ??= result.reason;
        continue;
      }
      const snapshot = result.value.queueTask ?? result.value.task;
      if (snapshot) {
        recordTaskEvent(snapshot);
        setTasks((current) => upsertTask(current, snapshot));
      }
    }
    if (failure) setNotice({ kind: 'error', message: failure instanceof Error ? failure.message : String(failure) });
  }

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<unknown>('launcher-navigate', (event) => {
      const destination = managerViewFromLauncherPayload(event.payload);
      if (destination) setView(destination);
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<'new-download' | 'resume-all' | 'pause-all'>('launcher-command', (event) => {
      if (event.payload === 'new-download') {
        setView('downloads');
        setDownloadTab('active');
        setNewDownloadOpen(true);
      }
      if (event.payload === 'resume-all') void runLauncherBatchAction('resume');
      if (event.payload === 'pause-all') void runLauncherBatchAction('pause');
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<string>('launcher-parse', (event) => {
      void parse(event.payload).catch((error) => setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) }));
    })
      .then((stop) => { if (disposed) stop(); else unlisten = stop; })
      .catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  async function parse(input: string, accessCode?: string) {
    const result = await parseDownload(input, accessCode);
    if (result.task) { recordTaskEvent(result.task); setTasks((current) => upsertTask(current, result.task!)); setSelectedTaskId(result.task.id); }
    if (result.confirmation) showConfirmation(result.confirmation);
    if (result.duplicate) {
      recordTaskEvent(result.duplicate.task);
      setTasks((current) => upsertTask(current, result.duplicate!.task));
      setSelectedTaskId(result.duplicate.task.id);
      enqueueDuplicate(result.duplicate);
    }
    return result;
  }

  async function resolveDuplicate(action: 'resume' | 'restart_missing' | 'overwrite') {
    if (!duplicate || duplicateActionInFlight.current) return;
    duplicateActionInFlight.current = true;
    const taskId = duplicate.task.id;
    try {
      const updated = await resolveDuplicateDownload(taskId, action);
      if (updated) {
        recordTaskEvent(updated);
        setTasks((current) => upsertTask(current, updated));
        setSelectedTaskId(updated.id);
      }
      setDuplicateQueue((current) => current[0]?.task.id === taskId ? current.slice(1) : current);
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    } finally {
      duplicateActionInFlight.current = false;
    }
  }

  async function openDuplicateDirectory() {
    if (!duplicate || duplicateActionInFlight.current) return;
    duplicateActionInFlight.current = true;
    const taskId = duplicate.task.id;
    try {
      await openTaskDirectory(taskId);
      setDuplicateQueue((current) => current[0]?.task.id === taskId ? current.slice(1) : current);
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    } finally {
      duplicateActionInFlight.current = false;
    }
  }

  async function confirm(selectedIds: string[], destinationDirectory?: string) {
    if (!confirmation) return;
    if (confirmation.taskId.startsWith('cloud-share://')) {
      const planSource = confirmation.taskId;
      const requestGeneration = confirmationGeneration.current;
      if (shareConfirmationsInFlight.current.has(requestGeneration)) return;
      shareConfirmationsInFlight.current.add(requestGeneration);
      try {
        const result = await confirmCloudShareDownload(planSource, selectedIds, destinationDirectory);
        const outcomes = result.outcomes;
        const cloudTasks = outcomes.flatMap((outcome) => outcome.queueTask ? [outcome.queueTask] : outcome.task ? [outcome.task] : outcome.duplicate ? [outcome.duplicate.task] : []);
        const duplicates = outcomes.flatMap((outcome) => outcome.duplicate ? [outcome.duplicate] : []);
        cloudTasks.forEach((cloudTask) => recordTaskEvent(cloudTask));
        setTasks((current) => cloudTasks.reduce((next, cloudTask) => upsertTask(next, cloudTask), current));
        setSelectedTaskId(cloudTasks.at(-1)?.id ?? null);
        setDuplicateQueue((current) => [...current, ...duplicates]);
        if (result.failures.length > 0) {
          const failedIds = new Set(result.failures.map((failure) => failure.selectedId));
          setConfirmation((current) => current?.taskId === planSource && confirmationGeneration.current === requestGeneration ? {
            ...current,
            resources: current.resources.map((resource) => ({ ...resource, selected: failedIds.has(resource.id) })),
          } : current);
          if (confirmationGeneration.current === requestGeneration) {
            const first = result.failures[0];
            const prefix = result.failures.length > 1 ? `${result.failures.length} 个文件未能开始下载：` : '';
            setNotice({ kind: 'error', message: `${prefix}${first.message}` });
          }
        } else {
          setConfirmation((current) => current?.taskId === planSource && confirmationGeneration.current === requestGeneration ? null : current);
          finishNewDownloadConfirmation(true);
        }
      } catch (error) {
        if (confirmationGeneration.current === requestGeneration) {
          setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
        }
      } finally {
        shareConfirmationsInFlight.current.delete(requestGeneration);
      }
      return;
    }
    const task: QueueTask = {
      id: confirmation.taskId, name: '多文件下载任务', kind: 'folder', status: 'queued', progress: 0, speed: '等待开始',
      children: confirmation.resources.filter((resource) => selectedIds.includes(resource.id)).map((resource) => ({ id: resource.id, name: resource.name, kind: 'file', status: 'queued', progress: 0, size: resource.size }))
    };
    const nativeTask = await (destinationDirectory
      ? confirmDownload(confirmation.taskId, selectedIds, destinationDirectory)
      : confirmDownload(confirmation.taskId, selectedIds)).catch(() => undefined);
    const confirmed = nativeTask ?? task;
    recordTaskEvent(confirmed);
    setTasks((current) => upsertTask(current, confirmed)); setSelectedTaskId(confirmed.id); setConfirmation(null);
    finishNewDownloadConfirmation(true);
  }

  function reopenTaskConfirmation(task: QueueTask) {
    if (task.status !== 'awaiting_user' || (task.kind !== 'magnet' && task.kind !== 'torrent')) return;
    showConfirmation({
      taskId: task.id,
      requiresConfirmation: true,
      resources: (task.files ?? []).map((file) => ({
        id: String(file.index),
        index: file.index,
        name: file.path,
        size: `${file.completed_length} B / ${file.length} B`,
        selected: file.selected,
      })),
      processing: ['文件选择确认后调用本地 aria2 下载'],
      isBitTorrent: true,
    });
  }

  async function taskCommand(id: string, command: TaskCommand) {
    const task = findQueueTask(tasks, id);
    if (!task) return;
    if (command === 'details') { setSelectedTaskId(id); setDetailsTaskId(id); return; }
    if (command === 'remove_record') { setPendingPurgeTaskIds([id]); return; }
    try {
      if (command === 'reauthorize') {
        const authorization = await cloudApi.reauthorizeFailedCloudTask(id);
        setTaskReauthorization(authorization);
        setNotice({ kind: 'success', message: authorization.instructions });
        return;
      }
      if (command === 'move_to_recycle') {
        const recycled = await moveTaskToRecycle(id);
        if (recycled) {
          recordTaskEvent(recycled);
          setTasks((current) => upsertTask(current, recycled));
        }
        return;
      }
      if (command === 'open_directory') {
        await openTaskDirectory(id);
        return;
      }
      if (command === 'play_videos') {
        await playTaskVideos(id);
        return;
      }
      if (command === 'copy_link') {
        if (!task.source) throw new Error('该任务没有可复制的来源链接');
        await writeClipboardText(task.source);
        setNotice({ kind: 'success', message: '下载链接已复制' });
        return;
      }
      const result = await runTaskAction(id, command);
      if (result.queueTask) {
        recordTaskEvent(result.queueTask);
        setTasks((current) => upsertTask(current, result.queueTask!));
      } else if (result.duplicate) {
        recordTaskEvent(result.duplicate.task);
        setTasks((current) => upsertTask(current, result.duplicate!.task));
        setSelectedTaskId(id);
        enqueueDuplicate(result.duplicate);
      } else if (result.task) {
        recordTaskEvent(result.task);
        setTasks((current) => upsertTask(current, result.task!));
      }
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function writeClipboardText(value: string) {
    if (isTauriRuntime()) {
      const { writeText } = await import('@tauri-apps/plugin-clipboard-manager');
      await writeText(value);
      return;
    }
    await navigator.clipboard.writeText(value);
  }

  async function batchTaskCommand(ids: readonly string[], command: BatchTaskCommand) {
    const uniqueIds = [...new Set(ids)];
    const targets = uniqueIds.map((id) => findQueueTask(tasks, id)).filter((task): task is QueueTask => Boolean(task));
    if (!targets.length) return;
    try {
      if (command === 'copy_link') {
        const links = targets.map((task) => task.source?.trim()).filter((source): source is string => Boolean(source));
        if (!links.length) throw new Error('所选任务没有可复制的来源链接');
        await writeClipboardText(links.join('\n'));
        setNotice({ kind: 'success', message: `已复制 ${links.length} 个下载链接` });
        return;
      }
      if (command === 'delete' && targets.every((task) => task.status === 'recycled' || task.status === 'cancelled')) {
        setPendingPurgeTaskIds(targets.map((task) => task.id));
        return;
      }

      const operations = targets.flatMap((task) => {
        if (command === 'pause') return task.status === 'downloading'
          ? [{ task, operation: runTaskAction(task.id, 'pause').then((result) => result.queueTask ?? result.task) }]
          : [];
        if (command === 'resume') {
          if (task.status === 'paused') return [{ task, operation: runTaskAction(task.id, 'resume').then((result) => result.queueTask ?? result.task) }];
          if (task.status === 'failed') return [{ task, operation: runTaskAction(task.id, 'retry').then((result) => result.queueTask ?? result.task) }];
          return [];
        }
        if (task.status === 'completed' || task.status === 'failed') {
          return [{ task, operation: moveTaskToRecycle(task.id) }];
        }
        return [{ task, operation: runTaskAction(task.id, 'cancel').then((result) => result.queueTask ?? result.task) }];
      });
      const results = await Promise.allSettled(operations.map(({ operation }) => operation));
      let failure: unknown;
      results.forEach((result) => {
        if (result.status === 'rejected') {
          failure ??= result.reason;
          return;
        }
        if (result.value) {
          recordTaskEvent(result.value);
          setTasks((current) => upsertTask(current, result.value!));
        }
      });
      if (failure) throw failure;
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function selectTaskFile(taskId: string, fileIndex: number, selected: boolean) {
    const task = findQueueTask(tasks, taskId);
    const files = task?.files;
    if (!task || !files?.some((file) => file.index === fileIndex)) return;
    const wasSelected = files.some((file) => file.index === fileIndex && file.selected);
    if (selected && wasSelected && task.status === 'paused') {
      await taskCommand(taskId, 'resume');
      return;
    }
    const selectedIndices = files
      .filter((file) => file.index === fileIndex ? selected : file.selected)
      .map((file) => file.index);
    if (!selectedIndices.length) {
      await taskCommand(taskId, 'pause');
      return;
    }
    try {
      const updated = await selectDownloadFiles(taskId, selectedIndices);
      if (updated) {
        recordTaskEvent(updated);
        setTasks((current) => upsertTask(current, updated));
      }
      if (selected && task.status === 'paused') await taskCommand(taskId, 'resume');
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function cancelTaskFile(taskId: string, fileIndex: number) {
    try {
      const updated = await cancelDownloadFile(taskId, fileIndex);
      if (updated) {
        recordTaskEvent(updated);
        setTasks((current) => upsertTask(current, updated));
      }
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function confirmTaskRemoval(deleteFiles: boolean) {
    const ids = pendingPurgeTaskIds ?? [];
    if (!ids.length) return;
    try {
      await purgeRecycledTasks(ids, deleteFiles);
      const removed = new Set(ids);
      setTasks((current) => current.filter((task) => !removed.has(task.id)));
      if (selectedTaskId && removed.has(selectedTaskId)) setSelectedTaskId(null);
      if (detailsTaskId && removed.has(detailsTaskId)) setDetailsTaskId(null);
      setPendingPurgeTaskIds(null);
      setNotice({ kind: 'success', message: deleteFiles ? '任务和下载文件已彻底删除' : '任务已从回收站彻底删除，下载文件已保留' });
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  function requestRecycleClear() {
    if (recycledTasks.length) setPendingPurgeTaskIds(recycledTasks.map((task) => task.id));
  }

  async function submitDownload() {
    const input = searchInput.trim();
    if (!input) return;
    await parse(input);
    setSearchInput('');
    setView('downloads');
    setDownloadTab('active');
  }

  function selectDownloadTab(tab: DownloadTab) {
    setDownloadTab(tab);
    setSelectedTaskId(null);
  }

  function acceptCloudDownload(value: unknown) {
    const outcome = value as TaskActionResult | undefined;
    if (outcome?.queueTask) {
      recordTaskEvent(outcome.queueTask);
      setTasks((current) => upsertTask(current, outcome.queueTask!));
      setSelectedTaskId(outcome.queueTask.id);
      setNotice({ kind: 'success', message: `已将「${outcome.queueTask.name}」加入下载队列` });
      return;
    }
    if (outcome?.duplicate) {
      recordTaskEvent(outcome.duplicate.task);
      setTasks((current) => upsertTask(current, outcome.duplicate!.task));
      setSelectedTaskId(outcome.duplicate.task.id);
      enqueueDuplicate(outcome.duplicate);
      return;
    }
    const task = outcome?.task ?? value;
    if (!isQueueTask(task)) {
      setNotice({ kind: 'error', message: '网盘下载任务的返回数据无效' });
      return;
    }
    recordTaskEvent(task);
    setTasks((current) => upsertTask(current, task));
    setSelectedTaskId(task.id);
    setNotice({ kind: 'success', message: `已将「${task.name}」加入下载队列` });
  }

  return (
    <main className="app-shell">
      <header className="app-header" onMouseDown={startDraggingFromTitleBar}>
        <div className="brand">
          <div className="brand-mark" data-icon-variant={resolvedTheme} aria-hidden="true"><img src={resolvedTheme === 'dark' ? darkMintIcon : mintWhiteIcon} alt="" /></div>
          <h1>万能下载</h1>
        </div>
        <button
          type="button"
          className="sidebar-toggle"
          aria-label={sidebarCollapsed ? '展开侧边栏' : '收起侧边栏'}
          aria-controls="primary-navigation"
          aria-expanded={!sidebarCollapsed}
          title={sidebarCollapsed ? '展开侧边栏' : '收起侧边栏'}
          onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
        ><SidebarToggleIcon collapsed={sidebarCollapsed} /></button>
        <label className="global-search">
          <span aria-hidden="true">⌕</span>
          <input
            aria-label="搜索任务或粘贴下载链接"
            placeholder="搜索任务或粘贴下载链接"
            value={searchInput}
            onChange={(event) => setSearchInput(event.target.value)}
            onKeyDown={(event) => { if (event.key === 'Enter') void submitDownload(); }}
          />
        </label>
        <button className="new-download" onClick={() => setNewDownloadOpen(true)}><span aria-hidden="true">＋</span>新建下载</button>
        <div className="window-controls" aria-label="窗口控制" style={{ marginLeft: 'auto', flex: '0 0 auto' }}>
          <button aria-label="最小化" onClick={() => void runWindowAction('minimize')}><WindowIcon kind="minimize" /></button>
          <button aria-label="最大化或还原" onClick={() => void runWindowAction('maximize')}><WindowIcon kind="maximize" /></button>
          <button className="window-close" aria-label="关闭" onClick={() => void runWindowAction('hide')}><WindowIcon kind="close" /></button>
        </div>
      </header>
      <EngineStatusBanner state={engineState} onRetry={() => {
        if (engineState.retrying) return;
        setEngineState((current) => ({ ...current, retrying: true, error: undefined }));
        void retryDownloadEngine().then(setEngineState).catch((error: unknown) => setEngineState({ available: false, retrying: false, error: String(error) }));
      }} />
      <div className={`workspace${sidebarCollapsed ? ' sidebar-collapsed' : ''}`}><nav id="primary-navigation" aria-label="主导航" aria-hidden={sidebarCollapsed}>
        <button className={view === 'downloads' ? 'nav-active' : ''} onClick={() => setView('downloads')}><span className="nav-icon" aria-hidden="true"><NavigationIcon kind="tasks" /></span><span>任务</span>{activeTasks.length > 0 && <b>{activeTasks.length}</b>}</button>
        <button className={view === 'cloud' ? 'nav-active' : ''} onClick={() => setView('cloud')}><span className="nav-icon" aria-hidden="true"><NavigationIcon kind="cloud" /></span><span>我的网盘</span></button>
        <button className={view === 'settings' ? 'nav-active' : ''} onClick={() => setView('settings')}><span className="nav-icon" aria-hidden="true"><NavigationIcon kind="settings" /></span><span>设置</span></button>
        <small>Windows Beta</small>
      </nav>
        {view === 'downloads' && <section className="downloads-view">
          <header className="download-tabs">
            <div className="download-tab-list workspace-tab-list" role="tablist" aria-label="下载任务">
              <button role="tab" aria-selected={downloadTab === 'new'} onClick={() => selectDownloadTab('new')}>新建下载</button>
              <button role="tab" aria-selected={downloadTab === 'active'} onClick={() => selectDownloadTab('active')}>下载中 <b>{activeTasks.length}</b></button>
              <button role="tab" aria-selected={downloadTab === 'completed'} onClick={() => selectDownloadTab('completed')}>已完成 <b>{completedTasks.length}</b></button>
              <button role="tab" aria-selected={downloadTab === 'failed'} onClick={() => selectDownloadTab('failed')}>失败 <b>{failedTasks.length}</b></button>
              <button role="tab" aria-selected={downloadTab === 'recycle'} onClick={() => selectDownloadTab('recycle')}>回收站 <b>{recycledTasks.length}</b></button>
            </div>
            <div className="download-tab-actions">
              {downloadTab === 'active' && bulkTaskAction && <button className="bulk-pause" onClick={() => void runLauncherBatchAction(bulkTaskAction)}>{bulkTaskAction === 'resume' ? '▷ 全部开始' : 'Ⅱ 全部暂停'}</button>}
              {downloadTab === 'recycle' && recycledTasks.length > 0 && <button className="bulk-pause" onClick={requestRecycleClear}>清空</button>}
            </div>
            <div className="speed-summary" aria-label="当前总下载速度"><strong>{currentSpeed}</strong></div>
            {downloadTab !== 'new' && <TaskSortControl value={taskSorts[downloadTab]} onChange={(mode) => setTaskSorts((current) => ({ ...current, [downloadTab]: mode }))} />}
          </header>
          {downloadTab === 'new' ? <NewDownloadTab onParse={parse} onAwaitConfirmation={awaitNewDownloadConfirmation} onComplete={() => setDownloadTab('active')} /> : <section className="queue-area">
            <DownloadQueue tasks={visibleTasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onCommand={taskCommand} onBatchCommand={batchTaskCommand} onFileSelection={selectTaskFile} onFileCancel={cancelTaskFile} onConfirm={reopenTaskConfirmation} />
          </section>}
        </section>}
        <section className="view-panel" hidden={view !== 'cloud'}><CloudWorkspace client={cloudClient} onDownloadTask={acceptCloudDownload} /></section>
        {view === 'settings' && <section className="view-panel"><SettingsPanel initial={settings} onChange={saveSettings} browseDirectory={browseDownloadDirectory} saveAiSettings={saveAiConfiguration} loadAiModels={listAiModels} /></section>}
      </div>
      {confirmation && <ParseConfirmDialog result={confirmation} downloadDirectory={settings.downloadDirectory} browseDirectory={browseDownloadDirectory} onConfirm={confirm} onSelectionChange={(selectedIds) => setConfirmation((current) => current ? {
        ...current,
        resources: current.resources.map((resource) => ({ ...resource, selected: selectedIds.includes(resource.id) }))
      } : current)} onClose={dismissConfirmation} />}
      {duplicate && <DuplicateDownloadDialog
        task={duplicate.task}
        state={duplicate.state}
        onOpenDirectory={() => void openDuplicateDirectory()}
        onResolve={(action) => void resolveDuplicate(action)}
        onRestartMissing={() => void resolveDuplicate('restart_missing')}
        onClose={() => dismissDuplicate(duplicate.task.id)}
      />}
      <NewDownloadDialog open={newDownloadOpen} placement="modal" onParse={async (input, accessCode) => {
        const result = await parse(input, accessCode);
        setView('downloads');
        setDownloadTab('active');
        return result;
      }} onClose={() => setNewDownloadOpen(false)} />
      {detailsTask && <TaskDetailsDialog task={detailsTask} onClose={() => setDetailsTaskId(null)} />}
      {pendingPurgeTasks.length > 0 && <RemoveTaskDialog taskNames={pendingPurgeTasks.map(displayTaskName)} onConfirm={(deleteFiles) => void confirmTaskRemoval(deleteFiles)} onClose={() => setPendingPurgeTaskIds(null)} />}
      {notice && <div className={`task-toast ${notice.kind}`} role="status">{notice.message}</div>}
    </main>
  );
}
