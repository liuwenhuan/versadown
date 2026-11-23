//! Microsoft's side of the browser sign-in. The loopback listener, PKCE and
//! `state` check are shared (`cloud::oauth`); this module contributes the
//! consent URL, how a failed round trip is reported and the token exchange
//! that turns the returned code into a stored session and an account.

use super::client::OnedriveClient;
use super::failure::OnedriveFailure;
use super::oauth::{self, OAuthConfig};
use crate::cloud::oauth::{AuthorizationCode, FlowError, PendingAuthorization, Redirect};
use crate::cloud::{CloudAccount, CloudHttpTransport, CredentialStore, QuarkError};
use std::time::Duration;

/// How long the user has to finish signing in.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Start a sign-in: returns the consent URL for the system browser.
pub(crate) fn begin_authorization(flow: &PendingAuthorization) -> Result<String, QuarkError> {
    start_flow(flow, &OAuthConfig::production()?, SIGN_IN_TIMEOUT)
}

pub(crate) fn start_flow(
    flow: &PendingAuthorization,
    config: &OAuthConfig,
    timeout: Duration,
) -> Result<String, QuarkError> {
    flow.begin(Redirect::LOCALHOST, timeout, flow_failure, |request| {
        oauth::authorize_url(
            config,
            request.redirect_uri,
            request.state,
            request.challenge,
        )
    })
}

fn flow_failure(error: FlowError) -> QuarkError {
    QuarkError::Onedrive(match error {
        FlowError::Denied => OnedriveFailure::AuthorizationDenied,
        FlowError::Failed => OnedriveFailure::AuthorizationFailed,
        FlowError::Timeout => OnedriveFailure::AuthorizationTimeout,
    })
}

/// Turn the browser's one-time code into a stored session with the build's
/// application id.
pub(crate) fn complete_authorization(
    http: &dyn CloudHttpTransport,
    store: &dyn CredentialStore,
    answer: &AuthorizationCode,
) -> Result<CloudAccount, QuarkError> {
    complete_with(http, store, &OAuthConfig::production()?, answer)
}

/// Report whose drive the code belongs to. The new session is validated in a
/// scratch store first, so a failed attempt never replaces a working one.
fn complete_with(
    http: &dyn CloudHttpTransport,
    store: &dyn CredentialStore,
    config: &OAuthConfig,
    answer: &AuthorizationCode,
) -> Result<CloudAccount, QuarkError> {
    let session = oauth::exchange_code(
        http,
        config,
        &answer.code,
        &answer.verifier,
        &answer.redirect_uri,
    )?;
    let candidate = crate::cloud::MemoryCredentialStore::default();
    session.save(&candidate)?;
    let account = OnedriveClient::with_config(http, &candidate, config.clone()).account()?;
    session.save(store)?;
    Ok(account)
}

#[cfg(test)]
mod tests;
