import { FormEvent, KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { cloudRecycleBinDirectoryId, cloudRootDirectoryId, type CloudAccount, type CloudAuthorization, type CloudAuthorizationCapabilities, type CloudCredentials, type CloudDirectory, type CloudDirectoryDownloadFailure, type CloudDirectoryDownloadOutcome, type CloudFile, type CloudProvider } from '../lib/cloudTypes';

export type { CloudAccount, CloudAuthorization, CloudAuthorizationCapabilities, CloudCredentials, CloudDirectory, CloudDirectoryDownloadOutcome, CloudFile, CloudProvider } from '../lib/cloudTypes';

export interface CloudManagerClient {
  getCloudAuthorizationCapabilities(provider: CloudProvider): Promise<CloudAuthorizationCapabilities>;
  beginCloudAuthorization(provider: CloudProvider): Promise<CloudAuthorization>;
  completeCloudAuthorization(provider: CloudProvider): Promise<CloudAccount>;
  loginCloudWithPassword(provider: CloudProvider, credentials: CloudCredentials): Promise<CloudAccount>;
  getCloudAccount(provider: CloudProvider): Promise<CloudAccount>;
  listCloudDirectory(provider: CloudProvider, parentId?: string): Promise<CloudDirectory>;
  createCloudDirectory(provider: CloudProvider, parentId: string, name: string): Promise<CloudFile | void>;
  renameCloudFile(provider: CloudProvider, fileId: string, name: string): Promise<void>;
  moveCloudFiles(provider: CloudProvider, fileIds: string[], targetParentId: string): Promise<void>;
  copyCloudFiles(provider: CloudProvider, fileIds: string[], targetParentId: string): Promise<void>;
  trashCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  deleteCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  restoreCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  downloadCloudFile(provider: CloudProvider, fileId: string): Promise<unknown>;
  downloadCloudDirectory(provider: CloudProvider, directoryId: string, directoryName: string): Promise<CloudDirectoryDownloadOutcome>;
  logoutCloud(provider: CloudProvider): Promise<void>;
  pickAndUploadFiles(provider: CloudProvider, parentId: string): Promise<readonly CloudFile[] | void>;
}

export interface CloudManagerProps {
  provider: CloudProvider;
  client?: CloudManagerClient;
  initialAccounts?: readonly CloudAccount[];
  onDownloadTask?(task: unknown): void;
  authorizationOnly?: boolean;
  restoreOnMount?: boolean;
  showAccountTabs?: boolean;
  showSessionActions?: boolean;
  onAccountAuthorized?(account: CloudAccount): void;
  onAccountLoggedOut?(provider: CloudProvider): void;
}

type DirectoryState = 'idle' | 'loading' | 'ready' | 'empty' | 'error' | 'expired';
type Editor = 'create' | 'rename' | null;
type DestinationAction = 'move' | 'copy';
type OperationState = { kind: 'working' | 'success' | 'error'; message: string } | null;
interface Breadcrumb { id: string; name: string }
type CloudMenuAction = 'open' | 'restore' | 'download' | 'rename' | 'move' | 'copy' | 'details' | 'delete' | 'create' | 'upload' | 'refresh';
type CloudContextMenuState = { kind: 'selection'; item: CloudFile; x: number; y: number } | { kind: 'background'; x: number; y: number };
type CloudHeaderMenuAction = 'rename' | 'move' | 'copy' | 'details';

const providerLabel: Record<CloudProvider, string> = { quark: '夸克', pikpak: 'PikPak', baidu: '百度', tianyi: '天翼', aliyun: '阿里', xunlei: '迅雷', uc: 'UC' };
const fixedAuthorizationCapabilities: Record<CloudProvider, CloudAuthorizationCapabilities> = {
  quark: { provider: 'quark', web: true, password: false, passwordLoginMode: 'none' },
  pikpak: { provider: 'pikpak', web: true, password: true, passwordLoginMode: 'rustDirect' },
  baidu: { provider: 'baidu', web: true, password: true, passwordLoginMode: 'rustDirect' },
  tianyi: { provider: 'tianyi', web: true, password: true, passwordLoginMode: 'rustDirect' },
  aliyun: { provider: 'aliyun', web: true, password: false, passwordLoginMode: 'officialWebView' },
  xunlei: { provider: 'xunlei', web: true, password: false, passwordLoginMode: 'officialWebView' },
  uc: { provider: 'uc', web: true, password: false, passwordLoginMode: 'officialWebView' }
};
const authorizationInitialPollMs = 800;
const authorizationPollMs = 1_800;

const unavailableClient: CloudManagerClient = {
  getCloudAuthorizationCapabilities: () => Promise.reject(new Error('网盘连接器尚未接入')),
  beginCloudAuthorization: () => Promise.reject(new Error('网盘连接器尚未接入')),
  completeCloudAuthorization: () => Promise.reject(new Error('网盘连接器尚未接入')),
  loginCloudWithPassword: () => Promise.reject(new Error('网盘连接器尚未接入')),
  getCloudAccount: () => Promise.reject(new Error('网盘连接器尚未接入')),
  listCloudDirectory: () => Promise.reject(new Error('网盘连接器尚未接入')),
  createCloudDirectory: () => Promise.reject(new Error('网盘连接器尚未接入')),
  renameCloudFile: () => Promise.reject(new Error('网盘连接器尚未接入')),
  moveCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  copyCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  trashCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  deleteCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  restoreCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  downloadCloudFile: () => Promise.reject(new Error('网盘连接器尚未接入')),
  downloadCloudDirectory: () => Promise.reject(new Error('网盘连接器尚未接入')),
  logoutCloud: () => Promise.reject(new Error('网盘连接器尚未接入')),
  pickAndUploadFiles: () => Promise.reject(new Error('网盘连接器尚未接入'))
};

function isCredentialExpired(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && (error as { code?: unknown }).code === 'credentials_expired';
}

function isUnauthorized(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && ['unauthorized', 'not_authorized'].includes(String((error as { code?: unknown }).code));
}

function isAuthorizationPending(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && ['authorization_pending', 'waiting', 'not_ready'].includes(String((error as { code?: unknown }).code));
}

function requiresOfficialWebAuthorization(error: unknown) {
  if (typeof error !== 'object' || error === null) return false;
  const code = 'code' in error ? String((error as { code?: unknown }).code).toLowerCase() : '';
  const message = errorMessage(error, '').toLowerCase();
  return ['web_auth_required', 'captcha_required', 'captcha_invalid', 'verification_required', 'web_verification_required', 'risk_verification_required'].includes(code)
    || /captcha|verification required|网页验证|安全验证|风险验证/.test(message);
}

function isSessionRenewedRetryOperation(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && (error as { code?: unknown }).code === 'session_renewed_retry_operation';
}

function isTerminalWebAuthorizationError(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && ['provider_error', 'invalid_response', 'credentials_expired'].includes(String((error as { code?: unknown }).code));
}

function errorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'object' && error !== null && 'message' in error && typeof (error as { message?: unknown }).message === 'string') {
    return (error as { message: string }).message;
  }
  return fallback;
}

