use alldownloader_lib::cloud::{
    aliyun::AliyunClient, CloudHttpRequest, CloudHttpResponse, CloudHttpTransport,
    CloudUploadTransport, CredentialStore, MemoryCredentialStore, QuarkError, UploadHttpRequest,
    UploadHttpResponse,
};
use std::{collections::VecDeque, io::Write, sync::Mutex};

struct RecordingHttp {
    responses: Mutex<VecDeque<CloudHttpResponse>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
    upload_responses: Mutex<VecDeque<UploadHttpResponse>>,
    upload_requests: Mutex<Vec<UploadHttpRequest>>,
}

impl RecordingHttp {
    fn sequence(bodies: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            responses: Mutex::new(
                bodies
                    .into_iter()
                    .map(|body| CloudHttpResponse {
                        status: 200,
                        body: body.into(),
                        ..Default::default()
                    })
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
            upload_responses: Mutex::new(VecDeque::new()),
            upload_requests: Mutex::new(Vec::new()),
        }
    }

    fn responses(responses: impl IntoIterator<Item = CloudHttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            upload_responses: Mutex::new(VecDeque::new()),
            upload_requests: Mutex::new(Vec::new()),
        }
    }

    fn with_upload_responses(
        mut self,
        responses: impl IntoIterator<Item = UploadHttpResponse>,
    ) -> Self {
        self.upload_responses = Mutex::new(responses.into_iter().collect());
        self
    }
}

impl CloudHttpTransport for RecordingHttp {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(QuarkError::InvalidResponse)
    }
}

impl CloudUploadTransport for RecordingHttp {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        self.upload_requests.lock().unwrap().push(request);
        self.upload_responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(QuarkError::InvalidResponse)
    }
}

#[test]
fn aliyun_refreshes_bearer_token_lists_root_and_persists_only_session_fields() {
    // This catches a refresh that omits grant_type, a list without the fresh bearer
    // token/drive id, or persistence of a transient signed link.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"expired","drive_id":"d1"}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[{"file_id":"f1","parent_file_id":"root","name":"book.pdf","type":"file","size":5,"updated_at":"2026-09-09T00:00:00.000Z"}]}"#,
    ]);

    let directory = AliyunClient::new(&store, &http).directory("root").unwrap();

    assert_eq!(directory.items[0].name, "book.pdf");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].url,
        "https://api.aliyundrive.com/token/refresh"
    );
    assert_eq!(requests[0].headers["X-Requested-With"], "XMLHttpRequest");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[0].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"grant_type":"refresh_token","refresh_token":"refresh"})
    );
    assert_eq!(
        requests[1].url,
        "https://api.aliyundrive.com/adrive/v2/user/get"
    );
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        requests[2].url,
        "https://api.aliyundrive.com/adrive/v3/file/list"
    );
    assert_eq!(requests[2].headers["Authorization"], "Bearer fresh");
    let list: serde_json::Value =
        serde_json::from_str(requests[2].body.as_deref().unwrap()).unwrap();
    assert_eq!(list["drive_id"], "d1");
    assert_eq!(list["parent_file_id"], "root");
    drop(requests);
    let persisted = String::from_utf8(store.load("aliyun").unwrap().unwrap()).unwrap();
    assert!(persisted.contains("next"));
    assert!(!persisted.contains("https://"));
    assert!(!persisted.contains("download_url"));
}

#[test]
fn aliyun_account_does_not_replace_old_session_when_root_validation_fails() {
    // This catches persisting a rotated refresh token before its replacement session
    // proves it can access the root directory.
    let store = MemoryCredentialStore::default();
    let original = br#"{"refresh_token":"old","access_token":"old-access","drive_id":"d1"}"#;
    store.save("aliyun", original).unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"fresh","refresh_token":"rotated","default_drive_id":"d1"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"code":"AccessTokenInvalid","message":"denied"}"#,
    ]);

    assert!(AliyunClient::new(&store, &http).account().is_err());

    assert_eq!(store.load("aliyun").unwrap().unwrap(), original);
}

