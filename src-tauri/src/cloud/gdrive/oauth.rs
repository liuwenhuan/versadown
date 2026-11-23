//! OAuth 2.0 for installed applications: authorization-code flow with PKCE
//! and a loopback redirect (RFC 8252). The system browser shows Google's own
//! consent page; the app only ever sees the one-time code on 127.0.0.1.
//!
//! Scope is `drive.readonly`. Tokens are never logged or put into errors.

use super::failure::GdriveFailure;
use crate::cloud::oauth::{
    form_post, AuthorizationCode, FlowError, PendingAuthorization, Redirect,
};
use crate::cloud::{CloudHttpTransport, QuarkError};
use serde_json::Value;
use std::time::Duration;

pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
pub const REVOKE_ENDPOINT: &str = "https://oauth2.googleapis.com/revoke";
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";

/// How long the browser has to come back.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// A Google "installed application" client. Its secret is not confidential
/// (Google documents that for desktop clients) but is still injected at build
/// time instead of being committed.
#[derive(Clone)]
pub struct OAuthClient {
    pub id: String,
    pub secret: Option<String>,
}

impl OAuthClient {
    /// The client compiled into this build, or `NotConfigured`.
    pub fn from_build() -> Result<Self, QuarkError> {
        let id = super::OAUTH_CLIENT_ID
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or(QuarkError::Gdrive(GdriveFailure::NotConfigured))?;
        Ok(Self {
            id: id.to_owned(),
            secret: super::OAUTH_CLIENT_SECRET
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
                .map(str::to_owned),
        })
    }
}

#[derive(Debug)]
pub struct TokenSet {
    pub access_token: String,
    /// Google returns a refresh token on the first exchange only (and rotates
    /// nothing on refresh), so a refresh keeps the stored one.
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    /// Space separated scopes actually granted (the user may untick some).
    pub scope: Option<String>,
}

pub fn authorization_url(
    client: &OAuthClient,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    let mut url = url::Url::parse(AUTH_ENDPOINT).expect("static endpoint");
    url.query_pairs_mut()
        .append_pair("client_id", &client.id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        // Offline access plus a forced consent screen guarantees a refresh
        // token even when the user authorized an earlier install before.
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    url.into()
}

/// Map a token endpoint reply. The error codes are fixed ASCII identifiers
/// defined by RFC 6749 and safe to show.
fn token_set(status: u16, body: &str) -> Result<TokenSet, QuarkError> {
    let value: Value = serde_json::from_str(body).map_err(|_| QuarkError::InvalidResponse)?;
    if let Some(error) = value["error"].as_str() {
        return Err(match error {
            // The refresh token was revoked, expired (testing-mode apps: 7
            // days) or the user changed their password.
            "invalid_grant" => QuarkError::AuthorizationExpired,
            "invalid_client" | "unauthorized_client" | "access_denied" | "disabled_client" => {
                QuarkError::Gdrive(GdriveFailure::ClientNotAllowed)
            }
            // Only the codes RFC 6749 defines are echoed; anything else is
            // free text that could carry request data.
            other => QuarkError::Provider(format!(
                "Google 授权服务拒绝请求（{}）",
                [
                    "invalid_request",
                    "invalid_scope",
                    "unsupported_grant_type",
                    "server_error",
                    "temporarily_unavailable",
                ]
                .into_iter()
                .find(|known| *known == other)
                .unwrap_or("unknown_error")
            )),
        });
    }
    if !(200..300).contains(&status) {
        return Err(QuarkError::Provider(format!(
            "Google 授权服务返回 HTTP {status}"
        )));
    }
    let access_token = value["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or(QuarkError::InvalidResponse)?
        .to_owned();
    Ok(TokenSet {
        access_token,
        refresh_token: value["refresh_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned),
        expires_in: value["expires_in"].as_i64().unwrap_or(3600).max(0),
        scope: value["scope"].as_str().map(str::to_owned),
    })
}

pub fn exchange_code(
    http: &dyn CloudHttpTransport,
    client: &OAuthClient,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenSet, QuarkError> {
    let mut pairs = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client.id.as_str()),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];
    if let Some(secret) = &client.secret {
        pairs.push(("client_secret", secret));
    }
    let response = http.send(form_post(TOKEN_ENDPOINT, &pairs))?;
    token_set(response.status, &response.body)
}

pub fn refresh_tokens(
    http: &dyn CloudHttpTransport,
    client: &OAuthClient,
    refresh_token: &str,
) -> Result<TokenSet, QuarkError> {
    let mut pairs = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client.id.as_str()),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = &client.secret {
        pairs.push(("client_secret", secret));
    }
    let response = http.send(form_post(TOKEN_ENDPOINT, &pairs))?;
    token_set(response.status, &response.body)
}

/// Turn the browser's one-time code into a stored session and report whose
/// drive it is. The tokens are validated in a scratch store first, so a failed
/// attempt never replaces a working session.
pub(crate) fn complete_authorization(
    store: &dyn crate::cloud::CredentialStore,
    http: &dyn CloudHttpTransport,
    client: &OAuthClient,
    answer: &AuthorizationCode,
) -> Result<crate::cloud::CloudAccount, QuarkError> {
    use super::{api::GdriveClient, session::{now, GdriveSession}};
    let tokens = exchange_code(
        http,
        client,
        &answer.code,
        &answer.verifier,
        &answer.redirect_uri,
    )?;
    // Google lets the user untick individual permissions on the consent page.
    if tokens
        .scope
        .as_deref()
        .is_some_and(|granted| !granted.split_whitespace().any(|scope| scope == SCOPE))
    {
        return Err(QuarkError::Gdrive(GdriveFailure::ConsentDenied));
    }
    let refresh_token = tokens.refresh_token.ok_or(QuarkError::InvalidResponse)?;
    let candidate = GdriveSession {
        access_token: tokens.access_token,
        refresh_token,
        expires_at: now() + tokens.expires_in,
        account_id: String::new(),
        display_name: String::new(),
    };
    let scratch = crate::cloud::MemoryCredentialStore::default();
    candidate.save(&scratch)?;
    let account = GdriveClient::new(http, &scratch)
        .with_oauth_client(client.clone())
        .account()?;
    GdriveSession {
        account_id: account.id.clone(),
        display_name: account.display_name.clone(),
        ..candidate
    }
    .save(store)?;
    Ok(account)
}

/// Best effort: tell Google the grant is no longer wanted. A failure changes
/// nothing locally (the credential is removed regardless).
pub fn revoke(http: &dyn CloudHttpTransport, token: &str) {
    let _ = http.send(form_post(REVOKE_ENDPOINT, &[("token", token)]));
}

// -------------------------------------------------------------- sign-in flow

/// Start the browser sign-in; returns the consent URL for the system browser.
pub(crate) fn begin(
    flow: &PendingAuthorization,
    client: &OAuthClient,
    timeout: Duration,
) -> Result<String, QuarkError> {
    flow.begin(Redirect::LOOPBACK_IP, timeout, flow_failure, |request| {
        authorization_url(
            client,
            request.redirect_uri,
            request.challenge,
            request.state,
        )
    })
}

fn flow_failure(error: FlowError) -> QuarkError {
    QuarkError::Gdrive(match error {
        FlowError::Denied => GdriveFailure::ConsentDenied,
        FlowError::Failed => GdriveFailure::ClientNotAllowed,
        FlowError::Timeout => GdriveFailure::ConsentTimeout,
    })
}

#[cfg(test)]
mod tests;
