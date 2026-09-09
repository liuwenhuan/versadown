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
