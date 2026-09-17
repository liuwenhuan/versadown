import { useEffect, useState } from 'react';
import { QueueTask } from '../lib/types';
import { formatBytes } from '../lib/downloadEvents';
import { buildTaskFileTree, type TaskFileTreeNode } from '../lib/taskFileTree';

type DetailPanel = 'files' | 'post-processing' | null;

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
  const [expandedPanel, setExpandedPanel] = useState<DetailPanel>('files');

  useEffect(() => {
    setExpandedFiles(new Set());
    setExpandedPanel('files');
  }, [task?.id]);

  if (!task) return <aside className="task-details muted"><b>选择一个任务</b><span>查看来源、错误和后处理状态。</span></aside>;
  const fileTree = buildTaskFileTree(task);
  const hasFiles = fileTree.length > 0;
  const filesExpanded = hasFiles && expandedPanel === 'files';
  const postProcessingExpanded = expandedPanel === 'post-processing' || (!hasFiles && expandedPanel === 'files');
  const postProcessingNote = task.post_processing_note?.trim() || '未设置后处理信息。';

  const togglePanel = (panel: Exclude<DetailPanel, null>) => {
    setExpandedPanel(current => current === panel ? null : panel);
  };

  return <aside className="task-details">
    <h2 className="task-detail-name">{task.name}</h2>
    <section className="task-detail-source" aria-labelledby="task-source-label">
      <b id="task-source-label">来源</b>
      <p>{task.source ?? '本地任务'}</p>
    </section>
    {hasFiles && <section className="detail-files">
      <button type="button" className="detail-panel-toggle" aria-expanded={filesExpanded} aria-label={`${filesExpanded ? '收起' : '展开'}文件信息`} onClick={() => togglePanel('files')}>
        <span>文件信息</span><svg className={filesExpanded ? 'expanded' : ''} aria-hidden="true" viewBox="0 0 24 24"><path d="m9 5 7 7-7 7" /></svg>
      </button>
      {filesExpanded && <div className="detail-file-list" role="region" aria-label="文件信息"><DetailFileTree nodes={fileTree} expanded={expandedFiles} depth={0} onToggle={(key) => setExpandedFiles(current => { const next = new Set(current); next.has(key) ? next.delete(key) : next.add(key); return next; })} /></div>}
    </section>}
    <section className={`task-post-processing-note${postProcessingExpanded ? ' is-expanded' : ''}`}>
      <button type="button" className="detail-panel-toggle" aria-expanded={postProcessingExpanded} aria-label={`${postProcessingExpanded ? '收起' : '展开'}后处理信息`} onClick={() => togglePanel('post-processing')}>
        <span>后处理信息</span><svg className={postProcessingExpanded ? 'expanded' : ''} aria-hidden="true" viewBox="0 0 24 24"><path d="m9 5 7 7-7 7" /></svg>
      </button>
      <div className="task-post-processing-content"><p>{postProcessingNote}</p></div>
    </section>
  </aside>;
}
