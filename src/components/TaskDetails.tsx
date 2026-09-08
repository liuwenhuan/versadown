import { QueueTask } from '../lib/types';
import { formatBytes, formatEta, formatSpeed, hasKnownTotal, transferProgress } from '../lib/downloadEvents';

export function TaskDetails({ task }: { task: QueueTask | undefined }) {
  if (!task) return <aside className="task-details muted"><b>选择一个任务</b><span>查看来源、错误和后处理状态。</span></aside>;
  return <aside className="task-details"><p className="eyebrow">任务详情</p><h2>{task.name}</h2><dl>
    <div><dt>状态</dt><dd>{task.status}</dd></div>
    <div><dt>传输进度</dt><dd>{hasKnownTotal(task) ? `${task.progress}% · ${transferProgress(task)}` : transferProgress(task)}</dd></div>
    <div><dt>下载速度</dt><dd>{formatSpeed(task.download_speed)}</dd></div>
    <div><dt>连接数</dt><dd>{task.connections ?? '未知'}</dd></div>
    <div><dt>预计剩余时间</dt><dd>{formatEta(task.eta_seconds)}</dd></div>
    <div><dt>aria2 GID</dt><dd>{task.aria_gid ?? '未分配'}</dd></div>
    {task.aria_error_code && <div><dt>aria2 错误码</dt><dd>{task.aria_error_code}</dd></div>}
    {task.error && <div><dt>错误</dt><dd>{task.error}</dd></div>}
    <div><dt>来源</dt><dd title={task.source}>{task.source ?? '本地任务'}</dd></div>
  </dl>{task.files?.length ? <section className="detail-files"><b>文件</b>{task.files.map((file) => <div key={file.index}><span>{file.path}</span><small>{formatBytes(file.completed_length)} / {formatBytes(file.length)} · {file.selected ? '已选择' : '已跳过'}</small></div>)}</section> : null}<section className="safety-note"><b>安全处理</b><span>仅执行已确认的解压、合并、改名和校验步骤。</span></section></aside>;
}
