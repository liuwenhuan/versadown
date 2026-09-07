import { useState } from 'react';

interface SettingsPanelProps { initial: { debugReports: boolean }; onChange(enabled: boolean): void; }

export function SettingsPanel({ initial, onChange }: SettingsPanelProps) {
  const [debugReports, setDebugReports] = useState(initial.debugReports);
  function update(enabled: boolean) { setDebugReports(enabled); onChange(enabled); }
  return <section className="settings-panel"><p className="eyebrow">本地安全与调试</p><h2>设置</h2><label className="setting-row"><span><b>启用调试报告</b><small>记录脱敏的解析与后处理步骤，供高级用户排查。正式版可移除该功能。</small></span><input aria-label="启用调试报告" type="checkbox" checked={debugReports} onChange={(event) => update(event.target.checked)} /></label><section className="setting-safety"><b>隐私规则</b><ul><li>标准链接优先使用本地规则解析。</li><li>AI 仅接收复杂任务的脱敏文本，绝不接收凭据或本地文件。</li><li>短信、扫码、验证码和未知表单必须由用户处理。</li></ul></section></section>;
}
