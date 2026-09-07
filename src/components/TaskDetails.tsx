import { QueueTask } from '../lib/types';

export function TaskDetails({ task }: { task: QueueTask | undefined }) {
  if (!task) return <aside className="task-details muted"><b>选择一个任务</b><span>查看来源、错误和后处理状态。</span></aside>;
  return <aside className="task-details"><p className="eyebrow">任务详情</p><h2>{task.name}</h2><dl><div><dt>状态</dt><dd>{task.status}</dd></div><div><dt>进度</dt><dd>{task.progress}%</dd></div><div><dt>来源</dt><dd title={task.source}>{task.source ?? '本地任务'}</dd></div></dl><section className="safety-note"><b>安全处理</b><span>仅执行已确认的解压、合并、改名和校验步骤。</span></section></aside>;
}
