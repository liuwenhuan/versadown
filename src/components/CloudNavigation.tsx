import type { ReactNode } from 'react';
import type { ProviderDescriptor } from '../lib/cloudTypes';
import type { CloudWorkspacePage } from './CloudWorkspace';

interface CloudNavigationProps {
  providers: readonly ProviderDescriptor[];
  activePage: CloudWorkspacePage | null;
  onSelect(page: CloudWorkspacePage): void;
  icon: ReactNode;
}

export function CloudNavigation({ providers, activePage, onSelect, icon }: CloudNavigationProps) {
  return <>
    <button type="button" className={activePage === 'management' ? 'nav-active' : ''} aria-current={activePage === 'management' ? 'page' : undefined} onClick={() => onSelect('management')}>
      <span className="nav-icon" aria-hidden="true">{icon}</span><span>我的网盘</span>
    </button>
    {providers.length > 0 && <div className="cloud-provider-navigation" role="group" aria-label="已授权网盘">
      {providers.map(provider => <button type="button" key={provider.provider} className={activePage === provider.provider ? 'nav-active' : ''} aria-current={activePage === provider.provider ? 'page' : undefined} onClick={() => onSelect(provider.provider)}>
        <span>{provider.displayName}网盘</span>
      </button>)}
    </div>}
  </>;
}
