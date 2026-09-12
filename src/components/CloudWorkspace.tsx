import { useEffect, useState } from 'react';
import type { CloudAccount, CloudProvider } from '../lib/cloudTypes';
import { CloudManager, CloudManagerClient } from './CloudManager';

export interface CloudWorkspaceProps {
  client: CloudManagerClient;
  onDownloadTask?(task: unknown): void;
  privacyDownloadEnabled?: boolean;
}

type WorkspaceTab = 'management' | CloudProvider;

const providers: readonly CloudProvider[] = ['quark', 'pikpak', 'baidu', 'tianyi', 'aliyun', 'xunlei', 'uc'];
const providerLabel: Record<CloudProvider, string> = { quark: '夸克', pikpak: 'PikPak', baidu: '百度', tianyi: '天翼', aliyun: '阿里', xunlei: '迅雷', uc: 'UC' };

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
  const [recoveringAccounts, setRecoveringAccounts] = useState(true);
  const [recoveryError, setRecoveryError] = useState<string | null>(null);
  const activeAccount = activeTab === 'management' ? null : accounts.find((account) => account.provider === activeTab) ?? null;

  function acceptAccount(account: CloudAccount) {
    setAccounts((current) => current.some((item) => item.provider === account.provider)
      ? current.map((item) => item.provider === account.provider ? account : item)
      : [...current, account]);
  }

  useEffect(() => {
    let active = true;
    setRecoveringAccounts(true);
    setRecoveryError(null);
    void Promise.all(providers.map(async (provider) => {
      try {
        const account = await client.getCloudAccount(provider);
        return account.provider === provider ? account : null;
      }
      catch (error) {
        if (active && !isUnauthorized(error)) setRecoveryError(errorMessage(error, '无法恢复网盘会话。'));
        return null;
      }
    })).then((recovered) => {
      if (!active) return;
      setAccounts(recovered.filter((account): account is CloudAccount => account !== null));
      setRecoveringAccounts(false);
    });
    return () => { active = false; };
  }, [client]);

  function removeAccount(provider: CloudProvider) {
    setAccounts((current) => current.filter((account) => account.provider !== provider));
    setActiveTab((current) => current === provider ? 'management' : current);
  }

  if (recoveringAccounts) {
    return <section className="cloud-workspace cloud-workspace-recovering" aria-label="我的网盘工作区">
      <div className="cloud-account-recovery" role="status" aria-label="正在读取网盘账号状态">
        <i aria-hidden="true" />
        <span>正在读取网盘账号状态…</span>
      </div>
    </section>;
  }

  return <section className="cloud-workspace" aria-label="我的网盘工作区">
    <div className="cloud-workspace-tabs workspace-tab-list" role="tablist" aria-label="我的网盘标签">
      <button role="tab" aria-selected={activeTab === 'management'} onClick={() => setActiveTab('management')}>网盘管理</button>
      {accounts.map((account) => <button key={account.provider} role="tab" aria-selected={activeTab === account.provider} onClick={() => setActiveTab(account.provider)}>{providerLabel[account.provider]}网盘</button>)}
    </div>

    {activeTab === 'management' && <section className="cloud-management-panel" aria-label="网盘管理">
      {recoveryError && <p className="cloud-operation-status error" role="alert">恢复账号失败：{recoveryError}</p>}
      <div className="cloud-provider-auths">{providers.map((provider) => {
        const account = accounts.find((item) => item.provider === provider);
        return <CloudManager
          key={`${provider}:${account?.id ?? 'unauthorized'}`}
          provider={provider}
          client={client}
          initialAccounts={account ? [account] : []}
          privacyDownloadEnabled={privacyDownloadEnabled}
          authorizationOnly
          restoreOnMount={false}
          showAccountTabs={false}
          onAccountAuthorized={acceptAccount}
          onAccountLoggedOut={removeAccount}
        />;
      })}</div>
    </section>}

    {activeAccount && <CloudManager
      key={`files:${activeAccount.id}`}
      provider={activeAccount.provider}
      client={client}
      initialAccounts={[activeAccount]}
      privacyDownloadEnabled={privacyDownloadEnabled}
      restoreOnMount={false}
      showAccountTabs={false}
      showSessionActions={false}
      onDownloadTask={onDownloadTask}
      onAccountAuthorized={acceptAccount}
      onAccountLoggedOut={removeAccount}
    />}
  </section>;
}
