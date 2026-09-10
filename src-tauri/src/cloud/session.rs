use super::{provider_capabilities, CloudProvider, CredentialStore, PasswordLoginMode, QuarkError};
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionWorkKind {
    Read,
    Mutation,
    Logout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudSessionPolicy {
    pub provider: CloudProvider,
    pub supports_password_fallback: bool,
}

pub fn cloud_session_policy(provider: CloudProvider) -> CloudSessionPolicy {
    CloudSessionPolicy {
        provider,
        supports_password_fallback: provider_capabilities(provider).password_login_mode
            == PasswordLoginMode::RustDirect,
    }
}

pub struct CloudPasswordCredential {
    provider: CloudProvider,
    account_id: String,
    username: Zeroizing<String>,
    password: Zeroizing<String>,
}

impl CloudPasswordCredential {
    pub fn new(
        provider: CloudProvider,
        account_id: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            account_id: account_id.into(),
            username: Zeroizing::new(username.into()),
            password: Zeroizing::new(password.into()),
        }
    }

    pub fn provider(&self) -> CloudProvider {
        self.provider
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn username(&self) -> &str {
        self.username.as_str()
    }

    pub fn password(&self) -> &str {
        self.password.as_str()
    }
}

impl fmt::Debug for CloudPasswordCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudPasswordCredential")
            .field("provider", &self.provider)
            .field("account_id", &self.account_id)
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Serialize)]
struct StoredPasswordCredential<'a> {
    account_id: &'a str,
    username: &'a str,
    password: &'a str,
}

#[derive(Serialize)]
struct StoredPasswordCredentials<'a> {
    version: u8,
    provider: CloudProvider,
    active_account_id: Option<&'a str>,
    credentials: Vec<StoredPasswordCredential<'a>>,
}

#[derive(Deserialize)]
struct PasswordCredentialVersion {
    version: u8,
}

