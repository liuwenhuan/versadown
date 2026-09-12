//! Durable authorization policy. Session secrets remain in the credential store.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthHealth {
    Disconnected,
    Restoring,
    Ready,
    Refreshing,
    NeedsSilentRelogin,
    NeedsInteractiveAuth,
    RiskBlocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthMaintenanceRecord {
    pub schema_version: u32,
    pub provider: CloudProvider,
    pub account_id: Option<String>,
    pub auth_method: Option<CloudAuthMethod>,
    pub session_revision: u64,
    pub expires_at: Option<i64>,
    pub next_refresh_at: Option<i64>,
    pub consecutive_failures: u32,
    pub next_allowed_attempt_at: Option<i64>,
    pub health: AuthHealth,
}

pub struct AuthSupervisor {
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    jitter: Box<dyn Fn() -> i64 + Send + Sync>,
    locks: HashMap<CloudProvider, tokio::sync::Mutex<()>>,
}
impl Default for AuthSupervisor {
    fn default() -> Self {
        Self::new(
            || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            },
            || 30 + (uuid::Uuid::new_v4().as_u128() % 91) as i64,
        )
    }
}
impl AuthSupervisor {
    pub fn new(
        clock: impl Fn() -> i64 + Send + Sync + 'static,
        jitter: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            clock: Box::new(clock),
            jitter: Box::new(jitter),
            locks: ProviderRegistry::global()
                .all()
                .iter()
                .map(|d| (d.provider, tokio::sync::Mutex::new(())))
                .collect(),
        }
    }
    pub fn with_provider<T>(
        &self,
        provider: CloudProvider,
        work: impl FnOnce() -> Result<T, CloudError>,
    ) -> Result<T, CloudError> {
        let _guard = self
            .locks
            .get(&provider)
            .ok_or(CloudError::ProviderUnsupported)?
            .blocking_lock();
        work()
    }
    pub fn record(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
    ) -> Result<AuthMaintenanceRecord, CloudError> {
        if let Some(bytes) = store.load(&format!("cloud-auth-maintenance-{}", provider.key()))? {
            let record: AuthMaintenanceRecord =
                serde_json::from_slice(&bytes).map_err(|_| CloudError::Storage)?;
            if record.schema_version != 1 || record.provider != provider {
                return Err(CloudError::Storage);
            }
            return Ok(record);
        }
        Ok(AuthMaintenanceRecord {
            schema_version: 1,
            provider,
            account_id: None,
            auth_method: None,
            session_revision: 0,
            expires_at: None,
            next_refresh_at: None,
            consecutive_failures: 0,
            next_allowed_attempt_at: None,
            health: AuthHealth::Disconnected,
        })
    }
    fn save(
        &self,
        store: &impl CredentialStore,
        record: &AuthMaintenanceRecord,
    ) -> Result<(), CloudError> {
        store.save(
            &format!("cloud-auth-maintenance-{}", record.provider.key()),
            &serde_json::to_vec(record).map_err(|_| CloudError::Storage)?,
        )
    }
    fn mark_ready(&self, record: &mut AuthMaintenanceRecord, expires: Option<i64>) {
        let now = (self.clock)();
        record.session_revision = record.session_revision.saturating_add(1);
        record.expires_at = expires;
        record.next_refresh_at = if matches!(
            record.provider,
            CloudProvider::Quark | CloudProvider::Baidu | CloudProvider::Uc
        ) {
            None
        } else {
            expires.map(|expiry| {
                let ttl = expiry.saturating_sub(now).max(0);
                now.saturating_add(ttl.saturating_mul(4) / 5)
                    .min(expiry.saturating_sub(300))
                    .saturating_sub((self.jitter)().clamp(30, 120))
                    .max(now)
            })
        };
        record.consecutive_failures = 0;
        record.next_allowed_attempt_at = None;
        record.health = AuthHealth::Ready;
    }
    pub fn ready(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
        account: Option<String>,
        method: Option<CloudAuthMethod>,
        expires: Option<i64>,
    ) -> Result<(), CloudError> {
        self.with_provider(provider, || {
            self.ready_locked(store, provider, account, method, expires)
        })
    }
    /// Interactive commands already own the provider guard while replacing credentials.
    pub(crate) fn ready_locked(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
        account: Option<String>,
        method: Option<CloudAuthMethod>,
        expires: Option<i64>,
    ) -> Result<(), CloudError> {
        let mut record = self.record(store, provider)?;
        record.account_id = account;
        record.auth_method = method;
        self.mark_ready(&mut record, expires);
        self.save(store, &record)
    }
    pub fn disconnect(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
    ) -> Result<(), CloudError> {
        self.with_provider(provider, || self.disconnect_locked(store, provider))
    }
    pub(crate) fn disconnect_locked(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
    ) -> Result<(), CloudError> {
        let mut record = self.record(store, provider)?;
        self.mark_ready(&mut record, None);
        record.account_id = None;
        record.auth_method = None;
        record.health = AuthHealth::Disconnected;
        self.save(store, &record)
    }
    pub fn restore(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
    ) -> Result<(), CloudError> {
        if store
            .load(&format!("cloud-auth-maintenance-{}", provider.key()))?
            .is_some()
        {
            return self.record(store, provider).map(|_| ());
        }
        if store.load(provider.key())?.is_none() {
            return Ok(());
        }
        let mut record = self.record(store, provider)?;
        record.health = AuthHealth::Restoring;
        self.save(store, &record)?;
        self.ready(
            store,
            provider,
            None,
            None,
            session_expiry(store, provider)?,
        )
    }
    pub fn is_due(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
    ) -> Result<bool, CloudError> {
        let record = self.record(store, provider)?;
        Ok(!matches!(
            record.health,
            AuthHealth::Disconnected | AuthHealth::RiskBlocked | AuthHealth::NeedsInteractiveAuth
        ) && record.consecutive_failures < 3
            && record
                .next_allowed_attempt_at
                .is_none_or(|next| next <= (self.clock)())
            && record
                .next_refresh_at
                .is_some_and(|next| next <= (self.clock)()))
    }
    pub fn maintain(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
        revision: Option<u64>,
        auth_error: bool,
        work: impl FnOnce() -> Result<(), CloudError>,
    ) -> Result<(), CloudError> {
        self.with_provider(provider, || {
            let mut record = self.record(store, provider)?;
            if revision.is_some_and(|revision| revision != record.session_revision) {
                return Ok(());
            }
            if record.consecutive_failures >= 3
                || record.health == AuthHealth::RiskBlocked
                || record
                    .next_allowed_attempt_at
                    .is_some_and(|next| next > (self.clock)())
            {
                return Err(CloudError::WebAuthorizationRequired);
            }
            if !auth_error && !self.is_due(store, provider)? {
                return Ok(());
            }
            record.health = if auth_error {
                AuthHealth::NeedsSilentRelogin
            } else {
                AuthHealth::Refreshing
            };
            // Reserve the attempt durably before contacting the provider. A crash
            // cannot create a tight login loop on the next launch.
            record.consecutive_failures = record.consecutive_failures.saturating_add(1);
            record.next_allowed_attempt_at = Some((self.clock)().saturating_add(
                match record.consecutive_failures {
                    1 => 300,
                    2 => 1800,
                    _ => 7200,
                },
            ));
            self.save(store, &record)?;
            match work() {
                Ok(()) => {
                    let expires = session_expiry(store, provider)?;
                    self.mark_ready(&mut record, expires);
                    self.save(store, &record)
                }
                Err(error) => {
                    record.health = if matches!(error, CloudError::CaptchaRequired) {
                        AuthHealth::RiskBlocked
                    } else if record.consecutive_failures >= 3 {
                        AuthHealth::NeedsInteractiveAuth
                    } else {
                        AuthHealth::NeedsSilentRelogin
                    };
                    self.save(store, &record)?;
                    Err(error)
                }
            }
        })
    }
    pub fn run<T>(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
        kind: SessionWorkKind,
        mut work: impl FnMut(&dyn CredentialStore) -> Result<T, CloudError>,
        mut recover: impl FnMut(bool) -> Result<(), CloudError>,
    ) -> Result<T, CloudError> {
        if kind == SessionWorkKind::Logout {
            return self.with_provider(provider, || work(store));
        }
        if self.is_due(store, provider)? {
            let revision = self.record(store, provider)?.session_revision;
            // A proactive refresh failure must not stop still-valid ordinary reads.
            let _ = self.maintain(store, provider, Some(revision), false, || recover(false));
        }
        let revision = self.record(store, provider)?.session_revision;
        let snapshot = SessionSnapshot::new(store, provider)?;
        let result = work(&snapshot);
        self.persist_snapshot(store, provider, revision, &snapshot)?;
        match result {
            Err(error @ CloudError::CaptchaRequired) => {
                self.with_provider(provider, || {
                    let mut record = self.record(store, provider)?;
                    if record.session_revision == revision {
                        record.health = AuthHealth::RiskBlocked;
                        self.save(store, &record)?;
                    }
                    Ok(())
                })?;
                Err(error)
            }
            Err(error)
                if matches!(
                    error,
                    CloudError::AuthorizationExpired | CloudError::NotAuthorized
                ) =>
            {
                if snapshot.initial.is_none() || store.load(provider.key())?.is_none() {
                    return Err(error);
                }
                self.maintain(store, provider, Some(revision), true, || recover(true))?;
                if kind == SessionWorkKind::Mutation {
                    return Err(CloudError::SessionRenewedRetryOperation);
                }
                let next = SessionSnapshot::new(store, provider)?;
                let revision = self.record(store, provider)?.session_revision;
                let result = work(&next);
                self.persist_snapshot(store, provider, revision, &next)?;
                result
            }
            result => result,
        }
    }
    fn persist_snapshot(
        &self,
        store: &impl CredentialStore,
        provider: CloudProvider,
        revision: u64,
        snapshot: &SessionSnapshot,
    ) -> Result<(), CloudError> {
        let current = snapshot.load(provider.key())?;
        if current == snapshot.initial {
            return Ok(());
        }
        self.with_provider(provider, || {
            if self.record(store, provider)?.session_revision != revision
                || store.load(provider.key())? != snapshot.initial
            {
                return Ok(());
            }
            if let Some(bytes) = current {
                store.save(provider.key(), &bytes)?;
            }
            Ok(())
        })
    }
}

