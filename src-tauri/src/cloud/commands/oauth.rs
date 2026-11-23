//! Browser based sign-in for providers that use OAuth (Google Drive,
//! OneDrive): the system browser shows the provider's consent page and the
//! redirect lands on a loopback port owned by this process (`cloud::oauth`).

use super::*;
use crate::cloud::gdrive::oauth::{OAuthClient, CONSENT_TIMEOUT};
use crate::cloud::oauth::{AuthorizationCode, PendingAuthorization};
use tauri_plugin_opener::OpenerExt;

/// The consent URL of `provider`, with the loopback listener already waiting.
fn consent_url(provider: CloudProvider, flow: &PendingAuthorization) -> Result<String, QuarkError> {
    match provider {
        CloudProvider::Gdrive => {
            gdrive::oauth::begin(flow, &OAuthClient::from_build()?, CONSENT_TIMEOUT)
        }
        CloudProvider::Onedrive => onedrive::begin_authorization(flow),
        _ => Err(QuarkError::ProviderUnsupported),
    }
}

/// A localization key: the UI translates it.
fn browser_instructions(provider: CloudProvider) -> &'static str {
    match provider {
        CloudProvider::Onedrive => "cloud.authorization.onedriveBrowser",
        _ => "cloud.gdrive.authorizationInstructions",
    }
}

fn account_for_code(
    provider: CloudProvider,
    store: &dyn CredentialStore,
    http: &dyn CloudHttpTransport,
    answer: &AuthorizationCode,
) -> Result<CloudAccount, QuarkError> {
    match provider {
        CloudProvider::Gdrive => {
            gdrive::oauth::complete_authorization(store, http, &OAuthClient::from_build()?, answer)
        }
        CloudProvider::Onedrive => onedrive::complete_authorization(http, store, answer),
        _ => Err(QuarkError::ProviderUnsupported),
    }
}

pub(crate) async fn begin_oauth_authorization(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAuthorization, QuarkError> {
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let _completion = cloud
        .authorization_completion
        .acquire(provider.key())
        .await?;
    cloud.clear_authorization_completion(provider)?;
    let flow = cloud.oauth.get(provider)?;
    let url = consent_url(provider, flow)?;
    if app.opener().open_url(url, None::<&str>).is_err() {
        flow.cancel();
        return Err(QuarkError::WebAuthorizationRequired);
    }
    Ok(CloudAuthorization {
        provider,
        window_label: String::new(),
        instructions: browser_instructions(provider).into(),
        completed: false,
    })
}

/// Polled by the UI while the user is in the browser: `AuthorizationPending`
/// until the redirect arrives, then the account.
pub(crate) async fn complete_oauth_authorization(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccount, QuarkError> {
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let Some(answer) = cloud.oauth.get(provider)?.poll()? else {
        return Err(QuarkError::AuthorizationPending);
    };
    let publish_app = app.clone();
    run(app, provider, move |state, http| {
        let account = account_for_code(provider, &state.store, http, &answer)?;
        state.auth_supervisor.ready_locked(
            &state.store,
            provider,
            Some(account.id.clone()),
            Some(account.auth_method),
            auth_supervisor::session_expiry(&state.store, provider)?,
        )?;
        auth::publish_authorization_completion(&publish_app, state, &account)?;
        Ok(account)
    })
    .await
}

/// Sign-out of an OAuth provider: abandon a pending round trip and, where the
/// provider has a revocation endpoint (Google), tell it the grant is no
/// longer wanted. Best effort, so signing out never needs the network.
pub(crate) fn end_oauth_session(
    state: &CloudState,
    provider: CloudProvider,
    http: &dyn CloudHttpTransport,
) {
    state.oauth.cancel(provider);
    if provider == CloudProvider::Gdrive {
        if let Ok(session) = gdrive::session::GdriveSession::load(&state.store) {
            gdrive::oauth::revoke(http, &session.refresh_token);
        }
    }
}
