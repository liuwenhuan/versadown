import type { CloudProvider, ProviderDescriptor } from '../lib/cloudTypes';

// Test transport fixture; production metadata comes exclusively from the backend.
export const cloudCatalog: ProviderDescriptor[] = [
  ['quark', '夸克', '0', 'none'], ['pikpak', 'PikPak', '0', 'rustDirect'],
  ['baidu', '百度', '/', 'rustDirect'], ['tianyi', '天翼', '-11', 'rustDirect'],
  ['aliyun', '阿里', 'root', 'officialWebView'], ['xunlei', '迅雷', '0', 'officialWebView'],
  ['uc', 'UC', '0', 'officialWebView']
].map(([provider, displayName, rootDirectoryId, passwordLoginMode]) => ({
  provider: provider as CloudProvider, displayName, rootDirectoryId,
  shareDisplayName: displayName + '网盘',
  shareDirectoryName: displayName + '分享目录',
  passwordLoginHosts: [],
  recycleBinDirectoryId: `__all_downloader_${provider}_recycle_bin__`,
  authorizationUrl: 'https://example.test', shareHosts: ['example.test'], sharePatterns: ['/s/'],
  authorization: { provider: provider as CloudProvider, web: true, password: passwordLoginMode === 'rustDirect', passwordLoginMode: passwordLoginMode as ProviderDescriptor['authorization']['passwordLoginMode'] },
  capabilities: { list: true, createDirectory: true, rename: true, move: true, copy: true, trash: true, delete: true, restore: true, download: true, upload: true, shareInspect: true, shareTransfer: true, recordedTransferResolve: true },
  maxUploadConcurrency: 2
}));
export const cloudRootDirectoryId = Object.fromEntries(cloudCatalog.map(d => [d.provider, d.rootDirectoryId])) as Record<CloudProvider, string>;
export const cloudRecycleBinDirectoryId = Object.fromEntries(cloudCatalog.map(d => [d.provider, d.recycleBinDirectoryId])) as Record<CloudProvider, string>;
