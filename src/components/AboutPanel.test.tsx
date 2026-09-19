import { render, screen, within } from '@testing-library/react';
import { AboutPanel } from './AboutPanel';

it('shows project acknowledgements and the ten declaration terms after usage instructions', () => {
  render(<AboutPanel icon="/icon.png" />);
  const help = screen.getByRole('region', { name: '使用说明' });
  const thanks = screen.getByRole('region', { name: '感谢' });
  const declaration = screen.getByRole('region', { name: '声明' });
  expect(help.compareDocumentPosition(thanks) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(thanks.compareDocumentPosition(declaration) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(within(thanks).getByRole('link', { name: 'aria2' })).toHaveAttribute('href', 'https://aria2.github.io/');
  expect(within(thanks).getByRole('link', { name: 'aria2-next' })).toHaveAttribute('href', 'https://github.com/AnInsomniacy/aria2-next');
  expect(within(thanks).getByRole('link', { name: 'Lucide' })).toHaveAttribute('href', 'https://lucide.dev/');
  expect(within(declaration).getAllByRole('listitem')).toHaveLength(10);
  expect(within(declaration).getByText('下载或使用本项目，即视为您已阅读并接受上述全部条款。')).toBeVisible();
  expect(screen.queryByRole('complementary', { name: '使用提醒' })).not.toBeInTheDocument();
});
