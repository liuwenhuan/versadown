import { render, screen, within } from '@testing-library/react';
import { AboutPanel } from './AboutPanel';

it('shows the ten declaration terms after usage instructions without the former notice box', () => {
  render(<AboutPanel icon="/icon.png" />);
  const help = screen.getByRole('region', { name: '使用说明' });
  const declaration = screen.getByRole('region', { name: '声明' });
  expect(help.compareDocumentPosition(declaration) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(within(declaration).getAllByRole('listitem')).toHaveLength(10);
  expect(within(declaration).getByText('下载或使用本项目，即视为您已阅读并接受上述全部条款。')).toBeVisible();
  expect(screen.queryByRole('complementary', { name: '使用提醒' })).not.toBeInTheDocument();
});
