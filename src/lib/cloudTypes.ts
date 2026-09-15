import type { TaskActionResult } from './types';

export type CloudProvider = 'quark' | 'pikpak' | 'baidu' | 'tianyi' | 'aliyun' | 'xunlei' | 'uc';
export type CloudAuthMethod = 'web' | 'password';

export interface ProviderCapabilities {
  list: boolean; createDirectory: boolean; rename: boolean; move: boolean; copy: boolean;
  trash: boolean; delete: boolean; restore: boolean; download: boolean; upload: boolean;
  shareInspect: boolean; shareTransfer: boolean; recordedTransferResolve: boolean;
}
export interface DownloadConcurrencyPolicy {
  maxConnectionsPerFile: number;
  maxConcurrentTasks: number;
}
export interface ProviderDescriptor {
  provider: CloudProvider;
  displayName: string;
  shareDisplayName: string;
  uploadDisplayName: string;
  shareDirectoryName: string;
  rootDirectoryId: string;
  recycleBinDirectoryId: string;
  authorization: CloudAuthorizationCapabilities;
  authorizationUrl: string;
  passwordLoginHosts: readonly string[];
  capabilities: ProviderCapabilities;
  shareHosts: readonly string[];
  sharePatterns: readonly string[];
  maxUploadConcurrency: number;
  downloadConcurrency: DownloadConcurrencyPolicy;
}

export interface CloudAccount { provider: CloudProvider; id: string; displayName: string; authMethod: CloudAuthMethod; expiresAt?: string; }
export type CloudStorageCapacity =
  | { kind: 'limited'; totalBytes: number; usedBytes: number }
  | { kind: 'unlimited'; usedBytes: number };
export interface CloudAccountCapacity {
  provider: CloudProvider;
  accountId: string;
  capacity: CloudStorageCapacity;
  updatedAt: number;
}
export type CloudCapacityChangeReason = 'upload' | 'copy' | 'transfer' | 'trash' | 'restore' | 'delete';
export interface CloudCapacityInvalidated {
  provider: CloudProvider;
  accountId: string;
  reason: CloudCapacityChangeReason;
}
export interface CloudAuthorizationStateEvent { provider: CloudProvider; state: 'Disconnected' | 'Restoring' | 'Ready' | 'Refreshing' | 'NeedsSilentRelogin' | 'NeedsInteractiveAuth' | 'RiskBlocked'; account: CloudAccount | null; }
export interface CloudFile { id: string; parentId: string; name: string; kind: 'file' | 'folder'; size?: number; modifiedAt?: string; }
export interface CloudDirectory { id: string; items: CloudFile[]; }
export interface CloudUploadFailure { inputIndex?: number; itemName: string; code: string; message: string; }
export interface CloudUploadOutcome {
  uploaded: readonly CloudFile[];
  failures: readonly CloudUploadFailure[];
  cancelled?: boolean;
}
export type CloudUploadAction = 'pause' | 'resume' | 'cancel';
export interface CloudUploadProgress {
  uploadId: string;
  provider: CloudProvider;
  phase: 'preparing' | 'uploading' | 'finalizing' | 'paused' | 'cancelling' | 'cancelled' | 'completed' | 'failed';
  totalBytes: number;
  uploadedBytes: number;
  speedBytesPerSecond: number;
  totalFiles: number;
  completedFiles: number;
  failedItems: number;
  currentFiles: string[];
  progress: number;
}
export interface CloudAuthorization { provider: CloudProvider; windowLabel: string; instructions: string; completed?: boolean; }
export type CloudPasswordLoginMode = 'none' | 'rustDirect' | 'officialWebView';
export interface CloudAuthorizationCapabilities { provider: CloudProvider; web: boolean; password: boolean; passwordLoginMode: CloudPasswordLoginMode; }
export interface SavedCloudCredentialInfo { username: string; hasSavedPassword: boolean; }
export type CloudCredentials = { username: string } & (
  | { password: string; useSavedPassword?: false }
  | { password?: never; useSavedPassword: true }
);
export type PasswordLoginRequest = CloudCredentials & { provider: CloudProvider };
export interface CloudError { code: 'not_authorized' | 'credentials_expired' | 'web_auth_required' | 'authorization_pending' | 'credential_storage_error' | 'invalid_response' | 'network_error' | 'provider_error' | 'invalid_input' | 'provider_unsupported' | 'pikpak_regional_membership_restricted' | 'pikpak_operation_verification_required' | 'session_renewed_retry_operation'; message: string; }
export interface CloudDirectoryDownloadFailure { relativePath: string; code: string; message: string; }
export interface CloudDirectoryDownloadOutcome {
  task: TaskActionResult;
  queuedCount: number;
  failures: readonly CloudDirectoryDownloadFailure[];
}
