use alldownloader_lib::cloud::{
    xunlei::{
        XunleiClient, XunleiClientProfile, XunleiResumableUpload, XunleiSession, XunleiWebContext,
    },
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport, QuarkError,
    UploadHttpRequest, UploadHttpResponse,
};
use std::{io::Write, sync::Mutex};

struct RecordingHttp {
    responses: Mutex<Vec<CloudHttpResponse>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
    upload_responses: Mutex<Vec<UploadHttpResponse>>,
    upload_requests: Mutex<Vec<UploadHttpRequest>>,
}

impl RecordingHttp {
    fn sequence(bodies: &[&str]) -> Self {
        Self {
            responses: Mutex::new(
                bodies
                    .iter()
                    .map(|body| CloudHttpResponse {
                        status: 200,
                        body: (*body).into(),
                        ..Default::default()
                    })
                    .rev()
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
            upload_responses: Mutex::new(Vec::new()),
            upload_requests: Mutex::new(Vec::new()),
        }
    }

    fn challenge() -> Self {
        Self::sequence(&[r#"{"url":"https://verify.xunlei.example/challenge"}"#])
    }

    fn requests(&self) -> Vec<CloudHttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn with_upload_responses(
        mut self,
        responses: impl IntoIterator<Item = UploadHttpResponse>,
    ) -> Self {
        let mut responses = responses.into_iter().collect::<Vec<_>>();
        responses.reverse();
        self.upload_responses = Mutex::new(responses);
        self
    }
}

impl CloudHttpTransport for RecordingHttp {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop()
            .ok_or(QuarkError::InvalidResponse)
    }
}

impl CloudUploadTransport for RecordingHttp {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        self.upload_requests.lock().unwrap().push(request);
        self.upload_responses
            .lock()
            .unwrap()
            .pop()
            .ok_or(QuarkError::InvalidResponse)
    }
}

fn profile() -> XunleiClientProfile {
    XunleiClientProfile {
        client_id: "official-browser-client".into(),
        client_secret: "browser-client-secret".into(),
        device_id: "device-1".into(),
        client_version: "1.0.0".into(),
        package_name: "com.xunlei.pan.web".into(),
        user_agent: "Mozilla/5.0 XunleiBrowser/1.0".into(),
    }
}

fn web_context() -> XunleiWebContext {
    XunleiWebContext {
        session_id: "web-session".into(),
        peer_id: "official-peer".into(),
        device_id: "official-device".into(),
        credit_key: "official-credit".into(),
    }
}

// Break caught: omitting the native refresh exchange, token rotation, or either
// required authenticated-drive header makes the directory request invalid.
#[test]
fn xunlei_refreshes_with_official_client_profile_and_sends_native_headers() {
    let http = RecordingHttp::sequence(&[
        r#"{"access_token":"access","refresh_token":"next","expires_in":7200,"sub":"user-1"}"#,
        r#"{"files":[{"id":"f1","name":"archive.zip","kind":"drive#file","phase":"PHASE_TYPE_COMPLETE","size":32,"parent_id":"0"}],"next_page_token":""}"#,
    ]);
    let client = XunleiClient::new(XunleiSession::refresh("refresh", profile()), &http);

    let directory = client.directory("0").unwrap();

    assert_eq!(directory.items[0].name, "archive.zip");
    assert_eq!(client.session().refresh_token, "next");
    let requests = http.requests();
    assert!(requests[0].url.ends_with("/v1/auth/token"));
    let refresh: serde_json::Value =
        serde_json::from_str(requests[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(refresh["grant_type"], "refresh_token");
    assert_eq!(refresh["client_id"], "official-browser-client");
    assert_eq!(refresh["client_secret"], "browser-client-secret");
    assert_eq!(
        requests[0].headers["X-Client-Id"],
        "official-browser-client"
    );
    assert_eq!(requests[0].headers["X-Client-Version"], "1.0.0");
    assert_eq!(requests[0].headers["X-Device-Id"], "device-1");
    assert_eq!(
        requests[0].headers["User-Agent"],
        "Mozilla/5.0 XunleiBrowser/1.0"
    );
    assert_eq!(requests[1].headers["Authorization"], "Bearer access");
    assert_eq!(requests[1].headers["X-Device-Id"], "device-1");
    assert_eq!(
        requests[1].headers["X-Client-Id"],
        "official-browser-client"
    );
    assert_eq!(requests[1].headers["X-Client-Version"], "1.0.0");
    assert_eq!(
        requests[1].headers["User-Agent"],
        "Mozilla/5.0 XunleiBrowser/1.0"
    );
}

// Break caught: Xunlei-specific expired-session codes would otherwise leak as
// generic provider errors and prevent the one native refresh retry.
#[test]
fn xunlei_expiry_codes_map_to_authorization_expired_before_generic_json_mapping() {
    for code in [4121, 10] {
        let http =
            RecordingHttp::sequence(&[&format!(r#"{{"error_code":{code},"error":"expired"}}"#)]);
        assert!(matches!(
            XunleiClient::new(XunleiSession::with_access("access", "", profile()), &http)
                .directory("0"),
            Err(QuarkError::AuthorizationExpired)
        ));
    }
}

// Break caught: diagnostics must not disclose session tokens, client secrets,
// or one-shot resumable-upload credentials.
#[test]
fn xunlei_sensitive_debug_output_is_redacted() {
    let mut session = XunleiSession::with_access("access-secret", "refresh-secret", profile());
    session.security_token = "captcha-secret".into();
    session.web_context = XunleiWebContext {
        session_id: "session-cookie-secret".into(),
        peer_id: "peer-cookie-secret".into(),
        device_id: "device-cookie-secret".into(),
        credit_key: "credit-cookie-secret".into(),
    };
    let upload = XunleiResumableUpload {
        upload_type: "UPLOAD_TYPE_RESUMABLE".into(),
        kind: "drive#resumable".into(),
        provider: "aws".into(),
        access_key_id: "access-key-secret".into(),
        access_key_secret: "upload-key-secret".into(),
        bucket: "bucket".into(),
        endpoint: "https://oss.xunlei.example".into(),
        expiration: "2030-01-01T00:00:00Z".into(),
        key: "object-key".into(),
        security_token: "upload-security-secret".into(),
    };

    let debug = format!("{session:?} {upload:?}");
    for sensitive in [
        "access-secret",
        "refresh-secret",
        "browser-client-secret",
        "captcha-secret",
        "session-cookie-secret",
        "peer-cookie-secret",
        "device-cookie-secret",
        "credit-cookie-secret",
        "access-key-secret",
        "upload-key-secret",
        "upload-security-secret",
    ] {
        assert!(!debug.contains(sensitive));
    }
}

#[test]
fn xunlei_web_session_retains_a_current_browser_access_token_without_a_client_secret() {
    // Break caught: requiring a native-only client secret rejects a valid
    // browser session before the command layer can save it and close the
    // official authorization window.
    let mut web_profile = profile();
    web_profile.client_secret.clear();

    let session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        web_profile,
        0,
    );

    assert!(session.is_ok());
    assert_eq!(session.unwrap().access_token, "current-browser-access");
}

#[test]
fn xunlei_web_session_without_native_profile_uses_the_current_browser_bearer() {
    // The official browser client exposes the user session but deliberately
    // does not expose its native client identity.  A completed browser login
    // must still be storable and use its current bearer for the first drive
    // request instead of remaining permanently pending.
    let mut session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .expect("official browser session without native identity is usable");
    session.web_context = web_context();
    let http = RecordingHttp::sequence(&[r#"{"files":[],"next_page_token":""}"#]);
    let client = XunleiClient::new(session, &http);

    assert!(client.directory("0").is_ok());
    let request = http.requests().pop().expect("one directory request");
    assert_eq!(
        request.headers.get("Authorization"),
        Some(&"Bearer current-browser-access".into())
    );
    assert!(!request.headers.contains_key("X-Client-Id"));
}

#[test]
fn xunlei_old_web_session_without_browser_cookie_context_requires_reauthorization() {
    // A pre-cookie-capture session must fail closed. Sending guessed peer or
    // session headers reaches Xunlei but is rejected as `invalid_argument`.
    let session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    let http = RecordingHttp::sequence(&[r#"{"files":[],"next_page_token":""}"#]);

    assert!(matches!(
        XunleiClient::new(session, &http).directory("0"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(http.requests().is_empty());
}

#[test]
fn xunlei_old_web_session_without_browser_cookie_context_cannot_restore_an_account() {
    let session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    let http = RecordingHttp::sequence(&[]);

    assert!(matches!(
        XunleiClient::new(session, &http).web_account(),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(http.requests().is_empty());
}

#[test]
fn xunlei_web_session_prefers_the_captured_sessionid_over_the_bearer() {
    let mut session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        XunleiClientProfile {
            user_agent: "OfficialBrowser/1.0".into(),
            ..XunleiClientProfile::default()
        },
        0,
    )
    .unwrap();
    session.web_context = XunleiWebContext {
        session_id: "web-session".into(),
        peer_id: "official-peer".into(),
        device_id: "official-device".into(),
        credit_key: "official-credit".into(),
    };
    let http = RecordingHttp::sequence(&[r#"{"files":[],"next_page_token":""}"#]);

    XunleiClient::new(session, &http).directory("0").unwrap();

    let request = http.requests().pop().unwrap();
    assert_eq!(
        request.headers.get("Peer-Id"),
        Some(&"official-peer".into())
    );
    assert_eq!(
        request.headers.get("Guid"),
        Some(&format!("{:x}", md5::compute(b"official-peer")))
    );
    assert_eq!(request.headers.get("User-Id"), Some(&"web-user-42".into()));
    assert_eq!(
        request.headers.get("Session-Id"),
        Some(&"web-session".into())
    );
    assert_eq!(
        request.headers.get("Credit-Key"),
        Some(&"official-credit".into())
    );
    assert_eq!(request.headers.get("Version-Code"), Some(&"1.92.98".into()));
    assert_eq!(
        request.headers.get("User-Agent"),
        Some(&"OfficialBrowser/1.0".into())
    );
    assert!(!request.headers.contains_key("Cookie"));
    let query = url::Url::parse(&request.url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    assert!(query.contains(&("_sessid".into(), "web-session".into())));
    assert!(query.contains(&("_uid".into(), "web-user-42".into())));
    assert!(query
        .iter()
        .any(|(key, value)| key == "_h[]" && value == "Credit-Key: official-credit"));
}

#[test]
fn xunlei_web_context_without_sessionid_falls_back_to_the_current_bearer() {
    let mut session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    session.web_context = XunleiWebContext {
        session_id: String::new(),
        peer_id: "official-peer".into(),
        device_id: "official-device".into(),
        credit_key: "official-credit".into(),
    };
    let http = RecordingHttp::sequence(&[r#"{"files":[],"next_page_token":""}"#]);

    XunleiClient::new(session, &http).directory("0").unwrap();

    let request = http.requests().pop().unwrap();
    assert_eq!(
        request.headers.get("Session-Id"),
        Some(&"current-browser-access".into())
    );
}

#[test]
fn xunlei_web_session_with_real_browser_identity_but_no_secret_uses_current_bearer() {
    // The official web client may expose its browser device/profile without
    // exposing a native refresh secret. That identity must be forwarded to
    // the drive request, but it must never make a still-current web bearer
    // look refreshable or expired.
    let mut web_profile = profile();
    web_profile.client_secret.clear();
    let mut session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        web_profile,
        0,
    )
    .expect("official web identity without a native secret is usable");
    session.web_context = web_context();
    let http = RecordingHttp::sequence(&[r#"{"files":[],"next_page_token":""}"#]);
    let client = XunleiClient::new(session, &http);

    assert!(client.directory("0").is_ok());
    let request = http.requests().pop().expect("one drive request");
    assert_eq!(
        request.headers.get("Authorization"),
        Some(&"Bearer current-browser-access".into())
    );
    assert_eq!(
        request.headers.get("X-Client-Id"),
        Some(&"official-browser-client".into())
    );
}

#[test]
fn xunlei_web_session_builds_a_display_account_without_network_or_refresh() {
    // Break caught: using account() to finish browser authorization tries a
    // native refresh requiring a secret that the web client never exposes.
    let mut web_profile = profile();
    web_profile.client_secret.clear();
    let mut session = XunleiSession::from_web_authorization(
        "current-browser-access",
        "browser-refresh",
        "web-user-42",
        "",
        web_profile,
        0,
    )
    .unwrap();
    session.web_context = web_context();
    let http = RecordingHttp::sequence(&[]);
    let client = XunleiClient::new(session, &http);

    let account = client.web_account().unwrap();

    assert_eq!(account.id, "web-user-42");
    assert_eq!(account.display_name, "web-user-42");
    assert!(http.requests().is_empty());
}

// Break caught: treating an interactive security challenge as an ordinary
// provider failure prevents the UI from returning the user to official web auth.
#[test]
fn xunlei_security_challenge_is_web_auth_not_link_expiry() {
    assert!(matches!(
        XunleiClient::new(
            XunleiSession::with_access("access", "refresh", profile()),
            &RecordingHttp::challenge()
        )
        .directory("0"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
}

// Break caught: accepting a non-TLS content link would expose downloads to a
// transport downgrade rather than returning a fresh secure URL.
#[test]
fn xunlei_download_returns_only_fresh_https_content_links() {
    let https = RecordingHttp::sequence(&[
        r#"{"id":"f1","name":"archive.zip","web_content_link":"https://cdn.xunlei.example/archive.zip?signature=short"}"#,
    ]);
    let client = XunleiClient::new(
        XunleiSession::with_access("access", "refresh", profile()),
        &https,
    );
    assert_eq!(
        client.download("f1").unwrap(),
        (
            "https://cdn.xunlei.example/archive.zip?signature=short".into(),
            "archive.zip".into()
        )
    );
    assert_eq!(https.requests()[0].headers["X-Device-Id"], "device-1");

    let insecure = RecordingHttp::sequence(&[
        r#"{"id":"f1","name":"archive.zip","web_content_link":"http://cdn.xunlei.example/archive.zip"}"#,
    ]);
    assert!(matches!(
        XunleiClient::new(
            XunleiSession::with_access("access", "refresh", profile()),
            &insecure
        )
        .download("f1"),
        Err(QuarkError::InvalidResponse)
    ));
}

// Break caught: changing a native mutation endpoint or payload would silently
// perform the wrong file operation or the wrong destination.
#[test]
fn xunlei_file_mutations_and_upload_initialization_use_native_drive_contracts() {
    let http = RecordingHttp::sequence(&[
        "{}",
        "{}",
        "{}",
        "{}",
        "{}",
        "{}",
        r#"{"upload_type":"UPLOAD_TYPE_RESUMABLE","resumable":{"kind":"drive#resumable","provider":"aws","params":{"access_key_id":"key-id","access_key_secret":"key-secret","bucket":"bucket","endpoint":"https://oss.xunlei.example","expiration":"2030-01-01T00:00:00Z","key":"object-key","security_token":"upload-token"}}}"#,
    ]);
    let client = XunleiClient::new(
        XunleiSession::with_access("access", "refresh", profile()),
        &http,
    );
    let ids = vec!["file-1".to_owned(), "folder-2".to_owned()];

    client.mkdir("parent", "new-folder").unwrap();
    client.rename("file-1", "renamed.zip").unwrap();
    client.move_files(&ids, "destination").unwrap();
    client.copy_files(&ids, "destination").unwrap();
    client.trash(&ids).unwrap();
    let upload = client
        .upload_init("parent", "upload.bin", 64, "content-gcid")
        .unwrap();
    assert_eq!(upload.provider, "aws");
    assert_eq!(upload.endpoint, "https://oss.xunlei.example");
    assert_eq!(upload.key, "object-key");

    let requests = http.requests();
    assert!(requests[0].url.ends_with("/drive/v1/files"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[0].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"kind":"drive#folder","parent_id":"parent","name":"new-folder"})
    );
    assert_eq!(requests[1].method, "PATCH");
    assert!(requests[1].url.ends_with("/drive/v1/files/file-1"));
    assert!(requests[2].url.ends_with("/drive/v1/files:batchMove"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[2].body.as_deref().unwrap()).unwrap()
            ["to"]["parent_id"],
        "destination"
    );
    assert!(requests[3].url.ends_with("/drive/v1/files:batchCopy"));
    assert_eq!(requests[4].method, "PATCH");
    assert!(requests[4].url.ends_with("/drive/v1/files/file-1/trash"));
    assert_eq!(requests[4].body.as_deref(), Some("{}"));
    assert_eq!(requests[5].method, "PATCH");
    assert!(requests[5].url.ends_with("/drive/v1/files/folder-2/trash"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[6].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"kind":"drive#file","parent_id":"parent","name":"upload.bin","size":64,"hash":"content-gcid","upload_type":"UPLOAD_TYPE_RESUMABLE"})
    );
}

// Break caught: showing an unfinished offline/upload task as a ready cloud
// file makes a non-downloadable object available to the user.
#[test]
fn xunlei_directory_requests_and_maps_only_complete_downloadable_files() {
    let http = RecordingHttp::sequence(&[
        r#"{"files":[{"id":"ready","name":"ready.zip","kind":"drive#file","phase":"PHASE_TYPE_COMPLETE","size":"7"},{"id":"running","name":"running.zip","kind":"drive#file","phase":"PHASE_TYPE_RUNNING","size":"3"}],"next_page_token":""}"#,
    ]);
    let client = XunleiClient::new(
        XunleiSession::with_access("access", "refresh", profile()),
        &http,
    );

    let directory = client.directory("0").unwrap();

    assert_eq!(directory.items.len(), 1);
    assert_eq!(directory.items[0].id, "ready");
    let request = http.requests().pop().unwrap();
    assert!(request.url.contains("__type=drive"));
    assert!(request.url.contains("with_audit=true"));
    assert!(request
        .url
        .contains("%22phase%22%3A%7B%22eq%22%3A%22PHASE_TYPE_COMPLETE%22%7D"));
}

#[test]
fn xunlei_upload_file_signs_https_s3_multipart_transfer_and_completes_it() {
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"xunlei-bytes").unwrap();
    let http = RecordingHttp::sequence(&[
        r#"{"upload_type":"UPLOAD_TYPE_RESUMABLE","file":{"id":"file-1","parent_id":"parent","name":"upload.bin","kind":"drive#file","size":12},"resumable":{"kind":"drive#resumable","provider":"aws","params":{"access_key_id":"access-key","access_key_secret":"secret-key","bucket":"bucket","endpoint":"https://s3.xunlei.example","expiration":"2030-01-01T00:00:00Z","key":"objects/upload.bin","security_token":"session-token"}}}"#,
    ])
    .with_upload_responses([
        UploadHttpResponse {
            status: 200,
            headers: Default::default(),
            body: "<InitiateMultipartUploadResult><UploadId>upload-id</UploadId></InitiateMultipartUploadResult>".into(),
        },
        UploadHttpResponse {
            status: 200,
            headers: [("etag".into(), "part-etag".into())].into(),
            body: String::new(),
        },
        UploadHttpResponse {
            status: 200,
            headers: Default::default(),
            body: "<CompleteMultipartUploadResult/>".into(),
        },
    ]);
    let client = XunleiClient::new(
        XunleiSession::with_access("access", "refresh", profile()),
        &http,
    );

    let file = client.upload_file("parent", local.path()).unwrap();

    assert_eq!(file.id, "file-1");
    let uploads = http.upload_requests.lock().unwrap();
    assert_eq!(uploads.len(), 3);
    assert_eq!(uploads[0].method, "POST");
    assert_eq!(
        uploads[0].url,
        "https://bucket.s3.xunlei.example/objects/upload.bin?uploads"
    );
    assert!(
        uploads[0].headers["Authorization"].starts_with("AWS4-HMAC-SHA256 Credential=access-key/")
    );
    assert_eq!(uploads[1].method, "PUT");
    assert!(uploads[1].url.contains("partNumber=1&uploadId=upload-id"));
    assert_eq!(uploads[1].body, b"xunlei-bytes");
    assert_eq!(uploads[2].method, "POST");
    assert!(uploads[2]
        .body
        .windows("part-etag".len())
        .any(|part| part == b"part-etag"));
}
