import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { FloatingLauncher } from './FloatingLauncher';

it('expands the 48px launcher and labels its action 开始解析下载', async () => {
  const user = userEvent.setup();
  render(<FloatingLauncher onParse={vi.fn()} activeCount={0} />);
  const trigger = screen.getByRole('button', { name: '打开下载输入' });
  expect(trigger).toHaveAttribute('data-size', '48');
  await user.click(trigger);
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeVisible();
});

it('submits a pasted input to the parser', async () => {
  const user = userEvent.setup();
  const onParse = vi.fn();
  render(<FloatingLauncher onParse={onParse} activeCount={1} />);
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'https://example.com/file.zip');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(onParse).toHaveBeenCalledWith('https://example.com/file.zip');
});
