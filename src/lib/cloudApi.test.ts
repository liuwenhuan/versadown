import { vi, expect, it } from 'vitest';

const native = { invoke: vi.fn().mockResolvedValue(undefined) };
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));

it('permanently deletes only the selected cloud recycle-bin files', async () => {
  const api = await import('./cloudApi');

  await api.deleteCloudFiles('pikpak', ['trashed-file-1']);

  expect(native.invoke).toHaveBeenCalledWith('cloud_delete', {
    provider: 'pikpak',
    fileIds: ['trashed-file-1']
  });
});

it('maps one directory parent task with its authoritative group root while preserving the queued count and redacted failures', async () => {
  native.invoke.mockResolvedValueOnce({
    task: { id: 'directory-1', name: '归档', kind: 'cloud', status: 'queued', progress: 0, queueTask: { id: 'group-1', name: '归档', kind: 'folder', status: 'queued', progress: 0, children: [{ id: 'directory-1', name: '归档', kind: 'cloud', status: 'queued', progress: 0 }] } },
    queuedCount: 1,
    failures: [{ relativePath: '归档/私密文件.txt', code: 'network_error', message: '网络或下载引擎错误' }]
  });
  const api = await import('./cloudApi');

  const outcome = await api.downloadCloudDirectory('quark', 'folder-1', '归档');

  expect(native.invoke).toHaveBeenCalledWith('cloud_download_directory', {
    provider: 'quark', directoryId: 'folder-1', directoryName: '归档'
  });
  expect(outcome).toEqual({
    task: { task: { id: 'directory-1', name: '归档', kind: 'cloud', status: 'queued', progress: 0 }, queueTask: { id: 'group-1', name: '归档', kind: 'folder', status: 'queued', progress: 0, children: [{ id: 'directory-1', name: '归档', kind: 'cloud', status: 'queued', progress: 0 }] } },
    queuedCount: 1,
    failures: [{ relativePath: '归档/私密文件.txt', code: 'network_error', message: '网络或下载引擎错误' }]
  });
});

it('marks cloud file and directory submissions as private only when explicitly requested', async () => {
  native.invoke.mockClear();
  native.invoke.mockResolvedValueOnce({ id: 'private-file' });
  native.invoke.mockResolvedValueOnce({ task: { id: 'private-directory' }, queuedCount: 0, failures: [] });
  const api = await import('./cloudApi');

  await api.downloadCloudFile('quark', 'file-1', true);
  await api.downloadCloudDirectory('quark', 'folder-1', '归档', true);

  expect(native.invoke).toHaveBeenNthCalledWith(1, 'cloud_download_url', {
    provider: 'quark', fileId: 'file-1', privateDownload: true
  });
  expect(native.invoke).toHaveBeenNthCalledWith(2, 'cloud_download_directory', {
    provider: 'quark', directoryId: 'folder-1', directoryName: '归档', privateDownload: true
  });
});
