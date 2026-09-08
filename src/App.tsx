import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { listen } from '@tauri-apps/api/event';
import { displayTaskName, DownloadQueue } from './components/DownloadQueue';
import { NewDownloadDialog } from './components/NewDownloadDialog';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { DuplicateDownloadDialog } from './components/DuplicateDownloadDialog';
import { RemoveTaskDialog } from './components/RemoveTaskDialog';
import { TaskDetailsDialog } from './components/TaskDetailsDialog';
import type { CloudManagerClient } from './components/CloudManager';
import { CloudWorkspace } from './components/CloudWorkspace';
import { SettingsPanel } from './components/SettingsPanel';
import { confirmDownload, defaultSettings, getSettings, isTauriRuntime, listPersistedTasks, openTaskDirectory, parseDownload, removeTaskRecord, resolveDuplicateDownload, runTaskAction, updateSettings } from './lib/api';
import * as cloudApi from './lib/cloudApi';
import { AppSettings, DuplicateDownload, ParseResult, QueueTask, TaskCommand } from './lib/types';
import { managerViewFromLauncherPayload, ManagerView } from './lib/managerNavigation';
import { getThemeMode, setThemeMode as setAppThemeMode, subscribeTheme, ThemeMode } from './lib/theme';
import { startTaskPolling } from './lib/taskPolling';

type DownloadTab = 'active' | 'completed';

