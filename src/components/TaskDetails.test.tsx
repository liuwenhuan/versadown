import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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

it('renders the source before file information without a duplicate task-name label', () => {
  render(<TaskDetails task={{
    id: 'task-source', name: 'collection', source: 'magnet:?xt=urn:btih:collection', kind: 'magnet', status: 'downloading', progress: 10,
    files: [{ index: 1, path: 'collection/video.mkv', length: 1_024, completed_length: 0, selected: true }]
  }} />);

  const source = screen.getByText('magnet:?xt=urn:btih:collection');
  const sourceSection = source.closest('.task-detail-source');
  const files = screen.getByRole('region', { name: '文件信息' });
  expect(screen.getByRole('heading', { name: 'collection' })).toBeVisible();
  expect(screen.queryByText('任务名称')).not.toBeInTheDocument();
  expect(sourceSection).not.toBeNull();
  expect(sourceSection!.compareDocumentPosition(files) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
});

it('shows a persisted extraction password and post-processing note with its line breaks', () => {
  render(<TaskDetails task={{
    id: 'task-note', name: 'archive.zip', kind: 'file', status: 'completed', progress: 100,
    post_processing_note: '解压密码：88in\n完成后拼接分卷并改名'
  }} />);

  expect(screen.getByText('解压密码和后处理')).toBeVisible();
  expect(screen.getByText(/解压密码：88in\s+完成后拼接分卷并改名/)).toBeVisible();
});

it('keeps nested detail files hidden until their folder is expanded', async () => {
  const user = userEvent.setup();
  render(<TaskDetails task={{
    id: 'task-tree', name: 'release', kind: 'torrent', status: 'downloading', progress: 0,
    files: [
      { index: 1, path: 'release/_crack/Win64/patch.dll', length: 8, completed_length: 4, selected: true },
      { index: 2, path: 'release/readme.txt', length: 2, completed_length: 2, selected: true },
    ],
  }} />);

  expect(screen.getByText('_crack')).toBeVisible();
  expect(screen.queryByText('Win64')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开 _crack' }));
  expect(screen.getByText('Win64')).toBeVisible();
});
