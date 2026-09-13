import { FormEvent, KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { LocalUploadPicker } from './LocalUploadPicker';
import type { LocalUploadListing } from '../lib/localUpload';
import type { CloudAuthorizationStateEvent } from '../lib/cloudTypes';
import type { ProviderDescriptor, CloudAccount, CloudAuthorization, CloudAuthorizationCapabilities, CloudCredentials, CloudDirectory, CloudDirectoryDownloadFailure, CloudDirectoryDownloadOutcome, CloudFile, CloudProvider, CloudUploadOutcome } from '../lib/cloudTypes';

export type { CloudAccount, CloudAuthorization, CloudAuthorizationCapabilities, CloudCredentials, CloudDirectory, CloudDirectoryDownloadOutcome, CloudFile, CloudProvider } from '../lib/cloudTypes';

export interface CloudManagerClient {
  getCloudProviderCatalog(): Promise<readonly ProviderDescriptor[]>;
  getCloudAuthorizationCapabilities(provider: CloudProvider): Promise<CloudAuthorizationCapabilities>;
  beginCloudAuthorization(provider: CloudProvider): Promise<CloudAuthorization>;
  openCloudOperationVerification?(provider: CloudProvider, expectedAccountId: string): Promise<void>;
  completeCloudAuthorization(provider: CloudProvider): Promise<CloudAccount>;
  getCompletedCloudAuthorization?(provider: CloudProvider): Promise<CloudAccount | null>;
  subscribeCloudAuthorization?(provider: CloudProvider, receive: (event: CloudAuthorizationStateEvent) => void): Promise<() => void>;
  loginCloudWithPassword(provider: CloudProvider, credentials: CloudCredentials): Promise<CloudAccount>;
  getSavedCloudCredentials?(provider: CloudProvider): Promise<import('../lib/cloudTypes').SavedCloudCredentialInfo | null>;
  getCloudAccount(provider: CloudProvider): Promise<CloudAccount>;
  listCloudDirectory(provider: CloudProvider, parentId?: string): Promise<CloudDirectory>;
  createCloudDirectory(provider: CloudProvider, parentId: string, name: string): Promise<CloudFile | void>;
  renameCloudFile(provider: CloudProvider, fileId: string, name: string): Promise<void>;
  moveCloudFiles(provider: CloudProvider, fileIds: string[], targetParentId: string): Promise<void>;
  copyCloudFiles(provider: CloudProvider, fileIds: string[], targetParentId: string): Promise<void>;
  trashCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  deleteCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  restoreCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  downloadCloudFile(provider: CloudProvider, fileId: string, fileName: string, privateDownload?: boolean): Promise<unknown>;
  downloadCloudDirectory(provider: CloudProvider, directoryId: string, directoryName: string, privateDownload?: boolean): Promise<CloudDirectoryDownloadOutcome>;
  logoutCloud(provider: CloudProvider): Promise<void>;
  pickAndUploadFiles(provider: CloudProvider, parentId: string): Promise<CloudUploadOutcome>;
  pickAndUploadDirectory(provider: CloudProvider, parentId: string): Promise<CloudUploadOutcome>;
  pickCloudUploadFiles?(): Promise<string[]>;
  pickCloudUploadDirectory?(): Promise<string | null>;
  uploadCloudFiles?(provider: CloudProvider, parentId: string, paths: string[]): Promise<CloudUploadOutcome>;
  uploadCloudDirectory?(provider: CloudProvider, parentId: string, path: string): Promise<CloudUploadOutcome>;
  listLocalUploadEntries?(path?: string): Promise<LocalUploadListing>;
}

export interface CloudManagerProps {
  descriptor: ProviderDescriptor;
  provider: CloudProvider;
  client?: CloudManagerClient;
  initialAccount?: CloudAccount | null;
  onDownloadTask?(task: unknown): void;
  authorizationOnly?: boolean;
  restoreOnMount?: boolean;
  showSessionActions?: boolean;
  privacyDownloadEnabled?: boolean;
  onAccountAuthorized?(account: CloudAccount): void;
  onAccountLoggedOut?(provider: CloudProvider): void;
}

type DirectoryState = 'idle' | 'loading' | 'ready' | 'empty' | 'error' | 'expired';
type Editor = 'create' | 'rename' | null;
type DestinationAction = 'move' | 'copy';
type OperationState = { kind: 'working' | 'success' | 'error'; message: string; operationVerification?: boolean } | null;
type PendingAuthorizationAction = { accountId: string; name: string; resume: () => Promise<void>; showOperation: boolean };
interface Breadcrumb { id: string; name: string }
type CloudMenuAction = 'open' | 'restore' | 'download' | 'rename' | 'move' | 'copy' | 'details' | 'delete' | 'create' | 'upload' | 'refresh';
type CloudContextMenuState = { kind: 'selection'; item: CloudFile; x: number; y: number } | { kind: 'background'; x: number; y: number };
type CloudHeaderMenuAction = 'rename' | 'move' | 'copy' | 'details';

const authorizationWatchdogMs = 10_000;

const unavailableClient: CloudManagerClient = {
  getCloudProviderCatalog: () => Promise.reject(new Error('网盘连接器尚未接入')),
  getCloudAuthorizationCapabilities: () => Promise.reject(new Error('网盘连接器尚未接入')),
  beginCloudAuthorization: () => Promise.reject(new Error('网盘连接器尚未接入')),
  completeCloudAuthorization: () => Promise.reject(new Error('网盘连接器尚未接入')),
  loginCloudWithPassword: () => Promise.reject(new Error('网盘连接器尚未接入')),
  getSavedCloudCredentials: () => Promise.resolve(null),
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
  pickAndUploadFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  pickAndUploadDirectory: () => Promise.reject(new Error('网盘连接器尚未接入'))
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
  if (code === 'pikpak_operation_verification_required') return false;
  const message = errorMessage(error, '').toLowerCase();
  return ['web_auth_required', 'captcha_required', 'captcha_invalid', 'verification_required', 'web_verification_required', 'risk_verification_required'].includes(code)
    || /captcha|verification required|网页验证|安全验证|风险验证/.test(message);
}

function requiresPikpakOperationVerification(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error
    && error.code === 'pikpak_operation_verification_required';
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

function isValidDateTime(year: number, month: number, day: number, hour: number, minute: number, second: number) {
  if (year < 1 || year > 9_999 || month < 1 || month > 12 || day < 1 || hour > 23 || minute > 59 || second > 59) return false;
  const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day <= daysInMonth[month - 1];
}

function formatLocalCloudDate(value: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})(?: (\d{2}):(\d{2})(?::(\d{2}))?)?$/.exec(value);
  if (!match) return null;
  const [, year, month, day, hour = '00', minute = '00', second = '00'] = match;
  if (!isValidDateTime(Number(year), Number(month), Number(day), Number(hour), Number(minute), Number(second))) return null;
  return `${year}-${month}-${day} ${hour}:${minute}:${second}`;
}

