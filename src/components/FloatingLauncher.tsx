import { DragEvent as ReactDragEvent, MouseEvent as ReactMouseEvent, PointerEvent, useRef, useState, useSyncExternalStore } from 'react';
import { getResolvedTheme, subscribeTheme } from '../lib/theme';
import type { ScanDownloadResult } from '../lib/types';

export type LauncherPointerPosition = { x: number; y: number };

interface FloatingLauncherProps {
  onScan?(input: string): Promise<ScanDownloadResult[]>;
  onActivateTaskList?(): void | Promise<void>;
  onDroppedInput?(input: string): void | Promise<void>;
  onStageDroppedTorrent?(file: File): Promise<string>;
  onDragStart?(pointer: LauncherPointerPosition, grabOffset?: LauncherPointerPosition): void;
  onDragMove?(pointer: LauncherPointerPosition): void;
  onDragEnd?(pointer: LauncherPointerPosition): void;
  onPrimaryClick?(): void;
  onContextMenu?(): void;
}

const dragThreshold = 5;

function screenPointerPosition(event: { screenX: number; screenY: number }): LauncherPointerPosition {
  // clientX/clientY are relative to this WebView. As the launcher window
  // moves, those values change even when the physical cursor is stationary,
  // creating a feedback loop that makes the icon oscillate. Screen-space
  // coordinates remain stable throughout the captured drag.
  return { x: event.screenX, y: event.screenY };
}

function draggedText(dataTransfer: DataTransfer): string {
  return [...new Set(['text/uri-list', 'text/plain']
    .map((type) => dataTransfer.getData(type).trim())
    .filter(Boolean))]
    .join('\n');
}

function isTorrentFile(file: File): boolean {
  return file.name.trim().toLocaleLowerCase().endsWith('.torrent');
}

function LauncherMark() {
  // The original 1024px export includes a large transparent margin.  Keep
  // the hit target at 64px, but crop that export here so the visible launcher
  // glyph itself is a true 64px-tall icon.
  return <svg className="launcher-mark" viewBox="175 123 674 778" aria-hidden="true">
    <defs>
      <linearGradient id="launcher-g-top" x1="30%" y1="0%" x2="70%" y2="100%">
        <stop className="launcher-top-start" offset="0%" stopColor="#C4DED4" />
        <stop className="launcher-top-end" offset="100%" stopColor="#ACC6BE" />
      </linearGradient>
      <linearGradient id="launcher-g-left" x1="0%" y1="10%" x2="100%" y2="90%">
        <stop className="launcher-left-start" offset="0%" stopColor="#ACC6BE" />
        <stop className="launcher-left-end" offset="100%" stopColor="#E6F2EC" />
      </linearGradient>
      <linearGradient id="launcher-g-right" x1="100%" y1="0%" x2="0%" y2="100%">
        <stop className="launcher-right-start" offset="0%" stopColor="#E6F2EC" />
        <stop className="launcher-right-end" offset="100%" stopColor="#9EC4B8" />
      </linearGradient>
      <linearGradient id="launcher-g-rim" x1="30%" y1="0%" x2="70%" y2="100%">
        <stop className="launcher-rim-start" offset="0%" stopColor="#D6EAE2" />
        <stop className="launcher-rim-end" offset="100%" stopColor="#ACC6BE" />
      </linearGradient>
      <clipPath id="launcher-hex-clip"><path d="M 480.155,158.776 Q 512.000,140.390 543.845,158.776 L 801.978,307.809 Q 833.823,326.195 833.823,362.967 L 833.823,661.033 Q 833.823,697.805 801.978,716.191 L 543.845,865.224 Q 512.000,883.610 480.155,865.224 L 222.022,716.191 Q 190.177,697.805 190.177,661.033 L 190.177,362.967 Q 190.177,326.195 222.022,307.809 Z" /></clipPath>
      <clipPath id="launcher-face-left"><path d="M 175.012,317.440 L 175.012,706.560 L 512.000,901.120 L 512.000,512.000 Z" /></clipPath>
      <clipPath id="launcher-face-right"><path d="M 848.988,317.440 L 848.988,706.560 L 512.000,901.120 L 512.000,512.000 Z" /></clipPath>
    </defs>
    <path className="launcher-rim" d="M 476.616,143.309 Q 512.000,122.880 547.384,143.309 L 813.604,297.011 Q 848.988,317.440 848.988,358.298 L 848.988,665.702 Q 848.988,706.560 813.604,726.989 L 547.384,880.691 Q 512.000,901.120 476.616,880.691 L 210.396,726.989 Q 175.012,706.560 175.012,665.702 L 175.012,358.298 Q 175.012,317.440 210.396,297.011 Z" fill="url(#launcher-g-rim)" />
    <g clipPath="url(#launcher-hex-clip)">
      <path d="M 175.012,317.440 L 512.000,122.880 L 848.988,317.440 L 512.000,512.000 Z" fill="url(#launcher-g-top)" />
      <path d="M 175.012,317.440 L 175.012,706.560 L 512.000,901.120 L 512.000,512.000 Z" fill="url(#launcher-g-left)" />
      <path d="M 848.988,317.440 L 848.988,706.560 L 512.000,901.120 L 512.000,512.000 Z" fill="url(#launcher-g-right)" />
      <g clipPath="url(#launcher-face-left)">
        <path className="launcher-logo launcher-logo-left" d="M 333.396,492.544 L 512.000,595.661 L 690.604,492.544 L 690.604,408.883 L 512.000,512.000 L 333.396,408.883 Z" fill="#FFFFFF" />
        <path className="launcher-logo launcher-logo-left" d="M 333.396,644.301 L 512.000,747.418 L 690.604,644.301 L 690.604,560.640 L 512.000,663.757 L 333.396,560.640 Z" fill="#FFFFFF" />
      </g>
      <g clipPath="url(#launcher-face-right)">
        <path className="launcher-logo launcher-logo-right" d="M 333.396,492.544 L 512.000,595.661 L 690.604,492.544 L 690.604,408.883 L 512.000,512.000 L 333.396,408.883 Z" fill="#F4FAF6" />
        <path className="launcher-logo launcher-logo-right" d="M 333.396,644.301 L 512.000,747.418 L 690.604,644.301 L 690.604,560.640 L 512.000,663.757 L 333.396,560.640 Z" fill="#F4FAF6" />
      </g>
    </g>
  </svg>;
}

