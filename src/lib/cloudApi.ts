import { invoke } from '@tauri-apps/api/core';
import type { DuplicateDownloadState, QueueTask, TaskActionResult } from './types';
import type { CloudAccount, CloudAuthorization, CloudCredentials, CloudDirectory, CloudFile, CloudProvider } from './cloudTypes';

export const beginCloudAuthorization = (provider: CloudProvider) => invoke<CloudAuthorization>('cloud_begin_authorization', { provider });
export const completeCloudAuthorization = (provider: CloudProvider) => invoke<CloudAccount>('cloud_complete_authorization', { provider });
export const loginCloudWithPassword = (provider: CloudProvider, credentials: CloudCredentials) => invoke<CloudAccount>('cloud_login_password', { provider, ...credentials });
export const getCloudAccount = (provider: CloudProvider) => invoke<CloudAccount>('cloud_account', { provider });
export const listCloudDirectory = (provider: CloudProvider, parentId = '0') => invoke<CloudDirectory>('cloud_list_directory', { provider, parentId });
export const createCloudDirectory = (provider: CloudProvider, parentId: string, name: string) => invoke<CloudFile>('cloud_create_directory', { provider, parentId, name });
export const renameCloudFile = (provider: CloudProvider, fileId: string, name: string) => invoke<void>('cloud_rename', { provider, fileId, name });
export const moveCloudFiles = (provider: CloudProvider, fileIds: string[], targetParentId: string) => invoke<void>('cloud_move', { provider, fileIds, targetParentId });
export const copyCloudFiles = (provider: CloudProvider, fileIds: string[], targetParentId: string) => invoke<void>('cloud_copy', { provider, fileIds, targetParentId });
export const trashCloudFiles = (provider: CloudProvider, fileIds: string[]) => invoke<void>('cloud_trash', { provider, fileIds });
type NativeCloudDownload = QueueTask & { duplicate?: DuplicateDownloadState };
export const downloadCloudFile = (provider: CloudProvider, fileId: string): Promise<TaskActionResult> => invoke<NativeCloudDownload>('cloud_download_url', { provider, fileId })
  .then((result) => result.duplicate ? { duplicate: { task: result, state: result.duplicate } } : { task: result });
export const logoutCloud = (provider: CloudProvider) => invoke<void>('cloud_logout', { provider });
export const uploadCloudFiles = (provider: CloudProvider, parentId: string, paths?: string[]) => invoke<CloudFile[]>('cloud_upload_files', { provider, parentId, paths: paths ?? null });
export const pickAndUploadFiles = (provider: CloudProvider, parentId: string) => uploadCloudFiles(provider, parentId);
