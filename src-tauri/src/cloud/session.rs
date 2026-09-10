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
    version: u8,
    provider: CloudProvider,
    account_id: &'a str,
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct LoadedPasswordCredential {
    version: u8,
    provider: CloudProvider,
    account_id: String,
    username: String,
    password: String,
}

impl Zeroize for LoadedPasswordCredential {
    fn zeroize(&mut self) {
        self.account_id.zeroize();
        self.username.zeroize();
        self.password.zeroize();
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

pub fn save_password_credential(
    store: &impl CredentialStore,
    credential: CloudPasswordCredential,
) -> Result<(), QuarkError> {
    let key = password_credential_key(credential.provider())?;
    let stored = StoredPasswordCredential {
        version: 1,
        provider: credential.provider(),
        account_id: credential.account_id(),
        username: credential.username(),
        password: credential.password(),
    };
    let mut bytes = serde_json::to_vec(&stored).map_err(|_| QuarkError::Storage)?;
    let result = store.save(key, &bytes);
    bytes.zeroize();
    result
}

pub fn load_password_credential(
    store: &impl CredentialStore,
    provider: CloudProvider,
) -> Result<Option<CloudPasswordCredential>, QuarkError> {
    let key = password_credential_key(provider)?;
    let Some(mut bytes) = store.load(key)? else {
        return Ok(None);
    };
    let parsed = serde_json::from_slice::<LoadedPasswordCredential>(&bytes);
    bytes.zeroize();
    let mut parsed = parsed.map_err(|_| QuarkError::Storage)?;
    if parsed.version != 1 || parsed.provider != provider {
        parsed.zeroize();
        return Err(QuarkError::Storage);
    }
    let credential = CloudPasswordCredential::new(
        parsed.provider,
        std::mem::take(&mut parsed.account_id),
        std::mem::take(&mut parsed.username),
        std::mem::take(&mut parsed.password),
    );
    parsed.zeroize();
    Ok(Some(credential))
}

pub fn remove_password_credential(
    store: &impl CredentialStore,
    provider: CloudProvider,
) -> Result<(), QuarkError> {
    store.remove(password_credential_key(provider)?)
}

pub fn clear_password_credential_if_account_differs(
    store: &impl CredentialStore,
    provider: CloudProvider,
    account_id: &str,
) -> Result<(), QuarkError> {
    if let Some(credential) = load_password_credential(store, provider)? {
        if credential.account_id() != account_id {
            remove_password_credential(store, provider)?;
        }
    }
    Ok(())
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