export function FloatingLauncher({ onScan, onActivateTaskList, onDroppedInput, onStageDroppedTorrent, onDragStart, onDragMove, onDragEnd, onPrimaryClick, onContextMenu }: FloatingLauncherProps) {
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const dragGrabOffset = useRef<LauncherPointerPosition>();
  const dragPointerId = useRef<number | null>(null);
  const dragging = useRef(false);
  const suppressClick = useRef(false);
  const resolvedTheme = useSyncExternalStore(subscribeTheme, getResolvedTheme, getResolvedTheme);
  const dropInFlight = useRef(false);
  const [dropTarget, setDropTarget] = useState(false);

  function beginPointer(event: PointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    dragOrigin.current = screenPointerPosition(event);
    dragGrabOffset.current = { x: event.clientX, y: event.clientY };
    dragPointerId.current = event.pointerId;
    dragging.current = false;
    suppressClick.current = false;
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* synthetic events have no native pointer */ }
  }

  function movePointer(event: PointerEvent<HTMLDivElement>) {
    const origin = dragOrigin.current;
    if (!origin || dragPointerId.current !== event.pointerId) return;
    const pointer = screenPointerPosition(event);
    if (!dragging.current) {
      if (Math.hypot(pointer.x - origin.x, pointer.y - origin.y) < dragThreshold) return;
      dragging.current = true;
      suppressClick.current = true;
      onDragStart?.(origin, dragGrabOffset.current);
    }
    onDragMove?.(pointer);
  }

  function endPointer(event: PointerEvent<HTMLDivElement>) {
    if (dragPointerId.current !== event.pointerId) return;
    if (dragging.current) onDragEnd?.(screenPointerPosition(event));
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
    dragOrigin.current = screenPointerPosition(event);
    dragGrabOffset.current = { x: event.clientX, y: event.clientY };
    dragPointerId.current = -1;
    dragging.current = false;
    suppressClick.current = false;
  }

  function moveMouseFallback(event: ReactMouseEvent<HTMLDivElement>) {
    if (dragPointerId.current !== -1) return;
    const origin = dragOrigin.current;
    if (!origin) return;
    const pointer = screenPointerPosition(event);
    if (!dragging.current) {
      if (Math.hypot(pointer.x - origin.x, pointer.y - origin.y) < dragThreshold) return;
      dragging.current = true;
      suppressClick.current = true;
      onDragStart?.(origin, dragGrabOffset.current);
    }
    onDragMove?.(pointer);
  }

  function endMouseFallback(event: ReactMouseEvent<HTMLDivElement>) {
    if (dragPointerId.current !== -1) return;
    if (dragging.current) onDragEnd?.(screenPointerPosition(event));
    dragging.current = false;
    dragOrigin.current = null;
    dragPointerId.current = null;
  }

  function clearSuppressedClick() {
    dragOrigin.current = null;
    if (suppressClick.current) {
      suppressClick.current = false;
      return;
    }
    onPrimaryClick?.();
  }

  function activateTaskList() {
    dragOrigin.current = null;
    if (suppressClick.current) return;
    void onActivateTaskList?.();
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
        onClick={clearSuppressedClick}
        onDoubleClick={activateTaskList}
        onContextMenu={(event) => {
          event.preventDefault();
          dragOrigin.current = null;
          onContextMenu?.();
        }}
      >
        <button className="launcher-trigger" data-size="64" data-icon-variant={resolvedTheme} aria-label="打开下载输入">
          <LauncherMark />
        </button>
      </div>
    </>
  );
}
