import { useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { displayTaskName, DownloadQueue } from './components/DownloadQueue';
import { NewDownloadDialog } from './components/NewDownloadDialog';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { DuplicateDownloadDialog } from './components/DuplicateDownloadDialog';
import { RemoveTaskDialog } from './components/RemoveTaskDialog';
import { TaskDetailsDialog } from './components/TaskDetailsDialog';
import type { CloudManagerClient } from './components/CloudManager';
import { CloudWorkspace } from './components/CloudWorkspace';
import { SettingsPanel } from './components/SettingsPanel';
import { EngineStatusBanner } from './components/EngineStatusBanner';
import { confirmDownload, defaultSettings, getDownloadEngineState, getSettings, isTauriRuntime, listAiModels, listPersistedTasks, moveTaskToRecycle, openTaskDirectory, parseDownload, playTaskVideos, purgeRecycledTasks, resolveDuplicateDownload, retryDownloadEngine, runTaskAction, updateAiSettings, updateSettings } from './lib/api';
import * as cloudApi from './lib/cloudApi';
import { mergeTaskSnapshot, subscribeToDownloadEvents, upsertTask } from './lib/downloadEvents';
import { AiSettingsInput, AppSettings, DownloadEngineState, DuplicateDownload, ParseResult, QueueTask, TaskActionResult, TaskCommand } from './lib/types';
import { managerViewFromLauncherPayload, ManagerView } from './lib/managerNavigation';

type DownloadTab = 'active' | 'completed' | 'failed' | 'recycle';

const inactiveStatuses = new Set<QueueTask['status']>(['completed', 'failed', 'cancelled', 'recycled']);
const cloudClient: CloudManagerClient = cloudApi;

function isQueueTask(value: unknown): value is QueueTask {
  if (typeof value !== 'object' || value === null) return false;
  const task = value as Partial<QueueTask>;
  return typeof task.id === 'string' && typeof task.name === 'string' && typeof task.status === 'string' && typeof task.progress === 'number';
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

export function App() {
  const [tasks, setTasks] = useState<QueueTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ParseResult | null>(null);
  const [duplicate, setDuplicate] = useState<DuplicateDownload | null>(null);
  const [view, setView] = useState<ManagerView>('downloads');
  const [downloadTab, setDownloadTab] = useState<DownloadTab>('active');
  const [searchInput, setSearchInput] = useState('');
  const [settings, setSettings] = useState<AppSettings>(defaultSettings);
  const [newDownloadOpen, setNewDownloadOpen] = useState(false);
  const [detailsTaskId, setDetailsTaskId] = useState<string | null>(null);
  const [pendingPurgeTaskIds, setPendingPurgeTaskIds] = useState<string[] | null>(null);
  const [notice, setNotice] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const activeTasks = useMemo(() => tasks.filter((task) => !inactiveStatuses.has(task.status)), [tasks]);
  const completedTasks = useMemo(() => tasks.filter((task) => task.status === 'completed'), [tasks]);
  const failedTasks = useMemo(() => tasks.filter((task) => task.status === 'failed'), [tasks]);
  const recycledTasks = useMemo(() => tasks.filter((task) => task.status === 'recycled' || task.status === 'cancelled'), [tasks]);
  const visibleTasks = downloadTab === 'active' ? activeTasks
    : downloadTab === 'completed' ? completedTasks
      : downloadTab === 'failed' ? failedTasks : recycledTasks;
  const detailsTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === detailsTaskId), [detailsTaskId, tasks]);
  const downloadingTasks = tasks.filter((task) => task.status === 'downloading');
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
  tasksRef.current = tasks;

  function recordTaskEvent(task: QueueTask) {
    taskEventEpoch.current += 1;
    taskEventVersions.current.set(task.id, taskEventEpoch.current);
  }

  function refreshTaskSnapshot(disposed: () => boolean) {
    const requestStartEpoch = taskEventEpoch.current;
    void listPersistedTasks().then((saved) => {
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
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
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
        refreshTaskSnapshot(() => disposed);
      }
    }).catch((error: unknown) => {
      if (!disposed) setNotice({ kind: 'error', message: String(error) });
    });
    return () => { disposed = true; unlisten?.(); };
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
    const refreshTasks = () => { refreshTaskSnapshot(() => disposed); };
    getCurrentWindow().onFocusChanged(({ payload }) => { if (payload) refreshTasks(); })
      .then((stop) => { if (disposed) stop(); else stopFocus = stop; })
      .catch(() => undefined);
    return () => { disposed = true; stopFocus?.(); };
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

  function saveSettings(next: AppSettings) {
    setSettings(next);
    void updateSettings(next).then(setSettings).catch(() => undefined);
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
      if (result.value.task) {
        recordTaskEvent(result.value.task);
        setTasks((current) => upsertTask(current, result.value.task!));
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

  async function parse(input: string) {
    const result = await parseDownload(input);
    if (result.task) { recordTaskEvent(result.task); setTasks((current) => upsertTask(current, result.task!)); setSelectedTaskId(result.task.id); }
    if (result.confirmation) setConfirmation(result.confirmation);
    if (result.duplicate) {
      recordTaskEvent(result.duplicate.task);
      setTasks((current) => upsertTask(current, result.duplicate!.task));
      setSelectedTaskId(result.duplicate.task.id);
      setDuplicate(result.duplicate);
    }
  }

  async function resolveDuplicate(action: 'resume' | 'restart_missing' | 'overwrite') {
    if (!duplicate) return;
    try {
      const updated = await resolveDuplicateDownload(duplicate.task.id, action);
      if (updated) {
        recordTaskEvent(updated);
        setTasks((current) => upsertTask(current, updated));
        setSelectedTaskId(updated.id);
      }
      setDuplicate(null);
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function openDuplicateDirectory() {
    if (!duplicate) return;
    try {
      await openTaskDirectory(duplicate.task.id);
      setDuplicate(null);
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function confirm(selectedIds: string[], destinationDirectory?: string) {
    if (!confirmation) return;
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
  }

  function reopenTaskConfirmation(task: QueueTask) {
    if (task.status !== 'awaiting_user' || (task.kind !== 'magnet' && task.kind !== 'torrent')) return;
    setConfirmation({
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
    const task = tasks.find((item) => item.id === id);
    if (!task) return;
    if (command === 'details') { setSelectedTaskId(id); setDetailsTaskId(id); return; }
    if (command === 'remove_record') { setPendingPurgeTaskIds([id]); return; }
    try {
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
        if (isTauriRuntime()) {
          const { writeText } = await import('@tauri-apps/plugin-clipboard-manager');
          await writeText(task.source);
        } else {
          await navigator.clipboard.writeText(task.source);
        }
        setNotice({ kind: 'success', message: '下载链接已复制' });
        return;
      }
      const result = await runTaskAction(id, command);
      if (result.duplicate) {
        recordTaskEvent(result.duplicate.task);
        setTasks((current) => upsertTask(current, result.duplicate!.task));
        setSelectedTaskId(id);
        setDuplicate(result.duplicate);
      } else if (result.task) {
        recordTaskEvent(result.task);
        setTasks((current) => upsertTask(current, result.task!));
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
    if (outcome?.duplicate) {
      recordTaskEvent(outcome.duplicate.task);
      setTasks((current) => upsertTask(current, outcome.duplicate!.task));
      setSelectedTaskId(outcome.duplicate.task.id);
      setDuplicate(outcome.duplicate);
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
      <header className="app-header" data-tauri-drag-region="deep">
        <div className="brand" data-tauri-drag-region>
          <div className="brand-mark" aria-hidden="true">↓</div>
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
        <button className={view === 'downloads' ? 'nav-active' : ''} onClick={() => setView('downloads')}><span className="nav-icon">↓</span><span>任务</span>{activeTasks.length > 0 && <b>{activeTasks.length}</b>}</button>
        <button className={view === 'cloud' ? 'nav-active' : ''} onClick={() => setView('cloud')}><span className="nav-icon">☁</span><span>我的网盘</span></button>
        <button className={view === 'settings' ? 'nav-active' : ''} onClick={() => setView('settings')}><span className="nav-icon">⚙</span><span>设置</span></button>
        <small>Windows Beta</small>
      </nav>
        {view === 'downloads' && <section className="downloads-view">
          <header className="download-tabs">
            <div className="download-tab-list workspace-tab-list" role="tablist" aria-label="下载任务">
              <button role="tab" aria-selected={downloadTab === 'active'} onClick={() => selectDownloadTab('active')}>下载中 <b>{activeTasks.length}</b></button>
              <button role="tab" aria-selected={downloadTab === 'completed'} onClick={() => selectDownloadTab('completed')}>已完成 <b>{completedTasks.length}</b></button>
              <button role="tab" aria-selected={downloadTab === 'failed'} onClick={() => selectDownloadTab('failed')}>失败 <b>{failedTasks.length}</b></button>
              <span className="recycle-tab-control"><button role="tab" aria-selected={downloadTab === 'recycle'} onClick={() => selectDownloadTab('recycle')}>回收站 <b>{recycledTasks.length}</b></button>{downloadTab === 'recycle' && recycledTasks.length > 0 && <button className="recycle-clear" onClick={requestRecycleClear}>清空</button>}</span>
            </div>
            {downloadingTasks.length > 0 && <button className="bulk-pause" onClick={() => void Promise.all(downloadingTasks.map((task) => taskCommand(task.id, 'pause')))}>Ⅱ&nbsp; 全部暂停</button>}
            <div className="speed-summary" aria-label="当前总下载速度"><strong>{currentSpeed}</strong></div>
          </header>
          <section className="queue-area">
            <DownloadQueue tasks={visibleTasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onCommand={taskCommand} onConfirm={reopenTaskConfirmation} />
          </section>
        </section>}
        <section className="view-panel" hidden={view !== 'cloud'}><CloudWorkspace client={cloudClient} onDownloadTask={acceptCloudDownload} /></section>
        {view === 'settings' && <section className="view-panel"><SettingsPanel initial={settings} onChange={saveSettings} browseDirectory={browseDownloadDirectory} saveAiSettings={saveAiConfiguration} loadAiModels={listAiModels} /></section>}
      </div>
      {confirmation && <ParseConfirmDialog result={confirmation} downloadDirectory={settings.downloadDirectory} browseDirectory={browseDownloadDirectory} onConfirm={confirm} onSelectionChange={(selectedIds) => setConfirmation((current) => current ? {
        ...current,
        resources: current.resources.map((resource) => ({ ...resource, selected: selectedIds.includes(resource.id) }))
      } : current)} onClose={() => setConfirmation(null)} />}
      {duplicate && <DuplicateDownloadDialog
        task={duplicate.task}
        state={duplicate.state}
        onOpenDirectory={() => void openDuplicateDirectory()}
        onResolve={(action) => void resolveDuplicate(action)}
        onRestartMissing={() => void resolveDuplicate('restart_missing')}
        onClose={() => setDuplicate(null)}
      />}
      <NewDownloadDialog open={newDownloadOpen} placement="modal" onParse={async (input) => {
        await parse(input);
        setView('downloads');
        setDownloadTab('active');
      }} onClose={() => setNewDownloadOpen(false)} />
      {detailsTask && <TaskDetailsDialog task={detailsTask} onClose={() => setDetailsTaskId(null)} />}
      {pendingPurgeTasks.length > 0 && <RemoveTaskDialog taskNames={pendingPurgeTasks.map(displayTaskName)} onConfirm={(deleteFiles) => void confirmTaskRemoval(deleteFiles)} onClose={() => setPendingPurgeTaskIds(null)} />}
      {notice && <div className={`task-toast ${notice.kind}`} role="status">{notice.message}</div>}
    </main>
  );
}
