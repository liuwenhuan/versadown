import { render, screen } from '@testing-library/react';
import { vi } from 'vitest';
import { CloudWorkspace } from './CloudWorkspace';

it('marks cloud workspace tabs for the shared top-level tab layout', async () => {
  const client = {
    getCloudAccount: vi.fn().mockRejectedValue({ code: 'unauthorized' }),
    getCloudAuthorizationCapabilities: vi.fn().mockResolvedValue({ web: true, password: false })
  };

  render(<CloudWorkspace client={client as never} />);

  expect(await screen.findByRole('tablist', { name: '我的网盘标签' })).toHaveClass('workspace-tab-list');
});
