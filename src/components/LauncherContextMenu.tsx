import { useCallback, useEffect, useRef, useState } from 'react';
import type { LauncherVisibility } from '../lib/types';

export type LauncherMenuAction = 'hide_manager' | 'new_download' | 'start_all' | 'pause_all' | 'quit';

interface LauncherContextMenuProps {
  downloadSpeed: string;
  managerVisible: boolean;
  visibility: LauncherVisibility;
  onAction(action: LauncherMenuAction): void;
  onVisibilityChange(visibility: LauncherVisibility): void;
  onClose(): void;
  onElement(element: HTMLElement | null): void;
}

function LauncherMenuIcon({ action }: { action: LauncherMenuAction | 'settings' | 'back' | 'always' | 'downloading' | 'hidden' }) {
  const paths: Record<typeof action, React.ReactNode> = {
    hide_manager: <><path d="M4 7.5h16v9H4z" /><path d="m4 12 4 3.5h8l4-3.5" /></>,
    new_download: <><path d="M12 4v16M4 12h16" /></>,
    start_all: <><path d="m7 5 10 7-10 7z" /><path d="M4 5v14" /></>,
    pause_all: <><path d="M8 5v14M16 5v14" /></>,
    quit: <><path d="M10 4H6.5A2.5 2.5 0 0 0 4 6.5v11A2.5 2.5 0 0 0 6.5 20H10" /><path d="m14 8 4 4-4 4M8 12h10" /></>,
    settings: <><path d="M5 6h14M5 12h14M5 18h14" /><circle cx="9" cy="6" r="1.5" /><circle cx="15" cy="12" r="1.5" /><circle cx="11" cy="18" r="1.5" /></>,
    back: <path d="m14 5-7 7 7 7" />,
    always: <><circle cx="12" cy="12" r="7" /><path d="M12 8v4l2.5 1.5" /></>,
    downloading: <><path d="M12 4v11" /><path d="m7.5 11.5 4.5 4.5 4.5-4.5" /><path d="M5 20h14" /></>,
    hidden: <><path d="M3 12s3.2-5 9-5 9 5 9 5-3.2 5-9 5-9-5-9-5Z" /><circle cx="12" cy="12" r="2.5" /><path d="m4 4 16 16" /></>,
  };
  return <svg aria-hidden="true" viewBox="0 0 24 24">{paths[action]}</svg>;
}

const visibilityOptions: Array<{ value: LauncherVisibility; label: string; icon: 'always' | 'downloading' | 'hidden' }> = [
  { value: 'always', label: '总是显示', icon: 'always' },
  { value: 'downloading', label: '仅下载时显示', icon: 'downloading' },
  { value: 'hidden', label: '关闭显示', icon: 'hidden' },
];

export function LauncherContextMenu({ downloadSpeed, managerVisible, visibility, onAction, onVisibilityChange, onClose, onElement }: LauncherContextMenuProps) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const captureMenu = useCallback((element: HTMLDivElement | null) => {
    menuRef.current = element;
    onElement(element);
  }, [onElement]);

  useEffect(() => {
    menuRef.current?.querySelector<HTMLButtonElement>('[role^="menuitem"]')?.focus();
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    const closeForBlur = () => onClose();
    window.addEventListener('keydown', closeForEscape);
    window.addEventListener('blur', closeForBlur);
    return () => {
      window.removeEventListener('keydown', closeForEscape);
      window.removeEventListener('blur', closeForBlur);
    };
  }, [onClose, settingsOpen]);

  function moveFocus(event: React.KeyboardEvent<HTMLDivElement>) {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]') ?? []);
    if (!items.length) return;
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : event.key === 'ArrowDown' ? (current + 1) % items.length : (current - 1 + items.length) % items.length;
    items[next].focus();
  }

  const runAction = (action: LauncherMenuAction) => {
    onClose();
    onAction(action);
  };

  return <div className="launcher-menu-backdrop" onPointerDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <div ref={captureMenu} className="task-context-menu launcher-context-menu" role="menu" aria-label={settingsOpen ? '悬浮球显示设置' : '悬浮球菜单'} onKeyDown={moveFocus}>
      {settingsOpen ? <>
        <header><b>悬浮球显示</b><button type="button" aria-label="返回悬浮球菜单" onClick={() => setSettingsOpen(false)}><LauncherMenuIcon action="back" /></button></header>
        {visibilityOptions.map((option) => <div key={option.value}><button role="menuitemradio" aria-checked={visibility === option.value} className={visibility === option.value ? 'selected' : undefined} onClick={() => { onVisibilityChange(option.value); onClose(); }}><LauncherMenuIcon action={option.icon} /><span>{option.label}</span>{visibility === option.value && <i aria-hidden="true">✓</i>}</button></div>)}
      </> : <>
        <div className="launcher-menu-speed" aria-label="当前整体下载速度"><span>当前下载速度</span><strong>{downloadSpeed}</strong></div>
        <div><button role="menuitem" onClick={() => runAction('hide_manager')}><LauncherMenuIcon action="hide_manager" /><span>{managerVisible ? '隐藏主界面' : '显示主界面'}</span></button></div>
        <div><button role="menuitem" onClick={() => runAction('new_download')}><LauncherMenuIcon action="new_download" /><span>新建下载</span></button></div>
        <div><button role="menuitem" onClick={() => runAction('start_all')}><LauncherMenuIcon action="start_all" /><span>开始全部任务</span></button></div>
        <div><button role="menuitem" onClick={() => runAction('pause_all')}><LauncherMenuIcon action="pause_all" /><span>暂停全部任务</span></button></div>
        <div className="menu-separated"><button role="menuitem" aria-haspopup="menu" onClick={() => setSettingsOpen(true)}><LauncherMenuIcon action="settings" /><span>悬浮球显示</span><i aria-hidden="true">›</i></button></div>
        <div className="menu-separated"><button role="menuitem" className="danger" onClick={() => runAction('quit')}><LauncherMenuIcon action="quit" /><span>退出万能下载</span></button></div>
      </>}
    </div>
  </div>;
}
