import { FormEvent, MouseEvent, useRef, useState } from 'react';

interface FloatingLauncherProps {
  activeCount: number;
  onParse(input: string): void;
  onExpandedChange?(expanded: boolean): void;
  onDragStart?(): void;
  onContextMenu?(): void;
}

const dragThreshold = 5;

export function FloatingLauncher({ activeCount, onParse, onExpandedChange, onDragStart, onContextMenu }: FloatingLauncherProps) {
  const [expanded, setExpanded] = useState(false);
  const [input, setInput] = useState('');
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const suppressClick = useRef(false);

  function setLauncherExpanded(next: boolean) {
    setExpanded(next);
    onExpandedChange?.(next);
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const text = input.trim();
    if (!text) return;
    onParse(text);
    setInput('');
    setLauncherExpanded(false);
  }

  function beginPointer(event: MouseEvent<HTMLButtonElement>) {
    if (event.button !== 0) return;
    dragOrigin.current = { x: event.clientX, y: event.clientY };
    suppressClick.current = false;
  }

  function movePointer(event: MouseEvent<HTMLButtonElement>) {
    const origin = dragOrigin.current;
    if (!origin || event.buttons !== 1 || suppressClick.current) return;
    if (Math.hypot(event.clientX - origin.x, event.clientY - origin.y) < dragThreshold) return;
    suppressClick.current = true;
    onDragStart?.();
  }

  function openFromClick() {
    dragOrigin.current = null;
    if (suppressClick.current) {
      suppressClick.current = false;
      return;
    }
    setLauncherExpanded(true);
  }

  if (!expanded) {
    return (
      <button
        className="launcher-trigger"
        data-size="48"
        aria-label="打开下载输入"
        onMouseDown={beginPointer}
        onMouseMove={movePointer}
        onMouseUp={() => { dragOrigin.current = null; }}
        onClick={openFromClick}
        onContextMenu={(event) => {
          event.preventDefault();
          dragOrigin.current = null;
          onContextMenu?.();
        }}
      >
        <span aria-hidden="true">↓</span>{activeCount > 0 && <i>{activeCount}</i>}
      </button>
    );
  }

  return (
    <aside className="launcher-panel" aria-label="快速解析下载">
      <div className="launcher-title"><strong>快速下载</strong><button aria-label="收起下载输入" onClick={() => setLauncherExpanded(false)}>×</button></div>
      <form onSubmit={submit}>
        <label htmlFor="download-input">下载内容</label>
        <textarea id="download-input" value={input} onChange={(event) => setInput(event.target.value)} placeholder="粘贴链接、分享地址、文本或网页地址" autoFocus />
        <p>标准链接直接规则解析；复杂内容才会请求 AI 辅助。</p>
        <button className="primary" type="submit">开始解析下载</button>
      </form>
    </aside>
  );
}
