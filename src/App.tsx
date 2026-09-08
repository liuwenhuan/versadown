import { useEffect, useMemo, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { DownloadQueue } from './components/DownloadQueue';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { TaskDetails } from './components/TaskDetails';
import { CloudManager } from './components/CloudManager';
import { SettingsPanel } from './components/SettingsPanel';
import { confirmDownload, isTauriRuntime, listPersistedTasks, parseDownload, runTaskAction } from './lib/api';
import { ParseResult, QueueTask } from './lib/types';
import { managerViewFromLauncherPayload, ManagerView } from './lib/managerNavigation';

export function App() {
  const [tasks, setTasks] = useState<QueueTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ParseResult | null>(null);
  const [view, setView] = useState<ManagerView>('downloads');
  const selectedTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === selectedTaskId), [selectedTaskId, tasks]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let active = true;
    listPersistedTasks().then((saved) => { if (active) setTasks(saved); }).catch(() => undefined);
    return () => { active = false; };
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
    const result = await parseDownload(input);
    if (result.task) { setTasks((current) => [result.task!, ...current]); setSelectedTaskId(result.task.id); }
    if (result.confirmation) setConfirmation(result.confirmation);
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

  async function taskAction(id: string, action: 'pause' | 'resume' | 'cancel') {
    const nextStatus = action === 'pause' ? 'paused' : action === 'resume' ? 'downloading' : 'cancelled';
    const nativeTask = await runTaskAction(id, action).catch(() => undefined);
    setTasks((current) => current.map((task) => task.id === id ? nativeTask ?? { ...task, status: nextStatus } : { ...task, children: task.children?.map((child) => child.id === id ? nativeTask ?? { ...child, status: nextStatus } : child) }));
  }

  return (
    <main className="app-shell">
      <header className="app-header">
        <div className="brand-mark" aria-hidden="true">↓</div>
        <div>
          <p className="eyebrow">ALLDOWNLOADER</p>
          <h1>下载管理</h1>
        </div>
        <div className="header-summary"><b>{tasks.filter((task) => task.status === 'downloading').length}</b><span>正在下载</span></div>
      </header>
      <div className="workspace"><nav aria-label="主导航"><button className={view === 'downloads' ? 'nav-active' : ''} onClick={() => setView('downloads')}>↓ 下载</button><button className={view === 'cloud' ? 'nav-active' : ''} onClick={() => setView('cloud')}>☁ 我的网盘</button><button className={view === 'settings' ? 'nav-active' : ''} onClick={() => setView('settings')}>⚙ 设置</button><small>Windows Beta</small></nav>
        {view === 'downloads' && <><section className="queue-area"><div className="queue-toolbar"><div><p className="eyebrow">本地下载引擎</p><h2>全部任务</h2></div><span>标准单文件会自动开始；目录与多文件需要确认。</span></div><DownloadQueue tasks={tasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onAction={taskAction} /></section><TaskDetails task={selectedTask} /></>}
        {view === 'cloud' && <section className="view-panel"><CloudManager provider="quark" /><CloudManager provider="pikpak" /></section>}
        {view === 'settings' && <section className="view-panel"><SettingsPanel initial={{ debugReports: false }} onChange={() => undefined} /></section>}
      </div>
      {confirmation && <ParseConfirmDialog result={confirmation} onConfirm={confirm} onClose={() => setConfirmation(null)} />}
    </main>
  );
}