/// Fixed recovery order shared by foreground work and scheduled maintenance.
/// Only explicit authentication failures permit advancing to another method.
pub fn recover_in_order(
    provider: CloudProvider,
    store: &impl CredentialStore,
    auth: &mut dyn driver::ProviderAuth,
    genuine_auth_error: bool,
    mut profile: impl FnMut() -> Result<bool, CloudError>,
) -> Result<(), CloudError> {
    fn auth_failure(error: &CloudError) -> bool {
        matches!(
            error,
            CloudError::AuthorizationExpired
                | CloudError::NotAuthorized
                | CloudError::WebAuthorizationRequired
        )
    }
    match auth.refresh_session() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) if auth_failure(&error) => {}
        Err(error) => return Err(error),
    }
    if cloud_session_policy(provider).supports_password_fallback {
        if let Some(credential) = load_password_credential(store, provider)? {
            // A browser-only password preference still uses its persistent profile.
            if !browser_password_login_preferred(store, provider)? {
                match auth.recover_password(&credential) {
                    Ok((account, session)) => {
                        if account.id.is_empty() || account.id != credential.account_id() {
                            return Err(CloudError::WebAuthorizationRequired);
                        }
                        store.save(provider.key(), &session)?;
                        return Ok(());
                    }
                    Err(error) if auth_failure(&error) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    if genuine_auth_error {
        match profile() {
            Ok(true) => return Ok(()),
            Ok(false) | Err(CloudError::AuthorizationPending) => {}
            Err(error) => return Err(error),
        }
    }
    Err(CloudError::WebAuthorizationRequired)
}

/// Operations see a private copy. Writes stay local until revision validation.
pub struct SessionSnapshot {
    provider: CloudProvider,
    initial: Option<Vec<u8>>,
    store: MemoryCredentialStore,
}
impl SessionSnapshot {
    pub fn new(store: &impl CredentialStore, provider: CloudProvider) -> Result<Self, CloudError> {
        let initial = store.load(provider.key())?;
        let snapshot = Self {
            provider,
            initial,
            store: MemoryCredentialStore::default(),
        };
        if let Some(bytes) = &snapshot.initial {
            snapshot.store.save(provider.key(), bytes)?;
        }
        Ok(snapshot)
    }
}
impl CredentialStore for SessionSnapshot {
    fn save(&self, key: &str, bytes: &[u8]) -> Result<(), CloudError> {
        if key != self.provider.key() {
            return Err(CloudError::Storage);
        }
        self.store.save(key, bytes)
    }
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, CloudError> {
        self.store.load(key)
    }
    fn remove(&self, key: &str) -> Result<(), CloudError> {
        self.store.remove(key)
    }
    fn automatic_session_recovery(&self) -> bool {
        false
    }
}

pub fn session_expiry(
    store: &impl CredentialStore,
    provider: CloudProvider,
) -> Result<Option<i64>, CloudError> {
    let Some(bytes) = store.load(provider.key())? else {
        return Ok(None);
    };
    // Cookie providers and legacy sessions may not be JSON or know their TTL.
    Ok(serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("expires_at").and_then(|v| v.as_i64()))
        .filter(|expiry| *expiry > 0))
}
