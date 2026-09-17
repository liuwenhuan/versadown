import { useState } from 'react';
import { QueueTask } from '../lib/types';
import { formatBytes, formatEta, formatSpeed, hasKnownTotal, transferProgress } from '../lib/downloadEvents';
import { buildTaskFileTree, type TaskFileTreeNode } from '../lib/taskFileTree';

function DetailFileTree({ nodes, expanded, depth, onToggle }: { nodes: readonly TaskFileTreeNode[]; expanded: ReadonlySet<string>; depth: number; onToggle(key: string): void }) {
  return <>{nodes.map(node => {
    if (node.kind === 'file') return <div key={node.key} className="detail-file detail-file-leaf" style={{ paddingLeft: `${depth * 18}px` }}><span><i aria-hidden="true">▱</i>{node.name}</span><small>{formatBytes(node.file.completed_length)} / {formatBytes(node.file.length)} · {node.file.selected ? '已选择' : '已跳过'}</small></div>;
    const isExpanded = expanded.has(node.key);
    return <div key={node.key} className="detail-file-branch">
      <div className="detail-file detail-folder" style={{ paddingLeft: `${depth * 18}px` }}>
        <button type="button" className="detail-file-expand" aria-label={`${isExpanded ? '收起' : '展开'} ${node.name}`} aria-expanded={isExpanded} onClick={() => onToggle(node.key)}><svg className={isExpanded ? 'expanded' : ''} aria-hidden="true" viewBox="0 0 24 24"><path d="m9 5 7 7-7 7" /></svg></button>
        <span><i aria-hidden="true">▰</i>{node.name}</span><small>{node.count} 项 · {formatBytes(node.bytes)}</small>
      </div>
      {isExpanded && <DetailFileTree nodes={node.children} expanded={expanded} depth={depth + 1} onToggle={onToggle} />}
    </div>;
  })}</>;
}

export function TaskDetails({ task }: { task: QueueTask | undefined }) {
  const [expandedFiles, setExpandedFiles] = useState<Set<string>>(() => new Set());
  if (!task) return <aside className="task-details muted"><b>选择一个任务</b><span>查看来源、错误和后处理状态。</span></aside>;
  const fileTree = buildTaskFileTree(task);
  return <aside className="task-details">
    <h2 className="task-detail-name">{task.name}</h2>
    <dl className="task-detail-summary">
      <div><dt>状态</dt><dd>{task.status}</dd></div>
      <div><dt>传输进度</dt><dd>{hasKnownTotal(task) ? `${task.progress}% · ${transferProgress(task)}` : transferProgress(task)}</dd></div>
      <div><dt>下载速度</dt><dd>{formatSpeed(task.download_speed)}</dd></div>
      <div><dt>连接数</dt><dd>{task.connections ?? '未知'}</dd></div>
      <div><dt>预计剩余时间</dt><dd>{formatEta(task.eta_seconds)}</dd></div>
      <div><dt>aria2 GID</dt><dd>{task.aria_gid ?? '未分配'}</dd></div>
      {task.aria_error_code && <div><dt>aria2 错误码</dt><dd>{task.aria_error_code}</dd></div>}
      {task.error && <div><dt>错误</dt><dd>{task.error}</dd></div>}
    </dl>
    <section className="task-detail-source" aria-labelledby="task-source-label">
      <b id="task-source-label">来源</b>
      <p title={task.source}>{task.source ?? '本地任务'}</p>
    </section>
    {fileTree.length ? <section className="detail-files" aria-label="文件信息"><b>文件信息</b><div className="detail-file-list"><DetailFileTree nodes={fileTree} expanded={expandedFiles} depth={0} onToggle={(key) => setExpandedFiles(current => { const next = new Set(current); next.has(key) ? next.delete(key) : next.add(key); return next; })} /></div></section> : null}
    {task.post_processing_note?.trim() ? <section className="task-post-processing-note"><b>解压密码和后处理</b><p>{task.post_processing_note}</p></section> : null}
    <section className="safety-note"><b>安全处理</b><span>仅执行已确认的解压、合并、改名和校验步骤。</span></section>
  </aside>;
}
