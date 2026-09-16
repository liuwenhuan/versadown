import React, { useState } from 'react';
import ReactDOM from 'react-dom/client';
import { LocalUploadPicker } from '../components/LocalUploadPicker';
import type { LocalUploadListing } from '../lib/localUpload';
import '../styles.css';

const roots = [{ path: 'C:/', name: 'C:\\', kind: 'directory' as const }, { path: 'D:/', name: 'D:\\', kind: 'directory' as const }];
async function listEntries(path = 'D:/上传示例'): Promise<LocalUploadListing> {
  if (path === 'D:/' || path === 'C:/') return { path, parentPath: null, roots, entries: [{ path: `${path}上传示例`, name: '上传示例', kind: 'directory', size: null, modifiedAt: 1789276800000 }] };
  if (path === 'D:/上传示例/照片') return { path, parentPath: 'D:/上传示例', roots, entries: [{ path: `${path}/旅行照片.jpg`, name: '旅行照片.jpg', kind: 'file', size: 1572864, modifiedAt: 1789276800000 }] };
  return { path: 'D:/上传示例', parentPath: 'D:/', roots, entries: [
    { path: 'D:/上传示例/照片', name: '照片', kind: 'directory', size: null, modifiedAt: 1789276800000 },
    { path: 'D:/上传示例/项目文档', name: '项目文档', kind: 'directory', size: null, modifiedAt: 1789276800000 },
    { path: 'D:/上传示例/使用说明.pdf', name: '使用说明.pdf', kind: 'file', size: 1536, modifiedAt: 1789276800000 },
    { path: 'D:/上传示例/VersaDown-安装包.zip', name: 'VersaDown-安装包.zip', kind: 'file', size: 104857600, modifiedAt: 1789276800000 },
    { path: 'D:/上传示例/一个较长的文件名称用于验证在较小窗口中能够正确省略和显示完整路径.txt', name: '一个较长的文件名称用于验证在较小窗口中能够正确省略和显示完整路径.txt', kind: 'file', size: 0, modifiedAt: null }
  ] };
}

function Preview() {
  const [open, setOpen] = useState(true);
  const [paths, setPaths] = useState<string[]>([]);
  return <main style={{ padding: 24 }}><h1>统一上传选择器预览</h1><p>仅使用模拟目录，不读取本地文件，也不会执行上传。</p><button onClick={() => setOpen(true)}>上传</button><button onClick={() => { document.documentElement.dataset.theme = document.documentElement.dataset.theme === 'deep-ocean' ? 'mint' : 'deep-ocean'; }}>切换主题</button><output>{JSON.stringify(paths)}</output>{open && <LocalUploadPicker listEntries={listEntries} onCancel={() => setOpen(false)} onConfirm={selected => { setPaths(selected); setOpen(false); }} />}</main>;
}
ReactDOM.createRoot(document.getElementById('root')!).render(<React.StrictMode><Preview /></React.StrictMode>);
