import { FormEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ParseDownloadResult, ScanDownloadResult } from '../lib/types';

export type NewDownloadDialogPlacement = 'launcher' | 'modal' | 'header';

export interface NewDownloadDialogProps {
  open: boolean;
  placement?: NewDownloadDialogPlacement;
  onParse(input: string, accessCode?: string): void | Promise<ParseDownloadResult | void>;
  onScan?(input: string): Promise<ScanDownloadResult[]>;
  onClose(): void;
  onOpenTaskList?(): void | Promise<void>;
  initialInput?: string;
}

// Parse errors currently cross IPC as text. All providers use the same action
// policy: unavailable shares close; explicit transient/auth/code errors retry.
function isUnavailableShare(message: string | null): boolean {
  if (!message) return false;
  // A generic suggestion to check a code is not a confirmed code error.
  if (/超时|请求(?:过于)?频繁|限流|(?:请|需要)(?:先|重新)?(?:登录|授权)|(?:登录|授权|令牌)(?:已)?(?:过期|失效|无效)|(?:提取码|访问码|分享码|密码)(?:(?:校验|验证)?(?:错误|失败|无效)|不正确|缺失)/.test(message)) return false;
  return /分享(?:链接|地址|文件|内容|的文件)?(?:已(?:经)?(?:被)?|被)?(?:取消|删除|失效|过期|屏蔽|封禁|不存在|无效|不可用|无权访问|禁止访问|拒绝访问)/.test(message)
    || /(?:已|已经)(?:被)?(?:取消|停止)(?:了)?分享/.test(message)
    || /分享[^。；\n]*封禁[^。；\n]*(?:无法访问|受限)/.test(message);
}

type DownloadBatch = { entries: ScanDownloadResult[]; index: number };

function directAutoStarts(entry: ScanDownloadResult, text: string): boolean {
  return entry.valid
    && entry.input === text
    && ['direct', 'ed2k', 'ftp', 'hls', 'dash', 'thunder'].includes(entry.kind);
}

