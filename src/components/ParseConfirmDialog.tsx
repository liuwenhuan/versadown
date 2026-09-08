import { useState } from 'react';
import { ParseResult } from '../lib/types';

interface ParseConfirmDialogProps {
  result: ParseResult;
  onConfirm(selectedIds: string[]): void;
  onClose(): void;
}

export function ParseConfirmDialog({ result, onConfirm, onClose }: ParseConfirmDialogProps) {
  const [resources, setResources] = useState(result.resources);
  const [showPassword, setShowPassword] = useState(false);
  const [showProcessing, setShowProcessing] = useState(false);
  const selected = resources.filter((item) => item.selected).map((item) => item.id);
  return <div className="modal-backdrop" role="presentation"><section className="confirm-dialog" role="dialog" aria-modal="true" aria-label="确认下载内容">
    <header><div><p className="eyebrow">解析结果</p><h2>选择要下载的文件</h2></div><button aria-label="关闭确认窗口" onClick={onClose}>×</button></header>
    {result.isBitTorrent && <p className="bt-selection-note">将在开始前选择这些 BT 文件；同一任务中的文件不能独立暂停。</p>}
    <div className="resource-list">{resources.map((resource) => <label className="resource" key={resource.id}><input type="checkbox" checked={resource.selected} onChange={() => setResources((items) => items.map((item) => item.id === resource.id ? { ...item, selected: !item.selected } : item))} /><span>{resource.name}</span><small>{resource.size}</small></label>)}</div>
    <button className="disclosure" onClick={() => setShowProcessing(!showProcessing)}>{showProcessing ? '收起' : '查看'}后处理与密码 {showProcessing ? '⌃' : '⌄'}</button>
    {showProcessing && <div className="processing-summary"><ul>{result.processing.map((step) => <li key={step}>{step}</li>)}</ul>{result.discoveredPassword && <p>解压密码：<code>{showPassword ? result.discoveredPassword : '••••••••'}</code><button onClick={() => setShowPassword(!showPassword)}>{showPassword ? '隐藏密码' : '显示密码'}</button></p>}</div>}
    <footer><button onClick={onClose}>返回</button><button className="primary" disabled={!selected.length} onClick={() => onConfirm(selected)}>下载已选 {selected.length} 项</button></footer>
  </section></div>;
}
