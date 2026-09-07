import { render, screen } from '@testing-library/react';
import { App } from './App';

it('renders the AllDownloader shell', () => {
  render(<App />);
  expect(screen.getByRole('heading', { name: '下载管理' })).toBeVisible();
});
