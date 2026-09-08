import { render, screen } from '@testing-library/react';
import { App } from './App';

it('renders the AllDownloader shell', () => {
  render(<App />);
  expect(screen.getByRole('heading', { name: '下载管理' })).toBeVisible();
});

it('leaves quick input to the independent native launcher window', () => {
  render(<App />);
  expect(screen.queryByRole('button', { name: '打开下载输入' })).not.toBeInTheDocument();
  expect(screen.getByRole('region', { name: '下载队列' })).toBeVisible();
});
