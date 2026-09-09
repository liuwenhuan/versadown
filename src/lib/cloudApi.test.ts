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

it('maps every directory download task while preserving the queued count and redacted failures', async () => {
  native.invoke.mockResolvedValueOnce({
    queuedCount: 1,
    tasks: [{ id: 'cloud-1', name: '说明.txt', kind: 'cloud', status: 'queued', progress: 0 }],
    failures: [{ relativePath: '归档/私密文件.txt', code: 'network_error', message: '网络或下载引擎错误' }]
  });
  const api = await import('./cloudApi');

  const outcome = await api.downloadCloudDirectory('quark', 'folder-1', '归档');

  expect(native.invoke).toHaveBeenCalledWith('cloud_download_directory', {
    provider: 'quark', directoryId: 'folder-1', directoryName: '归档'
  });
  expect(outcome).toEqual({
    queuedCount: 1,
    tasks: [{ task: { id: 'cloud-1', name: '说明.txt', kind: 'cloud', status: 'queued', progress: 0 } }],
    failures: [{ relativePath: '归档/私密文件.txt', code: 'network_error', message: '网络或下载引擎错误' }]
  });
});
