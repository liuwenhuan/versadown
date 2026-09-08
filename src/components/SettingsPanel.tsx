import { FormEvent, useEffect, useState } from 'react';
import { AppSettings, ProxyProtocol } from '../lib/types';

interface SettingsPanelProps {
  initial: AppSettings;
  onChange(settings: AppSettings): void;
  browseDirectory?(currentDirectory: string): Promise<string | null>;
}

export function SettingsPanel({ initial, onChange, browseDirectory }: SettingsPanelProps) {
  const [debugReports, setDebugReports] = useState(initial.debugReports);
  const [downloadDirectory, setDownloadDirectory] = useState(initial.downloadDirectory);
  const [proxy, setProxy] = useState(initial.proxy);
  const [saved, setSaved] = useState(false);

  useEffect(() => { setDebugReports(initial.debugReports); setDownloadDirectory(initial.downloadDirectory); setProxy(initial.proxy); }, [initial]);

  function updateDebugReports(enabled: boolean) {
    setDebugReports(enabled);
    onChange({ debugReports: enabled, downloadDirectory, proxy });
  }

  function updateProxy<Key extends keyof typeof proxy>(key: Key, value: (typeof proxy)[Key]) {
    setSaved(false);
    setProxy((current) => ({ ...current, [key]: value }));
  }

  function saveProxy(event: FormEvent) {
    event.preventDefault();
    onChange({ debugReports, downloadDirectory, proxy: { ...proxy, host: proxy.host.trim(), username: proxy.username.trim() } });
    setSaved(true);
  }

  function saveDownloadDirectory(event: FormEvent) {
    event.preventDefault();
    const normalized = downloadDirectory.trim();
    setDownloadDirectory(normalized);
    onChange({ debugReports, downloadDirectory: normalized, proxy });
  }

  async function browseForDownloadDirectory() {
    const selected = await browseDirectory?.(downloadDirectory);
    if (selected) setDownloadDirectory(selected);
  }

  return <section className="settings-panel">
    <p className="eyebrow">下载与应用设置</p><h2>设置</h2>
    <form className="directory-settings" onSubmit={saveDownloadDirectory}>
      <label><span><b>下载目录</b><small>后续新建任务默认保存到此目录。</small></span><input aria-label="下载目录" required value={downloadDirectory} placeholder="C:\\Users\\用户名\\Downloads" onChange={(event) => setDownloadDirectory(event.target.value)} /></label>
      <button className="secondary" type="button" aria-label="浏览下载目录" onClick={() => void browseForDownloadDirectory()}>浏览</button>
      <button className="primary" type="submit">保存下载目录</button>
    </form>
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
    <label className="setting-row"><span><b>启用调试报告</b><small>记录脱敏的解析与后处理步骤，供高级用户排查。正式版可移除该功能。</small></span><input aria-label="启用调试报告" type="checkbox" checked={debugReports} onChange={(event) => updateDebugReports(event.target.checked)} /></label>
    <section className="setting-safety"><b>隐私规则</b><ul><li>标准链接优先使用本地规则解析。</li><li>AI 仅接收复杂任务的脱敏文本，绝不接收凭据或本地文件。</li><li>代理账号配置仅由本机 aria2 使用。</li></ul></section>
  </section>;
}
