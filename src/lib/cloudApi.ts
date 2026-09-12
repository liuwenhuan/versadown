import { invoke } from '@tauri-apps/api/core';
import type { DuplicateDownloadState, QueueTask, TaskActionResult } from './types';
import { cloudRootDirectoryId, type CloudAccount, type CloudAuthorization, type CloudAuthorizationCapabilities, type CloudCredentials, type CloudDirectory, type CloudDirectoryDownloadFailure, type CloudDirectoryDownloadOutcome, type CloudFile, type CloudProvider } from './cloudTypes';

export const getCloudAuthorizationCapabilities = (provider: CloudProvider) => invoke<CloudAuthorizationCapabilities>('cloud_provider_capabilities', { provider });
export const beginCloudAuthorization = (provider: CloudProvider) => invoke<CloudAuthorization>('cloud_begin_authorization', { provider });
export const completeCloudAuthorization = (provider: CloudProvider) => invoke<CloudAccount>('cloud_complete_authorization', { provider });
export const reauthorizeFailedCloudTask = (taskId: string) => invoke<CloudAuthorization>('cloud_reauthorize_failed_task', { taskId });
export const loginCloudWithPassword = (provider: CloudProvider, credentials: CloudCredentials) => invoke<CloudAccount>('cloud_login_password', { provider, ...credentials });
export const getSavedCloudCredentials = (provider: CloudProvider) => invoke<CloudCredentials | null>('cloud_saved_password_credentials', { provider });
export const getCloudAccount = (provider: CloudProvider) => invoke<CloudAccount>('cloud_account', { provider });
export const listCloudDirectory = (provider: CloudProvider, parentId = cloudRootDirectoryId[provider]) => invoke<CloudDirectory>('cloud_list_directory', { provider, parentId });
export const createCloudDirectory = (provider: CloudProvider, parentId: string, name: string) => invoke<CloudFile>('cloud_create_directory', { provider, parentId, name });
export const renameCloudFile = (provider: CloudProvider, fileId: string, name: string) => invoke<void>('cloud_rename', { provider, fileId, name });
export const moveCloudFiles = (provider: CloudProvider, fileIds: string[], targetParentId: string) => invoke<void>('cloud_move', { provider, fileIds, targetParentId });
export const copyCloudFiles = (provider: CloudProvider, fileIds: string[], targetParentId: string) => invoke<void>('cloud_copy', { provider, fileIds, targetParentId });
export const trashCloudFiles = (provider: CloudProvider, fileIds: string[]) => invoke<void>('cloud_trash', { provider, fileIds });
/** Permanently deletes files already displayed inside a provider recycle bin. */
export const deleteCloudFiles = (provider: CloudProvider, fileIds: string[]) => invoke<void>('cloud_delete', { provider, fileIds });
/** Restores files already displayed inside a provider recycle bin. */
export const restoreCloudFiles = (provider: CloudProvider, fileIds: string[]) => invoke<void>('cloud_restore', { provider, fileIds });
type NativeCloudDownload = QueueTask & { duplicate?: DuplicateDownloadState; queueTask?: QueueTask };
type NativeCloudDirectoryDownload = {
  task: NativeCloudDownload;
  queuedCount: number;
  failures: CloudDirectoryDownloadFailure[];
};
function cloudDownloadResult(result: NativeCloudDownload): TaskActionResult {
  const { duplicate, queueTask, ...task } = result;
  return duplicate
    ? { duplicate: { task, state: duplicate }, ...(queueTask ? { queueTask } : {}) }
    : { task, ...(queueTask ? { queueTask } : {}) };
}
export const downloadCloudFile = (provider: CloudProvider, fileId: string, privateDownload = false): Promise<TaskActionResult> => invoke<NativeCloudDownload>('cloud_download_url', {
  provider,
  fileId,
  ...(privateDownload ? { privateDownload: true } : {})
})
  .then(cloudDownloadResult);
export const downloadCloudDirectory = (provider: CloudProvider, directoryId: string, directoryName: string, privateDownload = false): Promise<CloudDirectoryDownloadOutcome> => invoke<NativeCloudDirectoryDownload>('cloud_download_directory', {
  provider,
  directoryId,
  directoryName,
  ...(privateDownload ? { privateDownload: true } : {})
})
  .then((result) => ({ ...result, task: cloudDownloadResult(result.task) }));
export const logoutCloud = (provider: CloudProvider) => invoke<void>('cloud_logout', { provider });
export const uploadCloudFiles = (provider: CloudProvider, parentId: string, paths?: string[]) => invoke<CloudFile[]>('cloud_upload_files', { provider, parentId, paths: paths ?? null });
export const pickAndUploadFiles = (provider: CloudProvider, parentId: string) => uploadCloudFiles(provider, parentId);