function readableSize(size?: number) {
  if (size === undefined) return '—';
  if (size < 1024) return `${size} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = size / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(1)} ${units[unitIndex]}`;
}

function formatCloudModifiedAt(value: unknown) {
  if (typeof value === 'string' && /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(value)) return value;

  const numericValue = typeof value === 'number' ? value : typeof value === 'string' && /^-?\d+(?:\.\d+)?$/.test(value) ? Number(value) : NaN;
  const timestamp = Number.isFinite(numericValue)
    ? Math.abs(numericValue) < 100_000_000_000 ? numericValue * 1_000 : numericValue
    : typeof value === 'string' && /^\d{4}-\d{2}-\d{2}T.*(?:Z|[+-]\d{2}:?\d{2})$/i.test(value) ? Date.parse(value) : NaN;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return '—';
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
}

function CloudFileIcon({ kind }: { kind: CloudFile['kind'] }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24">{kind === 'folder'
    ? <><path d="M3.5 7.5h6l1.8 2h10.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.1l1.8 2h6.7a2.2 2.2 0 0 1 2.2 2.2v1.8" /></>
    : <><path d="M6 2.8h7l5 5v13.4H6z" /><path d="M13 2.8v5h5M9 13h6M9 16.5h6" /></>}</svg>;
}

function CloudActionIcon({ action }: { action: CloudMenuAction | 'back' | 'clear' }) {
  const paths: Record<CloudMenuAction | 'back' | 'clear', React.ReactNode> = {
    open: <><path d="M3.5 7.5h6l1.8 2h9.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="m12 13 2.5 2.5L18 12" /></>,
    restore: <><path d="M19 8a7.5 7.5 0 1 0 1 7" /><path d="m19 3 0 5-5 0" /></>,
    download: <><path d="M12 3v12m0 0 4-4m-4 4-4-4" /><path d="M5 20h14" /></>,
    rename: <><path d="m4 17-.7 3.7L7 20l10.8-10.8-3-3z" /><path d="m13.8 7.2 3 3" /></>,
    move: <><path d="M3.5 7.5h6l1.8 2h9.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="m10 14 2-2 2 2m-2-2v5" /></>,
    copy: <><rect x="8" y="8" width="11" height="11" rx="2" /><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" /></>,
    details: <><circle cx="12" cy="12" r="9" /><path d="M12 11v6M12 7.5h.01" /></>,
    delete: <><path d="M5 7h14M9 7V4h6v3M8 10v8M12 10v8M16 10v8M6.5 7l.8 14h9.4l.8-14" /></>,
    create: <><path d="M3.5 7.5h6l1.8 2h9.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M12 12v6M9 15h6" /></>,
    upload: <><path d="M12 16V4m0 0-4 4m4-4 4 4" /><path d="M5 20h14" /></>,
    refresh: <><path d="M19 8a7.5 7.5 0 1 0 1 7" /><path d="M19 3v5h-5" /></>,
    back: <path d="m14.5 5-7 7 7 7" />,
    clear: <><path d="m7 7 10 10" /><path d="m17 7-10 10" /></>
  };
  return <svg aria-hidden="true" viewBox="0 0 24 24">{paths[action]}</svg>;
}

function CloudContextMenu({ state, selectedCount, recycleBin, virtualRecycleBin, onAction, onClose }: { state: CloudContextMenuState; selectedCount: number; recycleBin: boolean; virtualRecycleBin: boolean; onAction(action: CloudMenuAction): void; onClose(): void }) {
  const menuRef = useRef<HTMLDivElement>(null);
  const items: Array<{ action: CloudMenuAction; label: string; danger?: boolean; separated?: boolean }> = state.kind === 'background'
    ? recycleBin ? [{ action: 'refresh', label: '刷新' }] : [{ action: 'create', label: '新建目录' }, { action: 'upload', label: '上传文件' }, { action: 'refresh', label: '刷新', separated: true }]
    : virtualRecycleBin
      ? [{ action: 'open', label: '打开' }]
      : recycleBin
      ? [{ action: 'restore', label: '恢复' }, { action: 'delete', label: '永久删除', danger: true, separated: true }]
      : [...(selectedCount === 1 && state.item.kind === 'folder' ? [{ action: 'open' as const, label: '打开' }] : []), { action: 'download', label: '下载' }, ...(selectedCount === 1 ? [{ action: 'rename' as const, label: '重命名', separated: true }] : []), { action: 'move', label: '移动', separated: selectedCount > 1 }, { action: 'copy', label: '复制到' }, ...(selectedCount === 1 ? [{ action: 'details' as const, label: '文件详情', separated: true }] : []), { action: 'delete', label: '删除', danger: true, separated: true }];
  const [menuSize, setMenuSize] = useState({ width: 210, height: items.length * 38 + 24 });
  const left = Math.max(8, Math.min(state.x, window.innerWidth - menuSize.width - 8));
  const top = Math.max(8, Math.min(state.y, window.innerHeight - menuSize.height - 8));

  useLayoutEffect(() => {
    const rect = menuRef.current?.getBoundingClientRect();
    if (!rect?.width || !rect.height) return;
    setMenuSize((current) => current.width === rect.width && current.height === rect.height
      ? current
      : { width: rect.width, height: rect.height });
  }, [state.x, state.y, items.length]);

  useEffect(() => {
    menuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
    const closeForPointer = (event: PointerEvent) => { if (!menuRef.current?.contains(event.target as Node)) onClose(); };
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    const close = () => onClose();
    document.addEventListener('pointerdown', closeForPointer);
    document.addEventListener('keydown', closeForEscape);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    window.addEventListener('scroll', close, true);
    return () => {
      document.removeEventListener('pointerdown', closeForPointer);
      document.removeEventListener('keydown', closeForEscape);
      window.removeEventListener('blur', close);
      window.removeEventListener('resize', close);
      window.removeEventListener('scroll', close, true);
    };
  }, [onClose]);

  function moveFocus(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const buttons = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
    const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : event.key === 'ArrowDown' ? (current + 1) % buttons.length : (current - 1 + buttons.length) % buttons.length;
    buttons[next]?.focus();
  }

  const title = state.kind === 'background' ? '文件区菜单' : `${state.item.name} 文件菜单`;
  return createPortal(<div ref={menuRef} className="cloud-context-menu" role="menu" aria-label={title} style={{ left, top }} onKeyDown={moveFocus}>
    {state.kind === 'selection' && <header><b>{state.item.name}</b>{selectedCount > 1 && <small>已选择 {selectedCount} 项</small>}</header>}
    {items.map((item) => <div key={item.action} className={item.separated ? 'menu-separated' : undefined}><button role="menuitem" className={item.danger ? 'danger' : undefined} onClick={() => onAction(item.action)}><CloudActionIcon action={item.action} /><span>{item.label}</span></button></div>)}
  </div>, document.body);
}

