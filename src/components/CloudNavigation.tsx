import type { ReactNode } from 'react';
import type { ProviderDescriptor } from '../lib/cloudTypes';
import type { CloudWorkspacePage } from './CloudWorkspace';
import { useTranslation } from '../i18n';
import { cloudProviderLabel } from '../i18n/cloudProviderLabel';

interface CloudNavigationProps {
  providers: readonly ProviderDescriptor[];
  activePage: CloudWorkspacePage | null;
  onSelect(page: CloudWorkspacePage): void;
  icon: ReactNode;
}

export function CloudNavigation({ providers, activePage, onSelect, icon }: CloudNavigationProps) {
  const { t } = useTranslation();
  return <>
    <button type="button" className={activePage === 'management' ? 'nav-active' : ''} aria-current={activePage === 'management' ? 'page' : undefined} onClick={() => onSelect('management')}>
      <span className="nav-icon" aria-hidden="true">{icon}</span><span>{t('cloud.myDrives')}</span>
    </button>
    {providers.length > 0 && <div className="cloud-provider-navigation" role="group" aria-label={t('cloud.authorizedDrives')}>
      {providers.map(provider => <button type="button" key={provider.provider} className={activePage === provider.provider ? 'nav-active' : ''} aria-current={activePage === provider.provider ? 'page' : undefined} onClick={() => onSelect(provider.provider)}>
        <span>{t('cloud.providerDrive', { name: cloudProviderLabel(provider.provider, t) })}</span>
      </button>)}
    </div>}
  </>;
}
