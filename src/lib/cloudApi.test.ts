import { vi, expect, it } from 'vitest';

const native = { invoke: vi.fn().mockResolvedValue(undefined) };
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));

it('loads the catalog and uses its root when listing without a parent', async () => {
  const api = await import('./cloudApi');
  native.invoke.mockResolvedValueOnce([{ provider: 'baidu', rootDirectoryId: '/dynamic-root' }]);
  native.invoke.mockResolvedValueOnce({ id: '/dynamic-root', items: [] });
  expect(await api.listCloudDirectory('baidu')).toEqual({ id: '/dynamic-root', items: [] });
  expect(native.invoke).toHaveBeenCalledWith('cloud_provider_catalog');
  expect(native.invoke).toHaveBeenCalledWith('cloud_list_directory', { provider: 'baidu', parentId: '/dynamic-root' });
});

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
