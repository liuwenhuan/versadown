use alldownloader_lib::cloud::{
    xunlei::{XunleiClient, XunleiClientProfile, XunleiResumableUpload, XunleiSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError,
};
use std::sync::Mutex;

struct RecordingHttp {
    responses: Mutex<Vec<CloudHttpResponse>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
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
        }
    }

    fn challenge() -> Self {
        Self::sequence(&[r#"{"url":"https://verify.xunlei.example/challenge"}"#])
    }

    fn requests(&self) -> Vec<CloudHttpRequest> {
        self.requests.lock().unwrap().clone()
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
        "access-key-secret",
        "upload-key-secret",
        "upload-security-secret",
    ] {
        assert!(!debug.contains(sensitive));
    }
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
