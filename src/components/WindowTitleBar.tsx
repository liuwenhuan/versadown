import type { MouseEvent } from 'react';
import type { MainWindowAction } from '../lib/mainWindow';

type WindowTitleBarProps = {
  title: string;
  maximized: boolean;
  onAction: (action: MainWindowAction) => void;
};

function stopControlPointer(event: MouseEvent<HTMLButtonElement>) {
  event.stopPropagation();
}

export function WindowTitleBar({ title, maximized, onAction }: WindowTitleBarProps) {
  return (
    <header className={`window-title-bar${maximized ? ' is-maximized' : ''}`}>
      <div
        className="window-title-drag-region"
        data-tauri-drag-region
        data-testid="titlebar-drag-region"
        onDoubleClick={() => onAction('toggle-maximize')}
      >
        <span className="window-title-brand" data-tauri-drag-region aria-hidden="true">↓</span>
        <span className="window-title-copy" data-tauri-drag-region>ALLDOWNLOADER</span>
        <span className="window-title-separator" data-tauri-drag-region aria-hidden="true">/</span>
        <span className="window-title-text" data-tauri-drag-region>{title}</span>
      </div>
      <div className="window-title-controls" aria-label="窗口控制">
        <button
          type="button"
          aria-label="最小化下载管理器"
          onPointerDown={stopControlPointer}
          onClick={() => onAction('minimize')}
        >
          <span aria-hidden="true">—</span>
        </button>
        <button
          type="button"
          aria-label={maximized ? '还原下载管理器' : '最大化下载管理器'}
          onPointerDown={stopControlPointer}
          onClick={() => onAction('toggle-maximize')}
        >
          <span aria-hidden="true">{maximized ? '❐' : '□'}</span>
        </button>
        <button
          className="window-title-close"
          type="button"
          aria-label="关闭下载管理器"
          onPointerDown={stopControlPointer}
          onClick={() => onAction('hide')}
        >
          <span aria-hidden="true">×</span>
        </button>
      </div>
    </header>
  );
}