export function NewDownloadDialog({ open, placement = 'launcher', onParse, onScan, onClose, onOpenTaskList, initialInput }: NewDownloadDialogProps) {
  const [input, setInput] = useState('');
  const [accessCode, setAccessCode] = useState('');
  const [accessCodePrompt, setAccessCodePrompt] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [batch, setBatch] = useState<DownloadBatch | null>(null);
  const unavailableShare = isUnavailableShare(error);
  const dialogRef = useRef<HTMLElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const accessCodeRef = useRef<HTMLInputElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);

  useEffect(() => { onCloseRef.current = onClose; }, [onClose]);

  useLayoutEffect(() => {
    if (!open) return;
    if (placement === 'modal') previousFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    inputRef.current?.focus();
    return () => {
      if (placement === 'modal' && previousFocusRef.current?.isConnected) previousFocusRef.current.focus();
    };
  }, [open, placement]);

  useEffect(() => {
    if (!open) { setIsSubmitting(false); setError(null); setAccessCode(''); setAccessCodePrompt(null); setBatch(null); }
  }, [open]);

  useEffect(() => {
    if (open) { setInput(initialInput ?? ''); setError(null); setBatch(null); }
  }, [initialInput, open]);

  useEffect(() => {
    if (accessCodePrompt) accessCodeRef.current?.focus();
  }, [accessCodePrompt]);

  function requestClose() {
    if (!isSubmitting) {
      setAccessCode('');
      setAccessCodePrompt(null);
      setBatch(null);
      onCloseRef.current();
    }
  }

  function openTaskList() {
    if (isSubmitting) return;
    void onOpenTaskList?.();
    requestClose();
  }

  function finishBatch() {
    setInput('');
    setAccessCode('');
    setAccessCodePrompt(null);
    setBatch(null);
    onCloseRef.current();
  }

  function advanceBatch(currentBatch: DownloadBatch) {
    const nextIndex = currentBatch.index + 1;
    setIsSubmitting(false);
    setAccessCode('');
    setAccessCodePrompt(null);
    setError(null);
    if (nextIndex >= currentBatch.entries.length) {
      finishBatch();
      return;
    }
    setBatch({ ...currentBatch, index: nextIndex });
  }

  async function parseCandidate(candidate: string, currentBatch?: DownloadBatch) {
    try {
      const result = await onParse(candidate, accessCode.trim() || undefined);
      if (result?.shareAccessCodeRequired) {
        setAccessCodePrompt(result.shareAccessCodeRequired.message);
        setIsSubmitting(false);
        return;
      }
      if (currentBatch) {
        advanceBatch(currentBatch);
      } else {
        finishBatch();
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setIsSubmitting(false);
    }
  }

  async function submitCurrentBatch(currentBatch: DownloadBatch) {
    const entry = currentBatch.entries[currentBatch.index];
    if (!entry.valid) {
      advanceBatch(currentBatch);
      return;
    }
    setIsSubmitting(true);
    setError(null);
    await parseCandidate(entry.input, currentBatch);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (isSubmitting) return;
    if (unavailableShare) { requestClose(); return; }
    if (batch) {
      await submitCurrentBatch(batch);
      return;
    }
    const text = input.trim();
    if (!text) return;
    setIsSubmitting(true);
    setError(null);
    try {
      const entries = onScan ? await onScan(text) : [];
      if (onScan && !(entries.length === 1 && directAutoStarts(entries[0], text))) {
        setBatch({ entries: entries.length ? entries : [{ input: text, valid: false, message: '未发现受支持的有效下载链接' }], index: 0 });
        setIsSubmitting(false);
        return;
      }
      await parseCandidate(text);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setIsSubmitting(false);
    }
  }

  function trapModalFocus(event: React.KeyboardEvent<HTMLElement>) {
    if (event.key === 'Escape') {
      event.preventDefault();
      requestClose();
      return;
    }
    if (placement !== 'modal' || event.key !== 'Tab') return;
    const focusable = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>('button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])') ?? []).filter((element) => element.tabIndex >= 0);
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  if (!open) return null;

  const batchEntry = batch?.entries[batch.index];
  const batchIsLast = batch ? batch.index + 1 === batch.entries.length : false;
  const actionClass = accessCodePrompt ? 'primary cloud-toolbar-primary cloud-authorization-continuation' : 'primary';
  const parseAction = unavailableShare
    ? <button className={actionClass} type="button" onClick={requestClose}>关闭</button>
    : batchEntry && !batchEntry.valid
      ? <button className={actionClass} type="button" disabled={isSubmitting} onClick={() => advanceBatch(batch!)}>{batchIsLast ? '跳过并完成' : '跳过并继续'}</button>
      : batchEntry
        ? <button className={actionClass} type="submit" disabled={isSubmitting}>{isSubmitting ? '正在解析…' : batchIsLast ? '确认并完成' : '确认并继续'}</button>
    : <button className={actionClass} type="submit" disabled={isSubmitting || !input.trim()}>{isSubmitting ? '正在解析…' : '开始解析下载'}</button>;

  const dialog = (
    <aside ref={dialogRef} className={placement === 'launcher' ? 'launcher-panel new-download-dialog' : placement === 'header' ? 'new-download-dialog header-download-dialog' : 'new-download-dialog modal'} role="dialog" aria-modal={placement === 'modal' || undefined} aria-label="新建下载" onKeyDown={trapModalFocus}>
      <div className="launcher-title"><span>新建下载</span><button aria-label={placement === 'modal' ? '关闭新建下载' : '收起下载输入'} disabled={isSubmitting} onClick={requestClose}>×</button></div>
      <form onSubmit={submit} aria-busy={isSubmitting}>
        {placement !== 'launcher' && <label htmlFor="download-input">下载内容</label>}
        <textarea ref={inputRef} id="download-input" aria-label="下载内容" value={input} disabled={isSubmitting || !!batch} onChange={(event) => {
          const value = event.target.value;
          if (value.trim() !== input.trim()) { setError(null); setAccessCode(''); setAccessCodePrompt(null); }
          setInput(value);
        }} placeholder="粘贴链接、分享地址、文本或网页地址" />
        <div className={`new-download-actions${accessCodePrompt ? ' cloud-toolbar' : ''}`}>
          {placement === 'launcher' && <button type="button" onClick={openTaskList} disabled={isSubmitting}>打开任务列表</button>}
          {batch && <span role="status">正在处理 {batch.index + 1}/{batch.entries.length}</span>}
          {batchEntry?.valid && <span>已识别有效下载链接，确认后才会创建任务。</span>}
          {batchEntry && !batchEntry.valid && <span role="alert" className="parse-failure">{batchEntry.message}</span>}
          {error && <span role="alert" className="parse-failure">{error}</span>}
          {accessCodePrompt ? <div className="new-download-tab-access-code">
            <span id="share-access-code-prompt" role="status">{accessCodePrompt}</span>
            <input ref={accessCodeRef} id="share-access-code" aria-label="访问码" aria-describedby="share-access-code-prompt" type="password" autoComplete="off" value={accessCode} disabled={isSubmitting} onChange={(event) => setAccessCode(event.target.value)} />
            {parseAction}
          </div> : parseAction}
        </div>
      </form>
    </aside>
  );

  if (placement === 'launcher' || placement === 'header') return dialog;
  return <div className="modal-backdrop new-download-backdrop" data-testid="new-download-backdrop" onClick={(event) => { if (event.target === event.currentTarget) requestClose(); }}>{dialog}</div>;
}
