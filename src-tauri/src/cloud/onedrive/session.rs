//! The stored OAuth session. `expires_at` is a Unix timestamp in seconds, the
//! field name the authorization supervisor reads to schedule proactive refresh.

use crate::cloud::{CredentialStore, QuarkError};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub(crate) const CREDENTIAL_KEY: &str = "onedrive";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct OnedriveSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}

impl Drop for OnedriveSession {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

// Tokens must never reach logs.
impl std::fmt::Debug for OnedriveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnedriveSession { [REDACTED] }")
    }
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

impl OnedriveSession {
    pub fn load(store: &dyn CredentialStore) -> Result<Option<Self>, QuarkError> {
        let Some(bytes) = store.load(CREDENTIAL_KEY)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| QuarkError::Storage)
    }

    pub fn save(&self, store: &dyn CredentialStore) -> Result<(), QuarkError> {
        let bytes = serde_json::to_vec(self).map_err(|_| QuarkError::Storage)?;
        store.save(CREDENTIAL_KEY, &bytes)
    }

    pub fn expires_within(&self, seconds: i64) -> bool {
        self.expires_at <= now_secs().saturating_add(seconds)
    }
}
