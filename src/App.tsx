import { useMemo, useState } from 'react';
import { DownloadQueue } from './components/DownloadQueue';
import { FloatingLauncher } from './components/FloatingLauncher';
import { ParseConfirmDialog } from './components/ParseConfirmDialog';
import { TaskDetails } from './components/TaskDetails';
import { parseLocally } from './lib/api';
import { ParseResult, QueueTask } from './lib/types';

export function App() {
  const [tasks, setTasks] = useState<QueueTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ParseResult | null>(null);
  const selectedTask = useMemo(() => tasks.flatMap((task) => [task, ...(task.children ?? [])]).find((task) => task.id === selectedTaskId), [selectedTaskId, tasks]);

  function parse(input: string) {
    const result = parseLocally(input);
    if (result.task) { setTasks((current) => [result.task!, ...current]); setSelectedTaskId(result.task.id); }
    if (result.confirmation) setConfirmation(result.confirmation);
  }

  function confirm(selectedIds: string[]) {
    if (!confirmation) return;
    const task: QueueTask = {
      id: confirmation.taskId, name: '多文件下载任务', kind: 'folder', status: 'queued', progress: 0, speed: '等待开始',
      children: confirmation.resources.filter((resource) => selectedIds.includes(resource.id)).map((resource) => ({ id: resource.id, name: resource.name, kind: 'file', status: 'queued', progress: 0, size: resource.size }))
    };
    setTasks((current) => [task, ...current]); setSelectedTaskId(task.id); setConfirmation(null);
  }

  function taskAction(id: string, action: 'pause' | 'resume' | 'cancel') {
    const nextStatus = action === 'pause' ? 'paused' : action === 'resume' ? 'downloading' : 'cancelled';
    setTasks((current) => current.map((task) => task.id === id ? { ...task, status: nextStatus } : { ...task, children: task.children?.map((child) => child.id === id ? { ...child, status: nextStatus } : child) }));
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
      <div className="workspace"><nav aria-label="主导航"><b className="nav-active">↓ 下载</b><button>☁ 我的网盘</button><button>⚙ 设置</button><small>Windows Beta</small></nav>
        <section className="queue-area"><div className="queue-toolbar"><div><p className="eyebrow">本地下载引擎</p><h2>全部任务</h2></div><span>标准单文件会自动开始；目录与多文件需要确认。</span></div><DownloadQueue tasks={tasks} selectedTaskId={selectedTaskId} onSelect={setSelectedTaskId} onAction={taskAction} /></section>
        <TaskDetails task={selectedTask} />
      </div>
      <FloatingLauncher activeCount={tasks.filter((task) => task.status === 'downloading').length} onParse={parse} />
      {confirmation && <ParseConfirmDialog result={confirmation} onConfirm={confirm} onClose={() => setConfirmation(null)} />}
    </main>
  );
}
