import { render, screen } from '@testing-library/react';
import { vi } from 'vitest';
import { CloudWorkspace } from './CloudWorkspace';
import { cloudCatalog } from '../test/cloudCatalog';

it('shows cloud management without the former content tab bar', async () => {
  const client = {
    getCloudProviderCatalog: vi.fn().mockResolvedValue(cloudCatalog),
    getCloudAccount: vi.fn().mockRejectedValue({ code: 'unauthorized' }),
    getCloudAuthorizationCapabilities: vi.fn().mockImplementation((provider: string) => Promise.resolve({ provider, web: true, password: false, passwordLoginMode: 'none' }))
  };

  render(<CloudWorkspace client={client as never} activePage="management" onPageChange={vi.fn()} onProvidersChange={vi.fn()} />);

  expect(await screen.findByRole('region', { name: '夸克账号授权' })).toBeVisible();
  expect(screen.queryByRole('tablist', { name: '我的网盘标签' })).not.toBeInTheDocument();
});
