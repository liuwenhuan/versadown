import React from 'react';
import ReactDOM from 'react-dom/client';
import { CloudWorkspace } from '../components/CloudWorkspace';
import type { CloudProvider } from '../lib/cloudTypes';
import '../styles.css';

const unauthorized = () => Promise.reject({ code: 'not_authorized', message: '尚未授权' });
const client = {
  beginCloudAuthorization: async (provider: CloudProvider) => ({ provider, windowLabel: '登录', instructions: '请在官方网页窗口完成登录。' }),
  completeCloudAuthorization: unauthorized,
  loginCloudWithPassword: unauthorized,
  getCloudAccount: unauthorized,
  listCloudDirectory: unauthorized,
  createCloudDirectory: unauthorized,
  renameCloudFile: unauthorized,
  moveCloudFiles: unauthorized,
  trashCloudFiles: unauthorized,
  downloadCloudFile: unauthorized,
  logoutCloud: unauthorized,
  pickAndUploadFiles: unauthorized,
};

document.documentElement.dataset.theme = 'dark';
ReactDOM.createRoot(document.getElementById('root')!).render(
  <main className="view-panel" style={{ minHeight: '100vh' }}>
    <CloudWorkspace client={client} />
  </main>,
);
