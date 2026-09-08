import { useEffect, useMemo, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { DownloadQueue } from './components/DownloadQueue';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { TaskDetails } from './components/TaskDetails';
import { CloudManager } from './components/CloudManager';
import { SettingsPanel } from './components/SettingsPanel';
import { WindowTitleBar } from './components/WindowTitleBar';
import { confirmDownload, isTauriRuntime, listPersistedTasks, parseDownload, runTaskAction, selectDownloadFiles } from './lib/api';
import { subscribeToDownloadEvents, upsertTask } from './lib/downloadEvents';
import { mainWindowIsMaximized, runMainWindowAction } from './lib/mainWindow';
import { ParseResult, QueueTask } from './lib/types';
import { managerViewFromLauncherPayload, ManagerView } from './lib/managerNavigation';

export function App() {
  const [tasks, setTasks] = useState<QueueTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ParseResult | null>(null);
  const [view, setView] = useState<ManagerView>('downloads');
  const [maximized, setMaximized] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const selectedTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === selectedTaskId), [selectedTaskId, tasks]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let active = true;
    let stopFocus: (() => void) | undefined;
    const refreshTasks = () => { void listPersistedTasks().then((saved) => { if (active) setTasks(saved); }).catch((error: unknown) => { if (active) setActionError(String(error)); }); };
    refreshTasks();
    getCurrentWindow().onFocusChanged(({ payload }) => { if (payload) refreshTasks(); })
      .then((stop) => { stopFocus = stop; })
      .catch(() => undefined);
    return () => { active = false; stopFocus?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    subscribeToDownloadEvents({
      onTask: (task) => { if (active) setTasks((current) => upsertTask(current, task)); },
      onGlobalStat: () => undefined
    }).then((stop) => { unlisten = stop; }).catch((error: unknown) => { if (active) setActionError(String(error)); });
    return () => { active = false; unlisten?.(); };
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
    let unlisten: (() => void) | undefined;
    listen<unknown>('launcher-navigate', (event) => {
      const destination = managerViewFromLauncherPayload(event.payload);
      if (destination) setView(destination);
    }).then((stop) => { unlisten = stop; }).catch(() => undefined);
    return () => { unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let unlisten: (() => void) | undefined;
    listen<string>('launcher-parse', (event) => { void parse(event.payload); })
      .then((stop) => { unlisten = stop; })
      .catch(() => undefined);
    return () => { unlisten?.(); };
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
    if (result.task) { setTasks((current) => [result.task!, ...current]); setSelectedTaskId(result.task.id); }
    if (result.confirmation) setConfirmation(result.confirmation);
  }

  async function confirm(selectedIds: string[]) {
    if (!confirmation) return;
    setActionError(null);
    const nativeTask = await confirmDownload(confirmation.taskId, selectedIds).catch((error: unknown) => { setActionError(String(error)); return undefined; });
    if (!nativeTask) return;
    setTasks((current) => upsertTask(current, nativeTask)); setSelectedTaskId(nativeTask.id); setConfirmation(null);
  }

  async function taskAction(id: string, action: 'pause' | 'resume' | 'cancel' | 'select' | 'skip') {
    setActionError(null);
    const parent = tasks.find((task) => task.id === id || task.children?.some((child) => child.id === id) || task.files?.some((file) => `${task.id}-file-${file.index}` === id));
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
      {actionError && <p className="action-error" role="alert">操作未完成：{actionError}</p>}
      <div className="workspace"><nav aria-label="主导航"><button className={view === 'downloads' ? 'nav-active' : ''} onClick={() => setView('downloads')}>↓ 下载</button><button className={view === 'cloud' ? 'nav-active' : ''} onClick={() => setView('cloud')}>☁ 我的网盘</button><button className={view === 'settings' ? 'nav-active' : ''} onClick={() => setView('settings')}>⚙ 设置</button><small>Windows Beta</small></nav>
        {view === 'downloads' && <><section className="queue-area"><div className="queue-toolbar"><div><p className="eyebrow">本地下载引擎</p><h2>全部任务</h2></div><span>标准单文件会自动开始；目录与多文件需要确认。</span></div><DownloadQueue tasks={tasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onAction={taskAction} /></section><TaskDetails task={selectedTask} /></>}
        {view === 'cloud' && <section className="view-panel"><CloudManager provider="quark" /><CloudManager provider="pikpak" /></section>}
        {view === 'settings' && <section className="view-panel"><SettingsPanel initial={{ debugReports: false }} onChange={() => undefined} /></section>}
      </div>
      {confirmation && <ParseConfirmDialog result={confirmation} onConfirm={confirm} onClose={() => setConfirmation(null)} />}
    </main>
  );
}
