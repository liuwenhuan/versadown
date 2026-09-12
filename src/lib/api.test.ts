import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));

beforeEach(() => {
  Object.defineProperty(window, '__TAURI_INTERNALS__', {
    configurable: true,
    value: {}
  });
  vi.clearAllMocks();
});

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
});

it('classifies ED2K, FTP, HLS and DASH in the browser fallback', async () => {
  const { parseLocally } = await import('./api');

  expect(parseLocally('ed2k://|file|Example%20Archive.zip|42|0123456789ABCDEF0123456789ABCDEF|/')).toMatchObject({
    task: { kind: 'ed2k', name: 'Example Archive.zip' }
  });
  expect(parseLocally('ftp://alice:secret@example.test/pub/archive.zip')).toMatchObject({
    task: { kind: 'ftp', name: 'archive.zip', source: 'ftp://example.test/pub/archive.zip' }
  });
  expect(parseLocally('https://media.test/live/master.m3u8?token=one')).toMatchObject({
    task: { kind: 'hls', name: 'master.m3u8' }
  });
  expect(parseLocally('https://media.test/video/manifest.mpd')).toMatchObject({
    task: { kind: 'dash', name: 'manifest.mpd' }
  });
});

it('maps a share confirmation without inventing an aria2 task', async () => {
  native.invoke.mockResolvedValueOnce({
    kind: 'share_confirmation',
    plan_source: 'cloud-share://uc/a/b',
    title: '照片',
    resources: [{ id: 'f1', name: '照片/a.jpg', size: '3 B', selected: true }]
  });
  const { parseDownload } = await import('./api');

  await expect(parseDownload('https://drive.uc.cn/s/id')).resolves.toMatchObject({
    confirmation: { taskId: 'cloud-share://uc/a/b' }
  });
});

it('maps an access-code requirement without creating a task', async () => {
  native.invoke.mockResolvedValueOnce({
    kind: 'access_code_required',
    provider: 'baidu',
    message: '请输入提取码后重新解析分享链接'
  });
  const { parseDownload } = await import('./api');

  await expect(parseDownload('https://pan.baidu.com/s/id')).resolves.toEqual({
    shareAccessCodeRequired: {
      provider: 'baidu',
      message: '请输入提取码后重新解析分享链接'
    }
  });
});

it('passes a transient access code only to the parse command', async () => {
  native.invoke.mockResolvedValueOnce({
    kind: 'access_code_required',
    provider: 'baidu',
    message: '请输入提取码后重新解析分享链接'
  });
  const { parseDownload } = await import('./api');

  await parseDownload('https://pan.baidu.com/s/id', '1234');

  expect(native.invoke).toHaveBeenCalledWith('parse_download', { input: 'https://pan.baidu.com/s/id', accessCode: '1234' });
});

it('sends opaque share plans to the cloud-share confirmation command', async () => {
  native.invoke.mockResolvedValueOnce({ outcomes: [], failures: [] });
  const { confirmCloudShareDownload } = await import('./api');

  await confirmCloudShareDownload(`cloud-share://baidu/${'a'.repeat(64)}/${'b'.repeat(64)}`, ['f1']);

  expect(native.invoke).toHaveBeenCalledWith('confirm_cloud_share_download', {
    planSource: `cloud-share://baidu/${'a'.repeat(64)}/${'b'.repeat(64)}`,
    selectedIds: ['f1']
  });
});

it('preserves every native share task outcome and maps duplicate metadata in order', async () => {
  native.invoke.mockResolvedValueOnce({
    outcomes: [
      { id: 'queued-1', name: 'first.bin', kind: 'cloud', status: 'queued', progress: 0 },
      { id: 'duplicate-1', name: 'existing.bin', kind: 'cloud', status: 'completed', progress: 100, duplicate: 'completed_files_present' },
      { id: 'queued-2', name: 'second.bin', kind: 'cloud', status: 'queued', progress: 0 }
    ],
    failures: [{ selectedId: 'f4', code: 'web_auth_required', message: '网盘需要网页验证' }]
  });
  const { confirmCloudShareDownload } = await import('./api');

  await expect(confirmCloudShareDownload('cloud-share://baidu/plan', ['f1', 'f2', 'f3'])).resolves.toEqual({
    outcomes: [
      { task: { id: 'queued-1', name: 'first.bin', kind: 'cloud', status: 'queued', progress: 0 } },
      {
        duplicate: {
          task: { id: 'duplicate-1', name: 'existing.bin', kind: 'cloud', status: 'completed', progress: 100 },
          state: 'completed_files_present'
        }
      },
      { task: { id: 'queued-2', name: 'second.bin', kind: 'cloud', status: 'queued', progress: 0 } }
    ],
    failures: [{ selectedId: 'f4', code: 'web_auth_required', message: '网盘需要网页验证' }]
  });
});

it('maps the tagged native task outcome without changing direct downloads', async () => {
  native.invoke.mockResolvedValueOnce({
    kind: 'task',
    task: { id: 'task-1', name: 'fixture.bin', kind: 'file', status: 'downloading', progress: 0 },
    requires_confirmation: false,
    resources: []
  });
  const { parseDownload } = await import('./api');

  await expect(parseDownload('https://example.test/fixture.bin')).resolves.toEqual({
    task: { id: 'task-1', name: 'fixture.bin', kind: 'file', status: 'downloading', progress: 0 }
  });
});

it('sends the privacy confirmation and private selection flags only when requested', async () => {
  native.invoke.mockResolvedValueOnce({ kind: 'task', task: { id: 'awaiting', name: 'fixture.bin', kind: 'file', status: 'awaiting_user', progress: 0 }, requires_confirmation: true, resources: [] });
  native.invoke.mockResolvedValueOnce({ id: 'private-direct' });
  native.invoke.mockResolvedValueOnce({ outcomes: [], failures: [] });
  const { confirmCloudShareDownload, confirmDownload, parseDownload } = await import('./api');

  await parseDownload('https://example.test/fixture.bin', undefined, true);
  await confirmDownload('awaiting', [], undefined, true);
  await confirmCloudShareDownload('cloud-share://quark/plan', ['f1'], undefined, true);

  expect(native.invoke).toHaveBeenNthCalledWith(1, 'parse_download', {
    input: 'https://example.test/fixture.bin', accessCode: undefined, requireConfirmation: true
  });
  expect(native.invoke).toHaveBeenNthCalledWith(2, 'confirm_download_command', {
    taskId: 'awaiting', selectedIds: [], privateDownload: true
  });
  expect(native.invoke).toHaveBeenNthCalledWith(3, 'confirm_cloud_share_download', {
    planSource: 'cloud-share://quark/plan', selectedIds: ['f1'], privateDownload: true
  });
});
