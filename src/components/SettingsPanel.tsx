import { FormEvent, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { AiSettings, AiSettingsInput, AppSettings, Aria2Settings, LauncherVisibility, MagnetMetadataSettings, ProxyProtocol } from '../lib/types';
import { getThemeMode, setThemeMode, subscribeTheme, themeOptions, ThemeMode } from '../lib/theme';

type SettingsTab = 'display' | 'network' | 'advanced';

const tabs: ReadonlyArray<{ id: SettingsTab; label: string }> = [
  { id: 'display', label: '常用' },
  { id: 'network', label: '网络' },
  { id: 'advanced', label: '高级' }
];

interface SettingsPanelProps {
  initial: AppSettings;
  onChange(settings: AppSettings): void | Promise<void>;
  browseDirectory?(currentDirectory: string): Promise<string | null>;
  saveAiSettings?(settings: AiSettingsInput): Promise<AiSettings>;
  loadAiModels?(settings: AiSettingsInput): Promise<string[]>;
  onNotice?(notice: { kind: 'success' | 'error'; message: string }): void;
  configurePrivacyDownload?(enabled: boolean, currentPassword?: string, newPassword?: string): Promise<AppSettings>;
}

const disabledAiSettings: AiSettings = { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false };
const defaultMagnetMetadataSettings: MagnetMetadataSettings = { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' };
const defaultAria2Settings: Aria2Settings = { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 4, btSeedRatio: 1.5, debugLogging: false, overallDownloadLimit: 0, p2pUploadLimit: 1048576 };
const mebibyte = 1024 * 1024;
type RateLimitInput = 'overallDownloadLimit' | 'p2pUploadLimit' | 'cloudUploadLimit';

function formatRateLimit(bytes: number) {
  return (bytes / mebibyte).toFixed(1);
}

function toRateLimitBytes(value: string) {
  if (!value || !/^\d*(?:\.\d?)?$/.test(value)) return null;
  const amount = Number(value);
  return Number.isFinite(amount) && amount >= 0 ? Math.round(amount * mebibyte) : null;
}

function createRateLimitInputs(aria2: Aria2Settings, cloudUploadLimit: number) {
  return {
    overallDownloadLimit: formatRateLimit(aria2.overallDownloadLimit),
    p2pUploadLimit: formatRateLimit(aria2.p2pUploadLimit),
    cloudUploadLimit: formatRateLimit(cloudUploadLimit)
  };
}

export function SettingsPanel({ initial, onChange, browseDirectory, saveAiSettings, loadAiModels, onNotice, configurePrivacyDownload }: SettingsPanelProps) {
  const [downloadDirectory, setDownloadDirectory] = useState(initial.downloadDirectory);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>(initial.launcherVisibility);
  const [proxy, setProxy] = useState(initial.proxy);
  const [ai, setAi] = useState<AiSettings>(initial.ai ?? disabledAiSettings);
  const magnetMetadata: MagnetMetadataSettings = initial.magnetMetadata ?? defaultMagnetMetadataSettings;
  const [aria2, setAria2] = useState<Aria2Settings>({ ...defaultAria2Settings, ...initial.aria2 });
  const [cloudUploadLimit, setCloudUploadLimit] = useState(initial.cloudUploadLimit ?? 0);
  const [rateLimitInputs, setRateLimitInputs] = useState(() => createRateLimitInputs({ ...defaultAria2Settings, ...initial.aria2 }, initial.cloudUploadLimit ?? 0));
  const [deleteTransferredShareFilesAfterDownload, setDeleteTransferredShareFilesAfterDownload] = useState(initial.deleteTransferredShareFilesAfterDownload ?? true);
  const [privacyDownloadEnabled, setPrivacyDownloadEnabled] = useState(initial.privacyDownloadEnabled ?? false);
  const [privacyPasswordConfigured, setPrivacyPasswordConfigured] = useState(initial.privacyPasswordConfigured ?? false);
  const [privacyCurrentPassword, setPrivacyCurrentPassword] = useState('');
  const [privacyNewPassword, setPrivacyNewPassword] = useState('');
  const [privacyPasswordConfirmation, setPrivacyPasswordConfirmation] = useState('');
  const [privacyStatus, setPrivacyStatus] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [aria2Status, setAria2Status] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [rateLimitStatus, setRateLimitStatus] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [aiApiKey, setAiApiKey] = useState('');
  const [aiModels, setAiModels] = useState<string[]>([]);
  const [aiStatus, setAiStatus] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [aiBusy, setAiBusy] = useState(false);
  const aiModelRequestInFlight = useRef(false);
  const [saved, setSaved] = useState(false);
  const [activeTab, setActiveTab] = useState<SettingsTab>('display');
  const themeMode = useSyncExternalStore(subscribeTheme, getThemeMode, getThemeMode);

  useEffect(() => { const nextAria2 = { ...defaultAria2Settings, ...initial.aria2 }; const nextCloudUploadLimit = initial.cloudUploadLimit ?? 0; setDownloadDirectory(initial.downloadDirectory); setLauncherVisibility(initial.launcherVisibility); setProxy(initial.proxy); setAi(initial.ai ?? disabledAiSettings); setAria2(nextAria2); setCloudUploadLimit(nextCloudUploadLimit); setRateLimitInputs(createRateLimitInputs(nextAria2, nextCloudUploadLimit)); setDeleteTransferredShareFilesAfterDownload(initial.deleteTransferredShareFilesAfterDownload ?? true); setPrivacyDownloadEnabled(initial.privacyDownloadEnabled ?? false); setPrivacyPasswordConfigured(initial.privacyPasswordConfigured ?? false); }, [initial]);

  function currentSettings(overrides: Partial<AppSettings> = {}): AppSettings {
    return { downloadDirectory, launcherVisibility, proxy, ai, magnetMetadata, aria2, ...(cloudUploadLimit || initial.cloudUploadLimit !== undefined ? { cloudUploadLimit } : {}), deleteTransferredShareFilesAfterDownload, ...overrides };
  }

  function saveWithoutStatus(settings: AppSettings) {
    void Promise.resolve(onChange(settings)).catch(() => undefined);
  }

  const currentAiInput = (): AiSettingsInput => ({
    enabled: ai.enabled,
    baseUrl: ai.baseUrl.trim(),
    model: ai.model.trim(),
    apiKey: aiApiKey.trim()
  });

  function notifyAi(notice: { kind: 'success' | 'error'; message: string }) {
    setAiStatus(notice);
    onNotice?.(notice);
  }

  function updateProxy<Key extends keyof typeof proxy>(key: Key, value: (typeof proxy)[Key]) {
    setSaved(false);
    setProxy((current) => ({ ...current, [key]: value }));
  }

  function saveProxy(event: FormEvent) {
    event.preventDefault();
    saveWithoutStatus(currentSettings({ proxy: { ...proxy, host: proxy.host.trim(), username: proxy.username.trim() } }));
    setSaved(true);
  }

  function saveDownloadDirectory(event: FormEvent) {
    event.preventDefault();
    commitDownloadDirectory(downloadDirectory);
  }

  function commitDownloadDirectory(directory: string) {
    const normalized = directory.trim();
    if (!normalized) return;
    setDownloadDirectory(normalized);
    saveWithoutStatus(currentSettings({ downloadDirectory: normalized }));
  }

  function updateLauncherVisibility(visibility: LauncherVisibility) {
    setLauncherVisibility(visibility);
    saveWithoutStatus(currentSettings({ launcherVisibility: visibility }));
  }

  function updateThemeMode(mode: ThemeMode) {
    setThemeMode(mode);
  }

  async function reloadAiModels() {
    if (aiModelRequestInFlight.current) return;
    const configuration = currentAiInput();
    setAiStatus(null);
    if (!configuration.baseUrl) {
      notifyAi({ kind: 'error', message: '无法获取模型列表：请先填写服务地址。' });
      return;
    }
    if (!configuration.apiKey && !ai.apiKeyConfigured) {
      notifyAi({ kind: 'error', message: '无法获取模型列表：请先填写 API Key。' });
      return;
    }
    aiModelRequestInFlight.current = true;
    setAiBusy(true);
    try {
      const models = await loadAiModels?.(configuration) ?? [];
      if (!models.length) throw new Error('服务未返回可选模型');
      setAiModels(models);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      notifyAi({ kind: 'error', message: `无法获取模型列表，可能是服务地址或 API Key 不正确：${detail}` });
    } finally {
      aiModelRequestInFlight.current = false;
      setAiBusy(false);
    }
  }

  async function selectAiModel(model: string) {
    if (!model) {
      setAi((current) => ({ ...current, model }));
      return;
    }
    const previous = ai;
    const configuration = { ...currentAiInput(), model };
    setAi((current) => ({ ...current, model }));
    setAiStatus(null);
    setAiBusy(true);
    try {
      const savedAi = await saveAiSettings?.(configuration) ?? { ...ai, model, apiKeyConfigured: ai.apiKeyConfigured || Boolean(aiApiKey.trim()) };
      setAi(savedAi);
      setAiApiKey('');
      notifyAi({ kind: 'success', message: 'AI 服务配置已保存。' });
    } catch (error) {
      setAi(previous);
      const detail = error instanceof Error ? error.message : String(error);
      notifyAi({ kind: 'error', message: `保存 AI 配置失败：${detail}` });
    } finally {
      setAiBusy(false);
    }
  }

  function updateTransferredShareCleanup(enabled: boolean) {
    setDeleteTransferredShareFilesAfterDownload(enabled);
    saveWithoutStatus(currentSettings({ deleteTransferredShareFilesAfterDownload: enabled }));
  }

  async function updatePrivacyDownload(enabled: boolean) {
    const previous = privacyDownloadEnabled;
    setPrivacyDownloadEnabled(enabled);
    setPrivacyStatus(null);
    if (!privacyPasswordConfigured) return;
    try {
      const saved = await configurePrivacyDownload?.(enabled)
        ?? currentSettings({ privacyDownloadEnabled: enabled, privacyPasswordConfigured: true });
      setPrivacyDownloadEnabled(saved.privacyDownloadEnabled ?? enabled);
      setPrivacyPasswordConfigured(saved.privacyPasswordConfigured ?? true);
      setPrivacyStatus({ kind: 'success', message: enabled ? '隐私下载已开启' : '隐私下载已关闭' });
    } catch (error) {
      setPrivacyDownloadEnabled(previous);
      setPrivacyStatus({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function savePrivacyDownload(event: FormEvent) {
    event.preventDefault();
    setPrivacyStatus(null);
    if (!privacyPasswordConfigured && privacyDownloadEnabled && !privacyNewPassword) {
      setPrivacyStatus({ kind: 'error', message: '请设置隐私模式密码' });
      return;
    }
    if (!privacyPasswordConfigured && privacyNewPassword !== privacyPasswordConfirmation) {
      setPrivacyStatus({ kind: 'error', message: '两次输入的隐私模式密码不一致' });
      return;
    }
    if (privacyPasswordConfigured && !privacyCurrentPassword) {
      setPrivacyStatus({ kind: 'error', message: '请输入旧密码' });
      return;
    }
    try {
      const saved = await configurePrivacyDownload?.(
        privacyDownloadEnabled,
        privacyPasswordConfigured ? privacyCurrentPassword : undefined,
        privacyNewPassword || undefined
      ) ?? currentSettings({ privacyDownloadEnabled, privacyPasswordConfigured: privacyPasswordConfigured || Boolean(privacyNewPassword) });
      setPrivacyPasswordConfigured(saved.privacyPasswordConfigured ?? Boolean(privacyNewPassword));
      setPrivacyCurrentPassword('');
      setPrivacyNewPassword('');
      setPrivacyPasswordConfirmation('');
      setPrivacyStatus({ kind: 'success', message: privacyDownloadEnabled ? '隐私下载已开启' : '隐私下载已关闭' });
    } catch (error) {
      setPrivacyStatus({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  function updateAria2<Key extends keyof Aria2Settings>(key: Key, value: Aria2Settings[Key]) {
    setAria2Status(null);
    setAria2((current) => ({ ...current, [key]: value }));
  }

  async function updateDebugLogging(enabled: boolean) {
    const previous = aria2;
    const next = { ...aria2, debugLogging: enabled };
    setAria2(next);
    setAria2Status(null);
    try {
      await onChange(currentSettings({ aria2: next }));
    } catch (error) {
      setAria2(previous);
      setAria2Status({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function saveAria2(event: FormEvent) {
    event.preventDefault();
    const savedAria2 = { ...aria2, rpcToken: aria2.rpcToken.trim() };
    setAria2(savedAria2);
    setAria2Status(null);
    try {
      await onChange(currentSettings({ aria2: savedAria2 }));
      setAria2Status({ kind: 'success', message: '已保存并立即应用' });
    } catch (error) {
      setAria2Status({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  function updateRateLimitInput(key: RateLimitInput, value: string) {
    if (!/^\d*(?:\.\d?)?$/.test(value)) return;
    setRateLimitStatus(null);
    setRateLimitInputs((current) => ({ ...current, [key]: value }));
    const bytes = toRateLimitBytes(value);
    if (bytes === null) return;
    if (key === 'cloudUploadLimit') {
      setCloudUploadLimit(bytes);
      return;
    }
    setAria2((current) => ({ ...current, [key]: bytes }));
  }

  function normalizeRateLimitInput(key: RateLimitInput) {
    const bytes = toRateLimitBytes(rateLimitInputs[key]) ?? (key === 'cloudUploadLimit' ? cloudUploadLimit : aria2[key]);
    setRateLimitInputs((current) => ({ ...current, [key]: formatRateLimit(bytes) }));
  }

  async function saveRateLimits(event: FormEvent) {
    event.preventDefault();
    const savedAria2 = {
      ...aria2,
      rpcToken: aria2.rpcToken.trim(),
      overallDownloadLimit: toRateLimitBytes(rateLimitInputs.overallDownloadLimit) ?? aria2.overallDownloadLimit,
      p2pUploadLimit: toRateLimitBytes(rateLimitInputs.p2pUploadLimit) ?? aria2.p2pUploadLimit
    };
    const savedCloudUploadLimit = toRateLimitBytes(rateLimitInputs.cloudUploadLimit) ?? cloudUploadLimit;
    setAria2(savedAria2);
    setCloudUploadLimit(savedCloudUploadLimit);
    setRateLimitInputs(createRateLimitInputs(savedAria2, savedCloudUploadLimit));
    setRateLimitStatus(null);
    try {
      await onChange(currentSettings({ aria2: savedAria2, cloudUploadLimit: savedCloudUploadLimit }));
      setRateLimitStatus({ kind: 'success', message: '已保存并立即应用' });
    } catch (error) {
      setRateLimitStatus({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  async function browseForDownloadDirectory() {
    const selected = await browseDirectory?.(downloadDirectory);
    if (selected) commitDownloadDirectory(selected);
  }

  return <section className="settings-panel" aria-label="设置">
    <div className="settings-tabs workspace-tab-list" role="tablist" aria-label="设置分类">
      {tabs.map((tab) => <button key={tab.id} role="tab" aria-selected={activeTab === tab.id} onClick={() => setActiveTab(tab.id)}>{tab.label}</button>)}
    </div>
    <div className="settings-content">
      {activeTab === 'display' && <>
        <section className="theme-setting" aria-labelledby="theme-setting-title">
          <header><span><b id="theme-setting-title">界面主题</b></span></header>
          <div className="theme-picker" role="radiogroup" aria-label="界面主题">
            <button type="button" role="radio" aria-checked={themeMode === 'auto'} aria-label="跟随系统主题" className="theme-option theme-option-auto" onClick={() => updateThemeMode('auto')}>
              <span className="theme-preview" aria-hidden="true"><i /><i /><i /></span>
              <span><b>跟随系统</b></span>
              <i className="theme-check" aria-hidden="true">✓</i>
            </button>
            {themeOptions.map((theme) => <button key={theme.id} type="button" role="radio" aria-checked={themeMode === theme.id} aria-label={`${theme.label}主题`} className="theme-option" onClick={() => updateThemeMode(theme.id)}>
              <span className="theme-preview" aria-hidden="true">{theme.preview.map((color) => <i key={color} style={{ background: color }} />)}</span>
              <span><b>{theme.label}</b></span>
              <i className="theme-check" aria-hidden="true">✓</i>
            </button>)}
          </div>
        </section>
        <form className="directory-settings" onSubmit={saveDownloadDirectory}>
          <label><span><b>下载目录</b></span><input aria-label="下载目录" required value={downloadDirectory} placeholder="C:\\Users\\用户名\\Downloads" onChange={(event) => setDownloadDirectory(event.target.value)} onBlur={(event) => commitDownloadDirectory(event.target.value)} /></label>
          <button className="secondary" type="button" aria-label="浏览下载目录" onClick={() => void browseForDownloadDirectory()}>浏览</button>
        </form>
        <label className="setting-row"><span><b>悬浮球显示</b></span><select aria-label="悬浮球显示" value={launcherVisibility} onChange={(event) => updateLauncherVisibility(event.target.value as LauncherVisibility)}><option value="always">总是显示</option><option value="downloading">仅下载时显示</option><option value="hidden">关闭显示</option></select></label>
        <label className="setting-row"><span><b>下载完成后自动删除转存文件</b></span><input aria-label="下载完成后自动删除转存文件" type="checkbox" checked={deleteTransferredShareFilesAfterDownload} onChange={(event) => updateTransferredShareCleanup(event.target.checked)} /></label>
      </>}
      {activeTab === 'network' && <>
      <form className="proxy-settings" onSubmit={saveProxy}>
        <div className="setting-row"><span><b>启用下载代理服务器</b></span><input aria-label="启用下载代理" type="checkbox" checked={proxy.enabled} onChange={(event) => updateProxy('enabled', event.target.checked)} /></div>
        <fieldset aria-disabled={!proxy.enabled}>
          <label><span>协议</span><select aria-label="代理协议" disabled={!proxy.enabled} value={proxy.protocol} onChange={(event) => updateProxy('protocol', event.target.value as ProxyProtocol)}><option value="http">HTTP</option><option value="https">HTTPS</option></select></label>
          <label className="proxy-host"><span>服务器</span><input aria-label="代理服务器" disabled={!proxy.enabled} required value={proxy.host} placeholder="127.0.0.1" onChange={(event) => updateProxy('host', event.target.value)} /></label>
          <label><span>端口</span><input aria-label="代理端口" disabled={!proxy.enabled} required type="number" min="1" max="65535" value={proxy.port} onChange={(event) => updateProxy('port', Number(event.target.value))} /></label>
          <label><span>用户名（可选）</span><input aria-label="代理用户名" disabled={!proxy.enabled} value={proxy.username} autoComplete="off" onChange={(event) => updateProxy('username', event.target.value)} /></label>
          <label><span>密码（可选）</span><input aria-label="代理密码" disabled={!proxy.enabled} type="password" value={proxy.password} autoComplete="new-password" onChange={(event) => updateProxy('password', event.target.value)} /></label>
          <button className="primary settings-inline-save" type="submit">保存代理设置</button>
        </fieldset>
        {saved && <div className="settings-actions"><span role="status">已保存，后续新任务生效</span></div>}
      </form>
      <form className="proxy-settings rate-limit-settings" onSubmit={saveRateLimits}>
        <div className="setting-row"><span><b>限速设置</b></span></div>
        <fieldset>
          <label><span>整体下载限速（MiB/s）</span><input aria-label="整体下载限速" type="number" min="0" step="0.1" required value={rateLimitInputs.overallDownloadLimit} onChange={(event) => updateRateLimitInput('overallDownloadLimit', event.target.value)} onBlur={() => normalizeRateLimitInput('overallDownloadLimit')} /></label>
          <label><span>P2P 上传限速（MiB/s）</span><input aria-label="P2P 上传限速" type="number" min="0" step="0.1" required value={rateLimitInputs.p2pUploadLimit} onChange={(event) => updateRateLimitInput('p2pUploadLimit', event.target.value)} onBlur={() => normalizeRateLimitInput('p2pUploadLimit')} /></label>
          <label><span>网盘上传限速（MiB/s）</span><input aria-label="网盘上传限速" type="number" min="0" step="0.1" required value={rateLimitInputs.cloudUploadLimit} onChange={(event) => updateRateLimitInput('cloudUploadLimit', event.target.value)} onBlur={() => normalizeRateLimitInput('cloudUploadLimit')} /></label>
          <button className="primary settings-inline-save" type="submit">保存限速设置</button>
        </fieldset>
        {rateLimitStatus && <div className="settings-actions"><span role="status" className={rateLimitStatus.kind}>{rateLimitStatus.message}</span></div>}
      </form>
      <form className="proxy-settings aria2-settings" onSubmit={saveAria2}>
        <div className="setting-row"><span><b>aria2 下载引擎</b></span></div>
        <fieldset>
          <label><span>RPC 端口</span><input aria-label="aria2 RPC 端口" required type="number" min="1024" max="65535" value={aria2.rpcPort} onChange={(event) => updateAria2('rpcPort', Number(event.target.value))} /></label>
          <label className="aria2-token"><span>RPC Token</span><input aria-label="aria2 RPC Token" required type="password" autoComplete="new-password" value={aria2.rpcToken} onChange={(event) => updateAria2('rpcToken', event.target.value)} /></label>
          <label><span>最大并行任务数</span><input aria-label="最大并行任务数" required type="number" min="1" max="64" value={aria2.maxConcurrentDownloads} onChange={(event) => updateAria2('maxConcurrentDownloads', Number(event.target.value))} /></label>
          <label><span>单任务并发连接数</span><input aria-label="单任务并发连接数" required type="number" min="1" max="16" value={aria2.maxConnectionsPerServer} onChange={(event) => updateAria2('maxConnectionsPerServer', Number(event.target.value))} /></label>
          <label className="aria2-seed-ratio"><span>BT 分享率</span><input aria-label="BT 分享率" required type="number" min="0.1" max="1000" step="0.1" value={aria2.btSeedRatio} onChange={(event) => updateAria2('btSeedRatio', Number(event.target.value))} /></label>
          <button className="primary settings-inline-save" type="submit">保存 aria2 设置</button>
        </fieldset>
        {aria2Status && <div className="settings-actions"><span role="status" className={aria2Status.kind}>{aria2Status.message}</span></div>}
      </form>
      </>}
      {activeTab === 'advanced' && <>
        <section className="aria2-debug-settings proxy-settings">
          <label className="setting-row"><span><b>打开日志调试</b><small>保存 aria2 引擎和本地 RPC 调试日志，用于排查下载问题；日志可能包含下载地址，仅在需要排障时开启。</small></span><input aria-label="打开日志调试" type="checkbox" checked={aria2.debugLogging} onChange={(event) => void updateDebugLogging(event.target.checked)} /></label>
          {aria2Status?.kind === 'error' && <div className="settings-actions"><span role="status" className="error">{aria2Status.message}</span></div>}
        </section>
        <form className="privacy-download-settings proxy-settings" onSubmit={(event) => void savePrivacyDownload(event)}>
          <label className="setting-row"><span><b>开启隐私下载</b><small>私密任务只会在本次应用会话解锁后显示；下载文件本身不会加密。</small></span><input aria-label="开启隐私下载" type="checkbox" checked={privacyDownloadEnabled} onChange={(event) => void updatePrivacyDownload(event.target.checked)} /></label>
          <fieldset className="privacy-password-fields" disabled={!privacyDownloadEnabled && !privacyPasswordConfigured}>
            {privacyPasswordConfigured && <label><span>输入旧密码</span><input aria-label="输入旧密码" type="password" autoComplete="current-password" value={privacyCurrentPassword} onChange={(event) => setPrivacyCurrentPassword(event.target.value)} /></label>}
            <label><span>设置新密码</span><input aria-label="设置新密码" type="password" autoComplete="new-password" placeholder={privacyPasswordConfigured ? '留空则保留现有密码' : '设置隐私模式密码'} value={privacyNewPassword} onChange={(event) => setPrivacyNewPassword(event.target.value)} /></label>
            {!privacyPasswordConfigured && <label><span>确认新密码</span><input aria-label="确认新密码" type="password" autoComplete="new-password" value={privacyPasswordConfirmation} onChange={(event) => setPrivacyPasswordConfirmation(event.target.value)} /></label>}
            <button className="primary privacy-save-password" type="submit">保存密码</button>
          </fieldset>
          {privacyStatus && <div className="settings-actions"><span role="status" className={privacyStatus.kind}>{privacyStatus.message}</span></div>}
        </form>
        <section className="ai-settings">
          <label className="setting-row"><span><b>启用 AI 智能分析</b></span><input aria-label="启用 AI 智能分析" type="checkbox" checked={ai.enabled} onChange={(event) => setAi((current) => ({ ...current, enabled: event.target.checked }))} /></label>
          <fieldset aria-busy={aiBusy}>
            <label><span>服务地址</span><input aria-label="AI 服务地址" required placeholder="https://api.openai.com/v1" value={ai.baseUrl} onChange={(event) => setAi((current) => ({ ...current, baseUrl: event.target.value }))} /></label>
            <label><span>API Key</span><input aria-label="AI API Key" type="password" autoComplete="new-password" placeholder={ai.apiKeyConfigured ? '已配置；留空则保持不变' : '输入服务 API Key'} value={aiApiKey} onChange={(event) => setAiApiKey(event.target.value)} /></label>
            <label><span>模型</span><select aria-label="AI 模型" required value={ai.model} onPointerDown={() => void reloadAiModels()} onFocus={() => void reloadAiModels()} onKeyDown={(event) => { if (event.key === 'Enter' || event.key === ' ' || event.key === 'ArrowDown') void reloadAiModels(); }} onChange={(event) => void selectAiModel(event.target.value)}><option value="">选择模型</option>{ai.model && !aiModels.includes(ai.model) && <option value={ai.model}>{ai.model}</option>}{aiModels.map((model) => <option key={model} value={model}>{model}</option>)}</select></label>
          </fieldset>
          {!onNotice && aiStatus && <div className="settings-actions"><span role="status" className={aiStatus.kind}>{aiStatus.message}</span></div>}
        </section>
      </>}
    </div>
  </section>;
}
