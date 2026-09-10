import { FormEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ParseDownloadResult } from '../lib/types';

export type NewDownloadDialogPlacement = 'launcher' | 'modal';

export interface NewDownloadDialogProps {
  open: boolean;
  placement?: NewDownloadDialogPlacement;
  onParse(input: string, accessCode?: string): void | Promise<ParseDownloadResult | void>;
  onClose(): void;
}

export function NewDownloadDialog({ open, placement = 'launcher', onParse, onClose }: NewDownloadDialogProps) {
  const [input, setInput] = useState('');
  const [accessCode, setAccessCode] = useState('');
  const [accessCodePrompt, setAccessCodePrompt] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
    if (!open) { setIsSubmitting(false); setError(null); setAccessCode(''); setAccessCodePrompt(null); }
  }, [open]);

  useEffect(() => {
    if (accessCodePrompt) accessCodeRef.current?.focus();
  }, [accessCodePrompt]);

  function requestClose() {
    if (!isSubmitting) {
      setAccessCode('');
      setAccessCodePrompt(null);
      onCloseRef.current();
    }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (isSubmitting) return;
    const text = input.trim();
    if (!text) return;
    setIsSubmitting(true);
    setError(null);
    try {
      const result = await onParse(text, accessCode.trim() || undefined);
      if (result?.shareAccessCodeRequired) {
        setAccessCodePrompt(result.shareAccessCodeRequired.message);
        setIsSubmitting(false);
        return;
      }
      setInput('');
      setAccessCode('');
      setAccessCodePrompt(null);
      onCloseRef.current();
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

  const dialog = (
    <aside ref={dialogRef} className={placement === 'launcher' ? 'launcher-panel new-download-dialog' : 'new-download-dialog modal'} role="dialog" aria-modal={placement === 'modal' || undefined} aria-label="新建下载" onKeyDown={trapModalFocus}>
      <div className="launcher-title"><strong>新建下载</strong><button aria-label={placement === 'modal' ? '关闭新建下载' : '收起下载输入'} disabled={isSubmitting} onClick={requestClose}>×</button></div>
      <form onSubmit={submit} aria-busy={isSubmitting}>
        <label htmlFor="download-input">下载内容</label>
        <textarea ref={inputRef} id="download-input" value={input} disabled={isSubmitting} onChange={(event) => setInput(event.target.value)} placeholder="粘贴链接、分享地址、文本或网页地址" />
        {accessCodePrompt && <label htmlFor="share-access-code">访问码<input ref={accessCodeRef} id="share-access-code" type="password" autoComplete="off" value={accessCode} disabled={isSubmitting} onChange={(event) => setAccessCode(event.target.value)} /></label>}
        <p>标准链接直接规则解析；复杂内容才会请求 AI 辅助。</p>
        {accessCodePrompt && <p role="status">{accessCodePrompt}</p>}
        {error && <p role="alert">{error}</p>}
        <button className="primary" type="submit" disabled={isSubmitting}>{isSubmitting ? '正在解析…' : '开始解析下载'}</button>
      </form>
    </aside>
  );

  if (placement === 'launcher') return dialog;
  return <div className="modal-backdrop new-download-backdrop" data-testid="new-download-backdrop" onClick={(event) => { if (event.target === event.currentTarget) requestClose(); }}>{dialog}</div>;
}
