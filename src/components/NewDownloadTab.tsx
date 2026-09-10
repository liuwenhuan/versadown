import { FormEvent, useRef, useState } from 'react';
import { ParseDownloadResult } from '../lib/types';

export interface NewDownloadTabProps {
  onParse(input: string, accessCode?: string): Promise<ParseDownloadResult>;
  onAwaitConfirmation(): Promise<boolean>;
  onComplete(): void;
}

export function splitDownloadInputs(value: string): string[] {
  return value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
}

export function NewDownloadTab({ onParse, onAwaitConfirmation, onComplete }: NewDownloadTabProps) {
  const [input, setInput] = useState('');
  const [accessCode, setAccessCode] = useState('');
  const [accessCodePrompt, setAccessCodePrompt] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [progress, setProgress] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pendingBatch = useRef<{ inputs: string[]; index: number } | null>(null);

  async function processBatch(inputs: string[], startIndex = 0, firstAccessCode?: string) {
    setIsSubmitting(true);
    setError(null);
    try {
      for (let index = startIndex; index < inputs.length; index += 1) {
        setProgress(`正在解析 ${index + 1}/${inputs.length}`);
        const result = await onParse(inputs[index], index === startIndex ? firstAccessCode : undefined);
        if (result.shareAccessCodeRequired) {
          pendingBatch.current = { inputs, index };
          setAccessCodePrompt(result.shareAccessCodeRequired.message);
          setProgress(null);
          setIsSubmitting(false);
          return;
        }
        setAccessCode('');
        setAccessCodePrompt(null);
        pendingBatch.current = null;
        if (result.confirmation && !(await onAwaitConfirmation())) {
          setProgress(null);
          setIsSubmitting(false);
          return;
        }
      }
      setInput('');
      setProgress(null);
      setIsSubmitting(false);
      onComplete();
    } catch (reason) {
      setProgress(null);
      setIsSubmitting(false);
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (isSubmitting) return;
    const pending = pendingBatch.current;
    if (pending) {
      const code = accessCode.trim();
      if (!code) return;
      void processBatch(pending.inputs, pending.index, code);
      return;
    }
    const inputs = splitDownloadInputs(input);
    if (inputs.length) void processBatch(inputs);
  }

  return <section className="new-download-tab-panel" role="tabpanel" aria-label="新建下载">
    <form onSubmit={submit} aria-busy={isSubmitting}>
      <textarea
        aria-label="下载链接"
        value={input}
        disabled={isSubmitting || Boolean(accessCodePrompt)}
        onChange={(event) => setInput(event.target.value)}
        placeholder="粘贴下载链接，多个链接请分行输入"
        autoFocus
      />
      {accessCodePrompt && <label className="new-download-tab-access-code">访问码<input type="password" autoComplete="off" aria-label="访问码" value={accessCode} disabled={isSubmitting} onChange={(event) => setAccessCode(event.target.value)} autoFocus /></label>}
      <footer>
        <span role={error ? 'alert' : 'status'}>{error ?? accessCodePrompt ?? progress}</span>
        <button className="primary" type="submit" disabled={isSubmitting || (accessCodePrompt ? !accessCode.trim() : splitDownloadInputs(input).length === 0)}>
          {isSubmitting ? progress ?? '正在解析…' : accessCodePrompt ? '提交访问码' : '开始解析下载'}
        </button>
      </footer>
    </form>
  </section>;
}
