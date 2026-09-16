import { useState } from 'react';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { readFileSync } from 'node:fs';
import { afterEach, vi } from 'vitest';
import { isTauriRuntime, listPersistedTasks } from '../lib/api';
import { NewDownloadDialog } from './NewDownloadDialog';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return { ...actual, isTauriRuntime: vi.fn(() => false), listPersistedTasks: vi.fn(async () => []) };
});

afterEach(() => {
  vi.mocked(isTauriRuntime).mockReturnValue(false);
  vi.mocked(listPersistedTasks).mockReset();
  vi.mocked(listPersistedTasks).mockResolvedValue([]);
});

it('keeps the launcher placement and its compact-window close label by default', () => {
  render(<NewDownloadDialog open onParse={vi.fn()} onClose={vi.fn()} />);

  expect(screen.getByRole('dialog', { name: '新建下载' })).toHaveClass('launcher-panel');
  expect(screen.getByRole('button', { name: '收起下载输入' })).toBeVisible();
  expect(screen.queryByTestId('new-download-backdrop')).not.toBeInTheDocument();
});

it('uses concise quick-add controls without explanatory labels', () => {
  render(<NewDownloadDialog open onParse={vi.fn()} onClose={vi.fn()} />);

  expect(screen.getByRole('button', { name: '打开任务列表' })).toBeVisible();
  expect(screen.queryByText('下载内容')).not.toBeInTheDocument();
  expect(screen.queryByText('标准链接直接规则解析；复杂内容才会请求 AI 辅助。')).not.toBeInTheDocument();
});

it.each(['modal', 'header'] as const)('omits parser implementation guidance in %s placement', (placement) => {
  render(<NewDownloadDialog open placement={placement} onParse={vi.fn()} onClose={vi.fn()} />);
  expect(screen.queryByText('标准链接直接规则解析；复杂内容才会请求 AI 辅助。')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeVisible();
});

it('uses a compact launcher card without a fixed blank lower area', () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    render(<NewDownloadDialog open onParse={vi.fn()} onClose={vi.fn()} />);

    const dialog = screen.getByRole('dialog', { name: '新建下载' });
    const input = screen.getByLabelText('下载内容');
    expect(getComputedStyle(dialog).width).toBe('410px');
    expect(getComputedStyle(dialog).height).toBe('auto');
    expect(getComputedStyle(input).height).toBe('88px');
    expect(dialog.querySelector('.launcher-title strong')).toBeNull();
  } finally {
    stylesheet.remove();
  }
});

it('enables parsing only after the quick-add input contains content', async () => {
  const user = userEvent.setup();
  render(<NewDownloadDialog open onParse={vi.fn()} onClose={vi.fn()} />);

  const submit = screen.getByRole('button', { name: '开始解析下载' });
  expect(submit).toBeDisabled();
  await user.type(screen.getByLabelText('下载内容'), 'https://example.com/file.zip');
  expect(submit).toBeEnabled();
});

it('confirms scanned candidates in order and skips invalid input without submitting it', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn().mockResolvedValue(undefined);
  const onClose = vi.fn();
  render(<NewDownloadDialog open placement="modal" onParse={onParse} onClose={onClose} onScan={async () => [
    { input: 'https://example.test/first.iso', valid: true, kind: 'direct' },
    { input: 'magnet:?xt=urn:btih:short', valid: false, message: '磁链格式无效' }
  ]} />);

  await user.type(screen.getByLabelText('下载内容'), '混合下载文本');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(screen.getByRole('status')).toHaveTextContent('正在处理 1/2');
  expect(screen.getByText('已识别有效下载链接，确认后才会创建任务。')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '确认并继续' }));
  await waitFor(() => expect(onParse).toHaveBeenCalledWith('https://example.test/first.iso', undefined));

  expect(screen.getByRole('status')).toHaveTextContent('正在处理 2/2');
  expect(screen.getByRole('alert')).toHaveTextContent('磁链格式无效');
  await user.click(screen.getByRole('button', { name: '跳过并完成' }));
  expect(onParse).toHaveBeenCalledTimes(1);
  expect(onClose).toHaveBeenCalledOnce();
});

