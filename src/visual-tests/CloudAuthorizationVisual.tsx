import React from 'react';
import ReactDOM from 'react-dom/client';
import { CloudManager } from '../components/CloudManager';
import type { CloudProvider } from '../lib/cloudTypes';
import '../styles.css';

const unauthorized = () => Promise.reject({ code: 'not_authorized', message: '尚未授权' });
const account = { provider: 'quark' as const, id: 'visual-account', displayName: '测试账号', authMethod: 'web' as const };
const client = {
  getCloudAuthorizationCapabilities: async (provider: CloudProvider) => ({ provider, web: true, password: false }),
  beginCloudAuthorization: async (provider: CloudProvider) => ({ provider, windowLabel: '登录', instructions: '请在官方网页窗口完成登录。' }),
  completeCloudAuthorization: unauthorized,
  loginCloudWithPassword: unauthorized,
  getCloudAccount: async () => account,
  listCloudDirectory: async (_provider: CloudProvider, parentId = '0') => ({ id: parentId, items: [
    { id: 'folder-1', parentId, name: '游戏备份', kind: 'folder' as const, modifiedAt: '2026-09-07 00:32' },
    { id: 'folder-2', parentId, name: '我的转存', kind: 'folder' as const, modifiedAt: '2026-08-28 16:57' },
    { id: 'folder-3', parentId, name: '我的资源', kind: 'folder' as const, modifiedAt: '2026-08-28 16:57' },
    { id: 'file-1', parentId, name: '游戏安装包.zip', kind: 'file' as const, size: 262.4 * 1024 * 1024, modifiedAt: '2026-08-28 16:57' }
  ] }),
  createCloudDirectory: async () => undefined,
  renameCloudFile: async () => undefined,
  moveCloudFiles: async () => undefined,
  copyCloudFiles: async () => undefined,
  trashCloudFiles: async () => undefined,
  deleteCloudFiles: async () => undefined,
  downloadCloudFile: async () => undefined,
  downloadCloudDirectory: async () => ({ queuedCount: 0, tasks: [], failures: [] }),
  logoutCloud: async () => undefined,
  pickAndUploadFiles: async () => undefined,
};

document.documentElement.dataset.theme = 'dark';
ReactDOM.createRoot(document.getElementById('root')!).render(
  <main className="view-panel" style={{ minHeight: '100vh' }}>
    <CloudManager provider="quark" client={client} initialAccounts={[account]} restoreOnMount={false} showAccountTabs={false} showSessionActions={false} />
  </main>,
);
