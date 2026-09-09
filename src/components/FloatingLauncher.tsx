import { MouseEvent, useRef, useState } from 'react';
import { NewDownloadDialog } from './NewDownloadDialog';

interface FloatingLauncherProps {
  activeCount: number;
  liveSpeed?: { label: string; value: string };
  onParse(input: string): void | Promise<void>;
  onExpandedChange?(expanded: boolean): void;
  onDragStart?(): void;
  onContextMenu?(): void;
}

const dragThreshold = 5;

export function FloatingLauncher({ activeCount, liveSpeed, onParse, onExpandedChange, onDragStart, onContextMenu }: FloatingLauncherProps) {
  const [expanded, setExpanded] = useState(false);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const suppressClick = useRef(false);

  function setLauncherExpanded(next: boolean) {
    setExpanded(next);
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
    setLauncherExpanded(true);
  }

  if (!expanded) {
    return (
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
        <button className="launcher-trigger" data-size="48" aria-label="打开下载输入">
          <span aria-hidden="true">↓</span>{activeCount > 0 && <i>{activeCount}</i>}
        </button>
        {liveSpeed && <span className="launcher-live-indicator" aria-label={liveSpeed.label}><span>↓</span>{liveSpeed.value}<small>/s</small></span>}
      </div>
    );
  }

  return <NewDownloadDialog open onParse={onParse} onClose={() => setLauncherExpanded(false)} />;
}
