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
    <section className="about-section" aria-labelledby="about-declaration-title">
      <h3 id="about-declaration-title">声明</h3>
      <ol className="about-instructions">
        <li>本项目仅供个人使用，未经许可请勿用于商业用途或大规模滥用。</li>
        <li>所用网盘接口均来自各网盘官方开放平台、官方公开接口及已知开源项目，仅解析用户主动提供的分享链接，不涉及破解或绕过版权保护。</li>
        <li>本项目用于自动化程序代替用户发起请求，提供基本使用功能，不会提升或绕过任何会员权限，不提供各网盘提供的增强服务，也不会突破网盘的限速策略；</li>
        <li>使用者应遵守各网盘服务商的用户协议与使用条款，以及所在地区的法律法规，违反所产生的后果自行承担。</li>
        <li>高频调用可能触发网盘风控，导致账号限流、封禁或来源 IP 被屏蔽，请合理控制请求频率，避免高并发与批量请求。</li>
        <li>网盘接口调整、风控策略变更等因素可能导致功能失效，本项目不对可用性与稳定性作任何担保。</li>
        <li>开发者不对使用本项目产生的任何直接或间接损失（账号封禁、数据丢失、法律纠纷等）承担责任。</li>
        <li>开发者保留随时修改本免责声明的权利，恕不另行通知。</li>
        <li>为了改进软件，以及提供更好服务，我们会收集所需的最小化运行信息。</li>
        <li>我们鼓励用户开通官方 VIP/SVIP以获得更好的下载与加速体验。</li>
      </ol>
      <p className="about-declaration-acceptance">下载或使用本项目，即视为您已阅读并接受上述全部条款。</p>
    </section>
  </article>;
}
