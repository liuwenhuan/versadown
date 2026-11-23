use super::*;
use crate::cloud::gdrive::test_support::{rule, ScriptedGoogle};
use base64::Engine;
use sha2::Digest;

fn client() -> OAuthClient {
    OAuthClient {
        id: "client-id.apps.googleusercontent.com".into(),
        secret: Some("desktop-secret".into()),
    }
}

fn query(url: &str) -> std::collections::HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

#[test]
fn authorization_url_requests_readonly_offline_access_with_pkce_and_no_secret() {
    let url = authorization_url(&client(), "http://127.0.0.1:5555", "CHALLENGE", "STATE");
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    let params = query(&url);
    assert_eq!(params["client_id"], "client-id.apps.googleusercontent.com");
    assert_eq!(params["redirect_uri"], "http://127.0.0.1:5555");
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["scope"], "https://www.googleapis.com/auth/drive.readonly");
    assert_eq!(params["code_challenge"], "CHALLENGE");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["state"], "STATE");
    assert_eq!(params["access_type"], "offline");
    assert!(!url.contains("desktop-secret"));
}

fn token_body(refresh: bool) -> String {
    let refresh = if refresh {
        r#","refresh_token":"R-1""#
    } else {
        ""
    };
    format!(r#"{{"access_token":"A-1","expires_in":3599,"token_type":"Bearer"{refresh}}}"#)
}

#[test]
fn code_exchange_posts_the_verifier_and_returns_both_tokens() {
    let http = ScriptedGoogle::new(vec![rule("oauth2.googleapis.com/token", vec![(200, &token_body(true))])]);
    let tokens = exchange_code(&http, &client(), "CODE", "VERIFIER", "http://127.0.0.1:5555").unwrap();
    assert_eq!(tokens.access_token, "A-1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("R-1"));
    assert_eq!(tokens.expires_in, 3599);
    let request = &http.requests.lock().unwrap()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers.get("Content-Type").unwrap(),
        "application/x-www-form-urlencoded"
    );
    let body: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(request.body.as_deref().unwrap().as_bytes())
            .into_owned()
            .collect();
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["code"], "CODE");
    assert_eq!(body["code_verifier"], "VERIFIER");
    assert_eq!(body["redirect_uri"], "http://127.0.0.1:5555");
    assert_eq!(body["client_secret"], "desktop-secret");
}

#[test]
fn a_client_without_secret_sends_none() {
    let http = ScriptedGoogle::new(vec![rule("token", vec![(200, &token_body(false))])]);
    let public = OAuthClient {
        id: "id".into(),
        secret: None,
    };
    refresh_tokens(&http, &public, "R-1").unwrap();
    let body = http.requests.lock().unwrap()[0].body.clone().unwrap();
    assert!(!body.contains("client_secret"));
    assert!(body.contains("grant_type=refresh_token"));
}

#[test]
fn a_revoked_grant_is_an_expired_authorization_and_a_bad_client_is_reported() {
    let revoked = ScriptedGoogle::new(vec![rule(
        "token",
        vec![(400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#)],
    )]);
    assert!(matches!(
        refresh_tokens(&revoked, &client(), "R-1"),
        Err(QuarkError::AuthorizationExpired)
    ));
    let rejected = ScriptedGoogle::new(vec![rule("token", vec![(401, r#"{"error":"invalid_client"}"#)])]);
    assert!(matches!(
        refresh_tokens(&rejected, &client(), "R-1"),
        Err(QuarkError::Gdrive(GdriveFailure::ClientNotAllowed))
    ));
    let broken = ScriptedGoogle::new(vec![rule("token", vec![(200, "not json")])]);
    assert!(matches!(
        refresh_tokens(&broken, &client(), "R-1"),
        Err(QuarkError::InvalidResponse)
    ));
}

#[test]
fn error_text_never_contains_tokens() {
    let http = ScriptedGoogle::new(vec![rule(
        "token",
        vec![(400, r#"{"error":"weird error: R-1 leaked"}"#)],
    )]);
    let error = refresh_tokens(&http, &client(), "R-1").unwrap_err();
    // Only a sanitized identifier survives, never free text from the reply.
    assert!(!error.to_string().contains("R-1"), "{error}");
    assert!(!error.to_string().contains("leaked"), "{error}");
}

fn get(redirect: &str, target: &str) -> String {
    use std::io::{Read, Write};
    let address = redirect.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn wait_for_answer(pending: &PendingAuthorization) -> Result<AuthorizationCode, QuarkError> {
    for _ in 0..300 {
        if let Some(code) = pending.poll()? {
            return Ok(code);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no answer");
}

#[test]
fn the_google_redirect_is_a_plain_loopback_address_and_the_code_comes_back() {
    let pending = PendingAuthorization::default();
    let url = begin(&pending, &client(), Duration::from_secs(30)).unwrap();
    let params = query(&url);
    let redirect = params["redirect_uri"].clone();
    assert!(redirect.starts_with("http://127.0.0.1:") && !redirect.ends_with('/'));
    // Only the redirect carrying this attempt's state completes the flow.
    assert!(get(&redirect, "/?code=forged&state=WRONG").starts_with("HTTP/1.1 400"));
    let page = get(&redirect, &format!("/?code=4%2FREAL&state={}", params["state"]));
    assert!(page.starts_with("HTTP/1.1 200"));
    let answer = wait_for_answer(&pending).unwrap();
    assert_eq!(*answer.code, "4/REAL");
    assert_eq!(answer.redirect_uri, redirect);
    // The verifier belongs to the challenge that was sent to Google.
    assert_eq!(
        params["code_challenge"],
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(answer.verifier.as_bytes()))
    );
}

#[test]
fn declining_consent_and_an_unanswered_browser_are_reported_as_such() {
    let pending = PendingAuthorization::default();
    let url = begin(&pending, &client(), Duration::from_secs(30)).unwrap();
    let params = query(&url);
    get(
        &params["redirect_uri"],
        &format!("/?error=access_denied&state={}", params["state"]),
    );
    match wait_for_answer(&pending) {
        Err(QuarkError::Gdrive(GdriveFailure::ConsentDenied)) => {}
        other => panic!("{:?}", other.err()),
    }
    begin(&pending, &client(), Duration::from_millis(150)).unwrap();
    match wait_for_answer(&pending) {
        Err(QuarkError::Gdrive(GdriveFailure::ConsentTimeout)) => {}
        other => panic!("{:?}", other.err()),
    }
    let url = begin(&pending, &client(), Duration::from_secs(30)).unwrap();
    let params = query(&url);
    get(
        &params["redirect_uri"],
        &format!("/?error=invalid_scope&state={}", params["state"]),
    );
    match wait_for_answer(&pending) {
        Err(QuarkError::Gdrive(GdriveFailure::ClientNotAllowed)) => {}
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn a_build_without_a_client_cannot_sign_in() {
    // Repository builds carry no real client id.
    if crate::cloud::gdrive::OAUTH_CLIENT_ID.is_none() {
        assert!(matches!(
            OAuthClient::from_build(),
            Err(QuarkError::Gdrive(GdriveFailure::NotConfigured))
        ));
    }
}