#[test]
fn aliyun_persists_a_validated_rotation_before_the_next_operation() {
    // This catches dropping a one-time rotated token after one operation, causing
    // the next operation to send the old now-invalid token.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"old","drive_id":"d1"}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"fresh-1","refresh_token":"rotated","default_drive_id":"d1"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{}"#,
        r#"{"access_token":"fresh-2","refresh_token":"final","default_drive_id":"d1"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{}"#,
    ]);
    let client = AliyunClient::new(&store, &http);

    client.rename("f1", "renamed.pdf").unwrap();
    client.rename("f2", "renamed-again.pdf").unwrap();

    let requests = http.requests.lock().unwrap();
    let refreshes = requests
        .iter()
        .filter(|request| request.url.ends_with("/token/refresh"))
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(refreshes[0].body.as_deref().unwrap()).unwrap()
            ["refresh_token"],
        "refresh"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(refreshes[1].body.as_deref().unwrap()).unwrap()
            ["refresh_token"],
        "rotated"
    );
    let renames = requests
        .iter()
        .filter(|request| request.url.contains("/file/update"))
        .collect::<Vec<_>>();
    assert_eq!(renames[0].url, "https://api.aliyundrive.com/v3/file/update");
    let body: serde_json::Value =
        serde_json::from_str(renames[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(body["check_name_mode"], "refuse");
    drop(requests);
    assert!(String::from_utf8(store.load("aliyun").unwrap().unwrap())
        .unwrap()
        .contains("final"));
}

#[test]
fn aliyun_refresh_400_codes_require_reauthorization_without_replacing_the_session() {
    // This catches response_json mapping a provider's 400 refresh response to a
    // generic provider error instead of a usable reauthorization state.
    for code in ["RefreshTokenExpired", "InvalidParameter.RefreshToken"] {
        let store = MemoryCredentialStore::default();
        let original = br#"{"refresh_token":"old","access_token":"old-access","drive_id":"d1"}"#;
        store.save("aliyun", original).unwrap();
        let http = RecordingHttp::responses([CloudHttpResponse {
            status: 400,
            body: serde_json::json!({"code":code}).to_string(),
            ..Default::default()
        }]);

        assert!(matches!(
            AliyunClient::new(&store, &http).directory("root"),
            Err(QuarkError::AuthorizationExpired)
        ));
        assert_eq!(store.load("aliyun").unwrap().unwrap(), original);
    }
}

#[test]
fn aliyun_upload_init_requires_https_part_urls_and_rename_uses_current_contract() {
    // This catches exposing an insecure temporary upload URL or regressing the
    // current rename endpoint/payload.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"r","access_token":"old","drive_id":"d1"}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"one","refresh_token":"one-r","default_drive_id":"d1"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"part_info_list":[{"part_number":1,"upload_url":"https://upload.example/part"}]}"#,
        r#"{"access_token":"two","refresh_token":"two-r","default_drive_id":"d1"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"part_info_list":[{"part_number":1,"upload_url":"http://upload.example/part"}]}"#,
    ]);

    let client = AliyunClient::new(&store, &http);
    assert_eq!(
        client.upload_init("root", "book.pdf", 5).unwrap()["part_info_list"][0]["upload_url"],
        "https://upload.example/part"
    );
    assert!(client.upload_init("root", "bad.pdf", 5).is_err());
    let requests = http.requests.lock().unwrap();
    let uploads = requests
        .iter()
        .filter(|r| r.url == "https://api.aliyundrive.com/v2/file/create")
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 2);
    let init: serde_json::Value =
        serde_json::from_str(uploads[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(init["type"], "file");
    assert_eq!(init["part_info_list"][0]["part_number"], 1);
}

#[test]
fn aliyun_upload_init_rejects_null_and_non_string_temporary_endpoints() {
    // This catches treating a present-but-malformed endpoint as absent and handing
    // an unusable or unsafe transfer response to the caller.
    for endpoint in [serde_json::Value::Null, serde_json::json!(17)] {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "aliyun",
                br#"{"refresh_token":"r","access_token":"old","drive_id":"d1"}"#,
            )
            .unwrap();
        let http = RecordingHttp::responses([
            CloudHttpResponse {
                status: 200,
                body: r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#
                    .into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"user_id":"u1","nick_name":"alice"}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"items":[]}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "part_info_list": [{
                        "part_number": 1,
                        "upload_url": endpoint,
                        "internal_upload_url": endpoint,
                    }]
                })
                .to_string(),
                ..Default::default()
            },
        ]);

        assert!(AliyunClient::new(&store, &http)
            .upload_init("root", "broken.pdf", 5)
            .is_err());
    }
}

