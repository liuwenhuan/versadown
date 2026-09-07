import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { vi } from 'vitest';
import { SettingsPanel } from './SettingsPanel';

it('keeps detailed report collection off by default', () => {
  render(<SettingsPanel initial={{ debugReports: false }} onChange={() => {}} />);
  expect(screen.getByRole('checkbox', { name: '启用调试报告' })).not.toBeChecked();
});

it('allows users to explicitly enable debug reports in beta', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<SettingsPanel initial={{ debugReports: false }} onChange={onChange} />);
  await user.click(screen.getByRole('checkbox', { name: '启用调试报告' }));
  expect(onChange).toHaveBeenCalledWith(true);
});
