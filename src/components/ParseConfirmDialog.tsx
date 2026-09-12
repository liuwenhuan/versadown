import { useEffect, useRef, useState } from 'react';
import { ConfirmResource, ParseResult } from '../lib/types';
import { formatBytes } from '../lib/downloadEvents';

interface ParseConfirmDialogProps {
  result: ParseResult;
  placement?: 'modal' | 'header';
  downloadDirectory?: string;
  browseDirectory?(currentDirectory: string): Promise<string | null>;
  privacyEnabled?: boolean;
  onConfirm(selectedIds: string[], destinationDirectory?: string, privateDownload?: boolean): void;
  onSelectionChange(selectedIds: string[]): void;
  onClose(): void;
}

type ResourceFilter = 'all' | 'video' | 'other';

const cloudProviderLabels: Record<string, string> = {
  quark: '夸克网盘', pikpak: 'PikPak', baidu: '百度网盘', tianyi: '天翼云盘', aliyun: '阿里云盘', xunlei: '迅雷网盘', uc: 'UC 网盘'
};

function cloudShareProvider(planSource: string): string | undefined {
  if (!planSource.startsWith('cloud-share://')) return undefined;
  return cloudProviderLabels[planSource.slice('cloud-share://'.length).split('/', 1)[0]];
}

function pathParts(value: string): string[] {
  return value.trim().replace(/\\/g, '/').replace(/\/{2,}/g, '/').split('/').filter(Boolean);
}

function relativePathParts(value: string, downloadDirectory?: string): string[] {
  const parts = pathParts(value);
  const directory = pathParts(downloadDirectory ?? '');
  const sharesDirectory = directory.length > 0 && directory.every((part, index) => parts[index]?.toLocaleLowerCase() === part.toLocaleLowerCase());
  const relative = sharesDirectory ? parts.slice(directory.length) : parts;
  return /^[a-z]:$/i.test(relative[0] ?? '') ? relative.slice(1) : relative;
}

function torrentRoot(resources: readonly ConfirmResource[], downloadDirectory?: string): string | undefined {
  const candidates = resources.map((resource) => relativePathParts(resource.name, downloadDirectory));
  if (!candidates.length || candidates.some((parts) => parts.length < 2)) return undefined;
  const root = candidates[0][0];
  return candidates.every((parts) => parts[0] === root) ? root : undefined;
}

function displayResourceName(resource: ConfirmResource, root: string | undefined, downloadDirectory?: string): string {
  const parts = relativePathParts(resource.name, downloadDirectory);
  if (!parts.length) return resource.name;
  return root && parts[0] === root ? parts.slice(1).join('/') || root : parts.join('/');
}

function displayResourceSize(value: string): string {
  const matches = [...value.matchAll(/(\d+)\s*B\b/gi)];
  const bytes = Number(matches.at(-1)?.[1]);
  return Number.isSafeInteger(bytes) ? formatBytes(bytes) : value;
}

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