function CloudSelectionOverflowMenu({ anchor, singleSelection, recycleBin, disabled, onAction, onClose }: { anchor: { x: number; y: number }; singleSelection: boolean; recycleBin: boolean; disabled: boolean; onAction(action: CloudHeaderMenuAction): void; onClose(): void }) {
  const menuRef = useRef<HTMLDivElement>(null);
  const items: Array<{ action: CloudHeaderMenuAction; label: string; separated?: boolean }> = [
    ...(recycleBin
      ? singleSelection ? [{ action: 'details' as const, label: '文件详情' }] : []
      : [
        ...(singleSelection ? [{ action: 'rename' as const, label: '重命名' }] : []),
        { action: 'move' as const, label: '移动', separated: singleSelection },
        { action: 'copy' as const, label: '复制到' },
        ...(singleSelection ? [{ action: 'details' as const, label: '文件详情', separated: true }] : [])
      ])
  ];
  const width = 180;
  const estimatedHeight = items.length * 38 + 14;
  const left = Math.max(8, Math.min(anchor.x - width, window.innerWidth - width - 8));
  const top = Math.max(8, Math.min(anchor.y, window.innerHeight - estimatedHeight - 8));

  useEffect(() => {
    menuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
    const closeForPointer = (event: PointerEvent) => { if (!menuRef.current?.contains(event.target as Node)) onClose(); };
    const closeForEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('pointerdown', closeForPointer);
    document.addEventListener('keydown', closeForEscape);
    window.addEventListener('blur', onClose);
    window.addEventListener('resize', onClose);
    window.addEventListener('scroll', onClose, true);
    return () => {
      document.removeEventListener('pointerdown', closeForPointer);
      document.removeEventListener('keydown', closeForEscape);
      window.removeEventListener('blur', onClose);
      window.removeEventListener('resize', onClose);
      window.removeEventListener('scroll', onClose, true);
    };
  }, [onClose]);

  return createPortal(<div ref={menuRef} className="cloud-context-menu cloud-selection-overflow-menu" role="menu" aria-label="更多文件操作" style={{ left, top }}>
    {items.map((item) => <div key={item.action} className={item.separated ? 'menu-separated' : undefined}><button role="menuitem" disabled={disabled} onClick={() => onAction(item.action)}><CloudActionIcon action={item.action} /><span>{item.label}</span></button></div>)}
  </div>, document.body);
}

function useModalFocusTrap(containerRef: React.RefObject<HTMLElement | null>, initialFocusRef: React.RefObject<HTMLElement | null>, onClose: () => void) {
  const previousFocus = useRef<HTMLElement | null>(null);
  const closeCallback = useRef(onClose);
  closeCallback.current = onClose;

  useEffect(() => {
    previousFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    initialFocusRef.current?.focus();
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        closeCallback.current();
        return;
      }
      if (event.key !== 'Tab') return;
      const focusable = Array.from(containerRef.current?.querySelectorAll<HTMLElement>('button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])') ?? []);
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (focusable.length === 1 || (!event.shiftKey && document.activeElement === last) || (event.shiftKey && document.activeElement === first) || !containerRef.current?.contains(document.activeElement)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      }
    };
    document.addEventListener('keydown', handleKey);
    return () => {
      document.removeEventListener('keydown', handleKey);
      if (previousFocus.current?.isConnected) previousFocus.current.focus();
    };
  }, [containerRef, initialFocusRef]);
}

function CloudFileDetails({ file, path, onClose }: { file: CloudFile; path: string; onClose(): void }) {
  const dialogRef = useRef<HTMLElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  useModalFocusTrap(dialogRef, closeRef, onClose);

  return createPortal(<div className="cloud-confirm-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <section ref={dialogRef} className="cloud-file-details" role="dialog" aria-modal="true" aria-label="文件详情">
      <header><span className={`cloud-details-icon ${file.kind}`}><CloudFileIcon kind={file.kind} /></span><div><h3>{file.name}</h3><small>{file.kind === 'folder' ? '文件夹' : '文件'}</small></div><button ref={closeRef} className="cloud-details-close" aria-label="关闭文件详情" onClick={onClose}>×</button></header>
      <dl><div><dt>位置</dt><dd>{path}</dd></div>{file.kind === 'file' && <div><dt>大小</dt><dd>{readableSize(file.size)}</dd></div>}<div><dt>修改时间</dt><dd>{formatCloudModifiedAt(file.modifiedAt)}</dd></div></dl>
    </section>
  </div>, document.body);
}

