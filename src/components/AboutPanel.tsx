import { version } from '../../src-tauri/tauri.conf.json';
import type { AppUpdate } from '../lib/types';

export function AboutPanel({ icon, update, onOpenUpdate }: { icon: string; update?: AppUpdate | null; onOpenUpdate?(update: AppUpdate): void }) {
  return <article className="about-panel">
    <header className="about-heading">
      <img src={icon} width="72" height="72" alt="" draggable={false} />
      <div>
        <div className="about-title-row"><h2>万能下载 VersaDown</h2>{update && <button type="button" className="about-update" aria-label={`下载新版本 ${update.version}`} onClick={() => onOpenUpdate?.(update)}><span>新版本发布：{update.version}</span><small>{update.notes}</small></button>}</div>
        <span className="about-version">版本 {version}</span>
      </div>
    </header>
    <p className="about-intro">一款支持多种平台（Windows、macOS、Linux）的全能下载工具，支持全部常用下载协议，可以对多种网盘文件进行集中管理，让下载任务更轻松、更方便。</p>
    <section className="about-section" aria-labelledby="about-features-title">
      <h3 id="about-features-title">主要功能</h3>
      <dl className="about-features">
        <div><dt>多类型下载</dt><dd>统一处理常见下载链接、磁力链接、种子及网盘分享链接。</dd></div>
        <div><dt>隐私下载</dt><dd>通过密码保护私密任务，解锁后才显示任务内容；下载文件本身不会加密。</dd></div>
        <div><dt>网盘管理</dt><dd>连接网盘账号，浏览文件并执行下载、上传等操作；具体功能以各网盘支持情况为准。</dd></div>
        <div><dt>智能下载</dt><dd>智能解析文本中的各种下载链接，并智能进行下载后处理。</dd></div>
      </dl>
    </section>
    <section className="about-section" aria-labelledby="about-help-title">
      <h3 id="about-help-title">使用说明</h3>
      <ol className="about-instructions">
        <li>在顶部输入框粘贴下载链接，按提示解析并创建下载任务；也可以输入关键词搜索本地任务。</li>
        <li>在“我的网盘”中添加并授权自己的账号，再浏览和管理网盘文件。</li>
        <li>在“设置”中选择下载保存位置，并按需要调整网络及下载选项。</li>
      </ol>
    </section>
    <aside className="about-notice" aria-label="使用提醒">请仅下载和管理您有权访问的内容。网盘功能需要本人授权，并受服务商接口、账号权限和使用条款限制。本软件不会破解网盘的会员机制和分享密码，也只收集用于改进软件，以及提供更好服务所需要的最小化运行信息。</aside>
  </article>;
}