export function ParseConfirmDialog({ result, placement = 'modal', downloadDirectory, browseDirectory, privacyEnabled = false, onConfirm, onSelectionChange, onClose }: ParseConfirmDialogProps) {
  const resources = result.resources;
  const [showPassword, setShowPassword] = useState(false);
  const [showProcessing, setShowProcessing] = useState(false);
  const [filter, setFilter] = useState<ResourceFilter>('all');
  const [destinationOverride, setDestinationOverride] = useState<string>();
  useEffect(() => setDestinationOverride(undefined), [downloadDirectory]);
  const destination = destinationOverride || downloadDirectory || '';
  const selected = resources.filter((item) => item.selected).map((item) => item.id);
  const canConfirm = resources.length === 0 ? !result.isBitTorrent : selected.length > 0;
  const visibleResources = filter === 'all' ? resources : resources.filter((resource) => resourceCategory(resource) === filter);
  const allSelected = resources.length > 0 && selected.length === resources.length;
  const root = result.isBitTorrent ? torrentRoot(resources, downloadDirectory) : undefined;
  const cloudProvider = cloudShareProvider(result.taskId);
  const taskTitle = root || (result.title && !result.title.startsWith('magnet:') ? result.title : (result.isBitTorrent ? 'BT 下载任务' : '下载内容'));
  const changeSelection = (resourceId: string) => {
    onSelectionChange(resources.map((item) => item.id === resourceId ? { ...item, selected: !item.selected } : item).filter((item) => item.selected).map((item) => item.id));
  };
  const toggleAll = () => onSelectionChange(allSelected ? [] : resources.map((resource) => resource.id));
  const chooseDestination = async () => {
    const selectedDirectory = await browseDirectory?.(destination || downloadDirectory || '');
    if (selectedDirectory) setDestinationOverride(selectedDirectory === downloadDirectory ? undefined : selectedDirectory);
  };
  const dialog = <section className={`confirm-dialog${placement === 'header' ? ' header-confirm-dialog' : ''}`} role="dialog" aria-modal={placement === 'modal' || undefined} aria-label="确认下载内容">
    <header><div className="confirm-dialog-title"><span aria-hidden="true">↓</span><h2>新建任务</h2></div><button aria-label="关闭确认窗口" onClick={onClose}>×</button></header>
    <div className="parse-task-summary"><span className={`parse-task-icon${result.isBitTorrent ? ' torrent' : ''}`} aria-hidden="true">{result.isBitTorrent ? 'BT' : '↓'}</span><div><strong title={taskTitle}>{taskTitle}</strong><p>{resources.length ? `已选 ${selected.length} / ${resources.length} 个文件` : result.isBitTorrent ? '正在连接元数据服务…' : '将作为单个文件下载'}</p>{cloudProvider && <p className="cloud-share-summary"><b>{cloudProvider}分享</b><span>必要时会自动转存后下载</span></p>}</div></div>
    <div className="parse-resource-card">
      {resources.length > 0 && <div className="parse-resource-toolbar">
        <label className="parse-select-all"><SelectAllCheckbox checked={allSelected} indeterminate={selected.length > 0 && !allSelected} onChange={toggleAll} /><span>全选</span></label>
        <b>已选 {selected.length} / {resources.length} 个文件</b>
        <div className="parse-resource-filters" role="group" aria-label="文件类型筛选">
          {(['all', 'video', 'other'] as ResourceFilter[]).map((option) => <button key={option} type="button" className={filter === option ? 'active' : ''} onClick={() => setFilter(option)}>{option === 'all' ? '全部' : option === 'video' ? '视频' : '其他'}</button>)}
        </div>
      </div>}
      <div className="resource-list">{resources.length ? visibleResources.map((resource) => { const displayName = displayResourceName(resource, root, downloadDirectory); return <label className={`resource${resource.selected ? ' selected' : ''}`} key={resource.id}><input type="checkbox" aria-label={`选择 ${displayName}`} checked={resource.selected} onChange={() => changeSelection(resource.id)} /><FileKindIcon resource={resource} /><span title={displayName}>{displayName}</span><small>{displayResourceSize(resource.size)}</small></label>; }) : <p className="resource-loading">正在获取文件信息…</p>}</div>
    </div>
    {(result.processing.length > 0 || result.discoveredPassword) && <button className="disclosure" onClick={() => setShowProcessing(!showProcessing)}>{showProcessing ? '收起' : '查看'}后处理与密码 {showProcessing ? '⌃' : '⌄'}</button>}
    {showProcessing && <div className="processing-summary"><ul>{result.processing.map((step) => <li key={step}>{step}</li>)}</ul>{result.discoveredPassword && <p>解压密码：<code>{showPassword ? result.discoveredPassword : '••••••••'}</code><button onClick={() => setShowPassword(!showPassword)}>{showPassword ? '隐藏密码' : '显示密码'}</button></p>}</div>}
    <footer><button type="button" className="parse-destination" aria-label="选择下载目录" onClick={() => void chooseDestination()}><span>下载到</span><strong title={destination || '本机下载目录'}>{destination || '本机下载目录'}</strong><i aria-hidden="true">更改</i></button><div><button onClick={onClose}>返回</button>{privacyEnabled && <button className="secondary" disabled={!canConfirm} onClick={() => onConfirm(selected, destinationOverride, true)}>隐私下载 <span>{selected.length}</span></button>}<button className="primary" disabled={!canConfirm} onClick={() => onConfirm(selected, destinationOverride)}>立即下载 <span>{selected.length}</span></button></div></footer>
  </section>;
  return placement === 'header' ? dialog : <div className="modal-backdrop" role="presentation">{dialog}</div>;
}