#[derive(Deserialize)]
struct LoadedPasswordCredentialV1 {
    version: u8,
    provider: CloudProvider,
    account_id: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct LoadedPasswordCredential {
    account_id: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct LoadedPasswordCredentials {
    version: u8,
    provider: CloudProvider,
    active_account_id: Option<String>,
    credentials: Vec<LoadedPasswordCredential>,
}

impl Zeroize for LoadedPasswordCredentialV1 {
    fn zeroize(&mut self) {
        self.account_id.zeroize();
        self.username.zeroize();
        self.password.zeroize();
    }
}

impl Zeroize for LoadedPasswordCredential {
    fn zeroize(&mut self) {
        self.account_id.zeroize();
        self.username.zeroize();
        self.password.zeroize();
    }
}

impl Zeroize for LoadedPasswordCredentials {
    fn zeroize(&mut self) {
        if let Some(active_account_id) = self.active_account_id.as_mut() {
            active_account_id.zeroize();
        }
        self.active_account_id = None;
        self.credentials.zeroize();
    }
}

fn password_credential_key(provider: CloudProvider) -> Result<&'static str, QuarkError> {
    if provider_capabilities(provider).password_login_mode != PasswordLoginMode::RustDirect {
        return Err(QuarkError::InvalidInput(
            "该网盘不支持账号密码自动恢复".to_owned(),
        ));
    }
    match provider {
        CloudProvider::Pikpak => Ok("cloud-password-pikpak"),
        CloudProvider::Baidu => Ok("cloud-password-baidu"),
        CloudProvider::Tianyi => Ok("cloud-password-tianyi"),
        CloudProvider::Quark
        | CloudProvider::Aliyun
        | CloudProvider::Xunlei
        | CloudProvider::Uc => {
            unreachable!("RustDirect providers must have a password credential key")
        }
    }
}

fn load_password_credentials(
    store: &impl CredentialStore,
    provider: CloudProvider,
) -> Result<Option<LoadedPasswordCredentials>, QuarkError> {
    let key = password_credential_key(provider)?;
    let Some(mut bytes) = store.load(key)? else {
        return Ok(None);
    };
    let version = match serde_json::from_slice::<PasswordCredentialVersion>(&bytes) {
        Ok(version) => version,
        Err(_) => {
            bytes.zeroize();
            return Err(QuarkError::Storage);
        }
    };
    let mut credentials = match version.version {
        // Version 1 stored one credential per provider. It remains readable and
        // becomes the active account when next persisted as version 2.
        1 => {
            let mut legacy = match serde_json::from_slice::<LoadedPasswordCredentialV1>(&bytes) {
                Ok(legacy) => legacy,
                Err(_) => {
                    bytes.zeroize();
                    return Err(QuarkError::Storage);
                }
            };
            if legacy.version != 1 || legacy.provider != provider {
                legacy.zeroize();
                bytes.zeroize();
                return Err(QuarkError::Storage);
            }
            let account_id = std::mem::take(&mut legacy.account_id);
            let username = std::mem::take(&mut legacy.username);
            let password = std::mem::take(&mut legacy.password);
            legacy.zeroize();
            LoadedPasswordCredentials {
                version: 2,
                provider,
                active_account_id: Some(account_id.clone()),
                credentials: vec![LoadedPasswordCredential {
                    account_id,
                    username,
                    password,
                }],
            }
        }
        2 => match serde_json::from_slice::<LoadedPasswordCredentials>(&bytes) {
            Ok(credentials) => credentials,
            Err(_) => {
                bytes.zeroize();
                return Err(QuarkError::Storage);
            }
        },
        _ => {
            bytes.zeroize();
            return Err(QuarkError::Storage);
        }
    };
    bytes.zeroize();
    if credentials.provider != provider || credentials.version != 2 {
        credentials.zeroize();
        return Err(QuarkError::Storage);
    }
    Ok(Some(credentials))
}

fn persist_password_credentials(
    store: &impl CredentialStore,
    credentials: &LoadedPasswordCredentials,
) -> Result<(), QuarkError> {
    let key = password_credential_key(credentials.provider)?;
    let stored = StoredPasswordCredentials {
        version: 2,
        provider: credentials.provider,
        active_account_id: credentials.active_account_id.as_deref(),
        credentials: credentials
            .credentials
            .iter()
            .map(|credential| StoredPasswordCredential {
                account_id: &credential.account_id,
                username: &credential.username,
                password: &credential.password,
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec(&stored).map_err(|_| QuarkError::Storage)?;
    let result = store.save(key, &bytes);
    bytes.zeroize();
    result
}

pub fn save_password_credential(
    store: &impl CredentialStore,
    credential: CloudPasswordCredential,
) -> Result<(), QuarkError> {
    let provider = credential.provider();
    let account_id = credential.account_id().to_owned();
    let username = credential.username().to_owned();
    let password = credential.password().to_owned();
    let mut credentials =
        load_password_credentials(store, provider)?.unwrap_or(LoadedPasswordCredentials {
            version: 2,
            provider,
            active_account_id: None,
            credentials: Vec::new(),
        });
    if let Some(existing) = credentials
        .credentials
        .iter_mut()
        .find(|existing| existing.account_id == account_id)
    {
        existing.username.zeroize();
        existing.password.zeroize();
        existing.username = username;
        existing.password = password;
    } else {
        credentials.credentials.push(LoadedPasswordCredential {
            account_id: account_id.clone(),
            username,
            password,
        });
    }
    if let Some(active_account_id) = credentials.active_account_id.as_mut() {
        active_account_id.zeroize();
    }
    credentials.active_account_id = Some(account_id);
    let result = persist_password_credentials(store, &credentials);
    credentials.zeroize();
    result
}

pub fn load_password_credential(
    store: &impl CredentialStore,
    provider: CloudProvider,
) -> Result<Option<CloudPasswordCredential>, QuarkError> {
    let Some(mut credentials) = load_password_credentials(store, provider)? else {
        return Ok(None);
    };
    let credential = credentials
        .active_account_id
        .as_deref()
        .and_then(|account_id| {
            credentials
                .credentials
                .iter()
                .find(|credential| credential.account_id == account_id)
        })
        .map(|credential| {
            CloudPasswordCredential::new(
                provider,
                credential.account_id.clone(),
                credential.username.clone(),
                credential.password.clone(),
            )
        });
    credentials.zeroize();
    Ok(credential)
}

pub fn remove_password_credential(
    store: &impl CredentialStore,
    provider: CloudProvider,
) -> Result<(), QuarkError> {
    store.remove(password_credential_key(provider)?)
}

/// Records which account owns the current session without deleting password
/// credentials for the provider's other accounts.
pub fn select_password_credential_account(
    store: &impl CredentialStore,
    provider: CloudProvider,
    account_id: &str,
) -> Result<(), QuarkError> {
    let Some(mut credentials) = load_password_credentials(store, provider)? else {
        return Ok(());
    };
    if let Some(active_account_id) = credentials.active_account_id.as_mut() {
        active_account_id.zeroize();
    }
    credentials.active_account_id = Some(account_id.to_owned());
    let result = persist_password_credentials(store, &credentials);
    credentials.zeroize();
    result
}

pub fn run_with_session_recovery<T>(
    provider: CloudProvider,
    store: &impl CredentialStore,
    kind: SessionWorkKind,
    mut work: impl FnMut() -> Result<T, QuarkError>,
    password_relogin: impl FnOnce(&CloudPasswordCredential) -> Result<(), QuarkError>,
) -> Result<T, QuarkError> {
    let first_result = work();
    let error = match first_result {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    if !is_recoverable_authorization_error(&error) || kind == SessionWorkKind::Logout {
        return Err(error);
    }
    if !cloud_session_policy(provider).supports_password_fallback {
        return Err(error);
    }
    let Some(credential) = load_password_credential(store, provider)? else {
        return Err(error);
    };
    password_relogin(&credential)?;
    drop(credential);
    match kind {
        SessionWorkKind::Read => work(),
        SessionWorkKind::Mutation => Err(QuarkError::SessionRenewedRetryOperation),
        SessionWorkKind::Logout => Err(error),
    }
}

fn is_recoverable_authorization_error(error: &QuarkError) -> bool {
    matches!(
        error,
        QuarkError::AuthorizationExpired | QuarkError::NotAuthorized
    )
}

#[cfg(test)]
mod tests {
    use super::{
        load_password_credential, save_password_credential, select_password_credential_account,
        CloudPasswordCredential,
    };
    use crate::cloud::{CloudProvider, CredentialStore, MemoryCredentialStore};

    #[test]
    fn preserves_another_accounts_password_after_web_authorization_switches_accounts() {
        let store = MemoryCredentialStore::default();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(CloudProvider::Baidu, "account-a", "alice", "secret-a"),
        )
        .unwrap();

        // A web authorization for account B must stop automatic login as A, but it
        // must not erase A's encrypted password. The user may switch back later.
        select_password_credential_account(&store, CloudProvider::Baidu, "account-b").unwrap();
        assert!(load_password_credential(&store, CloudProvider::Baidu)
            .unwrap()
            .is_none());

        select_password_credential_account(&store, CloudProvider::Baidu, "account-a").unwrap();
        let credential = load_password_credential(&store, CloudProvider::Baidu)
            .unwrap()
            .expect("the prior direct-login credential remains available");
        assert_eq!(credential.username(), "alice");
        assert_eq!(credential.password(), "secret-a");
    }

    #[test]
    fn restores_a_legacy_single_account_password_without_discarding_it() {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "cloud-password-baidu",
                br#"{"version":1,"provider":"baidu","account_id":"account-a","username":"alice","password":"secret-a"}"#,
            )
            .unwrap();

        let credential = load_password_credential(&store, CloudProvider::Baidu)
            .unwrap()
            .expect("existing version-one credentials remain usable after the upgrade");
        assert_eq!(credential.account_id(), "account-a");
        assert_eq!(credential.username(), "alice");
        assert_eq!(credential.password(), "secret-a");
    }
}