function formatCloudModifiedAt(value: unknown) {
  if (typeof value === 'string') {
    const localDate = formatLocalCloudDate(value);
    if (localDate) return localDate;
  }

  const numericValue = typeof value === 'number' ? value : typeof value === 'string' && /^-?\d+(?:\.\d+)?$/.test(value) ? Number(value) : NaN;
  const isoMatch = typeof value === 'string' ? /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})$/i.exec(value) : null;
  const timestamp = Number.isFinite(numericValue)
    ? Math.abs(numericValue) < 100_000_000_000 ? numericValue * 1_000 : numericValue
    : isoMatch && isValidDateTime(Number(isoMatch[1]), Number(isoMatch[2]), Number(isoMatch[3]), Number(isoMatch[4]), Number(isoMatch[5]), Number(isoMatch[6] ?? '0')) ? Date.parse(isoMatch[0]) : NaN;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return '—';
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
}

type CloudSortColumn = 'name' | 'size' | 'modifiedAt';
type CloudSortDirection = 'ascending' | 'descending';
type CloudSort = { column: CloudSortColumn; direction: CloudSortDirection };

const cloudNameCollator = new Intl.Collator('zh-CN', { numeric: true, sensitivity: 'base' });

function compareOptional<T>(left: T | undefined, right: T | undefined, compare: (left: T, right: T) => number) {
  if (left === undefined) return { comparison: right === undefined ? 0 : 1, missing: true };
  if (right === undefined) return { comparison: -1, missing: true };
  return { comparison: compare(left, right), missing: false };
}

function sortableCloudModifiedAt(value: unknown) {
  const formatted = formatCloudModifiedAt(value);
  return formatted === '—' ? undefined : formatted;
}

function sortableCloudSize(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : undefined;
}

function sortedCloudFiles(items: readonly CloudFile[], sort: CloudSort | null) {
  if (!sort) return items;
  return items.map((item, index) => ({ item, index })).sort((left, right) => {
    const { comparison, missing } = sort.column === 'name'
      ? { comparison: cloudNameCollator.compare(left.item.name, right.item.name), missing: false }
      : sort.column === 'size'
        ? compareOptional(sortableCloudSize(left.item.size), sortableCloudSize(right.item.size), (a, b) => a - b)
        : compareOptional(
          sortableCloudModifiedAt(left.item.modifiedAt),
          sortableCloudModifiedAt(right.item.modifiedAt),
          (a, b) => a.localeCompare(b),
        );
    if (comparison === 0) return left.index - right.index;
    return sort.direction === 'ascending' || missing ? comparison : -comparison;
  }).map(({ item }) => item);
}