it('requires confirmation for a standalone non-direct candidate', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn().mockResolvedValue(undefined);
  render(<NewDownloadDialog open placement="modal" onParse={onParse} onClose={vi.fn()} onScan={async () => [
    { input: 'ftp://example.test/pub/archive.iso', valid: true, kind: 'ftp' }
  ]} />);

  await user.type(screen.getByLabelText('下载内容'), 'ftp://example.test/pub/archive.iso');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(onParse).not.toHaveBeenCalled();
  expect(screen.getByRole('status')).toHaveTextContent('正在处理 1/1');
  expect(screen.getByRole('button', { name: '确认并完成' })).toBeEnabled();
});

it('closes the modal through Escape, its backdrop, and its modal close button', async () => {
  const user = userEvent.setup();
  const onClose = vi.fn();
  render(<NewDownloadDialog open placement="modal" onParse={vi.fn()} onClose={onClose} />);

  expect(screen.getByRole('button', { name: '关闭新建下载' })).toBeVisible();
  await user.keyboard('{Escape}');
  expect(onClose).toHaveBeenCalledTimes(1);

  await user.click(screen.getByTestId('new-download-backdrop'));
  expect(onClose).toHaveBeenCalledTimes(2);

  await user.click(screen.getByRole('button', { name: '关闭新建下载' }));
  expect(onClose).toHaveBeenCalledTimes(3);
});

it('waits for parsing before closing and prevents duplicate submissions', async () => {
  const user = userEvent.setup();
  let completeParse!: () => void;
  const parsing = new Promise<void>((resolve) => { completeParse = resolve; });
  const onParse = vi.fn(() => parsing);
  const onClose = vi.fn();
  render(<NewDownloadDialog open placement="modal" onParse={onParse} onClose={onClose} />);

  const input = screen.getByLabelText('下载内容');
  await user.type(input, 'https://example.com/file.zip');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(onParse).toHaveBeenCalledOnce();
  expect(screen.getByRole('button', { name: '正在解析…' })).toBeDisabled();
  expect(input).toHaveValue('https://example.com/file.zip');
  expect(onClose).not.toHaveBeenCalled();

  completeParse();
  await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  expect(input).toHaveValue('');
});

it('keeps the input open and reports a parsing failure', async () => {
  const user = userEvent.setup();
  const onClose = vi.fn();
  render(<NewDownloadDialog open placement="modal" onParse={() => Promise.reject(new Error('解析服务不可用'))} onClose={onClose} />);

  const input = screen.getByLabelText('下载内容');
  await user.type(input, 'magnet:?xt=urn:btih:bad');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  expect(await screen.findByRole('alert')).toHaveTextContent('解析服务不可用');
  expect(input).toHaveValue('magnet:?xt=urn:btih:bad');
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeEnabled();
});

it.each([
  '网盘请求失败：夸克分享已被取消',
  '网盘请求失败：夸克分享因账号封禁而无法访问',
  '网盘请求失败：PikPak 分享的文件不存在',
  '网盘请求失败：百度网盘分享链接已过期',
  '网盘请求失败：天翼云盘分享已取消',
  '网盘请求失败：阿里云盘分享已删除',
  '网盘请求失败：迅雷分享已删除',
  '网盘请求失败：UC 分享已失效或已停止分享',
  '网盘请求失败：百度分享无效或请求被拒绝',
  '网盘请求失败：夸克分享无效或已过期',
  '网盘请求失败：PikPak 分享已失效或无权访问',
  '网盘请求失败：迅雷分享已失效或无权访问',
  '网盘请求失败：天翼云盘分享不可用，请检查访问码、有效期及权限',
  '网盘请求失败：阿里云盘分享不可用，请检查分享有效期及访问权限',
  '网盘请求失败：UC 分享链接无权访问',
  '网盘请求失败：分享内容被封禁',
])('closes instead of reparsing unavailable shares across providers: %s', async (message) => {
  const user = userEvent.setup();
  const onParse = vi.fn().mockRejectedValue(message);
  const onClose = vi.fn();
  render(<NewDownloadDialog open placement="header" initialInput="https://example.test/share" onParse={onParse} onClose={onClose} />);
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  const alert = await screen.findByRole('alert');
  expect(alert).toHaveTextContent(message);
  const close = screen.getByRole('button', { name: '关闭', exact: true });
  expect(close.parentElement).toContainElement(alert);
  expect(alert).toHaveClass('parse-failure');
  expect(screen.queryByRole('button', { name: '开始解析下载' })).not.toBeInTheDocument();
  await user.click(close);
  expect(onClose).toHaveBeenCalledOnce();
  expect(onParse).toHaveBeenCalledOnce();
});

