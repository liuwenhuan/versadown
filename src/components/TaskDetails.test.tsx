import { render, screen } from '@testing-library/react';
import { TaskDetails } from './TaskDetails';

it('renders real aria2 transfer fields including GID, bytes, ETA, connections, and error code', () => {
  render(<TaskDetails task={{
    id: 'task-1', name: 'archive.iso', kind: 'file', status: 'failed', progress: 25,
    aria_gid: 'a1b2c3', total_length: 4_194_304, completed_length: 1_048_576,
    download_speed: 524_288, connections: 4, eta_seconds: 6, aria_error_code: '16',
    error: '磁盘空间不足', files: [{ index: 1, path: 'archive.iso', length: 4_194_304, completed_length: 1_048_576, selected: true }]
  }} />);

  expect(screen.getByText('a1b2c3')).toBeVisible();
  expect(screen.getAllByText(/1 MB \/ 4 MB/)[0]).toBeVisible();
  expect(screen.getByText('512 KB/s')).toBeVisible();
  expect(screen.getByText('4')).toBeVisible();
  expect(screen.getByText('6 秒')).toBeVisible();
  expect(screen.getByText('16')).toBeVisible();
  expect(screen.getAllByText('archive.iso')).toHaveLength(2);
});

it('shows downloaded bytes instead of a synthetic percent for an unknown total', () => {
  render(<TaskDetails task={{
    id: 'task-2', name: 'stream', kind: 'file', status: 'downloading', progress: 0,
    completed_length: 2_048, download_speed: 0
  }} />);

  expect(screen.getByText('2 KB')).toBeVisible();
  expect(screen.queryByText('0%')).not.toBeInTheDocument();
});

it('shows a persisted extraction password and post-processing note with its line breaks', () => {
  render(<TaskDetails task={{
    id: 'task-note', name: 'archive.zip', kind: 'file', status: 'completed', progress: 100,
    post_processing_note: '解压密码：88in\n完成后拼接分卷并改名'
  }} />);

  expect(screen.getByText('解压密码和后处理')).toBeVisible();
  expect(screen.getByText(/解压密码：88in\s+完成后拼接分卷并改名/)).toBeVisible();
});
