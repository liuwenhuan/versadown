import type { TaskActionResult } from './types';

export type CloudProvider = 'quark' | 'pikpak' | 'baidu' | 'tianyi' | 'aliyun' | 'xunlei' | 'uc';
export type CloudAuthMethod = 'web' | 'password';

export const cloudRootDirectoryId: Record<CloudProvider, string> = {
  quark: '0',
  pikpak: '0',
  baidu: '/',
  tianyi: '-11',
  aliyun: 'root',
  xunlei: '0',
  uc: '0'
};

export const cloudRecycleBinDirectoryId: Record<CloudProvider, string> = {
  quark: '__all_downloader_quark_recycle_bin__',
  pikpak: '__all_downloader_pikpak_recycle_bin__',
  baidu: '__all_downloader_baidu_recycle_bin__',
  tianyi: '__all_downloader_tianyi_recycle_bin__',
  aliyun: '__all_downloader_aliyun_recycle_bin__',
  xunlei: '__all_downloader_xunlei_recycle_bin__',
  uc: '__all_downloader_uc_recycle_bin__'
};

export interface CloudAccount { provider: CloudProvider; id: string; displayName: string; authMethod: CloudAuthMethod; expiresAt?: string; }
export interface CloudFile { id: string; parentId: string; name: string; kind: 'file' | 'folder'; size?: number; modifiedAt?: string; }
export interface CloudDirectory { id: string; items: CloudFile[]; }
export interface CloudAuthorization { provider: CloudProvider; windowLabel: string; instructions: string; }
export type CloudPasswordLoginMode = 'none' | 'rustDirect' | 'officialWebView';
export interface CloudAuthorizationCapabilities { provider: CloudProvider; web: boolean; password: boolean; passwordLoginMode: CloudPasswordLoginMode; }
export interface CloudCredentials { username: string; password: string; }
export interface CloudError { code: 'not_authorized' | 'credentials_expired' | 'web_auth_required' | 'authorization_pending' | 'credential_storage_error' | 'invalid_response' | 'network_error' | 'provider_error' | 'invalid_input' | 'provider_unsupported' | 'pikpak_regional_membership_restricted' | 'session_renewed_retry_operation'; message: string; }
export interface CloudDirectoryDownloadFailure { relativePath: string; code: string; message: string; }
export interface CloudDirectoryDownloadOutcome {
  queuedCount: number;
  tasks: readonly TaskActionResult[];
  failures: readonly CloudDirectoryDownloadFailure[];
}