const terminalStatuses = new Set<QueueTask['status']>(['completed', 'cancelled']);
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
  const [pendingRemovalId, setPendingRemovalId] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const themeMode = useSyncExternalStore(subscribeTheme, getThemeMode, getThemeMode);
  const activeTasks = useMemo(() => tasks.filter((task) => !terminalStatuses.has(task.status)), [tasks]);
  const completedTasks = useMemo(() => tasks.filter((task) => terminalStatuses.has(task.status)), [tasks]);
  const visibleTasks = downloadTab === 'active' ? activeTasks : completedTasks;
  const detailsTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === detailsTaskId), [detailsTaskId, tasks]);
  const downloadingTasks = tasks.filter((task) => task.status === 'downloading');
  const currentSpeed = formatSpeed(downloadingTasks.reduce((total, task) => total + (task.speedBytes ?? 0), 0));
  const pendingRemovalTask = tasks.find((task) => task.id === pendingRemovalId);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 2600);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    return startTaskPolling(listPersistedTasks, setTasks);
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
    listen<string>('launcher-parse', (event) => {
      void parse(event.payload).catch((error) => setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) }));
    })
      .then((stop) => { unlisten = stop; })
      .catch(() => undefined);
    return () => { unlisten?.(); };
  }, []);

  async function parse(input: string) {
    const result = await parseDownload(input);
    if (result.task) { setTasks((current) => [result.task!, ...current]); setSelectedTaskId(result.task.id); }
    if (result.confirmation) setConfirmation(result.confirmation);
    if (result.duplicate) {
      setTasks((current) => [result.duplicate!.task, ...current.filter((task) => task.id !== result.duplicate!.task.id)]);
      setSelectedTaskId(result.duplicate.task.id);
      setDuplicate(result.duplicate);
    }
  }

  async function resolveDuplicate(action: 'resume' | 'restart_missing' | 'overwrite') {
    if (!duplicate) return;
    try {
      const updated = await resolveDuplicateDownload(duplicate.task.id, action);
      if (updated) {
        setTasks((current) => [updated, ...current.filter((task) => task.id !== updated.id)]);
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

  async function confirm(selectedIds: string[]) {
    if (!confirmation) return;
    const task: QueueTask = {
      id: confirmation.taskId, name: '多文件下载任务', kind: 'folder', status: 'queued', progress: 0, speed: '等待开始',
      children: confirmation.resources.filter((resource) => selectedIds.includes(resource.id)).map((resource) => ({ id: resource.id, name: resource.name, kind: 'file', status: 'queued', progress: 0, size: resource.size }))
    };
    const nativeTask = await confirmDownload(confirmation.taskId).catch(() => undefined);
    setTasks((current) => [nativeTask ?? task, ...current]); setSelectedTaskId((nativeTask ?? task).id); setConfirmation(null);
  }

  async function taskCommand(id: string, command: TaskCommand) {
    const task = tasks.find((item) => item.id === id);
    if (!task) return;
    if (command === 'details') { setSelectedTaskId(id); setDetailsTaskId(id); return; }
    if (command === 'remove_record') { setPendingRemovalId(id); return; }
    try {
      if (command === 'open_directory') {
        await openTaskDirectory(id);
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
        setTasks((current) => [result.duplicate!.task, ...current.filter((item) => item.id !== id)]);
        setSelectedTaskId(id);
        setDuplicate(result.duplicate);
      } else if (result.task) {
        setTasks((current) => current.map((item) => item.id === id ? result.task! : item));
      }
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function confirmTaskRemoval() {
    if (!pendingRemovalId) return;
    const id = pendingRemovalId;
    try {
      await removeTaskRecord(id);
      setTasks((current) => current.filter((task) => task.id !== id));
      if (selectedTaskId === id) setSelectedTaskId(null);
      if (detailsTaskId === id) setDetailsTaskId(null);
      setPendingRemovalId(null);
      setNotice({ kind: 'success', message: '任务记录已移除，下载文件已保留' });
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
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

  function acceptCloudDownload(task: unknown) {
    if (!isQueueTask(task)) {
      setNotice({ kind: 'error', message: '网盘下载任务的返回数据无效' });
      return;
    }
    setTasks((current) => [task, ...current.filter((item) => item.id !== task.id)]);
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
        <div className="theme-switcher" role="group" aria-label="主题">
          {([['auto', '自动'], ['light', '浅色'], ['dark', '深色']] as const).map(([mode, label]) => (
            <button key={mode} aria-pressed={themeMode === mode} onClick={() => setAppThemeMode(mode as ThemeMode)}>{label}</button>
          ))}
        </div>
        <div className="window-controls" aria-label="窗口控制">
          <button aria-label="最小化" onClick={() => void runWindowAction('minimize')}><WindowIcon kind="minimize" /></button>
          <button aria-label="最大化或还原" onClick={() => void runWindowAction('maximize')}><WindowIcon kind="maximize" /></button>
          <button className="window-close" aria-label="关闭" onClick={() => void runWindowAction('hide')}><WindowIcon kind="close" /></button>
        </div>
      </header>
      <div className="workspace"><nav aria-label="主导航">
        <button className={view === 'downloads' ? 'nav-active' : ''} onClick={() => setView('downloads')}><span className="nav-icon">↓</span><span>下载</span>{activeTasks.length > 0 && <b>{activeTasks.length}</b>}</button>
        <button className={view === 'cloud' ? 'nav-active' : ''} onClick={() => setView('cloud')}><span className="nav-icon">☁</span><span>我的网盘</span></button>
        <button className={view === 'settings' ? 'nav-active' : ''} onClick={() => setView('settings')}><span className="nav-icon">⚙</span><span>设置</span></button>
        <small>Windows Beta</small>
      </nav>
        {view === 'downloads' && <section className="downloads-view">
          <header className="download-tabs" role="tablist" aria-label="下载任务">
            <button role="tab" aria-selected={downloadTab === 'active'} onClick={() => selectDownloadTab('active')}>下载中 <b>{activeTasks.length}</b></button>
            <button role="tab" aria-selected={downloadTab === 'completed'} onClick={() => selectDownloadTab('completed')}>已完成 <b>{completedTasks.length}</b></button>
          </header>
          <div className="transfer-summary">
            <div className="speed-summary"><strong>{currentSpeed}</strong><span>当前速度</span></div>
            <p><b>aria2</b> 本地下载引擎 · 标准单文件自动开始，目录与多文件解析后确认</p>
            {downloadingTasks.length > 0 && <button onClick={() => void Promise.all(downloadingTasks.map((task) => taskCommand(task.id, 'pause')))}>Ⅱ&nbsp; 全部暂停</button>}
          </div>
          <section className="queue-area">
            <DownloadQueue tasks={visibleTasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onCommand={taskCommand} />
          </section>
        </section>}
        <section className="view-panel" hidden={view !== 'cloud'}><CloudWorkspace client={cloudClient} onDownloadTask={acceptCloudDownload} /></section>
        {view === 'settings' && <section className="view-panel"><SettingsPanel initial={settings} onChange={saveSettings} browseDirectory={browseDownloadDirectory} /></section>}
      </div>
      {confirmation && <ParseConfirmDialog result={confirmation} onConfirm={confirm} onClose={() => setConfirmation(null)} />}
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
      {pendingRemovalTask && <RemoveTaskDialog taskName={displayTaskName(pendingRemovalTask)} onConfirm={() => void confirmTaskRemoval()} onClose={() => setPendingRemovalId(null)} />}
      {notice && <div className={`task-toast ${notice.kind}`} role="status">{notice.message}</div>}
    </main>
  );
}
