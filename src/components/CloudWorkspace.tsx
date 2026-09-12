import { useEffect, useState } from 'react';
import type { CloudAccount, CloudProvider, ProviderDescriptor } from '../lib/cloudTypes';
import { CloudManager, CloudManagerClient } from './CloudManager';

export interface CloudWorkspaceProps {
  client: CloudManagerClient;
  onDownloadTask?(task: unknown): void;
  privacyDownloadEnabled?: boolean;
}

type WorkspaceTab = 'management' | CloudProvider;


function isUnauthorized(error: unknown) {
  return typeof error === 'object' && error !== null && 'code' in error && ['unauthorized', 'not_authorized'].includes(String((error as { code?: unknown }).code));
}

function errorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'object' && error !== null && 'message' in error && typeof (error as { message?: unknown }).message === 'string') {
    return (error as { message: string }).message;
  }
  return fallback;
}

export function CloudWorkspace({ client, onDownloadTask, privacyDownloadEnabled = false }: CloudWorkspaceProps) {
  const [accounts, setAccounts] = useState<readonly CloudAccount[]>([]);
  const [activeTab, setActiveTab] = useState<WorkspaceTab>('management');
  const [catalog, setCatalog] = useState<readonly ProviderDescriptor[]>([]);
  const [pendingProviders, setPendingProviders] = useState<readonly CloudProvider[]>([]);
  const [loadingCatalog, setLoadingCatalog] = useState(true);
  const recoveringAccounts = loadingCatalog || pendingProviders.length > 0;
  const [recoveryError, setRecoveryError] = useState<string | null>(null);
  const [accountErrors, setAccountErrors] = useState<Partial<Record<CloudProvider, string>>>({});
  const activeAccount = activeTab === 'management' ? null : accounts.find((account) => account.provider === activeTab) ?? null;

  function acceptAccount(account: CloudAccount) {
    setAccountErrors(current => {
      const next = { ...current };
      delete next[account.provider];
      return next;
    });
    setAccounts((current) => current.some((item) => item.provider === account.provider)
      ? current.map((item) => item.provider === account.provider ? account : item)
      : [...current, account]);
  }

  useEffect(() => {
    let active = true;
    setLoadingCatalog(true);
    setRecoveryError(null);
    setAccountErrors({});
    setAccounts([]);
    setCatalog([]);
    setPendingProviders([]);
    setActiveTab('management');
    void client.getCloudProviderCatalog().then((descriptors) => {
      if (!active) return;
      setCatalog(descriptors);
      setPendingProviders(descriptors.map(d => d.provider));
      setLoadingCatalog(false);
      for (const { provider } of descriptors) {
        void client.getCloudAccount(provider).then(account => {
          if (active && account.provider === provider) acceptAccount(account);
        }).catch(error => {
          if (active && !isUnauthorized(error)) setAccountErrors(current => ({ ...current, [provider]: errorMessage(error, '无法恢复网盘会话。') }));
        }).finally(() => {
          if (active) setPendingProviders(current => current.filter(item => item !== provider));
        });
      }
    }).catch(error => {
      if (active) {
        setLoadingCatalog(false);
        setRecoveryError(errorMessage(error, '无法读取网盘目录。'));
      }
    });
    return () => { active = false; };
  }, [client]);

  function removeAccount(provider: CloudProvider) {
    setAccounts((current) => current.filter((account) => account.provider !== provider));
    setActiveTab((current) => current === provider ? 'management' : current);
  }

  return <section className="cloud-workspace" aria-label="我的网盘工作区">
    <div className="cloud-workspace-tabs workspace-tab-list" role="tablist" aria-label="我的网盘标签">
      <button role="tab" aria-selected={activeTab === 'management'} onClick={() => setActiveTab('management')}>网盘管理</button>
      {catalog.filter(d => accounts.some(a => a.provider === d.provider)).map(d => <button key={d.provider} role="tab" aria-selected={activeTab === d.provider} onClick={() => setActiveTab(d.provider)}>{d.displayName}网盘</button>)}
    </div>

    {activeTab === 'management' && <section className="cloud-management-panel" aria-label="网盘管理">
      {recoveringAccounts && <div className="cloud-account-recovery" role="status" aria-label="正在读取网盘账号状态"><i aria-hidden="true" /><span>正在读取网盘账号状态…</span></div>}
      {recoveryError && <p className="cloud-operation-status error" role="alert">恢复账号失败：{recoveryError}</p>}
      {catalog.filter(d => accountErrors[d.provider]).map(d => <p key={d.provider} className="cloud-operation-status error" role="alert">{d.displayName}恢复账号失败：{accountErrors[d.provider]}</p>)}
      <div className="cloud-provider-auths">{catalog.map((descriptor) => {
        const { provider, displayName } = descriptor;
        if (pendingProviders.includes(provider)) return <section key={provider} className={`cloud-manager cloud-authorization cloud-provider-${provider}`} aria-label={`${displayName}账号恢复`}><header className="cloud-heading"><h2>{displayName}网盘</h2></header><p role="status">正在读取账号状态…</p></section>;
        const account = accounts.find((item) => item.provider === provider);
        return <CloudManager
          key={`${provider}:${account?.id ?? 'unauthorized'}`}
          descriptor={descriptor}
          provider={provider}
          client={client}
          initialAccount={account ?? null}
          privacyDownloadEnabled={privacyDownloadEnabled}
          authorizationOnly
          restoreOnMount={false}
          onAccountAuthorized={acceptAccount}
          onAccountLoggedOut={removeAccount}
        />;
      })}</div>
    </section>}

    {activeAccount && <CloudManager
      key={`files:${activeAccount.provider}:${activeAccount.id}`}
      descriptor={catalog.find(d => d.provider === activeAccount.provider)!}
      provider={activeAccount.provider}
      client={client}
      initialAccount={activeAccount}
      privacyDownloadEnabled={privacyDownloadEnabled}
      restoreOnMount={false}
      showSessionActions={false}
      onDownloadTask={onDownloadTask}
      onAccountAuthorized={acceptAccount}
      onAccountLoggedOut={removeAccount}
    />}
  </section>;
}
