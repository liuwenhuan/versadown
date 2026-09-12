import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { CloudAuthorizationStateEvent } from './cloudTypes';
import type { DuplicateDownloadState, QueueTask, TaskActionResult } from './types';
import type { ProviderDescriptor, CloudAccount, CloudAuthorization, CloudAuthorizationCapabilities, CloudCredentials, CloudDirectory, CloudDirectoryDownloadFailure, CloudDirectoryDownloadOutcome, CloudFile, CloudProvider } from './cloudTypes';

export const getCloudProviderCatalog = () => invoke<ProviderDescriptor[]>('cloud_provider_catalog');

export const getCloudAuthorizationCapabilities = (provider: CloudProvider) => invoke<CloudAuthorizationCapabilities>('cloud_provider_capabilities', { provider });
export const beginCloudAuthorization = (provider: CloudProvider) => invoke<CloudAuthorization>('cloud_begin_authorization', { provider });
export const completeCloudAuthorization = (provider: CloudProvider) => invoke<CloudAccount>('cloud_complete_authorization', { provider });
export const resumeReauthorizedProviderTasks = (provider: CloudProvider) => invoke<void>('resume_reauthorized_provider_tasks', { provider });
const authorizationSubscribers = new Set<{ provider: CloudProvider; receive: (event: CloudAuthorizationStateEvent) => void }>();
let authorizationListener: Promise<() => void> | null = null;
export async function subscribeCloudAuthorization(provider: CloudProvider, receive: (event: CloudAuthorizationStateEvent) => void): Promise<() => void> {
  const subscriber = { provider, receive };
  authorizationSubscribers.add(subscriber);
  const subscription = authorizationListener ??= listen<CloudAuthorizationStateEvent>('cloud-authorization-state', event => {
    for (const observer of authorizationSubscribers) {
      if (event.payload.provider === observer.provider) observer.receive(event.payload);
    }
  });
  let unlisten: () => void;
  try { unlisten = await subscription; }
  catch (error) {
    authorizationSubscribers.delete(subscriber);
    if (authorizationListener === subscription) authorizationListener = null;
    throw error;
  }
  return () => {
    if (!authorizationSubscribers.delete(subscriber)) return;
    if (authorizationSubscribers.size === 0 && authorizationListener === subscription) {
      authorizationListener = null;
      unlisten();
    }
  };
}
export const reauthorizeFailedCloudTask = (taskId: string) => invoke<CloudAuthorization>('cloud_reauthorize_failed_task', { taskId });
export const loginCloudWithPassword = (provider: CloudProvider, credentials: CloudCredentials) => invoke<CloudAccount>('cloud_login_password', { provider, ...credentials });
export const getSavedCloudCredentials = (provider: CloudProvider) => invoke<import('./cloudTypes').SavedCloudCredentialInfo | null>('cloud_saved_password_credentials', { provider });
export const getCloudAccount = (provider: CloudProvider) => invoke<CloudAccount>('cloud_account', { provider });
export async function listCloudDirectory(provider: CloudProvider, parentId?: string): Promise<CloudDirectory> {
  if (parentId === undefined) {
    const descriptor = (await getCloudProviderCatalog()).find(item => item.provider === provider);
    if (!descriptor) throw { code: 'provider_unsupported', message: '网盘目录中没有此服务商' };
    parentId = descriptor.rootDirectoryId;
  }
  return invoke<CloudDirectory>('cloud_list_directory', { provider, parentId });
}
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
export const downloadCloudFile = (provider: CloudProvider, fileId: string): Promise<TaskActionResult> => invoke<NativeCloudDownload>('cloud_download_url', { provider, fileId })
  .then(cloudDownloadResult);
export const downloadCloudDirectory = (provider: CloudProvider, directoryId: string, directoryName: string): Promise<CloudDirectoryDownloadOutcome> => invoke<NativeCloudDirectoryDownload>('cloud_download_directory', { provider, directoryId, directoryName })
  .then((result) => ({ ...result, task: cloudDownloadResult(result.task) }));
export const logoutCloud = (provider: CloudProvider) => invoke<void>('cloud_logout', { provider });
export const uploadCloudFiles = (provider: CloudProvider, parentId: string, paths?: string[]) => invoke<CloudFile[]>('cloud_upload_files', { provider, parentId, paths: paths ?? null });
export const pickAndUploadFiles = (provider: CloudProvider, parentId: string) => uploadCloudFiles(provider, parentId);
