import { FormEvent, KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { CloudAccount, CloudAuthorization, CloudCredentials, CloudDirectory, CloudFile, CloudProvider } from '../lib/cloudTypes';

export type { CloudAccount, CloudAuthorization, CloudCredentials, CloudDirectory, CloudFile, CloudProvider } from '../lib/cloudTypes';

export interface CloudManagerClient {
  beginCloudAuthorization(provider: CloudProvider): Promise<CloudAuthorization>;
  completeCloudAuthorization(provider: CloudProvider): Promise<CloudAccount>;
  loginCloudWithPassword(provider: CloudProvider, credentials: CloudCredentials): Promise<CloudAccount>;
  getCloudAccount(provider: CloudProvider): Promise<CloudAccount>;
  listCloudDirectory(provider: CloudProvider, parentId?: string): Promise<CloudDirectory>;
  createCloudDirectory(provider: CloudProvider, parentId: string, name: string): Promise<CloudFile | void>;
  renameCloudFile(provider: CloudProvider, fileId: string, name: string): Promise<void>;
  moveCloudFiles(provider: CloudProvider, fileIds: string[], targetParentId: string): Promise<void>;
  trashCloudFiles(provider: CloudProvider, fileIds: string[]): Promise<void>;
  downloadCloudFile(provider: CloudProvider, fileId: string): Promise<unknown>;
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
type Editor = 'create' | 'rename' | 'move' | null;
type OperationState = { kind: 'working' | 'success' | 'error'; message: string } | null;
interface Breadcrumb { id: string; name: string }
type CloudMenuAction = 'open' | 'download' | 'rename' | 'move' | 'delete' | 'create' | 'upload' | 'refresh';
type CloudContextMenuState = { kind: 'selection'; item: CloudFile; x: number; y: number } | { kind: 'background'; x: number; y: number };

const providerLabel: Record<CloudProvider, string> = { quark: '夸克', pikpak: 'PikPak' };
const rootId = '0';
const authorizationInitialPollMs = 800;
const authorizationPollMs = 1_800;

const unavailableClient: CloudManagerClient = {
  beginCloudAuthorization: () => Promise.reject(new Error('网盘连接器尚未接入')),
  completeCloudAuthorization: () => Promise.reject(new Error('网盘连接器尚未接入')),
  loginCloudWithPassword: () => Promise.reject(new Error('网盘连接器尚未接入')),
  getCloudAccount: () => Promise.reject(new Error('网盘连接器尚未接入')),
  listCloudDirectory: () => Promise.reject(new Error('网盘连接器尚未接入')),
  createCloudDirectory: () => Promise.reject(new Error('网盘连接器尚未接入')),
  renameCloudFile: () => Promise.reject(new Error('网盘连接器尚未接入')),
  moveCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  trashCloudFiles: () => Promise.reject(new Error('网盘连接器尚未接入')),
  downloadCloudFile: () => Promise.reject(new Error('网盘连接器尚未接入')),
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

function isAuthorizationWindowUnavailable(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && (error as { code?: unknown }).code === 'web_auth_required';
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

function CloudFileIcon({ kind }: { kind: CloudFile['kind'] }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24">{kind === 'folder'
    ? <><path d="M3.5 7.5h6l1.8 2h10.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M3.5 7.5V5.7a2.2 2.2 0 0 1 2.2-2.2h4.1l1.8 2h6.7a2.2 2.2 0 0 1 2.2 2.2v1.8" /></>
    : <><path d="M6 2.8h7l5 5v13.4H6z" /><path d="M13 2.8v5h5M9 13h6M9 16.5h6" /></>}</svg>;
}

function CloudActionIcon({ action }: { action: CloudMenuAction | 'back' | 'clear' }) {
  const paths: Record<CloudMenuAction | 'back' | 'clear', React.ReactNode> = {
    open: <><path d="M3.5 7.5h6l1.8 2h9.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="m12 13 2.5 2.5L18 12" /></>,
    download: <><path d="M12 3v12m0 0 4-4m-4 4-4-4" /><path d="M5 20h14" /></>,
    rename: <><path d="m4 17-.7 3.7L7 20l10.8-10.8-3-3z" /><path d="m13.8 7.2 3 3" /></>,
    move: <><path d="M3.5 7.5h6l1.8 2h9.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="m10 14 2-2 2 2m-2-2v5" /></>,
    delete: <><path d="M5 7h14M9 7V4h6v3M8 10v8M12 10v8M16 10v8M6.5 7l.8 14h9.4l.8-14" /></>,
    create: <><path d="M3.5 7.5h6l1.8 2h9.2v8.8a2.2 2.2 0 0 1-2.2 2.2H5.7a2.2 2.2 0 0 1-2.2-2.2z" /><path d="M12 12v6M9 15h6" /></>,
    upload: <><path d="M12 16V4m0 0-4 4m4-4 4 4" /><path d="M5 20h14" /></>,
    refresh: <><path d="M19 8a7.5 7.5 0 1 0 1 7" /><path d="M19 3v5h-5" /></>,
    back: <path d="m14.5 5-7 7 7 7" />,
    clear: <><path d="m7 7 10 10" /><path d="m17 7-10 10" /></>
  };
  return <svg aria-hidden="true" viewBox="0 0 24 24">{paths[action]}</svg>;
}

function CloudContextMenu({ state, selectedCount, onAction, onClose }: { state: CloudContextMenuState; selectedCount: number; onAction(action: CloudMenuAction): void; onClose(): void }) {
  const menuRef = useRef<HTMLDivElement>(null);
  const items: Array<{ action: CloudMenuAction; label: string; danger?: boolean; separated?: boolean }> = state.kind === 'background'
    ? [{ action: 'create', label: '新建目录' }, { action: 'upload', label: '上传文件' }, { action: 'refresh', label: '刷新', separated: true }]
    : [...(selectedCount === 1 && state.item.kind === 'folder' ? [{ action: 'open' as const, label: '打开' }] : []), { action: 'download', label: '下载' }, ...(selectedCount === 1 ? [{ action: 'rename' as const, label: '重命名', separated: true }] : []), { action: 'move', label: '移动', separated: selectedCount > 1 }, { action: 'delete', label: '删除', danger: true, separated: true }];
  const width = 210;
  const estimatedHeight = items.length * 38 + 24;
  const left = Math.max(8, Math.min(state.x, window.innerWidth - width - 8));
  const top = Math.max(8, Math.min(state.y, window.innerHeight - estimatedHeight - 8));

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

export function CloudManager({ provider, client = unavailableClient, initialAccounts = [], onDownloadTask, authorizationOnly = false, restoreOnMount = true, showAccountTabs = true, showSessionActions = true, onAccountAuthorized, onAccountLoggedOut }: CloudManagerProps) {
  const label = providerLabel[provider];
  const providerAccounts = initialAccounts.filter((account) => account.provider === provider);
  const [accounts, setAccounts] = useState<readonly CloudAccount[]>(providerAccounts);
  const [activeAccountId, setActiveAccountId] = useState<string | null>(providerAccounts[0]?.id ?? null);
  const [directory, setDirectory] = useState<CloudDirectory | null>(null);
  const [directoryState, setDirectoryState] = useState<DirectoryState>('idle');
  const [breadcrumbs, setBreadcrumbs] = useState<readonly Breadcrumb[]>([{ id: rootId, name: '根目录' }]);
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [authorizationStatus, setAuthorizationStatus] = useState('未授权。请登录或在网页授权窗口中完成验证。');
  const [webAuthorization, setWebAuthorization] = useState<CloudAuthorization | null>(null);
  const [editor, setEditor] = useState<Editor>(null);
  const [editorValue, setEditorValue] = useState('');
  const [pendingTrash, setPendingTrash] = useState(false);
  const [pendingLogout, setPendingLogout] = useState(false);
  const [operation, setOperation] = useState<OperationState>(null);
  const [contextMenu, setContextMenu] = useState<CloudContextMenuState | null>(null);
  const authorizationCompletionInFlight = useRef(false);
  const operationInFlight = useRef(false);
  const selectionAnchor = useRef<number | null>(null);
  const selectAllCheckbox = useRef<HTMLInputElement>(null);

  const activeAccount = accounts.find((account) => account.id === activeAccountId) ?? null;
  const selected = directory?.items.filter((item) => selectedIds.includes(item.id)) ?? [];
  const currentDirectoryId = directory?.id ?? breadcrumbs.at(-1)?.id ?? rootId;
  const operationBusy = operation?.kind === 'working';
  const passwordSession = activeAccount?.authMethod === 'password';
  const authorizedLabel = passwordSession ? '已经登录' : '已经完成授权';
  const logoutLabel = passwordSession ? '退出登录' : '退出授权';
  const allSelected = Boolean(directory?.items.length) && selected.length === directory?.items.length;

  useEffect(() => {
    if (selectAllCheckbox.current) selectAllCheckbox.current.indeterminate = selected.length > 0 && !allSelected;
  }, [allSelected, selected.length]);

  async function loadDirectory(account: CloudAccount, parentId: string, nextBreadcrumbs: readonly Breadcrumb[]) {
    setDirectoryState('loading');
    setSelectedIds([]);
    setContextMenu(null);
    selectionAnchor.current = null;
    try {
      const next = await client.listCloudDirectory(provider, parentId);
      setDirectory(next);
      setBreadcrumbs(nextBreadcrumbs);
      setDirectoryState(next.items.length ? 'ready' : 'empty');
    } catch (error) {
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
      setAccounts((current) => current.some((item) => item.id === account.id) ? current : [...current, account]);
      setActiveAccountId(account.id);
    }).catch((error) => {
      if (active && !isUnauthorized(error)) setAuthorizationStatus(errorMessage(error, '无法恢复网盘会话。'));
    });
    return () => { active = false; };
  }, [authorizationOnly, client, provider, restoreOnMount]);

  async function acceptAccount(account: CloudAccount) {
    setAccounts((current) => current.some((item) => item.id === account.id) ? current.map((item) => item.id === account.id ? account : item) : [...current, account]);
    setActiveAccountId(account.id);
    setPassword('');
    setWebAuthorization(null);
    onAccountAuthorized?.(account);
    if (!authorizationOnly) await loadDirectory(account, rootId, [{ id: rootId, name: '根目录' }]);
  }

  async function addAccount(authorize: () => Promise<CloudAccount>) {
    setAuthorizationStatus('正在等待授权结果…');
    try { await acceptAccount(await authorize()); } catch (error) { setAuthorizationStatus(errorMessage(error, '授权失败，请重试。')); }
  }

  function submitPasswordLogin(event: FormEvent) {
    event.preventDefault();
    if (!username.trim() || !password) {
      setAuthorizationStatus('请输入账号和密码后再登录。');
      return;
    }
    void addAccount(() => client.loginCloudWithPassword(provider, { username: username.trim(), password }));
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
      setAuthorizationStatus(errorMessage(error, '网页授权未完成。'));
      if (fromPolling && isAuthorizationWindowUnavailable(error)) setWebAuthorization(null);
    } finally {
      authorizationCompletionInFlight.current = false;
    }
  }

  useEffect(() => {
    if (!webAuthorization || (activeAccount && directoryState !== 'expired')) return;
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
      if (isCredentialExpired(error)) {
        setDirectory(null);
        setDirectoryState('expired');
        setAuthorizationStatus(errorMessage(error, '登录凭据已过期。'));
      } else {
        setOperation({ kind: 'error', message: `${name}失败：${errorMessage(error, '请重试。')}` });
      }
    } finally {
      operationInFlight.current = false;
    }
  }

  function openEditor(next: Exclude<Editor, null>) {
    setContextMenu(null);
    setEditor(next);
    setEditorValue(next === 'rename' ? selected[0]?.name ?? '' : next === 'move' ? rootId : '');
  }

  function toggleSelection(itemId: string) {
    setSelectedIds((current) => current.includes(itemId) ? current.filter((id) => id !== itemId) : [...current, itemId]);
  }

  function selectRow(event: ReactMouseEvent, item: CloudFile, index: number) {
    if (event.shiftKey && selectionAnchor.current !== null && directory) {
      const from = Math.min(selectionAnchor.current, index);
      const to = Math.max(selectionAnchor.current, index);
      const range = directory.items.slice(from, to + 1).map((entry) => entry.id);
      setSelectedIds(event.ctrlKey || event.metaKey ? (current) => Array.from(new Set([...current, ...range])) : range);
    } else if (event.ctrlKey || event.metaKey) {
      toggleSelection(item.id);
    } else {
      setSelectedIds([item.id]);
    }
    selectionAnchor.current = index;
  }

  function openFolder(item: CloudFile) {
    if (!activeAccount || item.kind !== 'folder' || operationBusy) return;
    void loadDirectory(activeAccount, item.id, [...breadcrumbs, { id: item.id, name: item.name }]);
  }

  function downloadSelection() {
    if (!selected.length) return;
    void runOperation('下载', async () => {
      const tasks = await Promise.all(selected.map((item) => client.downloadCloudFile(provider, item.id)));
      tasks.forEach((task) => onDownloadTask?.(task));
    }, false);
  }

  function uploadFiles() {
    void runOperation('上传', () => client.pickAndUploadFiles(provider, currentDirectoryId));
  }

  function refreshDirectory() {
    if (activeAccount) void loadDirectory(activeAccount, currentDirectoryId, breadcrumbs);
  }

  function runContextAction(action: CloudMenuAction) {
    const menu = contextMenu;
    setContextMenu(null);
    if (action === 'open' && menu?.kind === 'selection') openFolder(menu.item);
    if (action === 'download') downloadSelection();
    if (action === 'rename') openEditor('rename');
    if (action === 'move') openEditor('move');
    if (action === 'delete') setPendingTrash(true);
    if (action === 'create') openEditor('create');
    if (action === 'upload') uploadFiles();
    if (action === 'refresh') refreshDirectory();
  }

  function handleFileListKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a') {
      event.preventDefault();
      setSelectedIds(directory?.items.map((item) => item.id) ?? []);
      return;
    }
    if (event.key === 'Escape') {
      setContextMenu(null);
      setEditor(null);
      return;
    }
    if (event.key === 'Enter' && selected.length === 1 && selected[0].kind === 'folder') {
      event.preventDefault();
      openFolder(selected[0]);
      return;
    }
    if (event.key === 'F2' && selected.length === 1) {
      event.preventDefault();
      openEditor('rename');
      return;
    }
    if (event.key === 'Delete' && selected.length) {
      event.preventDefault();
      setPendingTrash(true);
    }
  }

  function submitEditor(event: FormEvent) {
    event.preventDefault();
    if (!activeAccount || !editorValue.trim()) return;
    if (editor === 'create') void runOperation('新建目录', () => client.createCloudDirectory(provider, currentDirectoryId, editorValue.trim()).then(() => undefined));
    if (editor === 'rename' && selected[0]) void runOperation('重命名', () => client.renameCloudFile(provider, selected[0].id, editorValue.trim()));
    if (editor === 'move') void runOperation('移动', () => client.moveCloudFiles(provider, selectedIds.slice(), editorValue.trim()));
  }

  function authorizationView(expired = false) {
    return <section className={`cloud-manager cloud-authorization cloud-provider-${provider}`} aria-label={`${label}账号授权`}>
      <header className="cloud-heading"><div><p className="eyebrow">自有网盘 · 本地会话</p><h2>{label}网盘</h2><span>{expired ? '凭据失效后必须重新授权，文件列表不会继续显示。' : '授权完成前不会显示或加载任何文件。'}</span></div></header>
      {showAccountTabs && accounts.length > 0 && <div className="cloud-account-tabs" role="tablist" aria-label={`${label}账号`}>{accounts.map((account) => <button key={account.id} role="tab" aria-selected={account.id === activeAccountId}>{account.displayName} · {label}</button>)}</div>}
      <form className="cloud-login-form" onSubmit={submitPasswordLogin}>
        {provider === 'pikpak' && <><label>{label}账号<input aria-label={`${label}账号`} autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} /></label>
        <label>{label}密码<input aria-label={`${label}密码`} type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
        <button className="primary" type="submit">登录{label}账号</button></>}
        {provider === 'quark' && <p className="cloud-login-hint">夸克未提供可用的账号密码开放登录接口，请在官方网页窗口中登录。</p>}
        <button type="button" className="secondary" onClick={() => void beginWebAuthorization()}>使用网页授权</button>
        {webAuthorization && <button type="button" className="secondary" onClick={() => void completeWebAuthorization(false)}>我已完成授权</button>}
      </form>
      <p className="authorization-note" role="status"><b>{expired ? '凭据已过期' : '授权状态'}</b><span>{authorizationStatus}</span></p>
    </section>;
  }

  function sessionActions() {
    return <div className="cloud-session-actions"><button className="cloud-auth-state" disabled>{authorizedLabel}</button><button className="secondary" disabled={operationBusy} onClick={() => setPendingLogout(true)}>{logoutLabel}</button></div>;
  }

  function logoutConfirmation() {
    return pendingLogout && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label={`确认${logoutLabel}`}><h3>确认{logoutLabel}</h3><p>这只会清除本机授权会话，不会删除网盘中的任何文件。</p><button className="secondary" onClick={() => setPendingLogout(false)}>取消</button><button className="danger-button" onClick={() => { setPendingLogout(false); void runOperation(logoutLabel, async () => { await client.logoutCloud(provider); setAccounts([]); setActiveAccountId(null); setDirectory(null); setDirectoryState('idle'); setAuthorizationStatus('未授权。请登录或在网页授权窗口中完成验证。'); onAccountLoggedOut?.(provider); }, false); }}>确认{logoutLabel}</button></section></div>;
  }

  function accountManagementView() {
    return <section className={`cloud-manager cloud-authorization cloud-connected-account cloud-provider-${provider}`} aria-label={`${label}账号授权`}>
      <header className="cloud-heading"><div><p className="eyebrow">自有网盘 · 本地会话</p><h2>{label}网盘</h2><span>账号已连接，文件管理功能可从上方的“{label}网盘”标签进入。</span></div></header>
      <div className="cloud-account-summary"><span className="cloud-account-avatar" aria-hidden="true">{label.slice(0, 1)}</span><div><b>{activeAccount?.displayName}</b><small>{passwordSession ? '账号密码登录' : '官方网页授权'} · 凭据仅保存在本机</small></div></div>
      {sessionActions()}
      {logoutConfirmation()}
      {operation && <p className={`cloud-operation-status ${operation.kind}`} role={operation.kind === 'error' ? 'alert' : 'status'}>{operation.message}</p>}
    </section>;
  }

  if (authorizationOnly && activeAccount) return accountManagementView();
  if (authorizationOnly || !activeAccount) return authorizationView();
  if (directoryState === 'expired' && !showSessionActions) return <section className="cloud-manager" aria-label={`${label}文件管理器`}><header className="cloud-heading"><div><p className="eyebrow">自有网盘 · 本地会话</p><h2>{label}网盘</h2></div></header><p className="cloud-state cloud-error" role="alert">登录凭据已过期，请前往“网盘管理”重新授权。</p></section>;
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
        <div className="cloud-breadcrumbs" role="navigation" aria-label="当前目录">{breadcrumbs.map((crumb, index) => <span key={`${crumb.id}-${index}`}><button aria-current={index === breadcrumbs.length - 1 ? 'page' : undefined} disabled={index === breadcrumbs.length - 1} onClick={() => void loadDirectory(activeAccount, crumb.id, breadcrumbs.slice(0, index + 1))}>{crumb.name}</button>{index < breadcrumbs.length - 1 && <i aria-hidden="true">›</i>}</span>)}</div>
      </div>
      <div className="cloud-toolbar" aria-label="文件操作">
        <button disabled={operationBusy} onClick={() => openEditor('create')}><CloudActionIcon action="create" />新建目录</button>
        <button className="cloud-toolbar-primary" disabled={operationBusy} onClick={uploadFiles}><CloudActionIcon action="upload" />上传</button>
        <span className="cloud-toolbar-divider" aria-hidden="true" />
        <button disabled={operationBusy || !selected.length} onClick={downloadSelection}><CloudActionIcon action="download" />下载</button>
        <button disabled={operationBusy || selected.length !== 1} onClick={() => openEditor('rename')}><CloudActionIcon action="rename" />重命名</button>
        <button disabled={operationBusy || !selected.length} onClick={() => openEditor('move')}><CloudActionIcon action="move" />移动</button>
        <button disabled={operationBusy || !selected.length} onClick={() => setPendingTrash(true)}><CloudActionIcon action="delete" />删除</button>
        <button className="cloud-icon-button cloud-refresh-button" aria-label="刷新" title="刷新" disabled={operationBusy} onClick={refreshDirectory}><CloudActionIcon action="refresh" /></button>
      </div>
    </div>

    {selected.length > 0 && <div className="cloud-selection-summary"><span>已选择 {selected.length} 项</span><button onClick={() => setSelectedIds([])}><CloudActionIcon action="clear" />取消选择</button></div>}
    {editor && <form className="cloud-operation-editor" onSubmit={submitEditor} onKeyDown={(event) => { if (event.key === 'Escape') { event.preventDefault(); setEditor(null); } }}><label>{editor === 'create' ? '目录名称' : editor === 'rename' ? '新名称' : '目标目录'}{editor === 'move' ? <select aria-label="目标目录" value={editorValue} onChange={(event) => setEditorValue(event.target.value)} autoFocus><option value={rootId}>根目录</option>{directory?.items.filter((item) => item.kind === 'folder' && !selectedIds.includes(item.id)).map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select> : <input aria-label={editor === 'create' ? '目录名称' : '新名称'} value={editorValue} onChange={(event) => setEditorValue(event.target.value)} autoFocus />}</label><button className="primary" type="submit" disabled={operationBusy}>{editor === 'create' ? '确认新建目录' : editor === 'rename' ? '确认重命名' : '确认移动'}</button><button type="button" className="secondary" disabled={operationBusy} onClick={() => setEditor(null)}>取消</button></form>}
    {pendingTrash && <div className="cloud-confirm-backdrop"><section className="cloud-trash-confirm" role="alertdialog" aria-modal="true" aria-label="确认移入回收站"><h3>确认移入回收站</h3><p>所选 {selected.length} 个项目将移入回收站，不会永久删除。</p><button className="secondary" onClick={() => setPendingTrash(false)}>取消</button><button className="danger-button" onClick={() => { setPendingTrash(false); void runOperation('删除', () => client.trashCloudFiles(provider, selectedIds.slice())); }}>移入回收站</button></section></div>}
    {showSessionActions && logoutConfirmation()}
    {operation && <p className={`cloud-operation-status ${operation.kind}`} role={operation.kind === 'error' ? 'alert' : 'status'}>{operation.message}</p>}
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
      <div className="cloud-row cloud-columns"><span className="cloud-check-cell"><input ref={selectAllCheckbox} type="checkbox" aria-label="全选当前目录" checked={allSelected} onChange={(event) => setSelectedIds(event.target.checked ? directory?.items.map((item) => item.id) ?? [] : [])} /></span><span>名称</span><span>大小</span><span>修改时间</span></div>
      {directoryState === 'empty' ? <div className="cloud-empty"><CloudFileIcon kind="folder" /><b>此目录为空</b><span>可在这里上传文件或新建目录</span></div> : directory?.items.map((item, index) => {
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
          <span>{item.modifiedAt ?? '—'}</span>
        </div>;
      })}
    </div>}
    {contextMenu && <CloudContextMenu state={contextMenu} selectedCount={selected.length} onClose={() => setContextMenu(null)} onAction={runContextAction} />}
  </section>;
}
