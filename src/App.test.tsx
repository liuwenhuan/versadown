import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from './App';

it('renders the AllDownloader shell', () => {
  render(<App />);
  expect(screen.getByRole('heading', { name: '下载管理' })).toBeVisible();
});

it('starts a standard direct link immediately from the floating input', async () => {
  const user = userEvent.setup();
  render(<App />);
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'https://example.com/movie.mp4');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(await screen.findByRole('button', { name: '暂停 movie.mp4' })).toBeVisible();
  expect(screen.queryByRole('dialog', { name: '确认下载内容' })).not.toBeInTheDocument();
});

it('asks for file selection before starting a magnet task', async () => {
  const user = userEvent.setup();
  render(<App />);
  await user.click(screen.getByRole('button', { name: '打开下载输入' }));
  await user.type(screen.getByLabelText('下载内容'), 'magnet:?xt=urn:btih:abc');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(await screen.findByRole('dialog', { name: '确认下载内容' })).toBeVisible();
});
