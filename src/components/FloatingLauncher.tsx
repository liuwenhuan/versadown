import { DragEvent as ReactDragEvent, MouseEvent as ReactMouseEvent, PointerEvent, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react';
import { NewDownloadDialog } from './NewDownloadDialog';
import { getResolvedTheme, subscribeTheme } from '../lib/theme';
import type { ScanDownloadResult } from '../lib/types';
import type { LauncherSurfaceMode } from './LauncherWindow';
import type { LauncherPopupPlacement } from '../lib/launcherWindow';

export type LauncherPointerPosition = { x: number; y: number };

interface FloatingLauncherProps {
  onParse(input: string): void | Promise<void>;
  expanded?: boolean;
  mode?: LauncherSurfaceMode;
  placement?: LauncherPopupPlacement;
  onModeChange?(mode: LauncherSurfaceMode): void;
  onQuickAddElement?(element: HTMLElement | null): void;
  onScan?(input: string): Promise<ScanDownloadResult[]>;
  onOpenTaskList?(): void | Promise<void>;
  onDroppedInput?(input: string): void | Promise<void>;
  onStageDroppedTorrent?(file: File): Promise<string>;
  onExpandedChange?(expanded: boolean): void;
  onDragStart?(pointer: LauncherPointerPosition): void;
  onDragMove?(pointer: LauncherPointerPosition): void;
  onDragEnd?(pointer: LauncherPointerPosition): void;
  onContextMenu?(): void;
}

const dragThreshold = 5;

function draggedText(dataTransfer: DataTransfer): string {
  return [...new Set(['text/uri-list', 'text/plain']
    .map((type) => dataTransfer.getData(type).trim())
    .filter(Boolean))]
    .join('\n');
}

function isTorrentFile(file: File): boolean {
  return file.name.trim().toLocaleLowerCase().endsWith('.torrent');
}

function LauncherMark({ variant }: { variant: 'light' | 'dark' }) {
  const shell = variant === 'dark' ? '#2e3239' : '#8bc2b3';
  const chevron = variant === 'dark' ? '#c4e3d1' : '#ffffff';
  return (
    <svg className="launcher-mark" viewBox="0 0 128 128" aria-hidden="true" focusable="false">
      <path className="launcher-mark-shell" d="M64 10 112 37.5v53L64 118 16 90.5v-53Z" fill={shell} stroke="none" />
      <path className="launcher-mark-chevron" d="M38 49.5 64 64.5l26-15v9L64 73.5l-26-15Zm0 19L64 83.5l26-15v9L64 92.5l-26-15Z" fill={chevron} stroke="none" />
    </svg>
  );
}

export function FloatingLauncher({ onParse, expanded: expandedProp, mode, placement, onModeChange, onQuickAddElement, onScan, onOpenTaskList, onDroppedInput, onStageDroppedTorrent, onExpandedChange, onDragStart, onDragMove, onDragEnd, onContextMenu }: FloatingLauncherProps) {
  const [uncontrolledExpanded, setUncontrolledExpanded] = useState(false);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const dragPointerId = useRef<number | null>(null);
  const dragging = useRef(false);
  const suppressClick = useRef(false);
  const resolvedTheme = useSyncExternalStore(subscribeTheme, getResolvedTheme, getResolvedTheme);
  const expanded = mode ? mode === 'quick-add' : expandedProp ?? uncontrolledExpanded;
  const intent = useRef<LauncherSurfaceMode>(mode ?? (expanded ? 'quick-add' : 'compact'));
  const quickAddRef = useRef<HTMLDivElement>(null);
  const dropInFlight = useRef(false);
  const [dropTarget, setDropTarget] = useState(false);

  useLayoutEffect(() => {
    intent.current = mode ?? (expanded ? 'quick-add' : 'compact');
    onQuickAddElement?.(quickAddRef.current?.querySelector<HTMLElement>('.launcher-panel') ?? null);
  }, [mode, expanded, placement, onQuickAddElement]);

  function setLauncherExpanded(next: boolean) {
    intent.current = next ? 'quick-add' : 'compact';
    if (mode === undefined && expandedProp === undefined) setUncontrolledExpanded(next);
    onModeChange?.(intent.current);
    onExpandedChange?.(next);
  }

  function beginPointer(event: PointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    dragOrigin.current = { x: event.clientX, y: event.clientY };
    dragPointerId.current = event.pointerId;
    dragging.current = false;
    suppressClick.current = false;
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* synthetic events have no native pointer */ }
  }

  function movePointer(event: PointerEvent<HTMLDivElement>) {
    const origin = dragOrigin.current;
    if (!origin || dragPointerId.current !== event.pointerId) return;
    if (!dragging.current) {
      if (Math.hypot(event.clientX - origin.x, event.clientY - origin.y) < dragThreshold) return;
      dragging.current = true;
      suppressClick.current = true;
      setLauncherExpanded(false);
      onDragStart?.(origin);
    }
    onDragMove?.({ x: event.clientX, y: event.clientY });
  }

  function endPointer(event: PointerEvent<HTMLDivElement>) {
    if (dragPointerId.current !== event.pointerId) return;
    if (dragging.current) onDragEnd?.({ x: event.clientX, y: event.clientY });
    dragging.current = false;
    dragOrigin.current = null;
    dragPointerId.current = null;
    try { event.currentTarget.releasePointerCapture(event.pointerId); } catch { /* pointer may already be released */ }
  }

  // WebView2 dispatches Pointer Events before compatibility mouse events, so
  // this fallback remains dormant there once pointer capture is active.  It
  // keeps mouse-only runtimes (including the lightweight DOM test environment)
  // functional without weakening the captured production path.
  function beginMouseFallback(event: ReactMouseEvent<HTMLDivElement>) {
    if (dragPointerId.current !== null || event.button !== 0) return;
    dragOrigin.current = { x: event.clientX, y: event.clientY };
    dragPointerId.current = -1;
    dragging.current = false;
    suppressClick.current = false;
  }

  function moveMouseFallback(event: ReactMouseEvent<HTMLDivElement>) {
    if (dragPointerId.current !== -1) return;
    const origin = dragOrigin.current;
    if (!origin) return;
    if (!dragging.current) {
      if (Math.hypot(event.clientX - origin.x, event.clientY - origin.y) < dragThreshold) return;
      dragging.current = true;
      suppressClick.current = true;
      setLauncherExpanded(false);
      onDragStart?.(origin);
    }
    onDragMove?.({ x: event.clientX, y: event.clientY });
  }

  function endMouseFallback(event: ReactMouseEvent<HTMLDivElement>) {
    if (dragPointerId.current !== -1) return;
    if (dragging.current) onDragEnd?.({ x: event.clientX, y: event.clientY });
    dragging.current = false;
    dragOrigin.current = null;
    dragPointerId.current = null;
  }

  function openFromClick() {
    dragOrigin.current = null;
    if (suppressClick.current) {
      suppressClick.current = false;
      return;
    }
    setLauncherExpanded(intent.current !== 'quick-add');
  }

  function beginDrop(event: ReactDragEvent<HTMLDivElement>) {
    event.preventDefault();
    event.dataTransfer.dropEffect = 'copy';
    setDropTarget(true);
  }

  function leaveDrop(event: ReactDragEvent<HTMLDivElement>) {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropTarget(false);
  }

  async function acceptDrop(event: ReactDragEvent<HTMLDivElement>) {
    event.preventDefault();
    event.stopPropagation();
    setDropTarget(false);
    if (dropInFlight.current) return;
    dropInFlight.current = true;
    try {
      const inputs: string[] = [];
      const torrents = Array.from(event.dataTransfer.files).filter(isTorrentFile);
      if (torrents.length && onStageDroppedTorrent) {
        inputs.push(...await Promise.all(torrents.map((file) => onStageDroppedTorrent(file))));
      }
      const text = draggedText(event.dataTransfer);
      if (text && onScan) {
        const scanned = await onScan(text);
        inputs.push(...scanned.filter((item) => item.valid).map((item) => item.input));
      }
      const validInputs = [...new Set(inputs.map((input) => input.trim()).filter(Boolean))];
      if (!validInputs.length || !onDroppedInput) return;
      setLauncherExpanded(false);
      await onDroppedInput(validInputs.join('\n'));
    } finally {
      dropInFlight.current = false;
    }
  }

  return (
    <>
      <div
        className={`launcher-interaction-target${dropTarget ? ' launcher-drop-target' : ''}`}
        onPointerDown={beginPointer}
        onPointerMove={movePointer}
        onPointerUp={endPointer}
        onPointerCancel={endPointer}
        onMouseDown={beginMouseFallback}
        onMouseMove={moveMouseFallback}
        onMouseUp={endMouseFallback}
        onDragEnter={beginDrop}
        onDragOver={beginDrop}
        onDragLeave={leaveDrop}
        onDrop={(event) => { void acceptDrop(event); }}
        onClick={openFromClick}
        onContextMenu={(event) => {
          event.preventDefault();
          dragOrigin.current = null;
          intent.current = 'context-menu';
          onContextMenu?.();
        }}
      >
        <button className="launcher-trigger" data-size="64" data-icon-variant={resolvedTheme} aria-label="打开下载输入">
          <LauncherMark variant={resolvedTheme} />
        </button>
      </div>
      {expanded && <div ref={quickAddRef} className="launcher-quick-add-container"><NewDownloadDialog open placement="launcher" onParse={onParse} onScan={onScan} onClose={() => setLauncherExpanded(false)} onOpenTaskList={() => { void onOpenTaskList?.(); setLauncherExpanded(false); }} /></div>}
    </>
  );
}
