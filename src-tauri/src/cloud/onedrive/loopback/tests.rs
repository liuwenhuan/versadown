use super::*;
use crate::cloud::onedrive::session::OnedriveSession;
use crate::cloud::onedrive::test_support::ScriptedHttp;
use crate::cloud::MemoryCredentialStore;
use serde_json::json;
use std::collections::BTreeMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";

fn config() -> OAuthConfig {
    OAuthConfig::with_client_id("11111111-2222-3333-4444-555555555555")
}

struct Started {
    authorize: BTreeMap<String, String>,
    port: u16,
}

fn start(flow: &PendingAuthorization, timeout: Duration) -> Started {
    let url = start_flow(flow, &config(), timeout).unwrap();
    let url = url::Url::parse(&url).unwrap();
    let authorize: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    let redirect = url::Url::parse(&authorize["redirect_uri"]).unwrap();
    assert_eq!(redirect.host_str(), Some("localhost"));
    Started {
        port: redirect.port().unwrap(),
        authorize,
    }
}

/// What a browser does after the consent screen.
async fn browser_get(port: u16, target: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// What the sign-in command does when the UI polls.
fn complete(
    flow: &PendingAuthorization,
    http: &dyn CloudHttpTransport,
    store: &dyn CredentialStore,
) -> Result<CloudAccount, QuarkError> {
    let answer = flow.poll()?.ok_or(QuarkError::AuthorizationPending)?;
    complete_with(http, store, &config(), &answer)
}

fn graph_http() -> ScriptedHttp {
    ScriptedHttp::new()
        .on(
            "POST",
            TOKEN_URL,
            vec![(
                200,
                json!({"access_token": "AT", "refresh_token": "RT", "expires_in": 3600}),
            )],
        )
        .on(
            "GET",
            &format!("{GRAPH}/me/drive"),
            vec![(
                200,
                json!({"id": "D1", "owner": {"user": {"id": "U-1", "displayName": "Ada"}}}),
            )],
        )
}

#[tokio::test]
async fn the_authorization_request_is_pkce_with_a_loopback_redirect() {
    let flow = PendingAuthorization::default();
    let started = start(&flow, Duration::from_secs(30));
    assert_eq!(started.authorize["code_challenge_method"], "S256");
    assert_eq!(started.authorize["scope"], "Files.Read offline_access");
    assert!(started.authorize["redirect_uri"].starts_with("http://localhost:"));
    assert!(started.authorize["state"].len() >= 24);
}

#[tokio::test]
async fn a_valid_redirect_is_exchanged_for_stored_tokens() {
    let flow = PendingAuthorization::default();
    let started = start(&flow, Duration::from_secs(30));
    let http = graph_http();
    let store = MemoryCredentialStore::default();
    assert!(matches!(
        complete(&flow, &http, &store),
        Err(QuarkError::AuthorizationPending)
    ));

    // A stray request (favicon) and a forged state do not end the wait.
    assert!(browser_get(started.port, "/favicon.ico")
        .await
        .starts_with("HTTP/1.1 404"));
    assert!(browser_get(started.port, "/?code=evil&state=forged")
        .await
        .starts_with("HTTP/1.1 400"));
    settle().await;
    assert!(matches!(
        complete(&flow, &http, &store),
        Err(QuarkError::AuthorizationPending)
    ));

    let target = format!("/?code=THE-CODE&state={}", started.authorize["state"]);
    let page = browser_get(started.port, &target).await;
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("VersaDown"));
    settle().await;

    let account = complete(&flow, &http, &store).unwrap();
    assert_eq!(
        (account.id.as_str(), account.display_name.as_str()),
        ("U-1", "Ada")
    );
    let session = OnedriveSession::load(&store).unwrap().unwrap();
    assert_eq!(
        (
            session.access_token.as_str(),
            session.refresh_token.as_str()
        ),
        ("AT", "RT")
    );

    // The code goes out with the PKCE verifier matching the challenge, and the
    // same redirect URI the browser was sent to.
    let form: BTreeMap<_, _> = url::form_urlencoded::parse(
        http.sent("POST", TOKEN_URL)[0]
            .body
            .as_deref()
            .unwrap()
            .as_bytes(),
    )
    .into_owned()
    .collect();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "THE-CODE");
    assert_eq!(form["redirect_uri"], started.authorize["redirect_uri"]);
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use sha2::{Digest, Sha256};
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
        started.authorize["code_challenge"]
    );
    // The flow is consumed: a second poll has nothing to complete.
    assert!(matches!(
        complete(&flow, &http, &store),
        Err(QuarkError::WebAuthorizationRequired)
    ));
}

#[tokio::test]
async fn refusing_the_consent_screen_is_a_distinct_failure() {
    let flow = PendingAuthorization::default();
    let started = start(&flow, Duration::from_secs(30));
    let target = format!(
        "/?error=access_denied&error_description=nope&state={}",
        started.authorize["state"]
    );
    browser_get(started.port, &target).await;
    settle().await;
    let store = MemoryCredentialStore::default();
    assert!(matches!(
        complete(&flow, &graph_http(), &store),
        Err(QuarkError::Onedrive(OnedriveFailure::AuthorizationDenied))
    ));
    // The refusal was the final answer: a later poll has nothing to complete.
    assert!(matches!(
        complete(&flow, &graph_http(), &store),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(OnedriveSession::load(&store).unwrap().is_none());
}

#[tokio::test]
async fn an_unfinished_sign_in_times_out() {
    let flow = PendingAuthorization::default();
    start(&flow, Duration::from_millis(80));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(
        complete(&flow, &graph_http(), &MemoryCredentialStore::default()),
        Err(QuarkError::Onedrive(OnedriveFailure::AuthorizationTimeout))
    ));
}

#[tokio::test]
async fn a_rejected_code_exchange_stores_nothing() {
    let flow = PendingAuthorization::default();
    let started = start(&flow, Duration::from_secs(30));
    let target = format!("/?code=BAD&state={}", started.authorize["state"]);
    browser_get(started.port, &target).await;
    settle().await;
    let http = ScriptedHttp::new().on(
        "POST",
        TOKEN_URL,
        vec![(400, json!({"error": "invalid_grant"}))],
    );
    let store = MemoryCredentialStore::default();
    assert!(complete(&flow, &http, &store).is_err());
    assert!(OnedriveSession::load(&store).unwrap().is_none());
}

#[tokio::test]
async fn starting_again_cancels_the_previous_listener() {
    let flow = PendingAuthorization::default();
    let first = start(&flow, Duration::from_secs(30));
    let second = start(&flow, Duration::from_secs(30));
    settle().await;
    assert_ne!(first.port, second.port);
    assert!(TcpStream::connect(("127.0.0.1", first.port)).await.is_err());
    assert!(TcpStream::connect(("127.0.0.1", second.port)).await.is_ok());
}

#[tokio::test]
async fn abandoning_the_sign_in_closes_the_listener() {
    // What the command does when the browser cannot be opened.
    let flow = PendingAuthorization::default();
    let started = start(&flow, Duration::from_secs(30));
    flow.cancel();
    settle().await;
    assert!(TcpStream::connect(("127.0.0.1", started.port)).await.is_err());
    assert!(matches!(
        complete(&flow, &graph_http(), &MemoryCredentialStore::default()),
        Err(QuarkError::WebAuthorizationRequired)
    ));
}
