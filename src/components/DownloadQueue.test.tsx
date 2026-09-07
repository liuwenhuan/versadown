import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DownloadQueue } from './DownloadQueue';

const folderTask = {
  id: 'folder-1', name: 'Season 1', kind: 'folder', status: 'downloading', progress: 42,
  children: [{ id: 'file-1', name: 'episode-01.mkv', kind: 'file', status: 'downloading', progress: 50 }]
} as const;

it('keeps a folder as one row until the user expands it', async () => {
  const user = userEvent.setup();
  render(<DownloadQueue tasks={[folderTask]} selectedTaskId={null} onSelect={() => {}} onAction={() => {}} />);
  expect(screen.queryByText('episode-01.mkv')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
  expect(screen.getByText('episode-01.mkv')).toBeVisible();
});
