//! Browser sign-in shared by every OAuth provider (OneDrive):
//! authorization code flow with PKCE and a loopback redirect (RFC 8252).
//!
//! The system browser shows the provider's own consent page; this process only
//! ever sees the one-time code on a port it owns. A provider contributes the
//! authorization URL, the token exchange and how a failed round trip is
//! reported; PKCE, the `state` check, the listener, the timeout and the
//! cancellation live here. Secrets and tokens are never logged.

mod loopback;

use crate::cloud::{CloudHttpRequest, CloudProvider, QuarkError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use zeroize::Zeroizing;

fn random_url_safe(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

pub(crate) struct Pkce {
    pub verifier: Zeroizing<String>,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        // 32 random bytes encode to 43 unreserved characters (RFC 7636: 43..128).
        let verifier = Zeroizing::new(random_url_safe(32));
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// Unguessable value tying the redirect to the request that started it.
fn random_state() -> String {
    random_url_safe(24)
}

/// A `application/x-www-form-urlencoded` POST (token and revoke endpoints).
pub(crate) fn form_post(url: &str, pairs: &[(&str, &str)]) -> CloudHttpRequest {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    CloudHttpRequest {
        method: "POST",
        url: url.to_owned(),
        headers: BTreeMap::from([(
            "Content-Type".to_owned(),
            "application/x-www-form-urlencoded".to_owned(),
        )]),
        body: Some(body),
    }
}

/// Why the browser round trip ended without a code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlowError {
    /// The user declined on the consent page.
    Denied,
    /// The provider answered with another error, or without a code.
    Failed,
    /// Nobody answered in time (or the attempt was superseded).
    Timeout,
}

/// Each provider reports a failed round trip with its own error.
pub(crate) type FlowErrorMapper = fn(FlowError) -> QuarkError;

/// Where the provider's registered redirect points.
#[derive(Clone, Copy)]
pub(crate) struct Redirect {
    host: &'static str,
    path: &'static str,
}

impl Redirect {
    /// `http://127.0.0.1:<port>` (clients registered with the IP address).
    pub const LOOPBACK_IP: Self = Self {
        host: "127.0.0.1",
        path: "",
    };
    /// `http://localhost:<port>/` (Microsoft only matches the `localhost`
    /// name); the listener also covers `::1` since the name may resolve to it.
    pub const LOCALHOST: Self = Self {
        host: "localhost",
        path: "/",
    };
}

/// What a provider needs to put into its authorization URL.
pub(crate) struct AuthorizationRequest<'a> {
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub challenge: &'a str,
}

/// What a finished browser round trip yields.
pub(crate) struct AuthorizationCode {
    pub code: Zeroizing<String>,
    pub verifier: Zeroizing<String>,
    pub redirect_uri: String,
}

type AnswerSlot = Arc<Mutex<Option<Result<String, FlowError>>>>;

struct Pending {
    verifier: Zeroizing<String>,
    redirect_uri: String,
    answer: AnswerSlot,
    cancel: Arc<AtomicBool>,
    map_error: FlowErrorMapper,
}

/// The one authorization of a provider in flight. Starting another replaces it.
#[derive(Default)]
pub(crate) struct PendingAuthorization(Mutex<Option<Pending>>);

impl PendingAuthorization {
    /// Open the loopback listener and return the URL for the system browser.
    pub fn begin(
        &self,
        redirect: Redirect,
        timeout: Duration,
        map_error: FlowErrorMapper,
        authorization_url: impl FnOnce(&AuthorizationRequest<'_>) -> String,
    ) -> Result<String, QuarkError> {
        let (listeners, port) = loopback::bind(redirect.host)?;
        let redirect_uri = format!("http://{}:{port}{}", redirect.host, redirect.path);
        let pkce = Pkce::generate();
        let state = random_state();
        let url = authorization_url(&AuthorizationRequest {
            redirect_uri: &redirect_uri,
            state: &state,
            challenge: &pkce.challenge,
        });
        let answer: AnswerSlot = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let (answer, cancel) = (Arc::clone(&answer), Arc::clone(&cancel));
            std::thread::spawn(move || {
                let outcome = loopback::serve(&listeners, &state, timeout, &cancel);
                // A superseded or cancelled attempt reports nothing.
                if !cancel.load(Ordering::SeqCst) {
                    if let Ok(mut slot) = answer.lock() {
                        *slot = Some(outcome);
                    }
                }
            });
        }
        let mut pending = self.0.lock().map_err(|_| QuarkError::Storage)?;
        if let Some(previous) = pending.take() {
            previous.cancel.store(true, Ordering::SeqCst);
        }
        *pending = Some(Pending {
            verifier: pkce.verifier,
            redirect_uri,
            answer,
            cancel,
            map_error,
        });
        Ok(url)
    }

    /// `Ok(None)` while the user is still in the browser. A final answer,
    /// success or failure, consumes the pending authorization.
    pub fn poll(&self) -> Result<Option<AuthorizationCode>, QuarkError> {
        let mut pending = self.0.lock().map_err(|_| QuarkError::Storage)?;
        let Some(current) = pending.as_ref() else {
            return Err(QuarkError::WebAuthorizationRequired);
        };
        let answer = current
            .answer
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .take();
        let Some(outcome) = answer else {
            return Ok(None);
        };
        let current = pending.take().expect("checked above");
        match outcome {
            Ok(code) => Ok(Some(AuthorizationCode {
                code: Zeroizing::new(code),
                verifier: current.verifier,
                redirect_uri: current.redirect_uri,
            })),
            Err(failure) => Err((current.map_error)(failure)),
        }
    }

    pub fn cancel(&self) {
        if let Ok(mut pending) = self.0.lock() {
            if let Some(previous) = pending.take() {
                previous.cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// One pending authorization per OAuth provider.
pub(crate) struct OAuthFlows(HashMap<CloudProvider, PendingAuthorization>);

impl Default for OAuthFlows {
    fn default() -> Self {
        Self(
            CloudProvider::ALL
                .into_iter()
                .filter(|provider| provider.uses_oauth())
                .map(|provider| (provider, PendingAuthorization::default()))
                .collect(),
        )
    }
}

impl OAuthFlows {
    pub fn get(&self, provider: CloudProvider) -> Result<&PendingAuthorization, QuarkError> {
        self.0.get(&provider).ok_or(QuarkError::ProviderUnsupported)
    }

    pub fn cancel(&self, provider: CloudProvider) {
        if let Some(flow) = self.0.get(&provider) {
            flow.cancel();
        }
    }
}

#[cfg(test)]
mod tests;