it.each([
  '网盘请求失败：分享下载准备已取消',
  '分享请求超时，请重试',
  '阿里云盘分享请求频繁，请稍后重试',
  '百度网盘授权已过期，请重新授权',
  '分享访问码错误或缺失',
  '分享令牌已过期，请重新获取',
  '百度分享提取码校验失败，请检查提取码后重试',
  '百度分享验证已通过，但文件列表访问失败，请重试或重新网页授权',
  '分享不可用：网络请求超时，请重试',
  '分享不可用：请求频繁，请稍后重试',
  '分享链接无效：提取码错误',
  '分享不可用：请先登录后重试',
])('keeps retry available for recoverable failures: %s', async (message) => {
  const user = userEvent.setup();
  render(<NewDownloadDialog open initialInput="https://example.test/share" onParse={() => Promise.reject(new Error(message))} onClose={vi.fn()} />);
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(await screen.findByRole('alert')).toHaveTextContent(message);
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeEnabled();
});

it.each(['launcher', 'modal', 'header'] as const)('clears an unavailable-share failure when the user changes the link in %s', async (placement) => {
  const user = userEvent.setup();
  const onParse = vi.fn().mockRejectedValueOnce(new Error('网盘请求失败：百度分享无效或请求被拒绝')).mockResolvedValueOnce(undefined);
  render(<NewDownloadDialog open placement={placement} initialInput="https://example.test/old" onParse={onParse} onClose={vi.fn()} />);
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(await screen.findByRole('button', { name: '关闭', exact: true })).toBeEnabled();
  await user.clear(screen.getByLabelText('下载内容'));
  await user.type(screen.getByLabelText('下载内容'), 'https://example.test/new');
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(onParse).toHaveBeenLastCalledWith('https://example.test/new', undefined);
});

it('keeps the share link open, focuses an access-code field, and resubmits the code when required', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn()
    .mockResolvedValueOnce({ shareAccessCodeRequired: { provider: 'baidu', message: '请输入提取码' } })
    .mockResolvedValueOnce(undefined);
  const onClose = vi.fn();
  render(<NewDownloadDialog open placement="modal" onParse={onParse} onClose={onClose} />);

  const shareLink = screen.getByLabelText('下载内容');
  await user.type(shareLink, 'https://pan.baidu.com/s/example');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));

  const accessCode = await screen.findByLabelText('访问码');
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
  expect(shareLink).toHaveValue('https://pan.baidu.com/s/example');
  expect(accessCode).toHaveFocus();
  expect(onClose).not.toHaveBeenCalled();

  await user.type(accessCode, '1234');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  await waitFor(() => expect(onParse).toHaveBeenLastCalledWith('https://pan.baidu.com/s/example', '1234'));
});

function ModalHost() {
  const [open, setOpen] = useState(false);
  return <><button onClick={() => setOpen(true)}>打开新建下载</button><NewDownloadDialog open={open} placement="modal" onParse={() => {}} onClose={() => setOpen(false)} /></>;
}

