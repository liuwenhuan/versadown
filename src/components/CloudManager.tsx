import { useState } from 'react';
import { Provider } from '../lib/types';

const providerLabel: Record<Provider, string> = { quark: '夸克', pikpak: 'PikPak' };

const sampleItems = [
  { name: '我的文件夹', kind: 'folder', size: '—', modified: '今天 20:36' },
  { name: '下载示例.txt', kind: 'file', size: '12 KB', modified: '昨天 09:24' }
];

export function CloudManager({ provider }: { provider: Provider }) {
  const [notice, setNotice] = useState('');
  const label = providerLabel[provider];
  return <section className="cloud-manager"><header className="cloud-heading"><div><p className="eyebrow">自有网盘 · 本地会话</p><h2>{label}网盘</h2><span>此处是应用自绘文件管理器，不加载服务商官网页面。</span></div><button className="primary" onClick={() => setNotice('需要你在授权窗口中完成登录')}>连接 {label} 账号</button></header>
    {notice && <div className="authorization-note" role="status"><b>授权已暂停</b><span>{notice}；短信、扫码、验证码和动态文本均由你亲自处理。</span></div>}
    <div className="cloud-toolbar"><button onClick={() => setNotice('创建目录需要先完成账号授权')}>新建文件夹</button><button onClick={() => setNotice('上传需要先完成账号授权')}>上传</button><button onClick={() => setNotice('下载将交由本地 aria2 任务队列')}>下载</button><button onClick={() => setNotice('重命名、移动和删除会在授权后可用')}>管理</button></div>
    <div className="cloud-list" role="tree" aria-label={`${label}网盘文件`}>
      <div className="cloud-row cloud-columns"><span>名称</span><span>大小</span><span>修改时间</span></div>
      {sampleItems.map((item) => <div className="cloud-row" role="treeitem" key={item.name}><span><i className={item.kind}>{item.kind === 'folder' ? '□' : '▤'}</i>{item.name}</span><span>{item.size}</span><span>{item.modified}</span></div>)}
    </div>
    <p className="cloud-footnote">连接器会在授权后保存本机受保护的会话；服务商会话过期时才需要重新授权。</p>
  </section>;
}
