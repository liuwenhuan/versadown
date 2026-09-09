import { FormEvent, useEffect, useState, useSyncExternalStore } from 'react';
import { AiSettings, AiSettingsInput, AppSettings, LauncherVisibility, MagnetMetadataSettings, ProxyProtocol } from '../lib/types';
import { getThemeMode, setThemeMode, subscribeTheme, ThemeMode } from '../lib/theme';

type SettingsTab = 'display' | 'network' | 'advanced';

const tabs: ReadonlyArray<{ id: SettingsTab; label: string }> = [
  { id: 'display', label: '显示' },
  { id: 'network', label: '网络' },
  { id: 'advanced', label: '高级' }
];

interface SettingsPanelProps {
  initial: AppSettings;
  onChange(settings: AppSettings): void;
  browseDirectory?(currentDirectory: string): Promise<string | null>;
  saveAiSettings?(settings: AiSettingsInput): Promise<AiSettings>;
  loadAiModels?(settings: AiSettingsInput): Promise<string[]>;
}

const disabledAiSettings: AiSettings = { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false };
const defaultMagnetMetadataSettings: MagnetMetadataSettings = { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' };
const themeLabels: Record<ThemeMode, string> = { auto: '跟随系统', light: '浅色', dark: '深色' };

export function SettingsPanel({ initial, onChange, browseDirectory, saveAiSettings, loadAiModels }: SettingsPanelProps) {
  const [debugReports, setDebugReports] = useState(initial.debugReports);
  const [downloadDirectory, setDownloadDirectory] = useState(initial.downloadDirectory);
  const [launcherVisibility, setLauncherVisibility] = useState<LauncherVisibility>(initial.launcherVisibility);
  const [proxy, setProxy] = useState(initial.proxy);
  const [ai, setAi] = useState<AiSettings>(initial.ai ?? disabledAiSettings);
  const [magnetMetadata, setMagnetMetadata] = useState<MagnetMetadataSettings>(initial.magnetMetadata ?? defaultMagnetMetadataSettings);
  const [magnetMetadataSaved, setMagnetMetadataSaved] = useState(false);
  const [aiApiKey, setAiApiKey] = useState('');
  const [aiModels, setAiModels] = useState<string[]>([]);
  const [aiStatus, setAiStatus] = useState<{ kind: 'success' | 'error'; message: string } | null>(null);
  const [aiBusy, setAiBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [activeTab, setActiveTab] = useState<SettingsTab>('display');
  const themeMode = useSyncExternalStore(subscribeTheme, getThemeMode, getThemeMode);

  useEffect(() => { setDebugReports(initial.debugReports); setDownloadDirectory(initial.downloadDirectory); setLauncherVisibility(initial.launcherVisibility); setProxy(initial.proxy); setAi(initial.ai ?? disabledAiSettings); setMagnetMetadata(initial.magnetMetadata ?? defaultMagnetMetadataSettings); }, [initial]);

  const currentAiInput = (): AiSettingsInput => ({
    enabled: ai.enabled,
    baseUrl: ai.baseUrl.trim(),
    model: ai.model.trim(),
    apiKey: aiApiKey.trim()
  });

  function updateDebugReports(enabled: boolean) {
    setDebugReports(enabled);
    onChange({ debugReports: enabled, downloadDirectory, launcherVisibility, proxy, ai, magnetMetadata });
  }

  function updateProxy<Key extends keyof typeof proxy>(key: Key, value: (typeof proxy)[Key]) {
    setSaved(false);
    setProxy((current) => ({ ...current, [key]: value }));
  }

  function saveProxy(event: FormEvent) {
    event.preventDefault();
    onChange({ debugReports, downloadDirectory, launcherVisibility, proxy: { ...proxy, host: proxy.host.trim(), username: proxy.username.trim() }, ai, magnetMetadata });
    setSaved(true);
  }

  function saveDownloadDirectory(event: FormEvent) {
    event.preventDefault();
    const normalized = downloadDirectory.trim();
    setDownloadDirectory(normalized);
    onChange({ debugReports, downloadDirectory: normalized, launcherVisibility, proxy, ai, magnetMetadata });
  }

  function updateLauncherVisibility(visibility: LauncherVisibility) {
    setLauncherVisibility(visibility);
    onChange({ debugReports, downloadDirectory, launcherVisibility: visibility, proxy, ai, magnetMetadata });
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

  function saveMagnetMetadata(event: FormEvent) {
    event.preventDefault();
    const savedMetadata = { ...magnetMetadata, baseUrl: magnetMetadata.baseUrl.trim() };
    setMagnetMetadata(savedMetadata);
    onChange({ debugReports, downloadDirectory, launcherVisibility, proxy, ai, magnetMetadata: savedMetadata });
    setMagnetMetadataSaved(true);
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
        <label className="setting-row"><span><b>界面主题</b><small>立即切换界面外观；“跟随系统”会自动匹配 Windows 主题。</small></span><select aria-label="界面主题" value={themeMode} onChange={(event) => updateThemeMode(event.target.value as ThemeMode)}>{(['auto', 'light', 'dark'] as const).map((mode) => <option key={mode} value={mode}>{themeLabels[mode]}</option>)}</select></label>
        <form className="directory-settings" onSubmit={saveDownloadDirectory}>
          <label><span><b>下载目录</b><small>后续新建任务默认保存到此目录。</small></span><input aria-label="下载目录" required value={downloadDirectory} placeholder="C:\\Users\\用户名\\Downloads" onChange={(event) => setDownloadDirectory(event.target.value)} /></label>
          <button className="secondary" type="button" aria-label="浏览下载目录" onClick={() => void browseForDownloadDirectory()}>浏览</button>
          <button className="primary" type="submit">保存下载目录</button>
        </form>
        <label className="setting-row"><span><b>悬浮球显示</b><small>“关闭显示”后仍可在这里重新开启。</small></span><select aria-label="悬浮球显示" value={launcherVisibility} onChange={(event) => updateLauncherVisibility(event.target.value as LauncherVisibility)}><option value="always">总是显示</option><option value="downloading">仅下载时显示</option><option value="hidden">关闭显示</option></select></label>
      </>}
      {activeTab === 'network' && <form className="proxy-settings" onSubmit={saveProxy}>
        <div className="setting-row"><span><b>下载代理服务器</b><small>启用后，后续新建任务将携带代理参数启动 aria2。</small></span><input aria-label="启用下载代理" type="checkbox" checked={proxy.enabled} onChange={(event) => updateProxy('enabled', event.target.checked)} /></div>
        <fieldset disabled={!proxy.enabled}>
          <label><span>协议</span><select aria-label="代理协议" value={proxy.protocol} onChange={(event) => updateProxy('protocol', event.target.value as ProxyProtocol)}><option value="http">HTTP</option><option value="https">HTTPS</option></select></label>
          <label className="proxy-host"><span>服务器</span><input aria-label="代理服务器" required value={proxy.host} placeholder="127.0.0.1" onChange={(event) => updateProxy('host', event.target.value)} /></label>
          <label><span>端口</span><input aria-label="代理端口" required type="number" min="1" max="65535" value={proxy.port} onChange={(event) => updateProxy('port', Number(event.target.value))} /></label>
          <label><span>用户名（可选）</span><input aria-label="代理用户名" value={proxy.username} autoComplete="off" onChange={(event) => updateProxy('username', event.target.value)} /></label>
          <label><span>密码（可选）</span><input aria-label="代理密码" type="password" value={proxy.password} autoComplete="new-password" onChange={(event) => updateProxy('password', event.target.value)} /></label>
        </fieldset>
        <div className="settings-actions"><button className="primary" type="submit">保存代理设置</button>{saved && <span role="status">已保存，后续新任务生效</span>}</div>
      </form>}
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