#[test]
fn aliyun_upload_init_checks_each_temporary_endpoint_independently() {
    // A valid sibling must not short-circuit validation of the malformed field.
    let valid = serde_json::json!("https://upload.example/part");
    let invalid = [
        serde_json::json!(""),
        serde_json::json!(false),
        serde_json::Value::Null,
        serde_json::json!(17),
        serde_json::json!("http://upload.example/part"),
        serde_json::json!("https://upload.example/part#fragment"),
    ];
    let cases = invalid
        .iter()
        .cloned()
        .map(|bad| (bad, valid.clone()))
        .chain(invalid.iter().cloned().map(|bad| (valid.clone(), bad)));

    for (upload_url, internal_upload_url) in cases {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "aliyun",
                br#"{"refresh_token":"r","access_token":"old","drive_id":"d1"}"#,
            )
            .unwrap();
        let http = RecordingHttp::responses([
            CloudHttpResponse {
                status: 200,
                body: r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#
                    .into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"user_id":"u1","nick_name":"alice"}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"items":[]}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "part_info_list": [{
                        "part_number": 1,
                        "upload_url": upload_url,
                        "internal_upload_url": internal_upload_url,
                    }]
                })
                .to_string(),
                ..Default::default()
            },
        ]);

        assert!(AliyunClient::new(&store, &http)
            .upload_init("root", "independent-check.pdf", 5)
            .is_err());
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[3].url,
            "https://api.aliyundrive.com/v2/file/create"
        );
    }
}

#[test]
fn aliyun_upload_file_transfers_https_part_then_completes_without_persisting_temporary_url() {
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"cloud-bytes").unwrap();
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"old","drive_id":"drive"}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"access","refresh_token":"rotated","default_drive_id":"drive"}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"file_id":"file-1","parent_file_id":"root","name":"upload.bin","type":"file","upload_id":"upload-1","part_info_list":[{"part_number":1,"upload_url":"https://upload.example/part?temporary=secret"}]}"#,
        r#"{}"#,
    ])
    .with_upload_responses([UploadHttpResponse {
        status: 200,
        headers: Default::default(),
        body: String::new(),
    }]);

    let file = AliyunClient::new(&store, &http)
        .upload_file("root", local.path())
        .unwrap();

    assert_eq!(file.id, "file-1");
    let uploads = http.upload_requests.lock().unwrap();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].method, "PUT");
    assert_eq!(
        uploads[0].url,
        "https://upload.example/part?temporary=secret"
    );
    assert_eq!(uploads[0].body, b"cloud-bytes");
    drop(uploads);
    let requests = http.requests.lock().unwrap();
    let complete = requests.last().unwrap();
    assert_eq!(complete.url, "https://api.aliyundrive.com/v2/file/complete");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(complete.body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"drive_id":"drive","file_id":"file-1","upload_id":"upload-1"})
    );
    drop(requests);
    let persisted = String::from_utf8(store.load("aliyun").unwrap().unwrap()).unwrap();
    assert!(persisted.contains("rotated"));
    assert!(!persisted.contains("upload.example"));
    assert!(!persisted.contains("temporary=secret"));
}