it.each(['header', 'modal', 'launcher'] as const)('places the access-code prompt and styled input beside the parse button in %s', async (placement) => {
  const user = userEvent.setup();
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  const onParse = vi.fn()
    .mockResolvedValueOnce({ shareAccessCodeRequired: { provider: 'quark', message: '此分享需要访问码，请输入后重新解析' } })
    .mockResolvedValueOnce(undefined);
  try {
    render(<NewDownloadDialog open placement={placement} initialInput="https://pan.quark.cn/s/example" onParse={onParse} onClose={vi.fn()} />);
    await user.click(screen.getByRole('button', { name: '开始解析下载' }));
    const code = await screen.findByLabelText('访问码');
    const prompt = screen.getByRole('status');
    const submit = screen.getByRole('button', { name: '开始解析下载' });
    expect(code.parentElement).toBe(submit.parentElement);
    expect(prompt.parentElement).toBe(submit.parentElement);
    expect(getComputedStyle(code.parentElement!).display).toBe('flex');
    expect(getComputedStyle(code.parentElement!).flexWrap).not.toBe('wrap');
    expect(prompt.compareDocumentPosition(code) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(code.compareDocumentPosition(submit) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(getComputedStyle(code).height).toBe('36px');
    expect(getComputedStyle(code).borderRadius).toBe('8px');
    expect(code).toHaveAccessibleDescription('此分享需要访问码，请输入后重新解析');
    expect(code).toHaveFocus();
    await user.type(code, '1234');
    await user.keyboard('{Enter}');
    expect(onParse).toHaveBeenLastCalledWith('https://pan.quark.cn/s/example', '1234');
  } finally {
    stylesheet.remove();
  }
});

it('traps modal Tab navigation and restores focus to its opener when closed', async () => {
  const user = userEvent.setup();
  render(<ModalHost />);

  const opener = screen.getByRole('button', { name: '打开新建下载' });
  await user.click(opener);
  const input = screen.getByLabelText('下载内容');
  expect(input).toHaveFocus();
  await user.type(input, 'https://example.com/file.zip');

  await user.tab({ shift: true });
  expect(screen.getByRole('button', { name: '关闭新建下载' })).toHaveFocus();
  await user.tab({ shift: true });
  expect(screen.getByRole('button', { name: '开始解析下载' })).toHaveFocus();

  await user.click(screen.getByRole('button', { name: '关闭新建下载' }));
  expect(opener).toHaveFocus();
});

it('shows all pasted tasks in one dialog and submits one shared post-processing note', async () => {
  const user = userEvent.setup();
  const onSubmitBatch = vi.fn().mockResolvedValue(undefined);
  const onParse = vi.fn(async (input: string) => ({ confirmation: {
    taskId: input.includes('one') ? 'task-one' : 'task-two',
    title: input.includes('one') ? '目录一' : '目录二',
    requiresConfirmation: true,
    resources: [{ id: `${input}-file`, name: input.includes('one') ? 'one/a.zip' : 'two/b.zip', size: '10 MB', selected: true }],
    processing: []
  } }));
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput={'https://example.test/one\nhttps://example.test/two'}
    onScan={async () => [
      { input: 'https://example.test/one', valid: true, kind: 'direct' },
      { input: 'https://example.test/two', valid: true, kind: 'direct' }
    ]}
    onParse={onParse}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={onSubmitBatch}
    downloadDirectory="D:\\Downloads"
    onClose={vi.fn()}
  />);

  expect(await screen.findByText('目录一')).toBeVisible();
  expect(screen.getByText('目录二')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '展开目录一' }));
  expect(screen.getByText('a.zip')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '解压密码和后处理' }));
  await user.type(screen.getByRole('textbox', { name: '解压密码和后处理' }), '密码 88in\n完成后拼接');
  await user.click(screen.getByRole('button', { name: '新建任务' }));

  expect(onSubmitBatch).toHaveBeenCalledTimes(1);
  expect(onSubmitBatch.mock.calls[0][0]).toHaveLength(2);
  expect(onSubmitBatch.mock.calls[0][3]).toBe('密码 88in\n完成后拼接');
});

