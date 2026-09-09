use alldownloader_lib::cloud::{
    xunlei::{XunleiClient, XunleiSession},
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

// Break caught: omitting the native refresh exchange, token rotation, or either
// required authenticated-drive header makes the directory request invalid.
#[test]
fn xunlei_refreshes_rotates_token_and_sends_bearer_and_device_headers() {
    let http = RecordingHttp::sequence(&[
        r#"{"access_token":"access","refresh_token":"next","expires_in":7200,"sub":"user-1"}"#,
        r#"{"files":[{"id":"f1","name":"archive.zip","kind":"drive#file","size":32,"parent_id":"0"}],"next_page_token":""}"#,
    ]);
    let client = XunleiClient::new(XunleiSession::refresh("refresh", "device-1"), &http);

    let directory = client.directory("0").unwrap();

    assert_eq!(directory.items[0].name, "archive.zip");
    assert_eq!(client.session().refresh_token, "next");
    let requests = http.requests();
    assert!(requests[0].url.ends_with("/v1/auth/token"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[0].body.as_deref().unwrap()).unwrap()
            ["grant_type"],
        "refresh_token"
    );
    assert_eq!(requests[1].headers["Authorization"], "Bearer access");
    assert_eq!(requests[1].headers["X-Device-Id"], "device-1");
}

// Break caught: treating an interactive security challenge as an ordinary
// provider failure prevents the UI from returning the user to official web auth.
#[test]
fn xunlei_security_challenge_is_web_auth_not_link_expiry() {
    assert!(matches!(
        XunleiClient::new(
            XunleiSession::with_access("access", "refresh", "device-1"),
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
        XunleiSession::with_access("access", "refresh", "device-1"),
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
            XunleiSession::with_access("access", "refresh", "device-1"),
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
    let http = RecordingHttp::sequence(&["{}", "{}", "{}", "{}", "{}", "{}"]);
    let client = XunleiClient::new(
        XunleiSession::with_access("access", "refresh", "device-1"),
        &http,
    );
    let ids = vec!["file-1".to_owned(), "folder-2".to_owned()];

    client.mkdir("parent", "new-folder").unwrap();
    client.rename("file-1", "renamed.zip").unwrap();
    client.move_files(&ids, "destination").unwrap();
    client.copy_files(&ids, "destination").unwrap();
    client.trash(&ids).unwrap();
    client.upload_init("parent", "upload.bin", 64).unwrap();

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
    assert!(requests[4].url.ends_with("/drive/v1/files:batchTrash"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[5].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"kind":"drive#file","parent_id":"parent","name":"upload.bin","size":64})
    );
}