function CloudDestinationPicker({ action, provider, client, excludedIds, onChoose, onOfficialWebAuthorizationRequired, onClose }: { action: DestinationAction; provider: CloudProvider; client: CloudManagerClient; excludedIds: readonly string[]; onChoose(parentId: string): void; onOfficialWebAuthorizationRequired(error: unknown): void; onClose(): void }) {
  const rootId = cloudRootDirectoryId[provider];
  const [directory, setDirectory] = useState<CloudDirectory | null>(null);
  const [breadcrumbs, setBreadcrumbs] = useState<readonly Breadcrumb[]>([{ id: rootId, name: '根目录' }]);
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading');
  const [message, setMessage] = useState('');
  const actionLabel = action === 'copy' ? '复制' : '移动';
  const current = breadcrumbs.at(-1) ?? { id: rootId, name: '根目录' };
  const parent = breadcrumbs.at(-2);
  const dialogRef = useRef<HTMLElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const requestSequence = useRef(0);
  useModalFocusTrap(dialogRef, closeRef, onClose);

  async function load(parentId: string, nextBreadcrumbs: readonly Breadcrumb[]) {
    const requestId = ++requestSequence.current;
    setBreadcrumbs(nextBreadcrumbs);
    setDirectory(null);
    setState('loading');
    setMessage('');
    try {
      const nextDirectory = await client.listCloudDirectory(provider, parentId);
      if (requestId !== requestSequence.current) return;
      setDirectory(nextDirectory);
      setState('ready');
    } catch (error) {
      if (requestId !== requestSequence.current) return;
      if (requiresOfficialWebAuthorization(error)) {
        onOfficialWebAuthorizationRequired(error);
        return;
      }
      setMessage(errorMessage(error, '无法读取目录'));
      setState('error');
    }
  }

  useEffect(() => {
    void load(rootId, [{ id: rootId, name: '根目录' }]);
    return () => { requestSequence.current += 1; };
  }, [client, provider]);
  const recycleBinId = cloudRecycleBinDirectoryId[provider];
  const folders = directory?.items.filter((item) => item.kind === 'folder' && !excludedIds.includes(item.id) && item.id !== recycleBinId) ?? [];
  return createPortal(<div className="cloud-confirm-backdrop">
    <section ref={dialogRef} className="cloud-destination-picker" role="dialog" aria-modal="true" aria-label={`选择${actionLabel}位置`}>
      <header><div><h3>选择{actionLabel}位置</h3><small>当前目录：{current.name}</small></div><button ref={closeRef} aria-label={`关闭${actionLabel}位置选择`} onClick={onClose}>×</button></header>
      <div className="cloud-destination-path">
        {parent && <button className="cloud-icon-button" aria-label={`返回${parent.name}`} disabled={state === 'loading'} onClick={() => void load(parent.id, breadcrumbs.slice(0, -1))}><CloudActionIcon action="back" /></button>}
        <nav role="navigation" aria-label="目标目录">{breadcrumbs.map((crumb, index) => <span key={`${crumb.id}-${index}`}><button disabled={state === 'loading' || index === breadcrumbs.length - 1} onClick={() => void load(crumb.id, breadcrumbs.slice(0, index + 1))}>{crumb.name}</button>{index < breadcrumbs.length - 1 && <i>›</i>}</span>)}</nav>
      </div>
      <div className="cloud-destination-list">
        {state === 'loading' && <div className="cloud-loading-state" role="status"><i /><span>正在读取目录…</span></div>}
        {state === 'error' && <div className="cloud-state cloud-error" role="alert"><span>{message}</span><button className="secondary" onClick={() => void load(current.id, breadcrumbs)}>重试</button></div>}
        {state === 'ready' && (folders.length ? folders.map((folder) => <button key={folder.id} aria-label={`打开 ${folder.name}`} onClick={() => void load(folder.id, [...breadcrumbs, { id: folder.id, name: folder.name }])}><span><CloudFileIcon kind="folder" /></span><b>{folder.name}</b><i>›</i></button>) : <p>此目录没有子文件夹</p>)}
      </div>
      <footer><button className="secondary" onClick={onClose}>取消</button><button className="primary" disabled={state !== 'ready'} onClick={() => onChoose(current.id)}>{actionLabel}到此处</button></footer>
    </section>
  </div>, document.body);
}