function CloudFileIcon({ kind }: { kind: CloudFile['kind'] | 'recycle-bin' }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24">{kind === 'recycle-bin'
    ? <><path d="M5 7h14M9 7V4h6v3M8 10v8M12 10v8M16 10v8M6.5 7l.8 14h9.4l.8-14" /></>
    : kind === 'folder'
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
    ? recycleBin ? [{ action: 'refresh', label: '刷新' }] : [{ action: 'create', label: '新建目录' }, { action: 'upload', label: '上传' }, { action: 'refresh', label: '刷新', separated: true }]
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

function CloudDestinationPicker({ action, provider, descriptor, client, excludedIds, onChoose, onOfficialWebAuthorizationRequired, onClose }: { action: DestinationAction; provider: CloudProvider; descriptor: ProviderDescriptor; client: CloudManagerClient; excludedIds: readonly string[]; onChoose(parentId: string): void; onOfficialWebAuthorizationRequired(error: unknown): void; onClose(): void }) {
  const rootId = descriptor.rootDirectoryId;
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
  const recycleBinId = descriptor.recycleBinDirectoryId;
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

export function CloudManager({ provider, descriptor, client = unavailableClient, initialAccount = null, onDownloadTask, authorizationOnly = false, restoreOnMount = true, showSessionActions = true, privacyDownloadEnabled = false, onAccountAuthorized, onAccountLoggedOut }: CloudManagerProps) {
  const label = descriptor.displayName;
  const rootId = descriptor.rootDirectoryId;
  const [activeAccount, setActiveAccount] = useState<CloudAccount | null>(initialAccount?.provider === provider ? initialAccount : null);
  const [directory, setDirectory] = useState<CloudDirectory | null>(null);
  const [directoryState, setDirectoryState] = useState<DirectoryState>('idle');
  const [breadcrumbs, setBreadcrumbs] = useState<readonly Breadcrumb[]>([{ id: rootId, name: '根目录' }]);
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  const [sort, setSort] = useState<CloudSort | null>(null);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [savedUsername, setSavedUsername] = useState<string | null>(null);
  const useSavedPassword = savedUsername === username.trim() && !password;
  const [passwordLoginBusy, setPasswordLoginBusy] = useState(false);
  const passwordLoginInFlight = useRef(false);
  const passwordFieldsEdited = useRef(false);
  const passwordInputVersion = useRef(0);
  const effectiveAuthorizationCapabilities = descriptor.authorization;
  const supportsDirectPasswordLogin = effectiveAuthorizationCapabilities.password && effectiveAuthorizationCapabilities.passwordLoginMode === 'rustDirect';
  const [authorizationStatus, setAuthorizationStatus] = useState('未授权。请登录或在网页授权窗口中完成验证。');
  const [reauthorizationRequired, setReauthorizationRequired] = useState(false);
  const [openingOperationVerification, setOpeningOperationVerification] = useState(false);
  const [pendingAuthorizationName, setPendingAuthorizationName] = useState<string | null>(null);
  const [webAuthorization, setWebAuthorization] = useState<CloudAuthorization | null>(null);
  const [editor, setEditor] = useState<Editor>(null);
  const [editorValue, setEditorValue] = useState('');
  const [pendingTrash, setPendingTrash] = useState(false);
  const [pendingDownload, setPendingDownload] = useState<readonly CloudFile[] | null>(null);
  const [uploadPickerOpen, setUploadPickerOpen] = useState(false);
  const [pendingLogout, setPendingLogout] = useState(false);
  const [operation, setOperation] = useState<OperationState>(null);
  const [contextMenu, setContextMenu] = useState<CloudContextMenuState | null>(null);
  const [selectionMenuAnchor, setSelectionMenuAnchor] = useState<{ x: number; y: number } | null>(null);
  const [detailsFile, setDetailsFile] = useState<CloudFile | null>(null);
  const [destinationAction, setDestinationAction] = useState<DestinationAction | null>(null);
  const authorizationCompletionInFlight = useRef<number | null>(null);
  const webAuthorizationActive = useRef(false);
  const authorizationGeneration = useRef(0);
  const authorizationRegistered = useRef(false);
  const authorizationWake = useRef<(() => void) | null>(null);
  const authorizationWakePending = useRef(false);
  const operationInFlight = useRef(false);
  const pendingAuthorizationAction = useRef<PendingAuthorizationAction | null>(null);
  const selectionAnchor = useRef<string | null>(null);
  const selectAllCheckbox = useRef<HTMLInputElement>(null);
  const selectionMenuTrigger = useRef<HTMLButtonElement>(null);

  const sortedItems = sortedCloudFiles(directory?.items ?? [], sort);
  const selected = sortedItems.filter((item) => selectedIds.includes(item.id));
  const currentDirectoryId = directory?.id ?? breadcrumbs.at(-1)?.id ?? rootId;
  const recycleBinId = descriptor.recycleBinDirectoryId;
  const isRecycleBin = currentDirectoryId === recycleBinId;
  const isVirtualRecycleBinItem = (item: CloudFile | undefined) => item?.id === recycleBinId;
  const selectedVirtualRecycleBin = selected.some(isVirtualRecycleBinItem);
  const selectableItems = sortedItems.filter((item) => !isVirtualRecycleBinItem(item));
  const operationBusy = operation?.kind === 'working';
  const passwordSession = activeAccount?.authMethod === 'password';
  const authorizedLabel = passwordSession ? '已经登录' : '已经完成授权';
  const logoutLabel = passwordSession ? '退出登录' : '退出授权';
  const allSelected = Boolean(selectableItems.length) && selected.length === selectableItems.length;

  function queueAuthorizationContinuation(name: string, resume: () => Promise<void>, message: string, showOperation = true, expectedAccountId = activeAccount?.id) {
    if (!expectedAccountId) return;
    pendingAuthorizationAction.current = { accountId: expectedAccountId, name, resume, showOperation };
    setPendingAuthorizationName(name);
    setReauthorizationRequired(true);
    setAuthorizationStatus(message);
    if (showOperation) setOperation({ kind: 'error', message });
  }

  function needsInPlaceWebAuthorization(error: unknown) {
    return requiresOfficialWebAuthorization(error)
      || (isCredentialExpired(error) && effectiveAuthorizationCapabilities.web);
  }

  function showOperationVerification(error: unknown) {
    // This is not a failed login. Never queue an automatic replay of a mutation
    // when the provider is waiting for a separate interactive challenge.
    pendingAuthorizationAction.current = null;
    setPendingAuthorizationName(null);
    setReauthorizationRequired(false);
    setOperation({ kind: 'error', message: errorMessage(error, '请完成 PikPak 官方操作验证后重试。'), operationVerification: true });
  }

  async function openOperationVerification() {
    if (!activeAccount || !client.openCloudOperationVerification || openingOperationVerification) return;
    setOpeningOperationVerification(true);
    try {
      await client.openCloudOperationVerification(provider, activeAccount.id);
      setOperation({ kind: 'error', operationVerification: true, message: '请在官方页面完成操作验证；返回后重新执行原操作。不会自动永久删除文件。' });
    } catch (error) {
      setOperation({ kind: 'error', operationVerification: true, message: errorMessage(error, '无法打开官方操作验证页面，请稍后重试。') });
    } finally {
      setOpeningOperationVerification(false);
    }
  }

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
      return;
    }
    let active = true;
    client.getSavedCloudCredentials?.(provider).then((credentials) => {
      if (!active || passwordFieldsEdited.current || !credentials) return;
      setUsername(credentials.username);
      setSavedUsername(credentials.hasSavedPassword ? credentials.username : null);
    }).catch(() => undefined);
    return () => { active = false; };
  }, [client, provider, supportsDirectPasswordLogin, activeAccount?.id]);

  async function loadDirectory(account: CloudAccount, parentId: string, nextBreadcrumbs: readonly Breadcrumb[]) {
    setDirectoryState('loading');
    setSelectedIds([]);
    setContextMenu(null);
    selectionAnchor.current = null;
    try {
      const next = await client.listCloudDirectory(provider, parentId);
      const recycleBinId = descriptor.recycleBinDirectoryId;
      const items = parentId === rootId && !next.items.some((item) => item.id === recycleBinId)
        ? [...next.items, { id: recycleBinId, parentId: rootId, name: '回收站', kind: 'folder' as const }]
        : next.items;
      setDirectory({ ...next, items });
      setBreadcrumbs(nextBreadcrumbs);
      setDirectoryState(items.length ? 'ready' : 'empty');
      return true;
    } catch (error) {
      if (requiresPikpakOperationVerification(error)) {
        showOperationVerification(error);
        setDirectoryState('error');
        setAuthorizationStatus(errorMessage(error, '请完成官方操作验证后重试。'));
      } else if (needsInPlaceWebAuthorization(error)) {
        const detail = errorMessage(error, '');
        const message = `读取目录需要网页验证。${detail ? ` ${detail}` : ''}`;
        queueAuthorizationContinuation('加载目录', async () => { await loadDirectory(account, parentId, nextBreadcrumbs); }, message, Boolean(directory), account.id);
        setDirectoryState(directory ? (directory.items.length ? 'ready' : 'empty') : 'error');
        return false;
      }
      setDirectory(null);
      setDirectoryState(isCredentialExpired(error) ? 'expired' : 'error');
      setAuthorizationStatus(errorMessage(error, '无法读取此目录'));
      return false;
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
      setActiveAccount(account);
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
    const pending = pendingAuthorizationAction.current;
    const sameAccount = !pending
      || pending.accountId === account.id
      || (provider === 'tianyi' && (pending.accountId === 'tianyi-web' || account.id === 'tianyi-web'));
    if (pending && !sameAccount) {
      const message = '授权账号与当前会话不一致，已停止继续原操作。请使用原账号重新授权。';
      webAuthorizationActive.current = false;
      setWebAuthorization(null);
      setReauthorizationRequired(true);
      setAuthorizationStatus(message);
      if (pending.showOperation) setOperation({ kind: 'error', message });
      return false;
    }
    setActiveAccount(account);
    setWebAuthorization(null);
    setReauthorizationRequired(false);
    pendingAuthorizationAction.current = null;
    setPendingAuthorizationName(null);
    if (pending) setOperation(null);
    onAccountAuthorized?.(account);
    if (pending) await pending.resume();
    else if (!authorizationOnly) await loadDirectory(account, rootId, [{ id: rootId, name: '根目录' }]);
    return true;
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
    if (!supportsDirectPasswordLogin || passwordLoginInFlight.current) return;
    if (!username.trim() || (!password && !useSavedPassword)) {
      setAuthorizationStatus('请输入账号和密码后再登录。');
      return;
    }
    const credentials: CloudCredentials = useSavedPassword
      ? { username: username.trim(), useSavedPassword: true }
      : { username: username.trim(), password };
    const submittedInputVersion = passwordInputVersion.current;
    passwordLoginInFlight.current = true;
    setPasswordLoginBusy(true);
    try {
      await addAccount(() => client.loginCloudWithPassword(provider, credentials));
    } finally {
      // Submitted plaintext is released from component state after the attempt.
      if (credentials.password && passwordInputVersion.current === submittedInputVersion) {
        setPassword('');
        const saved = await client.getSavedCloudCredentials?.(provider).catch(() => null);
        if (passwordInputVersion.current === submittedInputVersion) {
          setSavedUsername(saved?.hasSavedPassword ? saved.username : null);
        }
      }
      passwordLoginInFlight.current = false;
      setPasswordLoginBusy(false);
    }
  }

  async function beginWebAuthorization() {
    if (webAuthorizationActive.current) return;
    const generation = ++authorizationGeneration.current;
    webAuthorizationActive.current = true;
    authorizationRegistered.current = false;
    authorizationWakePending.current = false;
    setWebAuthorization({ provider, windowLabel: '', instructions: '' });
    setAuthorizationStatus('正在准备网页授权窗口…');
    try {
      const authorization = await client.beginCloudAuthorization(provider);
      if (!webAuthorizationActive.current || generation !== authorizationGeneration.current) return;
      authorizationRegistered.current = true;
      setWebAuthorization(authorization);
      setAuthorizationStatus(authorization.instructions);
      if (authorizationWakePending.current) authorizationWake.current?.();
    } catch (error) {
      if (generation !== authorizationGeneration.current) return;
      webAuthorizationActive.current = false;
      setWebAuthorization(null);
      setAuthorizationStatus(errorMessage(error, '无法打开授权窗口。'));
    }
  }

  async function completeWebAuthorization(fromPolling: boolean) {
    const generation = authorizationGeneration.current;
    if (!webAuthorizationActive.current || !authorizationRegistered.current || authorizationCompletionInFlight.current === generation) return;
    authorizationCompletionInFlight.current = generation;
    try {
      const account = await client.completeCloudAuthorization(provider);
      if (!webAuthorizationActive.current || generation !== authorizationGeneration.current) return;
      ensureProviderAccount(account);
      const accepted = await acceptAccount(account);
      if (accepted) webAuthorizationActive.current = false;
    } catch (error) {
      if (!webAuthorizationActive.current || generation !== authorizationGeneration.current) return;
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
        webAuthorizationActive.current = false;
        if (activeAccount) {
          setReauthorizationRequired(true);
          setOperation({ kind: 'error', message });
        }
      }
      if (requiresOfficialWebAuthorization(error)) {
        webAuthorizationActive.current = false;
        setWebAuthorization(null);
      }
    } finally {
      if (authorizationCompletionInFlight.current === generation) authorizationCompletionInFlight.current = null;
    }
  }

  const completeWebAuthorizationRef = useRef(completeWebAuthorization);
  completeWebAuthorizationRef.current = completeWebAuthorization;
  const acceptAccountRef = useRef(acceptAccount);
  acceptAccountRef.current = acceptAccount;
  const waitingForWebAuthorization = webAuthorization !== null;
  useEffect(() => {
    if (!waitingForWebAuthorization) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let timer: number | undefined;
    const generation = authorizationGeneration.current;
    let querying = false;
    const current = () => !cancelled && webAuthorizationActive.current && generation === authorizationGeneration.current;
    const wake = async () => {
      if (!current() || querying) return;
      if (!authorizationRegistered.current) { authorizationWakePending.current = true; return; }
      querying = true;
      try {
        const account = await client.getCompletedCloudAuthorization?.(provider);
        if (!current() || account?.provider !== provider) return;
        const accepted = await acceptAccountRef.current(account);
        if (accepted && timer !== undefined) window.clearTimeout(timer);
      } catch { /* Lost wakeups are covered by the watchdog. */ }
      finally { querying = false; }
    };
    authorizationWake.current = () => { void wake(); };
    void client.subscribeCloudAuthorization?.(provider, event => {
      if (event.provider === provider && event.state === 'Ready') void wake();
    }).then(stop => { if (cancelled) stop(); else unlisten = stop; }).catch(() => { /* The watchdog also covers unavailable event delivery. */ });
    const poll = () => {
      timer = window.setTimeout(async () => {
        if (!current()) return;
        await completeWebAuthorizationRef.current(true);
        if (current()) poll();
      }, authorizationWatchdogMs);
    };
    poll();
    return () => {
      cancelled = true;
      if (generation === authorizationGeneration.current) {
        webAuthorizationActive.current = false;
        authorizationWake.current = null;
      }
      unlisten?.();
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [client, provider, waitingForWebAuthorization]);

  async function runOperation(name: string, action: () => Promise<unknown>, refresh = true, retryUploadFailures?: (failures: CloudUploadOutcome['failures']) => () => Promise<void>) {
    if (!activeAccount || operationInFlight.current) return;
    operationInFlight.current = true;
    setOperation({ kind: 'working', message: `${name}进行中…` });
    try {
      const result = await action();
      if (name.startsWith('上传') && isCloudUploadOutcome(result)) {
        if (result.uploaded.length === 0 && result.failures.length === 0) {
          setOperation(null);
          return;
        }
        if (refresh && !await loadDirectory(activeAccount, currentDirectoryId, breadcrumbs)) return;
        if (result.failures.length > 0) {
          const first = result.failures.find(failure => ['web_auth_required', 'credentials_expired', 'session_renewed_retry_operation'].includes(failure.code)) ?? result.failures[0];
          if (first.code === 'web_auth_required' || (first.code === 'credentials_expired' && effectiveAuthorizationCapabilities.web)) {
            const authorizationFailures = result.failures.filter(failure => failure.code === 'web_auth_required' || (failure.code === 'credentials_expired' && effectiveAuthorizationCapabilities.web));
            queueAuthorizationContinuation(name, retryUploadFailures?.(authorizationFailures) ?? (() => runOperation(name, action, refresh, retryUploadFailures)), `${name}需要网页验证。${first.message ? ` ${first.message}` : ''}`);
            return;
          }
          if (first.code === 'credentials_expired') {
            setDirectory(null);
            setDirectoryState('expired');
            setAuthorizationStatus(first.message || '登录凭据已过期。');
            return;
          }
          if (first.code === 'session_renewed_retry_operation') {
            setOperation({ kind: 'error', message: `已自动恢复登录；成功 ${result.uploaded.length} 项，失败 ${result.failures.length} 项，请重试失败项目` });
            return;
          }
          const listedFailures = result.failures.slice(0, 3).map(failure => `${failure.itemName}：${failure.message}`).join('；');
          const remainingFailures = result.failures.length > 3 ? `；另 ${result.failures.length - 3} 项` : '';
          setOperation({
            kind: 'error',
            message: `${name}完成：成功 ${result.uploaded.length} 项，失败 ${result.failures.length} 项：${listedFailures}${remainingFailures}`
          });
          return;
        }
        setOperation({ kind: 'success', message: `已上传 ${result.uploaded.length} 项` });
        setEditor(null);
        setEditorValue('');
        return;
      }
      if (refresh && !await loadDirectory(activeAccount, currentDirectoryId, breadcrumbs)) return;
      const message = name === '上传' && Array.isArray(result) ? `已上传 ${result.length} 个文件` : `${name}成功`;
      setOperation({ kind: 'success', message });
      setEditor(null);
      setEditorValue('');
    } catch (error) {
      if (requiresPikpakOperationVerification(error)) {
        showOperationVerification(error);
      } else if (needsInPlaceWebAuthorization(error)) {
        // Keep the existing account and directory alive while the official
        // browser proves the requested action. `cloud_begin_authorization`
        // only opens the WebView; the saved session is replaced later, after
        // a successful `cloud_complete_authorization` validation.
        const detail = errorMessage(error, '');
        queueAuthorizationContinuation(name, () => runOperation(name, action, refresh), `${name}需要网页验证。${detail ? ` ${detail}` : ''}`);
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
      setOperation(current => current?.kind === 'working'
        ? { kind: 'error', message: `${name}已执行，但列表刷新失败；请刷新列表确认结果。` }
        : current);
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
    const anchorIndex = selectionAnchor.current === null ? -1 : sortedItems.findIndex((entry) => entry.id === selectionAnchor.current);
    if (event.shiftKey && anchorIndex >= 0) {
      const from = Math.min(anchorIndex, index);
      const to = Math.max(anchorIndex, index);
      const range = sortedItems.slice(from, to + 1).filter((entry) => !isVirtualRecycleBinItem(entry)).map((entry) => entry.id);
      setSelectedIds(event.ctrlKey || event.metaKey ? (current) => Array.from(new Set([...current.filter((id) => id !== recycleBinId), ...range])) : range);
    } else if (event.ctrlKey || event.metaKey) {
      toggleSelection(item.id);
    } else {
      setSelectedIds([item.id]);
    }
    selectionAnchor.current = item.id;
  }

  function openFolder(item: CloudFile) {
    if (!activeAccount || isRecycleBin || item.kind !== 'folder' || operationBusy) return;
    void loadDirectory(activeAccount, item.id, [...breadcrumbs, { id: item.id, name: item.name }]);
  }

  function changeSort(column: CloudSortColumn) {
    setSort((current) => current?.column === column
      ? { column, direction: current.direction === 'ascending' ? 'descending' : 'ascending' }
      : { column, direction: 'ascending' });
  }

  function sortDescription(column: CloudSortColumn) {
    if (sort?.column !== column) return '未排序';
    return `当前为${sort.direction === 'ascending' ? '升序' : '降序'}`;
  }

  function submitDownload(items: readonly CloudFile[], privateDownload?: boolean) {
    void runOperation('下载', async () => {
      const directoryFailures: CloudDirectoryDownloadFailure[] = [];
      let queuedDirectoryFiles = 0;
      await Promise.all(items.map(async (item) => {
        if (item.kind === 'folder') {
          const outcome = privateDownload === undefined
            ? await client.downloadCloudDirectory(provider, item.id, item.name)
            : await client.downloadCloudDirectory(provider, item.id, item.name, privateDownload);
          onDownloadTask?.(outcome.task.queueTask ?? outcome.task);
          directoryFailures.push(...outcome.failures);
          queuedDirectoryFiles += outcome.queuedCount;
          return;
        }
        const task = privateDownload === undefined
          ? await client.downloadCloudFile(provider, item.id, item.name)
          : await client.downloadCloudFile(provider, item.id, item.name, privateDownload);
        onDownloadTask?.(task);
      }));
      if (directoryFailures.length) {
        const details = directoryFailures.slice(0, 5).map((failure) => `${failure.relativePath}：${failure.message}`).join('；');
        const remaining = directoryFailures.length > 5 ? `；另有 ${directoryFailures.length - 5} 项失败` : '';
        throw new Error(`目录下载已入队 ${queuedDirectoryFiles} 个文件；${details}${remaining}`);
      }
    }, false);
  }

  function downloadSelection() {
    if (!selected.length || selectedVirtualRecycleBin || isRecycleBin) return;
    if (privacyDownloadEnabled) {
      setPendingDownload(selected.slice());
      return;
    }
    submitDownload(selected);
  }

  function confirmPendingDownload(privateDownload: boolean) {
    const items = pendingDownload;
    setPendingDownload(null);
    if (items?.length) submitDownload(items, privateDownload);
  }

  function uploadFiles() {
    if (isRecycleBin || operationBusy) return;
    setUploadPickerOpen(true);
  }

  async function uploadSelectedFiles(paths: string[]) {
    await runOperation('上传', () => client.uploadCloudFiles!(provider, currentDirectoryId, paths), true, (failures) => {
      const indexed = failures
        .map((failure) => failure.inputIndex)
        .filter((index): index is number => typeof index === 'number' && Number.isInteger(index) && index >= 0 && index < paths.length);
      const retryPaths = indexed.length === failures.length ? indexed.map((index) => paths[index]) : paths;
      return () => uploadSelectedFiles(retryPaths);
    });
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
    if ((event.target as HTMLElement).closest('.cloud-sort-button')) return;
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
      <form className="cloud-login-form" onSubmit={submitPasswordLogin}>
        {supportsDirectPasswordLogin && <>
          <input aria-label="账号" placeholder="账号" autoComplete="username" value={username} onChange={(event) => { passwordFieldsEdited.current = true; passwordInputVersion.current += 1; setUsername(event.target.value); }} />
          <input aria-label="密码" placeholder={useSavedPassword ? "***" : "密码"} type="password" autoComplete="current-password" value={password} onChange={(event) => { passwordFieldsEdited.current = true; passwordInputVersion.current += 1; setSavedUsername(null); setPassword(event.target.value); }} />
          <button className="primary" type="submit" disabled={passwordLoginBusy}>{passwordLoginBusy ? '正在自动登录…' : '登录'}</button>
        </>}
        {effectiveAuthorizationCapabilities.web && <button type="button" className="secondary" disabled={passwordLoginBusy} onClick={() => void beginWebAuthorization()}>使用网页授权</button>}
        {webAuthorization && <button type="button" className="secondary" onClick={() => void completeWebAuthorization(false)}>我已完成授权</button>}
      </form>
      {showAuthorizationFeedback && <p className={`cloud-authorization-feedback${expired ? ' error' : ''}`} role="status">{expired && <b>凭据已过期</b>}<span>{authorizationStatus}</span></p>}
    </section>;
  }

  function authorizationContinuationButton() {
    if (operation?.operationVerification && client.openCloudOperationVerification) {
      return <button className="secondary cloud-authorization-continuation" disabled={openingOperationVerification || operationBusy} onClick={() => void openOperationVerification()}>打开 PikPak 操作验证</button>;
    }
    if (!pendingAuthorizationName || !effectiveAuthorizationCapabilities.web) return null;
    return <button className="secondary cloud-authorization-continuation" disabled={operationBusy || webAuthorization !== null} onClick={() => void beginWebAuthorization()}>打开 {label} 授权并继续{pendingAuthorizationName}</button>;
  }

  function sessionActions() {
    return <div className="cloud-session-actions"><button className="cloud-auth-state" disabled>{authorizedLabel}</button>{reauthorizationRequired && effectiveAuthorizationCapabilities?.web && <button className="secondary" disabled={operationBusy} onClick={() => void beginWebAuthorization()}>重新网页授权</button>}<button className="secondary" disabled={operationBusy} onClick={() => setPendingLogout(true)}>{logoutLabel}</button></div>;
  }

  function logoutConfirmation() {
    return pendingLogout && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label={`确认${logoutLabel}`}><h3>确认{logoutLabel}</h3><p>这只会清除本机授权会话，不会删除网盘中的任何文件。</p><button className="secondary" onClick={() => setPendingLogout(false)}>取消</button><button className="danger-button" onClick={() => { setPendingLogout(false); void runOperation(logoutLabel, async () => {
      await client.logoutCloud(provider);
      passwordFieldsEdited.current = false;
      passwordInputVersion.current += 1;
      setUsername('');
      setSavedUsername(null);
      setPassword('');
      setActiveAccount(null);
      setDirectory(null);
      setDirectoryState('idle');
      setAuthorizationStatus('未授权。请登录或在网页授权窗口中完成验证。');
      onAccountLoggedOut?.(provider);
    }, false); }}>确认{logoutLabel}</button></section></div>;
  }

  function accountManagementView() {
    return <section className={`cloud-manager cloud-authorization cloud-connected-account cloud-provider-${provider}`} aria-label={`${label}账号授权`}>
      <header className="cloud-heading"><h2>{label}网盘 <span className="cloud-account-name">· {activeAccount?.displayName}</span></h2></header>
      {sessionActions()}
      {logoutConfirmation()}
      {operation && <div className={`cloud-operation-status ${operation.kind}`} role={operation.kind === 'error' ? 'alert' : 'status'}><span>{operation.message}</span>{operation.kind === 'error' && authorizationContinuationButton()}</div>}
    </section>;
  }

  if (authorizationOnly && activeAccount) return accountManagementView();
  if (authorizationOnly || !activeAccount) return authorizationView();
  if (directoryState === 'expired' && !showSessionActions) return <section className="cloud-manager" aria-label={`${label}文件管理器`}><header className="cloud-heading"><h2>{label}网盘</h2></header><div className="cloud-state cloud-error" role="alert"><span>登录凭据已过期。</span>{authorizationContinuationButton()}</div></section>;
  if (directoryState === 'expired') return authorizationView(true);

  const parent = breadcrumbs.at(-2);
  return <section className="cloud-manager cloud-file-manager" aria-label={`${label}文件管理器`}>
    {showSessionActions && <div className="cloud-file-sessionbar">
      {showSessionActions && sessionActions()}
    </div>}

    <div className="cloud-filebar">
      <div className="cloud-pathbar">
        <button className="cloud-icon-button" aria-label="返回上一级" title="返回上一级" disabled={!parent || operationBusy} onClick={() => { if (parent) void loadDirectory(activeAccount, parent.id, breadcrumbs.slice(0, -1)); }}><CloudActionIcon action="back" /></button>
        <div className="cloud-breadcrumbs" role="navigation" aria-label="当前目录">{breadcrumbs.map((crumb, index) => <span key={`${crumb.id}-${index}`}><button title={crumb.name} aria-current={index === breadcrumbs.length - 1 ? 'page' : undefined} disabled={index === breadcrumbs.length - 1} onClick={() => void loadDirectory(activeAccount, crumb.id, breadcrumbs.slice(0, index + 1))}>{crumb.name}</button>{index < breadcrumbs.length - 1 && <i aria-hidden="true">›</i>}</span>)}</div>
      </div>
      <div className="cloud-toolbar cloud-toolbar--single-line" aria-label="文件操作">
        {!isRecycleBin && <button aria-label="上传" disabled={operationBusy} onClick={uploadFiles}><CloudActionIcon action="upload" />上传</button>}
        {!isRecycleBin && <button aria-label="新建目录" disabled={operationBusy} onClick={() => openEditor('create')}><CloudActionIcon action="create" />新建文件夹</button>}
        <button className="cloud-icon-button cloud-refresh-button" aria-label="刷新" title="刷新" disabled={operationBusy} onClick={refreshDirectory}><CloudActionIcon action="refresh" /></button>
      </div>
    </div>

    {editor && <form className="cloud-operation-editor" onSubmit={submitEditor} onKeyDown={(event) => { if (event.key === 'Escape') { event.preventDefault(); setEditor(null); } }}><label>{editor === 'create' ? '目录名称' : '新名称'}<input aria-label={editor === 'create' ? '目录名称' : '新名称'} value={editorValue} onChange={(event) => setEditorValue(event.target.value)} autoFocus /></label><button className="primary" type="submit" disabled={operationBusy}>{editor === 'create' ? '确认新建目录' : '确认重命名'}</button><button type="button" className="secondary" disabled={operationBusy} onClick={() => setEditor(null)}>取消</button></form>}
    {pendingTrash && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label={isRecycleBin ? '确认永久删除' : '确认移入回收站'}><h3>{isRecycleBin ? '确认永久删除' : '确认移入回收站'}</h3><p>{isRecycleBin ? `所选 ${selected.length} 个项目将被永久删除，不可恢复。` : `所选 ${selected.length} 个项目将移入回收站，不会永久删除。`}</p><button className="secondary" onClick={() => setPendingTrash(false)}>取消</button><button className="danger-button" onClick={() => { setPendingTrash(false); void runOperation(isRecycleBin ? '永久删除' : '删除', () => isRecycleBin ? client.deleteCloudFiles(provider, selectedIds.slice()) : client.trashCloudFiles(provider, selectedIds.slice())); }}>{isRecycleBin ? '永久删除' : '移入回收站'}</button></section></div>}
    {pendingDownload && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label="确认下载"><h3>确认下载</h3><p>所选 {pendingDownload.length} 个项目可作为普通任务或隐私任务加入下载队列。</p><button className="secondary" onClick={() => setPendingDownload(null)}>取消</button><button className="primary" onClick={() => confirmPendingDownload(false)}>立即下载</button><button className="secondary" onClick={() => confirmPendingDownload(true)}>隐私下载</button></section></div>}
    {showSessionActions && logoutConfirmation()}
    {operation && <div className={`cloud-operation-status ${operation.kind}`} role={operation.kind === 'error' ? 'alert' : 'status'}><span>{operation.message}</span>{operation.kind === 'error' && authorizationContinuationButton()}</div>}
    {!showSessionActions && reauthorizationRequired && effectiveAuthorizationCapabilities?.web && <button className="secondary" disabled={operationBusy} onClick={() => void beginWebAuthorization()}>重新网页授权</button>}
    {directoryState === 'loading' && <div className="cloud-loading-state" role="status"><i /><span>正在加载文件…</span></div>}
    {directoryState === 'error' && <div className="cloud-state cloud-error" role="alert"><b>加载失败</b><span>{authorizationStatus}</span>{authorizationContinuationButton()}<button className="secondary" onClick={refreshDirectory}>重试</button></div>}
    {(directoryState === 'ready' || directoryState === 'empty') && <div
      className="cloud-list"
      role="tree"
      aria-label={`${label}网盘文件`}
      tabIndex={0}
      onKeyDown={handleFileListKeyDown}
      onContextMenu={(event) => { if (!(event.target as HTMLElement).closest('[data-cloud-file-row]')) { event.preventDefault(); setSelectedIds([]); setContextMenu({ kind: 'background', x: event.clientX, y: event.clientY }); } }}
    >
      <div className="cloud-row cloud-columns"><span className="cloud-check-cell"><input ref={selectAllCheckbox} type="checkbox" aria-label="全选当前目录" checked={allSelected} onChange={(event) => setSelectedIds(event.target.checked ? selectableItems.map((item) => item.id) : [])} /></span><span className="cloud-name-column">{selected.length > 0 ? <span className="cloud-header-selection" role="toolbar" aria-label={`已选择 ${selected.length} 项的文件操作`}><b>已选择 {selected.length} 项</b>{selectedVirtualRecycleBin ? <span className="cloud-selection-hint">回收站仅可打开</span> : isRecycleBin ? <span className="cloud-selection-actions"><button disabled={operationBusy} onClick={() => void runOperation('恢复', () => client.restoreCloudFiles(provider, selectedIds.slice()))}><CloudActionIcon action="restore" />恢复</button><button className="danger" disabled={operationBusy} onClick={() => setPendingTrash(true)}><CloudActionIcon action="delete" />永久删除</button></span> : <span className="cloud-selection-actions"><button disabled={operationBusy} onClick={downloadSelection}><CloudActionIcon action="download" />下载</button><button className="danger" disabled={operationBusy} onClick={() => setPendingTrash(true)}><CloudActionIcon action="delete" />删除</button><button ref={selectionMenuTrigger} className="cloud-selection-more" aria-label="更多文件操作" aria-haspopup="menu" aria-expanded={Boolean(selectionMenuAnchor)} disabled={operationBusy} onClick={openSelectionOverflowMenu}>•••</button></span>}<button className="cloud-clear-selection" aria-label="取消选择" title="取消选择" onClick={() => setSelectedIds([])}><CloudActionIcon action="clear" /></button></span> : <button className="cloud-sort-button" type="button" aria-label="按名称排序" aria-description={sortDescription('name')} aria-pressed={sort?.column === 'name'} onClick={() => changeSort('name')}>名称<span aria-hidden="true">{sort?.column === 'name' ? sort.direction === 'ascending' ? '↑' : '↓' : '↕'}</span></button>}</span><span><button className="cloud-sort-button" type="button" aria-label="按大小排序" aria-description={sortDescription('size')} aria-pressed={sort?.column === 'size'} onClick={() => changeSort('size')}>大小<span aria-hidden="true">{sort?.column === 'size' ? sort.direction === 'ascending' ? '↑' : '↓' : '↕'}</span></button></span><span><button className="cloud-sort-button" type="button" aria-label="按修改时间排序" aria-description={sortDescription('modifiedAt')} aria-pressed={sort?.column === 'modifiedAt'} onClick={() => changeSort('modifiedAt')}>修改时间<span aria-hidden="true">{sort?.column === 'modifiedAt' ? sort.direction === 'ascending' ? '↑' : '↓' : '↕'}</span></button></span></div>
      {directoryState === 'empty' ? <div className="cloud-empty"><CloudFileIcon kind="folder" /><b>{isRecycleBin ? '回收站为空' : '此目录为空'}</b><span>{isRecycleBin ? '没有可恢复或永久删除的项目' : '可在这里上传文件或新建目录'}</span></div> : sortedItems.map((item, index) => {
        const isSelected = selectedIds.includes(item.id);
        const iconKind = isVirtualRecycleBinItem(item) ? 'recycle-bin' : item.kind;
        return <div
          className={`cloud-row cloud-file-row ${isSelected ? 'selected' : ''}`}
          data-cloud-file-row
          role="treeitem"
          aria-selected={isSelected}
          key={item.id}
          onClick={(event) => selectRow(event, item, index)}
          onDoubleClick={(event) => { event.preventDefault(); openFolder(item); }}
          onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); if (!isSelected) setSelectedIds([item.id]); selectionAnchor.current = item.id; setContextMenu({ kind: 'selection', item, x: event.clientX, y: event.clientY }); }}
        >
          <span className="cloud-check-cell"><input type="checkbox" aria-label={`选择 ${item.name}`} checked={isSelected} onClick={(event) => event.stopPropagation()} onChange={() => toggleSelection(item.id)} /></span>
          <span className="cloud-file-name"><i className={iconKind}><CloudFileIcon kind={iconKind} /></i><b title={item.name}>{item.name}</b></span>
          <span>{item.kind === 'folder' ? '—' : readableSize(item.size)}</span>
          <span>{formatCloudModifiedAt(item.modifiedAt)}</span>
        </div>;
      })}
    </div>}
    {uploadPickerOpen && <LocalUploadPicker listEntries={client.listLocalUploadEntries ?? unavailableLocalUploadListing} onCancel={() => setUploadPickerOpen(false)} onConfirm={paths => { setUploadPickerOpen(false); void uploadSelectedFiles(paths); }} />}
    {contextMenu && <CloudContextMenu state={contextMenu} selectedCount={selected.length} recycleBin={isRecycleBin} virtualRecycleBin={contextMenu.kind === 'selection' && isVirtualRecycleBinItem(contextMenu.item)} onClose={() => setContextMenu(null)} onAction={runContextAction} />}
    {selectionMenuAnchor && <CloudSelectionOverflowMenu anchor={selectionMenuAnchor} singleSelection={selected.length === 1} recycleBin={isRecycleBin} disabled={Boolean(operationBusy)} onClose={() => setSelectionMenuAnchor(null)} onAction={runSelectionOverflowAction} />}
    {detailsFile && <CloudFileDetails file={detailsFile} path={`/${breadcrumbs.slice(1).map((crumb) => crumb.name).join('/') || ''}`} onClose={() => setDetailsFile(null)} />}
    {destinationAction && <CloudDestinationPicker action={destinationAction} descriptor={descriptor} provider={provider} client={client} excludedIds={selectedIds} onOfficialWebAuthorizationRequired={(error) => { const action = destinationAction; setDestinationAction(null); queueAuthorizationContinuation(`选择${action === 'copy' ? '复制' : '移动'}目标`, async () => { setDestinationAction(action); }, errorMessage(error, `读取${action === 'copy' ? '复制' : '移动'}目标目录需要网页验证。`)); }} onClose={() => setDestinationAction(null)} onChoose={(targetParentId) => { const action = destinationAction; setDestinationAction(null); if (action === 'move') void runOperation('移动', () => client.moveCloudFiles(provider, selectedIds.slice(), targetParentId)); else void runOperation('复制', () => client.copyCloudFiles(provider, selectedIds.slice(), targetParentId)); }} />}
  </section>;
}

function isCloudUploadOutcome(value: unknown): value is CloudUploadOutcome {
  return typeof value === 'object' && value !== null
    && Array.isArray((value as CloudUploadOutcome).uploaded)
    && Array.isArray((value as CloudUploadOutcome).failures);
}

function unavailableLocalUploadListing(): Promise<LocalUploadListing> {
  return Promise.reject(new Error('本地文件选择器不可用，请使用最新版本。'));
}
