use alldownloader_lib::cloud::{
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CredentialStore, DpapiCredentialStore,
    MemoryCredentialStore, PikPakClient, QuarkError,
};
use std::sync::Mutex;

#[test]
fn legacy_pikpak_password_session_recovers_its_login_method() {
    let store = MemoryCredentialStore::default();
    store.save("pikpak", br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device","client_id":"YNxT9w7GMdWvEOKa"}"#).unwrap();
    let http = Sequence::new(&[r#"{"sub":"42","name":"alice"}"#]);
    let account = PikPakClient::new(&http, &store).account().unwrap();
    assert_eq!(account.auth_method, alldownloader_lib::cloud::CloudAuthMethod::Password);
}

#[cfg(windows)]
#[test]
fn dpapi_session_roundtrip_never_writes_plaintext() {
    let root = std::env::temp_dir().join(format!("cloud-dpapi-{}", uuid::Uuid::new_v4()));
    let store = DpapiCredentialStore::new(root.clone()).unwrap();
    store
        .save("quark", b"cookie-secret-do-not-persist-in-cleartext")
        .unwrap();
    let bytes = std::fs::read(root.join("quark.bin")).unwrap();
    assert!(!bytes.windows(13).any(|v| v == b"cookie-secret"));
    assert_eq!(
        store.load("quark").unwrap().unwrap(),
        b"cookie-secret-do-not-persist-in-cleartext"
    );
    store.remove("quark").unwrap();
    assert!(store.load("quark").unwrap().is_none());
    std::fs::remove_dir(root).unwrap();
}

struct Sequence(Mutex<(Vec<CloudHttpResponse>, Vec<CloudHttpRequest>)>);

#[test]
fn risk_limited_refreshes_action_captcha_once_and_persists_it() {
    assert_eq!(
        alldownloader_lib::cloud::pikpak_captcha_sign(
            "YNxT9w7GMdWvEOKa",
            "device",
            "1700000000000"
        ),
        "1.c78aafdaece3299f59a7bc4e4c10b2e8"
    );
    let store = MemoryCredentialStore::default();
    store.save("pikpak",br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device","captcha_token":"old-cap","user_id":"user-42"}"#).unwrap();
    let http = Sequence::new(&[
        r#"{"error":"riskLimited","error_code":9}"#,
        r#"{"captcha_token":"fresh-cap","expires_in":300}"#,
        r#"{"files":[],"next_page_token":""}"#,
    ]);
    assert!(PikPakClient::new(&http, &store)
        .list("0")
        .unwrap()
        .items
        .is_empty());
    let requests = &http.0.lock().unwrap().1;
    assert_eq!(requests.len(), 3);
    let payload: serde_json::Value =
        serde_json::from_str(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(payload["action"], "GET:/drive/v1/files");
    assert_eq!(payload["captcha_token"], "old-cap");
    assert_eq!(payload["meta"]["user_id"], "user-42");
    assert_eq!(payload["meta"]["client_version"], "1.53.2");
    assert!(payload["meta"]["captcha_sign"]
        .as_str()
        .unwrap()
        .starts_with("1."));
    assert_eq!(requests[1].headers["Authorization"], "Bearer access");
    assert_eq!(requests[1].headers["X-Device-Id"], "device");
    assert_eq!(requests[2].headers["X-Captcha-Token"], "fresh-cap");
    let session: serde_json::Value =
        serde_json::from_slice(&store.load("pikpak").unwrap().unwrap()).unwrap();
    assert_eq!(session["captcha_token"], "fresh-cap");
}

#[test]
fn risk_limited_retry_is_bounded_and_interactive_challenge_stops_retry() {
    for (middle, expected_requests) in [
        (r#"{"captcha_token":"fresh-cap","expires_in":300}"#, 3),
        (
            r#"{"url":"https://captcha.mypikpak.com/challenge","captcha_token":"pending"}"#,
            2,
        ),
    ] {
        let store = MemoryCredentialStore::default();
        store.save("pikpak",br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device","captcha_token":"old-cap"}"#).unwrap();
        let http = Sequence::new(&[
            r#"{"error":"riskLimited","error_code":9}"#,
            middle,
            r#"{"error":"riskLimited","error_code":9}"#,
        ]);
        assert!(matches!(
            PikPakClient::new(&http, &store).list("0"),
            Err(QuarkError::WebAuthorizationRequired)
        ));
        assert_eq!(http.0.lock().unwrap().1.len(), expected_requests);
    }
}
impl Sequence {
    fn new(bodies: &[&str]) -> Self {
        Self(Mutex::new((
            bodies
                .iter()
                .map(|b| CloudHttpResponse {
                    status: 200,
                    body: (*b).into(),
                    ..Default::default()
                })
                .rev()
                .collect(),
            Vec::new(),
        )))
    }
}
impl CloudHttpTransport for Sequence {
    fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let mut s = self.0.lock().unwrap();
        s.1.push(r);
        Ok(s.0.pop().expect("unexpected network request"))
    }
}

#[test]
fn pikpak_password_signin_persists_tokens_but_never_password() {
    let http = Sequence::new(&[
        r#"{"captcha_token":"cap"}"#,
        r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600,"sub":"42"}"#,
    ]);
    let store = MemoryCredentialStore::default();
    PikPakClient::new(&http, &store)
        .login("me@example.com", "ephemeral-password")
        .unwrap();
    let session = String::from_utf8(store.load("pikpak").unwrap().unwrap()).unwrap();
    assert!(session.contains("refresh"));
    assert!(!session.contains("ephemeral-password"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&session).unwrap()["auth_method"],
        "password"
    );
    let s = http.0.lock().unwrap();
    assert!(s.1[0].url.ends_with("/v1/shield/captcha/init"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(s.1[0].body.as_ref().unwrap()).unwrap()["action"],
        "POST:https://user.mypikpak.com/v1/auth/signin"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(s.1[1].body.as_ref().unwrap()).unwrap()
            ["captcha_token"],
        "cap"
    );
}

#[test]
fn pikpak_interactive_captcha_does_not_attempt_password_signin() {
    let http = Sequence::new(&[
        r#"{"url":"https://captcha.mypikpak.com/challenge","captcha_token":"cap"}"#,
    ]);
    let store = MemoryCredentialStore::default();
    assert!(matches!(
        PikPakClient::new(&http, &store).login("me@example.com", "password"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert_eq!(http.0.lock().unwrap().1.len(), 1);
}

#[test]
fn pikpak_lists_all_pages_and_rotates_expired_refresh_token() {
    let store = MemoryCredentialStore::default();
    store.save("pikpak",br#"{"access_token":"old","refresh_token":"old-refresh","expires_at":0,"device_id":"device","captcha_token":""}"#).unwrap();
    let http = Sequence::new(&[
        r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#,
        r#"{"files":[{"id":"one","name":"a","kind":"drive#folder","parent_id":""}],"next_page_token":"next"}"#,
        r#"{"files":[{"id":"two","name":"b","kind":"drive#file","size":"12","parent_id":""}],"next_page_token":""}"#,
    ]);
    let directory = PikPakClient::new(&http, &store).list("0").unwrap();
    assert_eq!(directory.items.len(), 2);
    assert_eq!(directory.items[1].size, Some(12));
    assert!(String::from_utf8(store.load("pikpak").unwrap().unwrap())
        .unwrap()
        .contains("rotated"));
    assert!(http.0.lock().unwrap().1[2].url.contains("page_token=next"));
}

#[test]
fn cloud_enqueue_returns_real_task_and_records_engine_failure() {
    use alldownloader_lib::{cloud::commands::enqueue_cloud_download, TaskStatus, TaskStore};
    let store = TaskStore::open_in_memory().unwrap();
    let task = enqueue_cloud_download(
        &store,
        "cloud://quark/real-file",
        "video.mp4",
        std::path::Path::new("D:/Downloads"),
        |source| {
            assert_eq!(source, "cloud://quark/real-file");
            Ok("0123456789abcdef".into())
        },
    )
    .unwrap();
    assert_eq!(task.source, "cloud://quark/real-file");
    assert_eq!(task.status, TaskStatus::Downloading);
    assert_eq!(task.aria2_gid.as_deref(), Some("0123456789abcdef"));
    assert!(enqueue_cloud_download(
        &store,
        "https://example.com/fail",
        "bad",
        std::path::Path::new("D:/Downloads"),
        |_| Err("engine offline".into())
    )
    .is_err());
    assert_eq!(store.list().unwrap()[0].status, TaskStatus::Failed);
}

#[test]
fn web_tokens_are_required_and_password_fields_are_discarded() {
    use alldownloader_lib::cloud::commands::parse_web_tokens;
    let s = parse_web_tokens(
        r#"{"access_token":"access","refresh_token":"refresh","password":"must-not-persist"}"#,
    )
    .unwrap();
    assert!(!serde_json::to_string(&s)
        .unwrap()
        .contains("must-not-persist"));
    assert_eq!(serde_json::to_value(&s).unwrap()["auth_method"], "web");
    assert!(parse_web_tokens(r#"{"access_token":"access"}"#).is_err());
    assert_eq!(
        serde_json::to_value(parse_web_tokens("null").err().unwrap()).unwrap()["code"],
        "authorization_pending"
    );
}

#[test]
fn web_refresh_keeps_public_web_client_instead_of_android_client() {
    use alldownloader_lib::cloud::commands::parse_web_tokens;
    let s = parse_web_tokens(
        r#"{"access_token":"web-access","refresh_token":"web-refresh","client_id":"web-client"}"#,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(s).unwrap()["client_id"], "web-client");
}

#[test]
fn anonymous_web_cookie_keeps_authorization_pending() {
    use alldownloader_lib::cloud::commands::authorization_result;
    let http = Sequence::new(&[r#"{"status":401,"code":31001}"#]);
    let error = authorization_result(
        alldownloader_lib::cloud::QuarkClient::new("anonymous=1".into(), &http).account(),
    )
    .err()
    .unwrap();
    assert_eq!(
        serde_json::to_value(error).unwrap()["code"],
        "authorization_pending"
    );
}