export function CloudManager({ provider, client = unavailableClient, initialAccounts = [], onDownloadTask, authorizationOnly = false, restoreOnMount = true, showAccountTabs = true, showSessionActions = true, onAccountAuthorized, onAccountLoggedOut }: CloudManagerProps) {
  const label = providerLabel[provider];
  const rootId = cloudRootDirectoryId[provider];
  const providerAccounts = initialAccounts.filter((account) => account.provider === provider);
  const [accounts, setAccounts] = useState<readonly CloudAccount[]>(providerAccounts);
  const [activeAccountId, setActiveAccountId] = useState<string | null>(providerAccounts[0]?.id ?? null);
  const [directory, setDirectory] = useState<CloudDirectory | null>(null);
  const [directoryState, setDirectoryState] = useState<DirectoryState>('idle');
  const [breadcrumbs, setBreadcrumbs] = useState<readonly Breadcrumb[]>([{ id: rootId, name: '根目录' }]);
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const effectiveAuthorizationCapabilities = fixedAuthorizationCapabilities[provider];
  const supportsDirectPasswordLogin = effectiveAuthorizationCapabilities.password && effectiveAuthorizationCapabilities.passwordLoginMode === 'rustDirect';
  const [authorizationStatus, setAuthorizationStatus] = useState('未授权。请登录或在网页授权窗口中完成验证。');
  const [reauthorizationRequired, setReauthorizationRequired] = useState(false);
  const [webAuthorization, setWebAuthorization] = useState<CloudAuthorization | null>(null);
  const [editor, setEditor] = useState<Editor>(null);
  const [editorValue, setEditorValue] = useState('');
  const [pendingTrash, setPendingTrash] = useState(false);
  const [pendingLogout, setPendingLogout] = useState(false);
  const [operation, setOperation] = useState<OperationState>(null);
  const [contextMenu, setContextMenu] = useState<CloudContextMenuState | null>(null);
  const [selectionMenuAnchor, setSelectionMenuAnchor] = useState<{ x: number; y: number } | null>(null);
  const [detailsFile, setDetailsFile] = useState<CloudFile | null>(null);
  const [destinationAction, setDestinationAction] = useState<DestinationAction | null>(null);
  const authorizationCompletionInFlight = useRef(false);
  const operationInFlight = useRef(false);
  const selectionAnchor = useRef<number | null>(null);
  const selectAllCheckbox = useRef<HTMLInputElement>(null);
  const selectionMenuTrigger = useRef<HTMLButtonElement>(null);

  const activeAccount = accounts.find((account) => account.id === activeAccountId) ?? null;
  const selected = directory?.items.filter((item) => selectedIds.includes(item.id)) ?? [];
  const currentDirectoryId = directory?.id ?? breadcrumbs.at(-1)?.id ?? rootId;
  const recycleBinId = cloudRecycleBinDirectoryId[provider];
  const isRecycleBin = currentDirectoryId === recycleBinId;
  const isVirtualRecycleBinItem = (item: CloudFile | undefined) => item?.id === recycleBinId;
  const selectedVirtualRecycleBin = selected.some(isVirtualRecycleBinItem);
  const selectableItems = directory?.items.filter((item) => !isVirtualRecycleBinItem(item)) ?? [];
  const operationBusy = operation?.kind === 'working';
  const passwordSession = activeAccount?.authMethod === 'password';
  const authorizedLabel = passwordSession ? '已经登录' : '已经完成授权';
  const logoutLabel = passwordSession ? '退出登录' : '退出授权';
  const allSelected = Boolean(selectableItems.length) && selected.length === selectableItems.length;

  useEffect(() => {
    if (selectAllCheckbox.current) selectAllCheckbox.current.indeterminate = selected.length > 0 && !allSelected;
  }, [allSelected, selected.length]);

  useEffect(() => {
    setSelectionMenuAnchor(null);
  }, [selectedIds]);

  useEffect(() => {
    if (!supportsDirectPasswordLogin) {
      setUsername('');
      setPassword('');
    }
  }, [supportsDirectPasswordLogin]);

  async function loadDirectory(account: CloudAccount, parentId: string, nextBreadcrumbs: readonly Breadcrumb[]) {
    setDirectoryState('loading');
    setSelectedIds([]);
    setContextMenu(null);
    selectionAnchor.current = null;
    try {
      const next = await client.listCloudDirectory(provider, parentId);
      const recycleBinId = cloudRecycleBinDirectoryId[provider];
      const items = parentId === rootId && !next.items.some((item) => item.id === recycleBinId)
        ? [...next.items, { id: recycleBinId, parentId: rootId, name: '回收站', kind: 'folder' as const }]
        : next.items;
      setDirectory({ ...next, items });
      setBreadcrumbs(nextBreadcrumbs);
      setDirectoryState(items.length ? 'ready' : 'empty');
    } catch (error) {
      if (requiresOfficialWebAuthorization(error) || (provider === 'xunlei' && isCredentialExpired(error))) {
        setReauthorizationRequired(true);
        setAuthorizationStatus(errorMessage(error, '读取目录需要网页验证。'));
        setDirectoryState(directory ? (directory.items.length ? 'ready' : 'empty') : 'error');
        if (directory) setOperation({ kind: 'error', message: '读取目录需要网页验证。请使用“重新网页授权”在官方窗口完成验证后重试。' });
        return;
      }
      setDirectory(null);
      setDirectoryState(isCredentialExpired(error) ? 'expired' : 'error');
      setAuthorizationStatus(errorMessage(error, '无法读取此目录'));
    }
  }

  useEffect(() => {
    if (!authorizationOnly && activeAccount && directoryState === 'idle') void loadDirectory(activeAccount, rootId, [{ id: rootId, name: '根目录' }]);
  }, [activeAccount, authorizationOnly, directoryState]);

  useEffect(() => {
    if (authorizationOnly || !restoreOnMount) return;
    let active = true;
    client.getCloudAccount(provider).then((account) => {
      if (!active) return;
      ensureProviderAccount(account);
      setAccounts((current) => current.some((item) => item.id === account.id) ? current : [...current, account]);
      setActiveAccountId(account.id);
    }).catch((error) => {
      if (active && !isUnauthorized(error)) setAuthorizationStatus(errorMessage(error, '无法恢复网盘会话。'));
    });
    return () => { active = false; };
  }, [authorizationOnly, client, provider, restoreOnMount]);

  function ensureProviderAccount(account: CloudAccount): CloudAccount {
    if (account.provider !== provider) {
      throw { code: 'invalid_response', message: '授权返回的服务商不匹配，请重新授权。' };
    }
    return account;
  }

  async function acceptAccount(account: CloudAccount) {
    ensureProviderAccount(account);
    setAccounts((current) => current.some((item) => item.id === account.id) ? current.map((item) => item.id === account.id ? account : item) : [...current, account]);
    setActiveAccountId(account.id);
    setPassword('');
    setWebAuthorization(null);
    setReauthorizationRequired(false);
    if (reauthorizationRequired) setOperation(null);
    onAccountAuthorized?.(account);
    if (!authorizationOnly) await loadDirectory(account, rootId, [{ id: rootId, name: '根目录' }]);
  }

  async function addAccount(authorize: () => Promise<CloudAccount>) {
    setAuthorizationStatus('正在等待授权结果…');
    try {
      await acceptAccount(await authorize());
    } catch (error) {
      if (requiresOfficialWebAuthorization(error)) setReauthorizationRequired(true);
      setAuthorizationStatus(errorMessage(error, '授权失败，请重试。'));
    }
  }

  async function submitPasswordLogin(event: FormEvent) {
    event.preventDefault();
    if (!supportsDirectPasswordLogin) return;
    if (!username.trim() || !password) {
      setAuthorizationStatus('请输入账号和密码后再登录。');
      return;
    }
    const credentials = { username: username.trim(), password };
    setUsername('');
    setPassword('');
    await addAccount(() => client.loginCloudWithPassword(provider, credentials));
  }

  async function beginWebAuthorization() {
    setAuthorizationStatus('正在准备网页授权窗口…');
    try {
      const authorization = await client.beginCloudAuthorization(provider);
      setWebAuthorization(authorization);
      setAuthorizationStatus(authorization.instructions);
    } catch (error) {
      setAuthorizationStatus(errorMessage(error, '无法打开授权窗口。'));
    }
  }

  async function completeWebAuthorization(fromPolling: boolean) {
    if (authorizationCompletionInFlight.current) return;
    authorizationCompletionInFlight.current = true;
    try {
      await acceptAccount(await client.completeCloudAuthorization(provider));
    } catch (error) {
      if (isAuthorizationPending(error)) {
        setAuthorizationStatus('正在等待你在授权窗口中完成登录…');
        return;
      }
      const message = errorMessage(error, '网页授权未完成。');
      setAuthorizationStatus(message);
      if (effectiveAuthorizationCapabilities?.passwordLoginMode === 'officialWebView' && isTerminalWebAuthorizationError(error)) {
        // A rejected verification must not automatically start another captcha.
        // Keep the account and let the user explicitly reopen authorization.
        setWebAuthorization(null);
        if (activeAccount) {
          setReauthorizationRequired(true);
          setOperation({ kind: 'error', message });
        }
      }
      if (fromPolling && requiresOfficialWebAuthorization(error)) setWebAuthorization(null);
    } finally {
      authorizationCompletionInFlight.current = false;
    }
  }

  useEffect(() => {
    if (!webAuthorization) return;
    let cancelled = false;
    let timer: number | undefined;
    const poll = () => {
      timer = window.setTimeout(async () => {
        await completeWebAuthorization(true);
        if (!cancelled) poll();
      }, timer === undefined ? authorizationInitialPollMs : authorizationPollMs);
    };
    poll();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [activeAccount, directoryState, webAuthorization]);

  async function runOperation(name: string, action: () => Promise<unknown>, refresh = true) {
    if (!activeAccount || operationInFlight.current) return;
    operationInFlight.current = true;
    setOperation({ kind: 'working', message: `${name}进行中…` });
    try {
      const result = await action();
      if (name === '上传' && Array.isArray(result) && result.length === 0) {
        setOperation(null);
        return;
      }
      if (refresh) await loadDirectory(activeAccount, currentDirectoryId, breadcrumbs);
      const message = name === '上传' && Array.isArray(result) ? `已上传 ${result.length} 个文件` : `${name}成功`;
      setOperation({ kind: 'success', message });
      setEditor(null);
      setEditorValue('');
    } catch (error) {
      if (requiresOfficialWebAuthorization(error) || (provider === 'xunlei' && isCredentialExpired(error))) {
        // Keep the existing account and directory alive while the official
        // browser proves the requested action. `cloud_begin_authorization`
        // only opens the WebView; the saved session is replaced later, after
        // a successful `cloud_complete_authorization` validation.
        setReauthorizationRequired(true);
        setOperation({ kind: 'error', message: `${name}需要网页验证。请使用“重新网页授权”在官方窗口完成验证后重试。` });
      } else if (isCredentialExpired(error)) {
        setDirectory(null);
        setDirectoryState('expired');
        setAuthorizationStatus(errorMessage(error, '登录凭据已过期。'));
      } else if (isSessionRenewedRetryOperation(error)) {
        setOperation({ kind: 'error', message: '已自动恢复登录，请重新执行本次操作' });
      } else {
        setOperation({ kind: 'error', message: `${name}失败：${errorMessage(error, '请重试。')}` });
      }
    } finally {
      operationInFlight.current = false;
    }
  }

  function openEditor(next: Exclude<Editor, null>) {
    if (isRecycleBin || selectedVirtualRecycleBin) return;
    setContextMenu(null);
    setEditor(next);
    setEditorValue(next === 'rename' ? selected[0]?.name ?? '' : '');
  }

  function toggleSelection(itemId: string) {
    setSelectedIds((current) => current.includes(itemId) ? current.filter((id) => id !== itemId) : [...current, itemId]);
  }

  function selectRow(event: ReactMouseEvent, item: CloudFile, index: number) {
    if (event.shiftKey && selectionAnchor.current !== null && directory) {
      const from = Math.min(selectionAnchor.current, index);
      const to = Math.max(selectionAnchor.current, index);
      const range = directory.items.slice(from, to + 1).filter((entry) => !isVirtualRecycleBinItem(entry)).map((entry) => entry.id);
      setSelectedIds(event.ctrlKey || event.metaKey ? (current) => Array.from(new Set([...current.filter((id) => id !== recycleBinId), ...range])) : range);
    } else if (event.ctrlKey || event.metaKey) {
      toggleSelection(item.id);
    } else {
      setSelectedIds([item.id]);
    }
    selectionAnchor.current = index;
  }

  function openFolder(item: CloudFile) {
    if (!activeAccount || isRecycleBin || item.kind !== 'folder' || operationBusy) return;
    void loadDirectory(activeAccount, item.id, [...breadcrumbs, { id: item.id, name: item.name }]);
  }

  function downloadSelection() {
    if (!selected.length || selectedVirtualRecycleBin || isRecycleBin) return;
    void runOperation('下载', async () => {
      const directoryFailures: CloudDirectoryDownloadFailure[] = [];
      let queuedDirectoryFiles = 0;
      await Promise.all(selected.map(async (item) => {
        if (item.kind === 'folder') {
          const outcome = await client.downloadCloudDirectory(provider, item.id, item.name);
          outcome.tasks.forEach((task) => onDownloadTask?.(task));
          directoryFailures.push(...outcome.failures);
          queuedDirectoryFiles += outcome.queuedCount;
          return;
        }
        const task = await client.downloadCloudFile(provider, item.id);
        onDownloadTask?.(task);
      }));
      if (directoryFailures.length) {
        const details = directoryFailures.slice(0, 5).map((failure) => `${failure.relativePath}：${failure.message}`).join('；');
        const remaining = directoryFailures.length > 5 ? `；另有 ${directoryFailures.length - 5} 项失败` : '';
        throw new Error(`目录下载已入队 ${queuedDirectoryFiles} 个文件；${details}${remaining}`);
      }
    }, false);
  }

  function uploadFiles() {
    if (isRecycleBin) return;
    void runOperation('上传', () => client.pickAndUploadFiles(provider, currentDirectoryId));
  }

  function refreshDirectory() {
    if (activeAccount) void loadDirectory(activeAccount, currentDirectoryId, breadcrumbs);
  }

  function runContextAction(action: CloudMenuAction) {
    const menu = contextMenu;
    setContextMenu(null);
    if (menu?.kind === 'selection' && isVirtualRecycleBinItem(menu.item) && action !== 'open') return;
    if (action === 'open' && menu?.kind === 'selection') openFolder(menu.item);
    if (action === 'download') downloadSelection();
    if (action === 'restore' && !selectedVirtualRecycleBin) void runOperation('恢复', () => client.restoreCloudFiles(provider, selectedIds.slice()));
    if (action === 'rename' && !isRecycleBin) openEditor('rename');
    if (action === 'move' && !isRecycleBin) setDestinationAction('move');
    if (action === 'copy' && !isRecycleBin) setDestinationAction('copy');
    if (action === 'details' && menu?.kind === 'selection') setDetailsFile(menu.item);
    if (action === 'delete') setPendingTrash(true);
    if (action === 'create' && !isRecycleBin) openEditor('create');
    if (action === 'upload') uploadFiles();
    if (action === 'refresh') refreshDirectory();
  }

  function openSelectionOverflowMenu() {
    const bounds = selectionMenuTrigger.current?.getBoundingClientRect();
    if (!bounds) return;
    setSelectionMenuAnchor({ x: bounds.right, y: bounds.bottom + 6 });
  }

  function runSelectionOverflowAction(action: CloudHeaderMenuAction) {
    setSelectionMenuAnchor(null);
    selectionMenuTrigger.current?.focus();
    if (selectedVirtualRecycleBin) return;
    if (action === 'rename' && !isRecycleBin) openEditor('rename');
    if (action === 'move' && !isRecycleBin) setDestinationAction('move');
    if (action === 'copy' && !isRecycleBin) setDestinationAction('copy');
    if (action === 'details' && selected.length === 1) setDetailsFile(selected[0]);
  }

  function handleFileListKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a') {
      event.preventDefault();
      setSelectedIds(selectableItems.map((item) => item.id));
      return;
    }
    if (event.key === 'Escape') {
      setContextMenu(null);
      setEditor(null);
      return;
    }
    if (event.key === 'Enter' && !isRecycleBin && selected.length === 1 && selected[0].kind === 'folder') {
      event.preventDefault();
      openFolder(selected[0]);
      return;
    }
    if (event.key === 'F2' && !isRecycleBin && !selectedVirtualRecycleBin && selected.length === 1) {
      event.preventDefault();
      openEditor('rename');
      return;
    }
    if (event.key === 'Delete' && selected.length && !selectedVirtualRecycleBin) {
      event.preventDefault();
      setPendingTrash(true);
    }
  }

  function submitEditor(event: FormEvent) {
    event.preventDefault();
    if (!activeAccount || isRecycleBin || selectedVirtualRecycleBin || !editorValue.trim()) return;
    if (editor === 'create') void runOperation('新建目录', () => client.createCloudDirectory(provider, currentDirectoryId, editorValue.trim()).then(() => undefined));
    if (editor === 'rename' && selected[0]) void runOperation('重命名', () => client.renameCloudFile(provider, selected[0].id, editorValue.trim()));
  }

  function authorizationView(expired = false) {
    const initialAuthorizationStatus = '未授权。请登录或在网页授权窗口中完成验证。';
    const showAuthorizationFeedback = authorizationStatus !== initialAuthorizationStatus;
    return <section className={`cloud-manager cloud-authorization cloud-provider-${provider}`} aria-label={`${label}账号授权`}>
      <header className="cloud-heading"><h2>{label}网盘</h2></header>
      {showAccountTabs && accounts.length > 0 && <div className="cloud-account-tabs" role="tablist" aria-label={`${label}账号`}>{accounts.map((account) => <button key={account.id} role="tab" aria-selected={account.id === activeAccountId}>{account.displayName} · {label}</button>)}</div>}
      <form className="cloud-login-form" onSubmit={submitPasswordLogin}>
        {supportsDirectPasswordLogin && <>
          <input aria-label="账号" placeholder="账号" autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} />
          <input aria-label="密码" placeholder="密码" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} />
          <button className="primary" type="submit">登录</button>
        </>}
        {effectiveAuthorizationCapabilities.web && <button type="button" className="secondary" onClick={() => void beginWebAuthorization()}>使用网页授权</button>}
        {webAuthorization && <button type="button" className="secondary" onClick={() => void completeWebAuthorization(false)}>我已完成授权</button>}
      </form>
      {showAuthorizationFeedback && <p className={`cloud-authorization-feedback${expired ? ' error' : ''}`} role="status">{expired && <b>凭据已过期</b>}<span>{authorizationStatus}</span></p>}
    </section>;
  }

  function sessionActions() {
    return <div className="cloud-session-actions"><button className="cloud-auth-state" disabled>{authorizedLabel}</button>{reauthorizationRequired && effectiveAuthorizationCapabilities?.web && <button className="secondary" disabled={operationBusy} onClick={() => void beginWebAuthorization()}>重新网页授权</button>}<button className="secondary" disabled={operationBusy} onClick={() => setPendingLogout(true)}>{logoutLabel}</button></div>;
  }

  function logoutConfirmation() {
    return pendingLogout && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label={`确认${logoutLabel}`}><h3>确认{logoutLabel}</h3><p>这只会清除本机授权会话，不会删除网盘中的任何文件。</p><button className="secondary" onClick={() => setPendingLogout(false)}>取消</button><button className="danger-button" onClick={() => { setPendingLogout(false); void runOperation(logoutLabel, async () => { await client.logoutCloud(provider); setAccounts([]); setActiveAccountId(null); setDirectory(null); setDirectoryState('idle'); setAuthorizationStatus('未授权。请登录或在网页授权窗口中完成验证。'); onAccountLoggedOut?.(provider); }, false); }}>确认{logoutLabel}</button></section></div>;
  }

  function accountManagementView() {
    return <section className={`cloud-manager cloud-authorization cloud-connected-account cloud-provider-${provider}`} aria-label={`${label}账号授权`}>
      <header className="cloud-heading"><h2>{label}网盘 <span className="cloud-account-name">· {activeAccount?.displayName}</span></h2></header>
      {sessionActions()}
      {logoutConfirmation()}
      {operation && <p className={`cloud-operation-status ${operation.kind}`} role={operation.kind === 'error' ? 'alert' : 'status'}>{operation.message}</p>}
    </section>;
  }

  if (authorizationOnly && activeAccount) return accountManagementView();
  if (authorizationOnly || !activeAccount) return authorizationView();
  if (directoryState === 'expired' && !showSessionActions) return <section className="cloud-manager" aria-label={`${label}文件管理器`}><header className="cloud-heading"><h2>{label}网盘</h2></header><p className="cloud-state cloud-error" role="alert">登录凭据已过期，请前往“网盘管理”重新授权。</p></section>;
  if (directoryState === 'expired') return authorizationView(true);

  const parent = breadcrumbs.at(-2);
  return <section className="cloud-manager cloud-file-manager" aria-label={`${label}文件管理器`}>
    {(showAccountTabs || showSessionActions) && <div className="cloud-file-sessionbar">
      {showAccountTabs && <div className="cloud-account-tabs" role="tablist" aria-label={`${label}账号`}>{accounts.map((account) => <button key={account.id} role="tab" aria-selected={account.id === activeAccountId} disabled={operationBusy} onClick={() => { setActiveAccountId(account.id); setDirectoryState('idle'); setDirectory(null); }}>{account.displayName} · {label}</button>)}</div>}
      {showSessionActions && sessionActions()}
    </div>}

    <div className="cloud-filebar">
      <div className="cloud-pathbar">
        <button className="cloud-icon-button" aria-label="返回上一级" title="返回上一级" disabled={!parent || operationBusy} onClick={() => { if (parent) void loadDirectory(activeAccount, parent.id, breadcrumbs.slice(0, -1)); }}><CloudActionIcon action="back" /></button>
        <div className="cloud-breadcrumbs" role="navigation" aria-label="当前目录">{breadcrumbs.map((crumb, index) => <span key={`${crumb.id}-${index}`}><button title={crumb.name} aria-current={index === breadcrumbs.length - 1 ? 'page' : undefined} disabled={index === breadcrumbs.length - 1} onClick={() => void loadDirectory(activeAccount, crumb.id, breadcrumbs.slice(0, index + 1))}>{crumb.name}</button>{index < breadcrumbs.length - 1 && <i aria-hidden="true">›</i>}</span>)}</div>
      </div>
      <div className="cloud-toolbar cloud-toolbar--single-line" aria-label="文件操作">
        {!isRecycleBin && <button className="cloud-toolbar-primary" aria-label="上传" disabled={operationBusy} onClick={uploadFiles}><CloudActionIcon action="upload" />上传文件</button>}
        {!isRecycleBin && <button aria-label="新建目录" disabled={operationBusy} onClick={() => openEditor('create')}><CloudActionIcon action="create" />新建文件夹</button>}
        <button className="cloud-icon-button cloud-refresh-button" aria-label="刷新" title="刷新" disabled={operationBusy} onClick={refreshDirectory}><CloudActionIcon action="refresh" /></button>
      </div>
    </div>

    {editor && <form className="cloud-operation-editor" onSubmit={submitEditor} onKeyDown={(event) => { if (event.key === 'Escape') { event.preventDefault(); setEditor(null); } }}><label>{editor === 'create' ? '目录名称' : '新名称'}<input aria-label={editor === 'create' ? '目录名称' : '新名称'} value={editorValue} onChange={(event) => setEditorValue(event.target.value)} autoFocus /></label><button className="primary" type="submit" disabled={operationBusy}>{editor === 'create' ? '确认新建目录' : '确认重命名'}</button><button type="button" className="secondary" disabled={operationBusy} onClick={() => setEditor(null)}>取消</button></form>}
    {pendingTrash && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label={isRecycleBin ? '确认永久删除' : '确认移入回收站'}><h3>{isRecycleBin ? '确认永久删除' : '确认移入回收站'}</h3><p>{isRecycleBin ? `所选 ${selected.length} 个项目将被永久删除，不可恢复。` : `所选 ${selected.length} 个项目将移入回收站，不会永久删除。`}</p><button className="secondary" onClick={() => setPendingTrash(false)}>取消</button><button className="danger-button" onClick={() => { setPendingTrash(false); void runOperation(isRecycleBin ? '永久删除' : '删除', () => isRecycleBin ? client.deleteCloudFiles(provider, selectedIds.slice()) : client.trashCloudFiles(provider, selectedIds.slice())); }}>{isRecycleBin ? '永久删除' : '移入回收站'}</button></section></div>}
    {showSessionActions && logoutConfirmation()}
    {operation && <p className={`cloud-operation-status ${operation.kind}`} role={operation.kind === 'error' ? 'alert' : 'status'}>{operation.message}</p>}
    {!showSessionActions && reauthorizationRequired && effectiveAuthorizationCapabilities?.web && <button className="secondary" disabled={operationBusy} onClick={() => void beginWebAuthorization()}>重新网页授权</button>}
    {directoryState === 'loading' && <div className="cloud-loading-state" role="status"><i /><span>正在加载文件…</span></div>}
    {directoryState === 'error' && <div className="cloud-state cloud-error" role="alert"><b>加载失败</b><span>{authorizationStatus}</span><button className="secondary" onClick={refreshDirectory}>重试</button></div>}
    {(directoryState === 'ready' || directoryState === 'empty') && <div
      className="cloud-list"
      role="tree"
      aria-label={`${label}网盘文件`}
      tabIndex={0}
      onKeyDown={handleFileListKeyDown}
      onContextMenu={(event) => { if (!(event.target as HTMLElement).closest('[data-cloud-file-row]')) { event.preventDefault(); setSelectedIds([]); setContextMenu({ kind: 'background', x: event.clientX, y: event.clientY }); } }}
    >
      <div className="cloud-row cloud-columns"><span className="cloud-check-cell"><input ref={selectAllCheckbox} type="checkbox" aria-label="全选当前目录" checked={allSelected} onChange={(event) => setSelectedIds(event.target.checked ? selectableItems.map((item) => item.id) : [])} /></span><span className="cloud-name-column">{selected.length > 0 ? <span className="cloud-header-selection" role="toolbar" aria-label={`已选择 ${selected.length} 项的文件操作`}><b>已选择 {selected.length} 项</b>{selectedVirtualRecycleBin ? <span className="cloud-selection-hint">回收站仅可打开</span> : isRecycleBin ? <span className="cloud-selection-actions"><button disabled={operationBusy} onClick={() => void runOperation('恢复', () => client.restoreCloudFiles(provider, selectedIds.slice()))}><CloudActionIcon action="restore" />恢复</button><button className="danger" disabled={operationBusy} onClick={() => setPendingTrash(true)}><CloudActionIcon action="delete" />永久删除</button></span> : <span className="cloud-selection-actions"><button disabled={operationBusy} onClick={downloadSelection}><CloudActionIcon action="download" />下载</button><button className="danger" disabled={operationBusy} onClick={() => setPendingTrash(true)}><CloudActionIcon action="delete" />删除</button><button ref={selectionMenuTrigger} className="cloud-selection-more" aria-label="更多文件操作" aria-haspopup="menu" aria-expanded={Boolean(selectionMenuAnchor)} disabled={operationBusy} onClick={openSelectionOverflowMenu}>•••</button></span>}<button className="cloud-clear-selection" aria-label="取消选择" title="取消选择" onClick={() => setSelectedIds([])}><CloudActionIcon action="clear" /></button></span> : '名称'}</span><span>大小</span><span>修改时间</span></div>
      {directoryState === 'empty' ? <div className="cloud-empty"><CloudFileIcon kind="folder" /><b>{isRecycleBin ? '回收站为空' : '此目录为空'}</b><span>{isRecycleBin ? '没有可恢复或永久删除的项目' : '可在这里上传文件或新建目录'}</span></div> : directory?.items.map((item, index) => {
        const isSelected = selectedIds.includes(item.id);
        return <div
          className={`cloud-row cloud-file-row ${isSelected ? 'selected' : ''}`}
          data-cloud-file-row
          role="treeitem"
          aria-selected={isSelected}
          key={item.id}
          onClick={(event) => selectRow(event, item, index)}
          onDoubleClick={(event) => { event.preventDefault(); openFolder(item); }}
          onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); if (!isSelected) setSelectedIds([item.id]); selectionAnchor.current = index; setContextMenu({ kind: 'selection', item, x: event.clientX, y: event.clientY }); }}
        >
          <span className="cloud-check-cell"><input type="checkbox" aria-label={`选择 ${item.name}`} checked={isSelected} onClick={(event) => event.stopPropagation()} onChange={() => toggleSelection(item.id)} /></span>
          <span className="cloud-file-name"><i className={item.kind}><CloudFileIcon kind={item.kind} /></i><b title={item.name}>{item.name}</b></span>
          <span>{item.kind === 'folder' ? '—' : readableSize(item.size)}</span>
          <span>{formatCloudModifiedAt(item.modifiedAt)}</span>
        </div>;
      })}
    </div>}
    {contextMenu && <CloudContextMenu state={contextMenu} selectedCount={selected.length} recycleBin={isRecycleBin} virtualRecycleBin={contextMenu.kind === 'selection' && isVirtualRecycleBinItem(contextMenu.item)} onClose={() => setContextMenu(null)} onAction={runContextAction} />}
    {selectionMenuAnchor && <CloudSelectionOverflowMenu anchor={selectionMenuAnchor} singleSelection={selected.length === 1} recycleBin={isRecycleBin} disabled={Boolean(operationBusy)} onClose={() => setSelectionMenuAnchor(null)} onAction={runSelectionOverflowAction} />}
    {detailsFile && <CloudFileDetails file={detailsFile} path={`/${breadcrumbs.slice(1).map((crumb) => crumb.name).join('/') || ''}`} onClose={() => setDetailsFile(null)} />}
    {destinationAction && <CloudDestinationPicker action={destinationAction} provider={provider} client={client} excludedIds={selectedIds} onOfficialWebAuthorizationRequired={(error) => { setDestinationAction(null); setReauthorizationRequired(true); setAuthorizationStatus(errorMessage(error, '读取目标目录需要网页验证。')); setOperation({ kind: 'error', message: `读取${destinationAction === 'copy' ? '复制' : '移动'}目标目录需要网页验证。请使用“重新网页授权”在官方窗口完成验证后重新执行本次操作。` }); }} onClose={() => setDestinationAction(null)} onChoose={(targetParentId) => { const action = destinationAction; setDestinationAction(null); if (action === 'move') void runOperation('移动', () => client.moveCloudFiles(provider, selectedIds.slice(), targetParentId)); else void runOperation('复制', () => client.copyCloudFiles(provider, selectedIds.slice(), targetParentId)); }} />}
  </section>;
}
