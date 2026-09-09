use alldownloader_lib::cloud::{
    aliyun::AliyunClient, CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CredentialStore,
    MemoryCredentialStore, QuarkError,
};
use std::{collections::VecDeque, sync::Mutex};

struct RecordingHttp {
    responses: Mutex<VecDeque<CloudHttpResponse>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
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
        }
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
        r#"{"items":[{"file_id":"f1","parent_file_id":"root","name":"book.pdf","type":"file","size":5,"updated_at":"2026-09-09T00:00:00.000Z"}]}"#,
    ]);

    let directory = AliyunClient::new(&store, &http).directory("root").unwrap();

    assert_eq!(directory.items[0].name, "book.pdf");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].url,
        "https://auth.aliyundrive.com/v2/account/token"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[0].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"grant_type":"refresh_token","refresh_token":"refresh"})
    );
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].url,
        "https://api.aliyundrive.com/adrive/v3/file/list"
    );
    assert_eq!(requests[1].headers["Authorization"], "Bearer fresh");
    let list: serde_json::Value =
        serde_json::from_str(requests[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(list["drive_id"], "d1");
    assert_eq!(list["parent_file_id"], "root");
    drop(requests);
    let persisted = String::from_utf8(store.load("aliyun").unwrap().unwrap()).unwrap();
    assert!(persisted.contains("next"));
    assert!(!persisted.contains("https://"));
    assert!(!persisted.contains("download_url"));
}

#[test]
fn aliyun_does_not_replace_old_session_when_root_validation_fails() {
    // This catches persisting a rotated refresh token before its replacement session
    // proves it can access the root directory.
    let store = MemoryCredentialStore::default();
    let original = br#"{"refresh_token":"old","access_token":"old-access","drive_id":"d1"}"#;
    store.save("aliyun", original).unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"fresh","refresh_token":"rotated","default_drive_id":"d1"}"#,
        r#"{"code":"AccessTokenInvalid","message":"denied"}"#,
    ]);

    assert!(AliyunClient::new(&store, &http).directory("root").is_err());

    assert_eq!(store.load("aliyun").unwrap().unwrap(), original);
}

#[test]
fn aliyun_uses_native_file_endpoints_and_rejects_non_https_download_links() {
    // This catches changing an action endpoint/payload or accepting a link that can
    // disclose data over cleartext transport.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"old","drive_id":"d1"}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{"file_id":"folder1","parent_file_id":"root","name":"new-folder","type":"folder"}"#,
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{}"#,
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{}"#,
        r#"{}"#,
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{}"#,
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{}"#,
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{"url":"http://insecure.example/download"}"#,
    ]);
    let client = AliyunClient::new(&store, &http);

    client.mkdir("root", "new-folder").unwrap();
    client.rename("f1", "renamed.pdf").unwrap();
    client
        .move_files(&["f1".into(), "f2".into()], "target")
        .unwrap();
    client.copy_files(&["f1".into()], "target").unwrap();
    client.trash(&["f1".into()]).unwrap();
    assert!(client.download("f1").is_err());

    let requests = http.requests.lock().unwrap();
    let actions = requests
        .iter()
        .filter(|request| !request.url.contains("account/token"))
        .collect::<Vec<_>>();
    assert_eq!(
        actions
            .iter()
            .map(|request| request.url.as_str())
            .collect::<Vec<_>>(),
        vec![
            "https://api.aliyundrive.com/v2/file/create",
            "https://api.aliyundrive.com/v2/file/update",
            "https://api.aliyundrive.com/v2/file/move",
            "https://api.aliyundrive.com/v2/file/move",
            "https://api.aliyundrive.com/v2/file/copy",
            "https://api.aliyundrive.com/v2/recyclebin/trash",
            "https://api.aliyundrive.com/v2/file/get_download_url",
        ]
    );
    assert_eq!(actions[0].headers["Authorization"], "Bearer fresh");
    let create: serde_json::Value =
        serde_json::from_str(actions[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(create["type"], "folder");
    assert_eq!(create["parent_file_id"], "root");
    let move_body: serde_json::Value =
        serde_json::from_str(actions[2].body.as_deref().unwrap()).unwrap();
    assert_eq!(move_body["file_id"], "f1");
    assert_eq!(move_body["to_parent_file_id"], "target");
}
