use super::*;
use std::io::{Read, Write};
use std::net::TcpStream;

fn failure_code(error: FlowError) -> QuarkError {
    match error {
        FlowError::Denied => QuarkError::AuthorizationExpired,
        FlowError::Failed => QuarkError::InvalidResponse,
        FlowError::Timeout => QuarkError::AuthorizationPending,
    }
}

fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn get(port: u16, target: &str, language: Option<&str>) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let language = language
        .map(|value| format!("Accept-Language: {value}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET {target} HTTP/1.1\r\nHost: localhost\r\n{language}\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

struct Started {
    redirect_uri: String,
    port: u16,
    state: String,
    challenge: String,
}

fn begin(pending: &PendingAuthorization, redirect: Redirect, timeout: Duration) -> Started {
    let url = pending
        .begin(redirect, timeout, failure_code, |request| {
            format!(
                "https://idp.example/authorize?redirect_uri={}&state={}&code_challenge={}",
                url::form_urlencoded::byte_serialize(request.redirect_uri.as_bytes())
                    .collect::<String>(),
                request.state,
                request.challenge
            )
        })
        .unwrap();
    let params = query(&url);
    let redirect_uri = params["redirect_uri"].clone();
    Started {
        port: url::Url::parse(&redirect_uri).unwrap().port().unwrap(),
        redirect_uri,
        state: params["state"].clone(),
        challenge: params["code_challenge"].clone(),
    }
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
fn pkce_challenge_is_the_url_safe_sha256_of_the_verifier() {
    // RFC 7636 appendix B.
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(
        "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".as_bytes(),
    ));
    assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let pkce = Pkce::generate();
    assert_eq!(pkce.verifier.len(), 43);
    assert!(pkce
        .verifier
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    assert_eq!(
        pkce.challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()))
    );
    assert_ne!(*pkce.verifier, *Pkce::generate().verifier);
    assert!(random_state().len() >= 24);
}

#[test]
fn the_redirect_follows_the_providers_registered_shape() {
    let pending = PendingAuthorization::default();
    let ip = begin(&pending, Redirect::LOOPBACK_IP, Duration::from_secs(30));
    assert!(ip.redirect_uri.starts_with("http://127.0.0.1:") && !ip.redirect_uri.ends_with('/'));
    let named = begin(&pending, Redirect::LOCALHOST, Duration::from_secs(30));
    assert!(
        named.redirect_uri.starts_with("http://localhost:") && named.redirect_uri.ends_with('/')
    );
}

#[test]
fn the_loopback_accepts_only_the_redirect_with_the_right_state() {
    for redirect in [Redirect::LOOPBACK_IP, Redirect::LOCALHOST] {
        let pending = PendingAuthorization::default();
        let started = begin(&pending, redirect, Duration::from_secs(30));
        assert!(pending.poll().unwrap().is_none());
        // Probes and forged redirects must not end the flow.
        assert!(get(started.port, "/favicon.ico", None).starts_with("HTTP/1.1 404"));
        assert!(get(started.port, "/?code=forged&state=WRONG", None).starts_with("HTTP/1.1 400"));
        assert!(get(started.port, "/?code=forged", None).starts_with("HTTP/1.1 400"));
        assert!(pending.poll().unwrap().is_none());
        let page = get(
            started.port,
            &format!("/?code=4%2FREAL&state={}", started.state),
            Some("zh-CN,zh;q=0.9"),
        );
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("授权已完成") && !page.contains("Authorization complete"));
        let answer = wait_for_answer(&pending).unwrap();
        assert_eq!(*answer.code, "4/REAL");
        assert_eq!(answer.redirect_uri, started.redirect_uri);
        // The verifier belongs to the challenge that was sent to the provider.
        assert_eq!(
            started.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(answer.verifier.as_bytes()))
        );
        // Consumed: nothing pending any more.
        assert!(matches!(
            pending.poll(),
            Err(QuarkError::WebAuthorizationRequired)
        ));
    }
}

