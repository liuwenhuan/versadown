import { FormEvent, useEffect, useState, useSyncExternalStore } from 'react';
import { AiSettings, AiSettingsInput, AppSettings, Aria2Settings, LauncherVisibility, MagnetMetadataSettings, ProxyProtocol } from '../lib/types';
import { getThemeMode, setThemeMode, subscribeTheme, themeOptions, ThemeMode } from '../lib/theme';

type SettingsTab = 'display' | 'network' | 'advanced';

const tabs: ReadonlyArray<{ id: SettingsTab; label: string }> = [
  { id: 'display', label: '显示' },
  { id: 'network', label: '网络' },
  { id: 'advanced', label: '高级' }
];

interface SettingsPanelProps {
  initial: AppSettings;
  onChange(settings: AppSettings): void | Promise<void>;
  browseDirectory?(currentDirectory: string): Promise<string | null>;
  saveAiSettings?(settings: AiSettingsInput): Promise<AiSettings>;
  loadAiModels?(settings: AiSettingsInput): Promise<string[]>;
  configurePrivacyDownload?(enabled: boolean, password?: string): Promise<AppSettings>;
}

const disabledAiSettings: AiSettings = { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false };
const defaultMagnetMetadataSettings: MagnetMetadataSettings = { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' };
const defaultAria2Settings: Aria2Settings = { rpcPort: 6800, rpcToken: '', maxConcurrentDownloads: 5, maxConnectionsPerServer: 4, btSeedRatio: 1.5 };
export function SettingsPanel({ initial, onChange, browseDirectory, saveAiSettings, loadAiModels, configurePrivacyDownload }: SettingsPanelProps) {
  const [debugReports, setDebugReports] = useState(initial.debugReports);
  const [downloadDirectory, setDownloadDirectory] = useState(initial.downloadDirectory);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>(initial.launcherVisibility);
  const [proxy, setProxy] = useState(initial.proxy);
  const [ai, setAi] = useState<AiSettings>(initial.ai ?? disabledAiSettings);
  const [magnetMetadata, setMagnetMetadata] = useState<MagnetMetadataSettings>(initial.magnetMetadata ?? defaultMagnetMetadataSettings);
  const [aria2, setAria2] = useState<Aria2Settings>(initial.aria2 ?? defaultAria2Settings);
  const [deleteTransferredShareFilesAfterDownload, setDeleteTransferredShareFilesAfterDownload] = useState(initial.deleteTransferredShareFilesAfterDownload ?? true);
  const [privacyDownloadEnabled, setPrivacyDownloadEnabled] = useState(initial.privacyDownloadEnabled ?? false);
  const [privacyPasswordConfigured, setPrivacyPasswordConfigured] = useState(initial.privacyPasswordConfigured ?? false);
  const [privacyPassword, setPrivacyPassword] = useState('');
  const [privacyPasswordConfirmation, setPrivacyPasswordConfirmation] = useState('');
  const [privacyStatus, setPrivacyStatus] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [magnetMetadataSaved, setMagnetMetadataSaved] = useState(false);
  const [aria2Status, setAria2Status] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [aiApiKey, setAiApiKey] = useState('');
  const [aiModels, setAiModels] = useState<string[]>([]);
  const [aiStatus, setAiStatus] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [aiBusy, setAiBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [activeTab, setActiveTab] = useState<SettingsTab>('display');
  const themeMode = useSyncExternalStore(subscribeTheme, getThemeMode, getThemeMode);

  useEffect(() => { setDebugReports(initial.debugReports); setDownloadDirectory(initial.downloadDirectory); setLauncherVisibility(initial.launcherVisibility); setProxy(initial.proxy); setAi(initial.ai ?? disabledAiSettings); setMagnetMetadata(initial.magnetMetadata ?? defaultMagnetMetadataSettings); setAria2(initial.aria2 ?? defaultAria2Settings); setDeleteTransferredShareFilesAfterDownload(initial.deleteTransferredShareFilesAfterDownload ?? true); setPrivacyDownloadEnabled(initial.privacyDownloadEnabled ?? false); setPrivacyPasswordConfigured(initial.privacyPasswordConfigured ?? false); }, [initial]);

  function currentSettings(overrides: Partial<AppSettings> = {}): AppSettings {
    return { debugReports, downloadDirectory, launcherVisibility, proxy, ai, magnetMetadata, aria2, deleteTransferredShareFilesAfterDownload, ...overrides };
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

  function updateDebugReports(enabled: boolean) {
    setDebugReports(enabled);
    saveWithoutStatus(currentSettings({ debugReports: enabled }));
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
    const normalized = downloadDirectory.trim();
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

  async function fetchAiModels() {
    setAiStatus(null);
    setAiBusy(true);
    try {
      const models = await loadAiModels?.(currentAiInput()) ?? [];
      setAiModels(models);
      setAiStatus({ kind: 'success', message: models.length ? `已获取 ${models.length} 个模型` : '服务未返回可选模型。' });
    } catch (error) {
      setAiStatus({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    } finally {
      setAiBusy(false);
    }
  }

  async function saveAiConfiguration(event: FormEvent) {
    event.preventDefault();
    setAiStatus(null);
    setAiBusy(true);
    try {
      const savedAi = await saveAiSettings?.(currentAiInput()) ?? { ...ai, apiKeyConfigured: ai.apiKeyConfigured || Boolean(aiApiKey.trim()) };
      setAi(savedAi);
      setAiApiKey('');
      setAiStatus({ kind: 'success', message: 'AI 服务配置已保存。' });
    } catch (error) {
      setAiStatus({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    } finally {
      setAiBusy(false);
    }
  }

  function updateTransferredShareCleanup(enabled: boolean) {
    setDeleteTransferredShareFilesAfterDownload(enabled);
    saveWithoutStatus(currentSettings({ deleteTransferredShareFilesAfterDownload: enabled }));
  }

  async function savePrivacyDownload(event: FormEvent) {
    event.preventDefault();
    setPrivacyStatus(null);
    if (privacyDownloadEnabled && privacyPassword && privacyPassword !== privacyPasswordConfirmation) {
      setPrivacyStatus({ kind: 'error', message: '两次输入的隐私模式密码不一致' });
      return;
    }
    try {
      const saved = await configurePrivacyDownload?.(privacyDownloadEnabled, privacyPassword || undefined)
        ?? currentSettings({ privacyDownloadEnabled, privacyPasswordConfigured: privacyPasswordConfigured || Boolean(privacyPassword) });
      setPrivacyPasswordConfigured(saved.privacyPasswordConfigured ?? Boolean(privacyPassword));
      setPrivacyPassword('');
      setPrivacyPasswordConfirmation('');
      setPrivacyStatus({ kind: 'success', message: privacyDownloadEnabled ? '隐私下载已开启' : '隐私下载已关闭' });
    } catch (error) {
      setPrivacyStatus({ kind: 'error', message: error instanceof Error ? error.message : String(error) });
    }
  }

  function saveMagnetMetadata(event: FormEvent) {
    event.preventDefault();
    const savedMetadata = { ...magnetMetadata, baseUrl: magnetMetadata.baseUrl.trim() };
    setMagnetMetadata(savedMetadata);
    saveWithoutStatus(currentSettings({ magnetMetadata: savedMetadata }));
    setMagnetMetadataSaved(true);
  }

  function updateAria2<Key extends keyof Aria2Settings>(key: Key, value: Aria2Settings[Key]) {
    setAria2Status(null);
    setAria2((current) => ({ ...current, [key]: value }));
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

  async function browseForDownloadDirectory() {
    const selected = await browseDirectory?.(downloadDirectory);
    if (selected) setDownloadDirectory(selected);
  }

  return <section className="settings-panel" aria-label="设置">
    <div className="settings-tabs workspace-tab-list" role="tablist" aria-label="设置分类">
      {tabs.map((tab) => <button key={tab.id} role="tab" aria-selected={activeTab === tab.id} onClick={() => setActiveTab(tab.id)}>{tab.label}</button>)}
    </div>
    <div className="settings-content">
      {activeTab === 'display' && <>
        <section className="theme-setting" aria-labelledby="theme-setting-title">
          <header><span><b id="theme-setting-title">界面主题</b><small>点击色块立即切换并保存；跟随系统会自动使用薄荷青或深海蓝。</small></span></header>
          <div className="theme-picker" role="radiogroup" aria-label="界面主题">
            <button type="button" role="radio" aria-checked={themeMode === 'auto'} aria-label="跟随系统主题" className="theme-option theme-option-auto" onClick={() => updateThemeMode('auto')}>
              <span className="theme-preview" aria-hidden="true"><i /><i /><i /></span>
              <span><b>跟随系统</b><small>自动</small></span>
              <i className="theme-check" aria-hidden="true">✓</i>
            </button>
            {themeOptions.map((theme) => <button key={theme.id} type="button" role="radio" aria-checked={themeMode === theme.id} aria-label={`${theme.label}主题`} className="theme-option" onClick={() => updateThemeMode(theme.id)}>
              <span className="theme-preview" aria-hidden="true">{theme.preview.map((color) => <i key={color} style={{ background: color }} />)}</span>
              <span><b>{theme.label}</b><small>{theme.appearance === 'dark' ? '深色' : '浅色'}</small></span>
              <i className="theme-check" aria-hidden="true">✓</i>
            </button>)}
          </div>
        </section>
        <form className="directory-settings" onSubmit={saveDownloadDirectory}>
          <label><span><b>下载目录</b><small>后续新建任务默认保存到此目录。</small></span><input aria-label="下载目录" required value={downloadDirectory} placeholder="C:\\Users\\用户名\\Downloads" onChange={(event) => setDownloadDirectory(event.target.value)} /></label>
          <button className="secondary" type="button" aria-label="浏览下载目录" onClick={() => void browseForDownloadDirectory()}>浏览</button>
          <button className="primary" type="submit">保存下载目录</button>
        </form>
        <label className="setting-row"><span><b>悬浮球显示</b><small>“关闭显示”后仍可在这里重新开启。</small></span><select aria-label="悬浮球显示" value={launcherVisibility} onChange={(event) => updateLauncherVisibility(event.target.value as LauncherVisibility)}><option value="always">总是显示</option><option value="downloading">仅下载时显示</option><option value="hidden">关闭显示</option></select></label>
        <label className="setting-row"><span><b>下载完成后自动删除转存文件</b><small>仅删除本应用为下载创建的转存文件，不会删除您的其他网盘文件。</small></span><input aria-label="下载完成后自动删除转存文件" type="checkbox" checked={deleteTransferredShareFilesAfterDownload} onChange={(event) => updateTransferredShareCleanup(event.target.checked)} /></label>
        <form className="privacy-download-settings proxy-settings" onSubmit={(event) => void savePrivacyDownload(event)}>
          <label className="setting-row"><span><b>开启隐私下载</b><small>私密任务只会在本次应用会话解锁后显示；下载文件本身不会加密。</small></span><input aria-label="开启隐私下载" type="checkbox" checked={privacyDownloadEnabled} onChange={(event) => { setPrivacyDownloadEnabled(event.target.checked); setPrivacyStatus(null); }} /></label>
          <fieldset disabled={!privacyDownloadEnabled}>
            <label><span>隐私模式密码</span><input aria-label="隐私模式密码" type="password" autoComplete="new-password" placeholder={privacyPasswordConfigured ? '留空则保留现有密码' : '设置隐私模式密码'} value={privacyPassword} onChange={(event) => setPrivacyPassword(event.target.value)} /></label>
            <label><span>确认隐私模式密码</span><input aria-label="确认隐私模式密码" type="password" autoComplete="new-password" value={privacyPasswordConfirmation} onChange={(event) => setPrivacyPasswordConfirmation(event.target.value)} /></label>
          </fieldset>
          <div className="settings-actions"><button className="primary" type="submit">保存隐私下载设置</button>{privacyStatus && <span role="status" className={privacyStatus.kind}>{privacyStatus.message}</span>}</div>
        </form>
      </>}
      {activeTab === 'network' && <>
      <form className="proxy-settings" onSubmit={saveProxy}>
        <div className="setting-row"><span><b>下载代理服务器</b><small>启用后，后续新建任务将携带代理参数启动 aria2。</small></span><input aria-label="启用下载代理" type="checkbox" checked={proxy.enabled} onChange={(event) => updateProxy('enabled', event.target.checked)} /></div>
        <fieldset disabled={!proxy.enabled}>
          <label><span>协议</span><select aria-label="代理协议" value={proxy.protocol} onChange={(event) => updateProxy('protocol', event.target.value as ProxyProtocol)}><option value="http">HTTP</option><option value="https">HTTPS</option></select></label>
          <label className="proxy-host"><span>服务器</span><input aria-label="代理服务器" required value={proxy.host} placeholder="127.0.0.1" onChange={(event) => updateProxy('host', event.target.value)} /></label>
          <label><span>端口</span><input aria-label="代理端口" required type="number" min="1" max="65535" value={proxy.port} onChange={(event) => updateProxy('port', Number(event.target.value))} /></label>
          <label><span>用户名（可选）</span><input aria-label="代理用户名" value={proxy.username} autoComplete="off" onChange={(event) => updateProxy('username', event.target.value)} /></label>
          <label><span>密码（可选）</span><input aria-label="代理密码" type="password" value={proxy.password} autoComplete="new-password" onChange={(event) => updateProxy('password', event.target.value)} /></label>
        </fieldset>
        <div className="settings-actions"><button className="primary" type="submit">保存代理设置</button>{saved && <span role="status">已保存，后续新任务生效</span>}</div>
      </form>
      <form className="proxy-settings aria2-settings" onSubmit={saveAria2}>
        <div className="setting-row"><span><b>aria2 下载引擎</b><small>端口与 Token 保存后会立即重启本机 aria2 并恢复任务；并行和分享规则同时生效。</small></span></div>
        <fieldset>
          <label><span>RPC 端口</span><input aria-label="aria2 RPC 端口" required type="number" min="1024" max="65535" value={aria2.rpcPort} onChange={(event) => updateAria2('rpcPort', Number(event.target.value))} /></label>
          <label className="aria2-token"><span>RPC Token</span><input aria-label="aria2 RPC Token" required type="password" autoComplete="new-password" value={aria2.rpcToken} onChange={(event) => updateAria2('rpcToken', event.target.value)} /></label>
          <label><span>最大并行任务数</span><input aria-label="最大并行任务数" required type="number" min="1" max="64" value={aria2.maxConcurrentDownloads} onChange={(event) => updateAria2('maxConcurrentDownloads', Number(event.target.value))} /></label>
          <label><span>单任务并发连接数</span><input aria-label="单任务并发连接数" required type="number" min="1" max="16" value={aria2.maxConnectionsPerServer} onChange={(event) => updateAria2('maxConnectionsPerServer', Number(event.target.value))} /></label>
          <label><span>BT 分享率</span><input aria-label="BT 分享率" required type="number" min="0.1" max="1000" step="0.1" value={aria2.btSeedRatio} onChange={(event) => updateAria2('btSeedRatio', Number(event.target.value))} /></label>
        </fieldset>
        <div className="settings-actions"><button className="primary" type="submit">保存 aria2 设置</button>{aria2Status && <span role="status" className={aria2Status.kind}>{aria2Status.message}</span>}</div>
      </form>
      </>}
      {activeTab === 'advanced' && <>
        <form className="ai-settings" onSubmit={saveMagnetMetadata}>
          <label className="setting-row"><span><b>磁链元数据服务</b><small>磁链优先经此服务获取种子和文件清单；服务不可用时自动回退到本地 aria2 DHT。</small></span><input aria-label="启用磁链元数据服务" type="checkbox" checked={magnetMetadata.enabled} onChange={(event) => setMagnetMetadata((current) => ({ ...current, enabled: event.target.checked }))} /></label>
          <fieldset disabled={!magnetMetadata.enabled}>
            <label><span>服务地址</span><input aria-label="磁链元数据服务地址" required placeholder="http://peer.aigutta.com:8080" value={magnetMetadata.baseUrl} onChange={(event) => { setMagnetMetadataSaved(false); setMagnetMetadata((current) => ({ ...current, baseUrl: event.target.value })); }} /></label>
          </fieldset>
          <div className="settings-actions"><button className="primary" type="submit">保存磁链服务</button>{magnetMetadataSaved && <span role="status">已保存，后续磁链任务生效</span>}</div>
        </form>
        <form className="ai-settings" onSubmit={(event) => void saveAiConfiguration(event)}>
          <label className="setting-row"><span><b>启用 AI 智能分析</b><small>仅用于规则无法可靠解析的文本、网页和后处理说明。</small></span><input aria-label="启用 AI 智能分析" type="checkbox" checked={ai.enabled} onChange={(event) => setAi((current) => ({ ...current, enabled: event.target.checked }))} /></label>
          <fieldset disabled={aiBusy}>
            <label><span>服务地址</span><input aria-label="AI 服务地址" required placeholder="https://api.openai.com/v1" value={ai.baseUrl} onChange={(event) => setAi((current) => ({ ...current, baseUrl: event.target.value }))} /></label>
            <label><span>API Key</span><input aria-label="AI API Key" type="password" autoComplete="new-password" placeholder={ai.apiKeyConfigured ? '已配置；留空则保持不变' : '输入服务 API Key'} value={aiApiKey} onChange={(event) => setAiApiKey(event.target.value)} /></label>
            <label><span>模型</span><select aria-label="AI 模型" required value={ai.model} onChange={(event) => setAi((current) => ({ ...current, model: event.target.value }))}><option value="">选择模型</option>{ai.model && !aiModels.includes(ai.model) && <option value={ai.model}>{ai.model}</option>}{aiModels.map((model) => <option key={model} value={model}>{model}</option>)}</select></label>
          </fieldset>
          <div className="settings-actions"><button className="secondary" type="button" onClick={() => void fetchAiModels()} disabled={aiBusy}>获取可用模型</button><button className="primary" type="submit" disabled={aiBusy}>{aiBusy ? '正在保存…' : '保存 AI 配置'}</button>{aiStatus && <span role="status" className={aiStatus.kind}>{aiStatus.message}</span>}</div>
        </form>
        <label className="setting-row"><span><b>启用调试报告</b><small>记录脱敏的解析与后处理步骤，供高级用户排查。</small></span><input aria-label="启用调试报告" type="checkbox" checked={debugReports} onChange={(event) => updateDebugReports(event.target.checked)} /></label>
        <section className="setting-safety"><b>隐私规则</b><ul><li>标准链接优先使用本地规则解析。</li><li>AI 仅接收复杂任务的脱敏文本，绝不接收凭据或本地文件。</li><li>代理账号配置仅由本机 aria2 使用。</li></ul></section>
      </>}
    </div>
  </section>;
}