it('filters batch resources by category and toggles the visible category selection', async () => {
  const user = userEvent.setup();
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput="magnet:?xt=urn:btih:categorized"
    onScan={async () => [{ input: 'magnet:?xt=urn:btih:categorized', valid: true, kind: 'magnet' }]}
    onParse={async () => ({ confirmation: {
      taskId: 'categorized', title: '分类目录', requiresConfirmation: true, isBitTorrent: true, processing: [],
      resources: [
        { id: 'video', name: '分类目录/movie.mkv', size: '1 GB', selected: true },
        { id: 'image', name: '分类目录/poster.jpg', size: '2 MB', selected: true },
        { id: 'archive', name: '分类目录/source.zip', size: '2 GB', selected: true },
        { id: 'other', name: '分类目录/readme.txt', size: '3 KB', selected: true },
      ],
    } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  await user.click(await screen.findByRole('button', { name: '展开分类目录' }));
  expect(screen.queryByRole('button', { name: '压缩包' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '图片' }));

  expect(screen.getByText('poster.jpg')).toBeVisible();
  expect(screen.queryByText('movie.mkv')).not.toBeInTheDocument();
  expect(screen.queryByText('source.zip')).not.toBeInTheDocument();
  expect(screen.queryByText('readme.txt')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '其他' }));

  expect(screen.getByText('source.zip')).toBeVisible();
  expect(screen.queryByText('movie.mkv')).not.toBeInTheDocument();
  expect(screen.getByText('readme.txt')).toBeVisible();
  await user.click(screen.getByRole('checkbox', { name: '全选当前筛选结果' }));
  expect(screen.getByRole('checkbox', { name: '选择 source.zip' })).not.toBeChecked();
});

it('sorts expanded batch resources by name and size', async () => {
  const user = userEvent.setup();
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput="magnet:?xt=urn:btih:sorted"
    onScan={async () => [{ input: 'magnet:?xt=urn:btih:sorted', valid: true, kind: 'magnet' }]}
    onParse={async () => ({ confirmation: {
      taskId: 'sorted', title: '排序目录', requiresConfirmation: true, isBitTorrent: true, processing: [],
      resources: [
        { id: 'zeta', name: '排序目录/zeta.txt', size: '20 MB', selected: true },
        { id: 'alpha', name: '排序目录/alpha.mp4', size: '3 GB', selected: true },
        { id: 'middle', name: '排序目录/middle.zip', size: '1 KB', selected: true },
      ],
    } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  await user.click(await screen.findByRole('button', { name: '展开排序目录' }));
  expect(screen.queryByRole('group', { name: '文件排序' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '排序文件' }));
  expect(screen.getByRole('menu', { name: '文件排序' })).toBeVisible();
  await user.click(screen.getByRole('menuitemradio', { name: '按文件名称从 A 到 Z' }));
  expect(screen.getAllByRole('checkbox', { name: /选择 (?:alpha|middle|zeta)/ }).map(input => input.getAttribute('aria-label'))).toEqual([
    '选择 alpha.mp4', '选择 middle.zip', '选择 zeta.txt',
  ]);

  expect(screen.queryByRole('menu', { name: '文件排序' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '排序文件' }));
  await user.click(screen.getByRole('menuitemradio', { name: '按文件大小从小到大' }));
  expect(screen.getAllByRole('checkbox', { name: /选择 (?:alpha|middle|zeta)/ }).map(input => input.getAttribute('aria-label'))).toEqual([
    '选择 middle.zip', '选择 zeta.txt', '选择 alpha.mp4',
  ]);
});

it('derives a directory task name when a share parser returns a generic title', async () => {
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput="https://share.example.test/generic-title"
    onScan={async () => [{ input: 'https://share.example.test/generic-title', valid: true, kind: 'cloud' }]}
    onParse={async () => ({ confirmation: {
      taskId: 'generic-title', title: '网盘分享文件', requiresConfirmation: true, processing: [],
      resources: [
        { id: 'photo', name: '旅行相册/photo.jpg', size: '219.9 KB', selected: true },
        { id: 'video', name: '旅行相册/video.mp4', size: '924.2 MB', selected: true },
      ],
    } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  expect(await screen.findByText('旅行相册')).toBeVisible();
  expect(screen.queryByText('网盘分享文件')).not.toBeInTheDocument();
});

it('renders expanded resources as a compact tree table with aligned type and size columns', async () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const user = userEvent.setup();
    render(<NewDownloadDialog
      open
      placement="modal"
      initialInput="https://share.example.test/photo"
      onScan={async () => [{ input: 'https://share.example.test/photo', valid: true, kind: 'cloud' }]}
      onParse={async () => ({ confirmation: {
        taskId: 'photo', title: '相册', requiresConfirmation: true, processing: [],
        resources: [{ id: 'photo', name: '相册/photo.jpg', size: '219.9 KB', selected: true }],
      } })}
      onDiscardPrepared={vi.fn()}
      onSubmitBatch={vi.fn()}
      onClose={vi.fn()}
    />);

    await user.click(await screen.findByRole('button', { name: '展开相册' }));
    expect(screen.getByText('JPG')).toBeVisible();
    expect(screen.getByText('photo.jpg')).toBeVisible();
    expect(screen.queryByText('相册/photo.jpg')).not.toBeInTheDocument();
    expect(screen.getByText('已选 1 / 1 个文件，共 219.9 KB')).toBeVisible();
    expect(getComputedStyle(screen.getByRole('checkbox', { name: '全选当前筛选结果' }).closest('label')!)).toMatchObject({ display: 'flex', marginTop: '0px' });
    const row = screen.getByRole('checkbox', { name: '选择 photo.jpg' }).closest('.resource');
    expect(row).not.toBeNull();
    expect(row).toHaveClass('batch-resource-row');
    expect(within(row!).getByText('jpg')).toHaveClass('resource-extension');
    expect(getComputedStyle(row!).display).toBe('grid');
    expect(getComputedStyle(row!).gridTemplateColumns.split(' ').length).toBe(5);
    expect(parseFloat(getComputedStyle(row!).minHeight)).toBeLessThanOrEqual(36);
    const dialog = screen.getByRole('dialog', { name: '新建下载' });
    expect(getComputedStyle(dialog).width).toContain('720px');
    const expand = screen.getByRole('button', { name: '收起相册' });
    expect(expand.querySelector('svg')).not.toBeNull();
    expect(getComputedStyle(expand)).toMatchObject({ display: 'grid', placeItems: 'center' });
    const size = within(row!).getByText('219.9 KB');
    expect(getComputedStyle(size).justifySelf).toBe('end');
  } finally {
    stylesheet.remove();
  }
});

it('keeps batch task rows at their content height while post-processing is expanded', async () => {
  const stylesheet = document.createElement('style');
  stylesheet.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.appendChild(stylesheet);
  try {
    const user = userEvent.setup();
    const entries = Array.from({ length: 10 }, (_, index) => `https://example.test/file-${index}.zip`);
    render(<NewDownloadDialog
      open
      placement="modal"
      initialInput={entries.join('\n')}
      onScan={async () => entries.map(input => ({ input, valid: true, kind: 'direct' }))}
      onParse={async input => ({ confirmation: {
        taskId: input, title: input.split('/').at(-1), requiresConfirmation: true, processing: [], resources: [],
      } })}
      onDiscardPrepared={vi.fn()}
      onSubmitBatch={vi.fn()}
      onClose={vi.fn()}
    />);

    expect(await screen.findByText('已选 10 个任务')).toBeVisible();
    await user.click(screen.getByRole('button', { name: '解压密码和后处理' }));
    const list = screen.getByRole('region', { name: '待新建任务' });
    expect(getComputedStyle(list).gridAutoRows).toBe('max-content');
    expect(getComputedStyle(list).alignContent).toBe('start');
    expect(getComputedStyle(screen.getByRole('textbox', { name: '解压密码和后处理' })).flexShrink).toBe('0');
  } finally {
    stylesheet.remove();
  }
});

it('shows files from an existing multi-file magnet instead of a fallback single item', async () => {
  const user = userEvent.setup();
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput="magnet:?xt=urn:btih:existing"
    onScan={async () => [{ input: 'magnet:?xt=urn:btih:existing', valid: true, kind: 'magnet' }]}
    onParse={async () => ({ duplicate: {
      task: {
        id: 'existing-magnet', name: '合集', kind: 'magnet', status: 'downloading', progress: 35,
        files: [
          { index: 1, path: '合集/episode-01.mp4', length: 100, completed_length: 35, selected: true },
          { index: 2, path: '合集/episode-02.mp4', length: 200, completed_length: 0, selected: true },
        ],
      },
      state: 'existing_active',
    } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  expect(await screen.findByText('已存在任务 · 2 个文件')).toBeVisible();
  await user.click(screen.getByRole('button', { name: '展开合集' }));
  expect(screen.getByText('episode-01.mp4')).toBeVisible();
  expect(screen.getByText('episode-02.mp4')).toBeVisible();
  expect(screen.queryByText('1 项可下载')).not.toBeInTheDocument();
});

it('keeps a magnet with pending metadata out of the single-item fallback', async () => {
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput="magnet:?xt=urn:btih:pending"
    onScan={async () => [{ input: 'magnet:?xt=urn:btih:pending', valid: true, kind: 'magnet' }]}
    onParse={async () => ({ confirmation: {
      taskId: 'pending-magnet', title: '待获取的合集', requiresConfirmation: true,
      resources: [], processing: [], isBitTorrent: true,
    } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  expect(await screen.findByText('正在获取文件信息')).toBeVisible();
  expect(screen.queryByText('1 项可下载')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: '新建任务' })).toBeDisabled();
});

it('refreshes a pending magnet into its actual file list', async () => {
  vi.mocked(isTauriRuntime).mockReturnValue(true);
  vi.mocked(listPersistedTasks).mockResolvedValue([{
    id: 'pending-magnet', name: '待获取的合集', kind: 'magnet', status: 'waiting', progress: 0,
    files: [
      { index: 1, path: '待获取的合集/episode-01.mp4', length: 100, completed_length: 0, selected: true },
      { index: 2, path: '待获取的合集/episode-02.mp4', length: 200, completed_length: 0, selected: true },
    ],
  }]);
  const user = userEvent.setup();
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput="magnet:?xt=urn:btih:pending"
    onScan={async () => [{ input: 'magnet:?xt=urn:btih:pending', valid: true, kind: 'magnet' }]}
    onParse={async () => ({ confirmation: {
      taskId: 'pending-magnet', title: '待获取的合集', requiresConfirmation: true,
      resources: [], processing: [], isBitTorrent: true,
    } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  expect(await screen.findByText('2 项可下载')).toBeVisible();
  expect(screen.getByRole('button', { name: '新建任务' })).toBeEnabled();
  await user.click(screen.getByRole('button', { name: '展开待获取的合集' }));
  expect(screen.getByText('episode-01.mp4')).toBeVisible();
  expect(screen.getByText('episode-02.mp4')).toBeVisible();
});

it('allows a pending task to be deselected so ready tasks can be submitted', async () => {
  const user = userEvent.setup();
  let resolveSecond!: (value: unknown) => void;
  const onParse = vi.fn()
    .mockResolvedValueOnce({ confirmation: { taskId: 'ready', title: '已就绪', requiresConfirmation: true, resources: [], processing: [] } })
    .mockReturnValueOnce(new Promise(resolve => { resolveSecond = resolve; }));
  render(<NewDownloadDialog
    open
    placement="modal"
    initialInput={'https://example.test/ready\nhttps://example.test/pending'}
    onScan={async () => [
      { input: 'https://example.test/ready', valid: true, kind: 'direct' },
      { input: 'https://example.test/pending', valid: true, kind: 'direct' }
    ]}
    onParse={onParse}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  expect(await screen.findByText('已就绪')).toBeVisible();
  expect(screen.getByRole('button', { name: '新建任务' })).toBeDisabled();
  await user.click(screen.getByRole('checkbox', { name: '选择 https://example.test/pending' }));
  expect(screen.getByRole('button', { name: '新建任务' })).toBeEnabled();
  await act(async () => {
    resolveSecond!({ confirmation: { taskId: 'pending', title: '稍后完成', requiresConfirmation: true, resources: [], processing: [] } });
  });
});

it('keeps the explicit empty new-task command usable inside the unified dialog', async () => {
  const user = userEvent.setup();
  const onScan = vi.fn(async (input: string) => [{ input, valid: true as const, kind: 'direct' as const }]);
  render(<NewDownloadDialog
    open
    placement="modal"
    onScan={onScan}
    onParse={async input => ({ confirmation: { taskId: 'manual', title: 'manual.zip', requiresConfirmation: true, resources: [], processing: [] } })}
    onDiscardPrepared={vi.fn()}
    onSubmitBatch={vi.fn()}
    onClose={vi.fn()}
  />);

  await user.type(screen.getByRole('textbox', { name: '下载内容' }), 'https://example.test/manual.zip');
  await user.click(screen.getByRole('button', { name: '解析任务' }));

  expect(await screen.findByText('manual.zip')).toBeVisible();
  expect(onScan).toHaveBeenCalledWith('https://example.test/manual.zip');
});
