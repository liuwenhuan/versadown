import { useState } from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { readFileSync } from 'node:fs';
import { vi } from 'vitest';
import { NewDownloadDialog } from './NewDownloadDialog';

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
  '天翼云盘分享不可用，请检查访问码、有效期及权限',
  '百度分享无效或请求被拒绝',
  '分享令牌已过期，请重新获取',
  '迅雷分享已失效或无权访问',
  'PikPak 分享已失效或无权访问',
  '夸克分享无效或已过期',
])('keeps retry available for recoverable or ambiguous failures: %s', async (message) => {
  const user = userEvent.setup();
  render(<NewDownloadDialog open initialInput="https://example.test/share" onParse={() => Promise.reject(new Error(message))} onClose={vi.fn()} />);
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(await screen.findByRole('alert')).toHaveTextContent(message);
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeEnabled();
});

it('clears an unavailable-share failure when the user changes the link', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn().mockRejectedValueOnce(new Error('PikPak 分享已删除')).mockResolvedValueOnce(undefined);
  render(<NewDownloadDialog open initialInput="https://example.test/old" onParse={onParse} onClose={vi.fn()} />);
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
