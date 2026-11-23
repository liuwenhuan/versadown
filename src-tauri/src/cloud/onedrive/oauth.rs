//! OAuth2 authorization code flow with PKCE for a public client.
//!
//! The application id is not part of the repository: it is injected at build
//! time through `VERSADOWN_ONEDRIVE_CLIENT_ID`. The tenant is `consumers`
//! (personal Microsoft accounts only) and the scope is read-only.

use super::failure::OnedriveFailure;
use super::session::{now_secs, OnedriveSession};
use crate::cloud::oauth::form_post;
use crate::cloud::{CloudHttpTransport, QuarkError};
use serde_json::Value;

pub(crate) const SCOPE: &str = "Files.Read offline_access";

/// Application id of the build, if one was injected.
pub(crate) fn build_client_id() -> Option<&'static str> {
    option_env!("VERSADOWN_ONEDRIVE_CLIENT_ID")
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Whether this build can sign in at all.
pub(crate) fn oauth_configured() -> bool {
    build_client_id().is_some()
}

#[derive(Clone, Debug)]
pub(crate) struct OAuthConfig {
    pub client_id: String,
    pub authorize_url: String,
    pub token_url: String,
    pub graph_base: String,
}

impl OAuthConfig {
    pub fn with_client_id(client_id: &str) -> Self {
        Self {
            client_id: client_id.to_owned(),
            authorize_url: "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize"
                .into(),
            token_url: "https://login.microsoftonline.com/consumers/oauth2/v2.0/token".into(),
            graph_base: "https://graph.microsoft.com/v1.0".into(),
        }
    }

    /// The configuration of this build; the id is empty when none was injected.
    pub fn for_build() -> Self {
        Self::with_client_id(build_client_id().unwrap_or_default())
    }

    /// The configuration of this build, or `ClientIdMissing`.
    pub fn production() -> Result<Self, QuarkError> {
        build_client_id()
            .map(Self::with_client_id)
            .ok_or(QuarkError::Onedrive(OnedriveFailure::ClientIdMissing))
    }
}

pub(crate) fn authorize_url(
    config: &OAuthConfig,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &config.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_mode", "query")
        .append_pair("scope", SCOPE)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "select_account")
        .finish();
    format!("{}?{query}", config.authorize_url)
}

fn token_request(
    http: &dyn CloudHttpTransport,
    config: &OAuthConfig,
    form: &[(&str, &str)],
) -> Result<OnedriveSession, QuarkError> {
    if config.client_id.is_empty() {
        return Err(QuarkError::Onedrive(OnedriveFailure::ClientIdMissing));
    }
    let mut pairs = form.to_vec();
    pairs.push(("client_id", config.client_id.as_str()));
    pairs.push(("scope", SCOPE));
    let response = http.send(form_post(&config.token_url, &pairs))?;
    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    if response.status != 200 {
        return Err(token_error(response.status, &value));
    }
    let access = value["access_token"]
        .as_str()
        .filter(|token| !token.is_empty());
    let refresh = value["refresh_token"]
        .as_str()
        .filter(|token| !token.is_empty());
    let (Some(access), Some(refresh)) = (access, refresh) else {
        return Err(QuarkError::InvalidResponse);
    };
    let lifetime = value["expires_in"].as_i64().unwrap_or(3600).max(0);
    Ok(OnedriveSession {
        access_token: access.to_owned(),
        refresh_token: refresh.to_owned(),
        expires_at: now_secs().saturating_add(lifetime),
    })
}

/// A rejected grant means the refresh token (or code) is dead: revoked access,
/// a changed password or an expired code. Provider text is never surfaced.
fn token_error(status: u16, body: &Value) -> QuarkError {
    match (status, body["error"].as_str()) {
        (400 | 401, Some("invalid_grant")) => QuarkError::AuthorizationExpired,
        (429, _) | (500..=599, _) => QuarkError::Onedrive(OnedriveFailure::ServiceBusy),
        _ => QuarkError::Onedrive(OnedriveFailure::AuthorizationFailed),
    }
}

pub(crate) fn exchange_code(
    http: &dyn CloudHttpTransport,
    config: &OAuthConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OnedriveSession, QuarkError> {
    token_request(
        http,
        config,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ],
    )
}

/// Microsoft rotates refresh tokens; the response's new one replaces the old.
pub(crate) fn refresh(
    http: &dyn CloudHttpTransport,
    config: &OAuthConfig,
    refresh_token: &str,
) -> Result<OnedriveSession, QuarkError> {
    token_request(
        http,
        config,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn authorize_url_requests_only_read_access_in_the_consumers_tenant() {
        let config = OAuthConfig::with_client_id("00000000-0000-0000-0000-000000000000");
        let url = url::Url::parse(&authorize_url(
            &config,
            "http://localhost:51234/",
            "st",
            "ch",
        ))
        .unwrap();
        assert_eq!(url.host_str(), Some("login.microsoftonline.com"));
        assert!(url.path().starts_with("/consumers/"));
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["scope"], "Files.Read offline_access");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["redirect_uri"], "http://localhost:51234/");
        assert_eq!(query["response_type"], "code");
    }
}