#[test]
fn the_landing_page_follows_the_browser_language() {
    let pending = PendingAuthorization::default();
    let started = begin(&pending, Redirect::LOOPBACK_IP, Duration::from_secs(30));
    let page = get(
        started.port,
        &format!("/?code=c&state={}", started.state),
        Some("en-US,en;q=0.8"),
    );
    assert!(page.contains("Authorization complete") && !page.contains("授权已完成"));
    wait_for_answer(&pending).unwrap();
    // Without a language header (a bare client) both are shown.
    let started = begin(&pending, Redirect::LOCALHOST, Duration::from_secs(30));
    let page = get(
        started.port,
        &format!("/?code=c&state={}", started.state),
        None,
    );
    assert!(page.contains("授权已完成") && page.contains("Authorization complete"));
    wait_for_answer(&pending).unwrap();
}

#[test]
fn declining_or_a_provider_error_end_the_flow_once() {
    let pending = PendingAuthorization::default();
    let started = begin(&pending, Redirect::LOOPBACK_IP, Duration::from_secs(30));
    let page = get(
        started.port,
        &format!("/?error=access_denied&state={}", started.state),
        Some("en"),
    );
    assert!(page.contains("not completed"));
    assert!(matches!(
        wait_for_answer(&pending),
        Err(QuarkError::AuthorizationExpired)
    ));
    // The failure was the final answer: it is not reported again.
    assert!(matches!(
        pending.poll(),
        Err(QuarkError::WebAuthorizationRequired)
    ));

    let started = begin(&pending, Redirect::LOOPBACK_IP, Duration::from_secs(30));
    get(
        started.port,
        &format!("/?error=server_error&state={}", started.state),
        None,
    );
    assert!(matches!(
        wait_for_answer(&pending),
        Err(QuarkError::InvalidResponse)
    ));
    // A redirect with the right state but no code is a failure, not a wait.
    let started = begin(&pending, Redirect::LOOPBACK_IP, Duration::from_secs(30));
    get(started.port, &format!("/?state={}", started.state), None);
    assert!(matches!(
        wait_for_answer(&pending),
        Err(QuarkError::InvalidResponse)
    ));
}

#[test]
fn an_unanswered_browser_times_out_and_a_new_attempt_cancels_the_old_one() {
    let pending = PendingAuthorization::default();
    begin(&pending, Redirect::LOOPBACK_IP, Duration::from_millis(150));
    assert!(matches!(
        wait_for_answer(&pending),
        Err(QuarkError::AuthorizationPending)
    ));
    let first = begin(&pending, Redirect::LOCALHOST, Duration::from_secs(30));
    let second = begin(&pending, Redirect::LOCALHOST, Duration::from_secs(30));
    assert_ne!(first.port, second.port);
    // The first listener is gone: nothing answers on its port any more.
    std::thread::sleep(Duration::from_millis(200));
    assert!(TcpStream::connect(("127.0.0.1", first.port)).is_err());
    assert!(TcpStream::connect(("127.0.0.1", second.port)).is_ok());
    assert!(pending.poll().unwrap().is_none());
    pending.cancel();
    assert!(matches!(
        pending.poll(),
        Err(QuarkError::WebAuthorizationRequired)
    ));
}

#[test]
fn every_oauth_provider_has_its_own_pending_authorization() {
    let flows = OAuthFlows::default();
    for provider in CloudProvider::ALL {
        assert_eq!(
            flows.get(provider).is_ok(),
            provider.uses_oauth(),
            "{provider:?}"
        );
    }
    let microsoft = flows.get(CloudProvider::Onedrive).unwrap();
    begin(microsoft, Redirect::LOOPBACK_IP, Duration::from_secs(30));
    flows.cancel(CloudProvider::Onedrive);
    assert!(matches!(
        microsoft.poll(),
        Err(QuarkError::WebAuthorizationRequired)
    ));
}
