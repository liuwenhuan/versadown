import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { WindowTitleBar } from './WindowTitleBar';

it('renders a drag region and hides through the close button', async () => {
  const user = userEvent.setup();
  const onAction = vi.fn();

  render(<WindowTitleBar title="下载管理" maximized={false} onAction={onAction} />);

  expect(screen.getByTestId('titlebar-drag-region')).toHaveAttribute('data-tauri-drag-region');
  expect(screen.getByText('ALLDOWNLOADER')).toHaveAttribute('data-tauri-drag-region');
  expect(screen.getByText('下载管理')).toHaveAttribute('data-tauri-drag-region');
  await user.click(screen.getByRole('button', { name: '关闭下载管理器' }));
  expect(onAction).toHaveBeenCalledWith('hide');
});

it('routes minimize and maximize actions while controls remain outside the drag region', async () => {
  const user = userEvent.setup();
  const onAction = vi.fn();

  render(<WindowTitleBar title="下载管理" maximized={false} onAction={onAction} />);

  const dragRegion = screen.getByTestId('titlebar-drag-region');
  expect(dragRegion).not.toContainElement(screen.getByRole('button', { name: '最小化下载管理器' }));
  expect(dragRegion).not.toContainElement(screen.getByRole('button', { name: '最大化下载管理器' }));
  await user.click(screen.getByRole('button', { name: '最小化下载管理器' }));
  await user.click(screen.getByRole('button', { name: '最大化下载管理器' }));
  expect(onAction).toHaveBeenNthCalledWith(1, 'minimize');
  expect(onAction).toHaveBeenNthCalledWith(2, 'toggle-maximize');
});

it('uses the restore label while the window is maximized', async () => {
  render(<WindowTitleBar title="下载管理" maximized onAction={vi.fn()} />);

  expect(screen.getByRole('button', { name: '还原下载管理器' })).toBeVisible();
});

it('toggles maximize when the user double-clicks the custom title drag region', async () => {
  const user = userEvent.setup();
  const onAction = vi.fn();

  render(<WindowTitleBar title="下载管理" maximized={false} onAction={onAction} />);
  await user.dblClick(screen.getByTestId('titlebar-drag-region'));

  expect(onAction).toHaveBeenCalledOnce();
  expect(onAction).toHaveBeenCalledWith('toggle-maximize');
});
