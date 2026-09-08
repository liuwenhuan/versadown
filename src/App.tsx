import { useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { DownloadQueue } from './components/DownloadQueue';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { TaskDetails } from './components/TaskDetails';
import { CloudManager } from './components/CloudManager';
import { SettingsPanel } from './components/SettingsPanel';
import { WindowTitleBar } from './components/WindowTitleBar';
import { EngineStatusBanner } from './components/EngineStatusBanner';
import { confirmDownload, getDownloadEngineState, isTauriRuntime, listPersistedTasks, parseDownload, retryDownloadEngine, runTaskAction, selectDownloadFiles } from './lib/api';
import { formatBytes, mergeTaskSnapshot, subscribeToDownloadEvents, upsertTask } from './lib/downloadEvents';
import { mainWindowIsMaximized, runMainWindowAction } from './lib/mainWindow';
import { DownloadEngineState, ParseResult, QueueTask } from './lib/types';
import { managerViewFromLauncherPayload, ManagerView } from './lib/managerNavigation';

export function App() {
  const [tasks, setTasks] = useState<QueueTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ParseResult | null>(null);
  const [view, setView] = useState<ManagerView>('downloads');
  const [maximized, setMaximized] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [eventsReady, setEventsReady] = useState(false);
  const [engineState, setEngineState] = useState<DownloadEngineState>({ available: true, retrying: false });
  const taskEventEpoch = useRef(0);
  const taskEventVersions = useRef(new Map<string, number>());
  const engineEventEpoch = useRef(0);
  const selectedTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === selectedTaskId), [selectedTaskId, tasks]);

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
    }).catch((error: unknown) => { if (!disposed()) setActionError(String(error)); });
  }

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
              size: `${formatBytes(file.completed_length)} / ${formatBytes(file.length)}`,
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
    }).catch((error: unknown) => { if (!disposed) setActionError(String(error)); });
    return () => { disposed = true; unlisten?.(); };
  }, []);

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
        if (!disposed && engineEventEpoch.current === requestEpoch) {
          setEngineState({ available: false, retrying: false, error: String(error) });
        }
      });
    }).catch((error: unknown) => {
      if (!disposed) setEngineState({ available: false, retrying: false, error: String(error) });
    });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const refreshMaximized = () => {
      void mainWindowIsMaximized().then((value) => {
        if (!disposed) setMaximized(value);
      }).catch(() => undefined);
    };
    refreshMaximized();
    getCurrentWindow().onResized(refreshMaximized)
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

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
    listen<string>('launcher-parse', (event) => { void parse(event.payload); })
      .then((stop) => { if (disposed) stop(); else unlisten = stop; })
      .catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  async function parse(input: string) {
    setActionError(null);
    let result: { task?: QueueTask; confirmation?: ParseResult };
    try {
      result = await parseDownload(input);
    } catch (error) {
      setActionError(String(error));
      return;
    }
    if (result.task) { recordTaskEvent(result.task); setTasks((current) => upsertTask(current, result.task!)); setSelectedTaskId(result.task.id); }
    if (result.confirmation) setConfirmation(result.confirmation);
  }

  async function confirm(selectedIds: string[]) {
    if (!confirmation) return;
    setActionError(null);
    const nativeTask = await confirmDownload(confirmation.taskId, selectedIds).catch((error: unknown) => { setActionError(String(error)); return undefined; });
    if (!nativeTask) return;
    setTasks((current) => upsertTask(current, nativeTask)); setSelectedTaskId(nativeTask.id); setConfirmation(null);
  }

  async function retryEngine() {
    if (engineState.retrying) return;
    setEngineState((current) => ({ ...current, retrying: true, error: undefined }));
    try {
      setEngineState(await retryDownloadEngine());
    } catch (error) {
      setEngineState({ available: false, retrying: false, error: String(error) });
    }
  }

  function reopenTaskConfirmation(task: QueueTask) {
    if (task.status !== 'awaiting_user' || (task.kind !== 'magnet' && task.kind !== 'torrent')) return;
    setConfirmation({
      taskId: task.id,
      requiresConfirmation: true,
      resources: (task.files ?? []).map((file) => ({
        id: String(file.index), index: file.index, name: file.path,
        size: `${formatBytes(file.completed_length)} / ${formatBytes(file.length)}`,
        selected: file.selected
      })),
      processing: ['文件选择确认后调用本地 aria2 下载'],
      isBitTorrent: true
    });
  }

  async function taskAction(id: string, action: 'pause' | 'resume' | 'cancel' | 'select' | 'skip') {
    setActionError(null);
    const parent = tasks.find((task) => task.id === id || task.children?.some((child) => child.id === id) || task.files?.some((file) => `${task.id}-file-${file.index}` === id));
    if ((action === 'select' || action === 'skip') && parent?.status === 'awaiting_user') return;
    const file = parent?.files?.find((item) => `${parent.id}-file-${item.index}` === id) ?? parent?.children?.find((item) => item.id === id);
    const fileIndex = file && ('index' in file ? file.index : file.file_index);
    const nativeTask = action === 'select' || action === 'skip'
      ? parent && fileIndex !== undefined ? await selectDownloadFiles(parent.id, (parent.files ?? []).filter((item) => action === 'select' ? item.selected || item.index === fileIndex : item.selected && item.index !== fileIndex).map((item) => item.index)).catch((error: unknown) => { setActionError(String(error)); return undefined; }) : undefined
      : await runTaskAction(id, action).catch((error: unknown) => { setActionError(String(error)); return undefined; });
    if (nativeTask) setTasks((current) => upsertTask(current, nativeTask));
  }

  return (
    <main className="app-shell">
      <WindowTitleBar
        title="下载管理"
        maximized={maximized}
        onAction={(action) => { void runMainWindowAction(action); }}
      />
      <header className="app-header">
        <div className="brand-mark" aria-hidden="true">↓</div>
        <div>
          <p className="eyebrow">ALLDOWNLOADER</p>
          <h1>下载管理</h1>
        </div>
        <div className="header-summary"><b>{tasks.filter((task) => task.status === 'downloading').length}</b><span>正在下载</span></div>
      </header>
      <EngineStatusBanner state={engineState} onRetry={() => { void retryEngine(); }} />
      {actionError && <p className="action-error" role="alert">操作未完成：{actionError}</p>}
      <div className="workspace"><nav aria-label="主导航"><button className={view === 'downloads' ? 'nav-active' : ''} onClick={() => setView('downloads')}>↓ 下载</button><button className={view === 'cloud' ? 'nav-active' : ''} onClick={() => setView('cloud')}>☁ 我的网盘</button><button className={view === 'settings' ? 'nav-active' : ''} onClick={() => setView('settings')}>⚙ 设置</button><small>Windows Beta</small></nav>
        {view === 'downloads' && <><section className="queue-area"><div className="queue-toolbar"><div><p className="eyebrow">本地下载引擎</p><h2>全部任务</h2></div><span>标准单文件会自动开始；目录与多文件需要确认。</span></div><DownloadQueue tasks={tasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onAction={taskAction} onConfirm={reopenTaskConfirmation} /></section><TaskDetails task={selectedTask} /></>}
        {view === 'cloud' && <section className="view-panel"><CloudManager provider="quark" /><CloudManager provider="pikpak" /></section>}
        {view === 'settings' && <section className="view-panel"><SettingsPanel initial={{ debugReports: false }} onChange={() => undefined} /></section>}
      </div>
      {confirmation && <ParseConfirmDialog result={confirmation} onConfirm={confirm} onSelectionChange={(selectedIds) => setConfirmation((current) => current ? { ...current, resources: current.resources.map((resource) => ({ ...resource, selected: selectedIds.includes(resource.id) })) } : current)} onClose={() => setConfirmation(null)} />}
    </main>
  );
}
