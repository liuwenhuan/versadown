import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { TaskDetails } from './TaskDetails';

it('renders only the task name, source, file list, and post-processing information', () => {
  render(<TaskDetails task={{
    id: 'task-1', name: 'archive.iso', kind: 'file', status: 'failed', progress: 25,
    aria_gid: 'a1b2c3', total_length: 4_194_304, completed_length: 1_048_576,
    download_speed: 524_288, connections: 4, eta_seconds: 6, aria_error_code: '16', source: 'https://example.com/archive.iso',
    error: '磁盘空间不足', post_processing_note: '解压密码：88in', files: [{ index: 1, path: 'archive.iso', length: 4_194_304, completed_length: 1_048_576, selected: true }]
  }} />);

  expect(screen.getByRole('heading', { name: 'archive.iso' })).toBeVisible();
  expect(screen.getByText('https://example.com/archive.iso')).toBeVisible();
  expect(screen.getByRole('region', { name: '文件信息' })).toBeVisible();
  expect(screen.getByText('解压密码：88in')).toBeVisible();
  expect(screen.queryByText('状态')).not.toBeInTheDocument();
  expect(screen.queryByText('传输进度')).not.toBeInTheDocument();
  expect(screen.queryByText('安全处理')).not.toBeInTheDocument();
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

it('keeps file list and post-processing content mutually expanded', async () => {
  const user = userEvent.setup();
  render(<TaskDetails task={{
    id: 'task-note', name: 'archive.zip', kind: 'file', status: 'completed', progress: 100, files: [{ index: 1, path: 'archive.zip', length: 8, completed_length: 8, selected: true }],
    post_processing_note: '解压密码：88in\n完成后拼接分卷并改名'
  }} />);

  expect(screen.getByRole('region', { name: '文件信息' })).toBeVisible();
  expect(screen.getByRole('button', { name: '展开后处理信息' })).toBeVisible();
  await user.click(screen.getByRole('button', { name: '展开后处理信息' }));
  expect(screen.getByText(/解压密码：88in\s+完成后拼接分卷并改名/)).toBeVisible();
  expect(screen.queryByRole('region', { name: '文件信息' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开文件信息' }));
  expect(screen.getByRole('region', { name: '文件信息' })).toBeVisible();
  expect(screen.getByRole('button', { name: '展开后处理信息' })).toBeVisible();
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

it('only provides expand controls for actual directories', () => {
  render(<TaskDetails task={{
    id: 'task-cloud-tree', name: '倩种1987', kind: 'folder', status: 'completed', progress: 100,
    children: [
      {
        id: 'video', name: '씨받이[倩种].The.Surrogate.Woman.1986.web-1080P.mp4', kind: 'file', status: 'completed', progress: 100,
        children: [],
      },
      {
        id: 'subtitles', name: '字幕', kind: 'folder', status: 'completed', progress: 100,
        children: [{ id: 'subtitle', name: '简体中字.srt', kind: 'file', status: 'completed', progress: 100, children: [] }],
      },
    ],
  }} />);

  expect(screen.queryByRole('button', { name: '展开 씨받이[倩种].The.Surrogate.Woman.1986.web-1080P.mp4' })).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: '展开 字幕' })).toBeVisible();
});
