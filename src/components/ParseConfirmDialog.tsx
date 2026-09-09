import { useEffect, useRef, useState } from 'react';
import { ConfirmResource, ParseResult } from '../lib/types';

interface ParseConfirmDialogProps {
  result: ParseResult;
  downloadDirectory?: string;
  onConfirm(selectedIds: string[]): void;
  onSelectionChange(selectedIds: string[]): void;
  onClose(): void;
}

type ResourceFilter = 'all' | 'video' | 'other';

function resourceCategory(resource: ConfirmResource): Exclude<ResourceFilter, 'all'> {
  return /\.(?:3gp|avi|flv|m4v|mkv|mov|mp4|mpeg|mpg|rmvb|ts|webm|wmv)$/i.test(resource.name) ? 'video' : 'other';
}

function FileKindIcon({ resource }: { resource: ConfirmResource }) {
  const category = resourceCategory(resource);
  const extension = resource.name.split('.').pop()?.slice(0, 4).toUpperCase();
  return <span className={`parse-file-icon ${category}`} aria-hidden="true">{category === 'video' ? '▶' : extension || '文'}</span>;
}

function SelectAllCheckbox({ checked, indeterminate, onChange }: { checked: boolean; indeterminate: boolean; onChange(): void }) {
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (input.current) input.current.indeterminate = indeterminate;
  }, [indeterminate]);
  return <input ref={input} type="checkbox" aria-label="全选文件" checked={checked} onChange={onChange} />;
}

export function ParseConfirmDialog({ result, downloadDirectory, onConfirm, onSelectionChange, onClose }: ParseConfirmDialogProps) {
  const resources = result.resources;
  const [showPassword, setShowPassword] = useState(false);
  const [showProcessing, setShowProcessing] = useState(false);
  const [filter, setFilter] = useState<ResourceFilter>('all');
  const selected = resources.filter((item) => item.selected).map((item) => item.id);
  const visibleResources = filter === 'all' ? resources : resources.filter((resource) => resourceCategory(resource) === filter);
  const allSelected = resources.length > 0 && selected.length === resources.length;
  const taskTitle = result.title || (result.isBitTorrent ? 'BT 下载任务' : '下载内容');
  const changeSelection = (resourceId: string) => {
    onSelectionChange(resources.map((item) => item.id === resourceId ? { ...item, selected: !item.selected } : item).filter((item) => item.selected).map((item) => item.id));
  };
  const toggleAll = () => onSelectionChange(allSelected ? [] : resources.map((resource) => resource.id));
  return <div className="modal-backdrop" role="presentation"><section className="confirm-dialog" role="dialog" aria-modal="true" aria-label="确认下载内容">
    <header><div className="confirm-dialog-title"><span aria-hidden="true">↓</span><h2>新建任务</h2></div><button aria-label="关闭确认窗口" onClick={onClose}>×</button></header>
    <div className="parse-task-summary"><span className={`parse-task-icon${result.isBitTorrent ? ' torrent' : ''}`} aria-hidden="true">{result.isBitTorrent ? 'BT' : '↓'}</span><div><strong title={taskTitle}>{taskTitle}</strong><p>{resources.length ? `已选 ${selected.length} / ${resources.length} 个文件` : '正在连接元数据服务…'}</p></div></div>
    <div className="parse-resource-card">
      {resources.length > 0 && <div className="parse-resource-toolbar">
        <label className="parse-select-all"><SelectAllCheckbox checked={allSelected} indeterminate={selected.length > 0 && !allSelected} onChange={toggleAll} /><span>全选</span></label>
        <b>已选 {selected.length} / {resources.length} 个文件</b>
        <div className="parse-resource-filters" role="group" aria-label="文件类型筛选">
          {(['all', 'video', 'other'] as ResourceFilter[]).map((option) => <button key={option} type="button" className={filter === option ? 'active' : ''} onClick={() => setFilter(option)}>{option === 'all' ? '全部' : option === 'video' ? '视频' : '其他'}</button>)}
        </div>
      </div>}
      <div className="resource-list">{resources.length ? visibleResources.map((resource) => <label className={`resource${resource.selected ? ' selected' : ''}`} key={resource.id}><input type="checkbox" aria-label={`选择 ${resource.name}`} checked={resource.selected} onChange={() => changeSelection(resource.id)} /><FileKindIcon resource={resource} /><span title={resource.name}>{resource.name}</span><small>{resource.size}</small></label>) : <p className="resource-loading">正在获取文件信息…</p>}</div>
    </div>
    {(result.processing.length > 0 || result.discoveredPassword) && <button className="disclosure" onClick={() => setShowProcessing(!showProcessing)}>{showProcessing ? '收起' : '查看'}后处理与密码 {showProcessing ? '⌃' : '⌄'}</button>}
    {showProcessing && <div className="processing-summary"><ul>{result.processing.map((step) => <li key={step}>{step}</li>)}</ul>{result.discoveredPassword && <p>解压密码：<code>{showPassword ? result.discoveredPassword : '••••••••'}</code><button onClick={() => setShowPassword(!showPassword)}>{showPassword ? '隐藏密码' : '显示密码'}</button></p>}</div>}
    <footer><div className="parse-destination"><span>下载到</span><strong title={downloadDirectory || '本机下载目录'}>{downloadDirectory || '本机下载目录'}</strong></div><div><button onClick={onClose}>返回</button><button className="primary" disabled={!selected.length} onClick={() => onConfirm(selected)}>立即下载 <span>{selected.length}</span></button></div></footer>
  </section></div>;
}
