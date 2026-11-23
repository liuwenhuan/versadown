//! The stored Google session: OAuth tokens in the encrypted credential store,
//! under the provider key. The access token also reaches the local range
//! gateway (never aria2); the refresh token never leaves this store.

use crate::cloud::{CloudProvider, CredentialStore, QuarkError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct GdriveSession {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds; the name is what the authorization supervisor reads to
    /// schedule proactive renewal.
    pub expires_at: i64,
    /// Drive `permissionId` of the user: stable across email changes.
    pub account_id: String,
    pub display_name: String,
}

// Tokens are credentials: a derived Debug would print them.
impl std::fmt::Debug for GdriveSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GdriveSession")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

impl GdriveSession {
    pub fn load(store: &dyn CredentialStore) -> Result<Self, QuarkError> {
        let bytes = store
            .load(CloudProvider::Gdrive.key())?
            .ok_or(QuarkError::NotAuthorized)?;
        serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)
    }

    pub fn save(&self, store: &dyn CredentialStore) -> Result<(), QuarkError> {
        if self.access_token.is_empty() || self.refresh_token.is_empty() {
            return Err(QuarkError::AuthorizationExpired);
        }
        store.save(
            CloudProvider::Gdrive.key(),
            &serde_json::to_vec(self).map_err(|_| QuarkError::Storage)?,
        )
    }

    /// Whether the access token is (about to be) expired.
    pub fn needs_refresh(&self) -> bool {
        self.expires_at <= now() + 60
    }
}
