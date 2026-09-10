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
  native.invoke.mockResolvedValueOnce([]);
  const { confirmCloudShareDownload } = await import('./api');

  await confirmCloudShareDownload(`cloud-share://baidu/${'a'.repeat(64)}/${'b'.repeat(64)}`, ['f1']);

  expect(native.invoke).toHaveBeenCalledWith('confirm_cloud_share_download', {
    planSource: `cloud-share://baidu/${'a'.repeat(64)}/${'b'.repeat(64)}`,
    selectedIds: ['f1']
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
