import { MouseEvent, useRef, useState, useSyncExternalStore } from 'react';
import { NewDownloadDialog } from './NewDownloadDialog';
import { getResolvedTheme, subscribeTheme } from '../lib/theme';
import type { ScanDownloadResult } from '../lib/types';

interface FloatingLauncherProps {
  onParse(input: string): void | Promise<void>;
  expanded?: boolean;
  onScan?(input: string): Promise<ScanDownloadResult[]>;
  onOpenTaskList?(): void | Promise<void>;
  onExpandedChange?(expanded: boolean): void;
  onDragStart?(): void;
  onContextMenu?(): void;
}

const dragThreshold = 5;

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

export function FloatingLauncher({ onParse, expanded: expandedProp, onScan, onOpenTaskList, onExpandedChange, onDragStart, onContextMenu }: FloatingLauncherProps) {
  const [uncontrolledExpanded, setUncontrolledExpanded] = useState(false);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const suppressClick = useRef(false);
  const resolvedTheme = useSyncExternalStore(subscribeTheme, getResolvedTheme, getResolvedTheme);
  const expanded = expandedProp ?? uncontrolledExpanded;

  function setLauncherExpanded(next: boolean) {
    if (expandedProp === undefined) setUncontrolledExpanded(next);
    onExpandedChange?.(next);
  }

  function beginPointer(event: MouseEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    dragOrigin.current = { x: event.clientX, y: event.clientY };
    suppressClick.current = false;
  }

  function movePointer(event: MouseEvent<HTMLDivElement>) {
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
    setLauncherExpanded(!expanded);
  }

  return (
    <>
      <div
        className="launcher-interaction-target"
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
        <button className="launcher-trigger" data-size="64" data-icon-variant={resolvedTheme} aria-label="打开下载输入">
          <LauncherMark variant={resolvedTheme} />
        </button>
      </div>
      {expanded && <NewDownloadDialog open onParse={onParse} onScan={onScan} onClose={() => setLauncherExpanded(false)} onOpenTaskList={() => { void onOpenTaskList?.(); setLauncherExpanded(false); }} />}
    </>
  );
}
