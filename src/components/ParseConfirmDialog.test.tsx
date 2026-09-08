import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ParseConfirmDialog } from './ParseConfirmDialog';

const result = {
  taskId: 'task-1', requiresConfirmation: true,
  resources: [
    { id: 'a', name: 'part-01.zip', size: '1.2 GB', selected: true },
    { id: 'b', name: 'part-02.zip', size: '1.2 GB', selected: true }
  ],
  processing: ['解压分卷并合并'],
  discoveredPassword: 'pass-123'
};

it('keeps discovered password masked until the user reveals it', async () => {
  const user = userEvent.setup();
  render(<ParseConfirmDialog result={result} onConfirm={() => {}} onSelectionChange={() => {}} onClose={() => {}} />);
  expect(screen.queryByText('pass-123')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: /查看后处理与密码/ }));
  expect(screen.queryByText('pass-123')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '显示密码' }));
  expect(screen.getByText('pass-123')).toBeVisible();
});

it('labels BitTorrent confirmation choices as select-file semantics', () => {
  render(<ParseConfirmDialog result={{ ...result, isBitTorrent: true }} onConfirm={() => {}} onSelectionChange={() => {}} onClose={() => {}} />);
  expect(screen.getByRole('heading', { name: '选择要下载的文件' })).toBeVisible();
  expect(screen.getByRole('button', { name: '下载已选 2 项' })).toBeVisible();
  expect(screen.getByText(/将在开始前选择这些 BT 文件/)).toBeVisible();
});
