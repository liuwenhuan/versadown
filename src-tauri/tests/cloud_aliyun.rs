#[path = "support/upload.rs"]
mod upload_support;
use versadown_lib::cloud::{
    aliyun::{AliyunClient, AliyunSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport, CredentialStore,
    MemoryCredentialStore, QuarkError, UploadHttpRequest, UploadHttpResponse,
};
use std::{collections::VecDeque, io::Write, sync::Mutex};

#[test]
fn aliyun_refresh_preserves_provider_ttl_for_durable_maintenance() {
    use versadown_lib::cloud::{
        auth_supervisor::session_expiry, CloudProvider, ProviderRegistry,
    };
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"access_token":"old","refresh_token":"refresh","drive_id":"drive"}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#,
    ]);
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(ProviderRegistry::global()
        .driver(CloudProvider::Aliyun, &store, &http)
        .auth()
        .refresh_session()
        .unwrap());
    assert!(session_expiry(&store, CloudProvider::Aliyun)
        .unwrap()
        .is_some_and(|expires| expires >= before + 3600 && expires <= before + 3605));
}

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
        let request = upload_support::consume(request)?;
        self.upload_requests.lock().unwrap().push(request);
        self.upload_responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(QuarkError::InvalidResponse)
    }
}

#[test]
fn aliyun_recycle_list_and_restore_use_drive_scoped_ids() {
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"access","drive_id":"drive-42","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[],"next_marker":""}"#,
        r#"{"items":[{"file_id":"recycle-record-1","file_name":"trashed.txt","type":"file","size":7}],"next_marker":""}"#,
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[],"next_marker":""}"#,
        r#"{}"#,
    ]);
    let client = AliyunClient::new(&store, &http);

    let bin = client.list_recycle_bin().unwrap();
    assert_eq!(bin.items[0].id, "recycle-record-1");
    assert_eq!(bin.items[0].parent_id, bin.id);
    client
        .restore_recycle_entries(&[bin.items[0].id.clone()])
        .unwrap();

    let requests = http.requests.lock().unwrap();
    let recycle_requests = requests
        .iter()
        .filter(|request| request.url.contains("/v2/recyclebin/"))
        .collect::<Vec<_>>();
    assert_eq!(recycle_requests.len(), 2);
    assert_eq!(
        recycle_requests[0].url,
        "https://api.aliyundrive.com/v2/recyclebin/list"
    );
    assert_eq!(
        recycle_requests[1].url,
        "https://api.aliyundrive.com/v2/recyclebin/restore"
    );
    for request in &recycle_requests {
        let body: serde_json::Value =
            serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["drive_id"], "drive-42");
    }
    let restore: serde_json::Value =
        serde_json::from_str(recycle_requests[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(restore["file_id"], "recycle-record-1");
}

#[test]
fn aliyun_recycle_permanent_delete_uses_file_delete_and_accepts_no_content() {
    // Break caught: the recycle-bin endpoint only supports listing, trashing,
    // and restoring. Permanent deletion must use FileDelete, whose successful
    // single-file response is HTTP 204 with no JSON body.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"access","drive_id":"drive-42","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::responses([
        CloudHttpResponse {
            status: 200,
            body: r#"{"user_id":"u1","nick_name":"alice"}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"items":[],"next_marker":""}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 204,
            body: String::new(),
            ..Default::default()
        },
    ]);

    AliyunClient::new(&store, &http)
        .delete_recycle_entries(&["trashed-file-1".into()])
        .expect("the FileDelete no-content success response must complete permanent deletion");

    let requests = http.requests.lock().unwrap();
    let delete = requests.last().unwrap();
    assert_eq!(delete.method, "POST");
    assert_eq!(delete.url, "https://api.aliyundrive.com/v2/file/delete");
    assert_eq!(delete.headers["Authorization"], "Bearer access");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(delete.body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"drive_id":"drive-42","file_id":"trashed-file-1"})
    );
}

#[test]
fn aliyun_trash_is_idempotent_only_when_the_file_is_already_absent() {
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"access","drive_id":"drive-42","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::responses([
        CloudHttpResponse {
            status: 200,
            body: r#"{"user_id":"u1","nick_name":"alice"}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"items":[],"next_marker":""}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 404,
            body: r#"{"code":"NotFound.FileId","message":"The resource file_id cannot be found."}"#
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
            body: r#"{"items":[],"next_marker":""}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 404,
            body: r#"{"code":"NotFound.DriveId","message":"The drive cannot be found."}"#.into(),
            ..Default::default()
        },
    ]);
    let client = AliyunClient::new(&store, &http);

    client
        .trash(&["already-absent".into()])
        .expect("an already absent file is already in the desired state");
    assert!(client.trash(&["missing-drive".into()]).is_err());
}

#[test]
fn aliyun_root_exposes_a_reserved_recycle_bin_folder() {
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"access","drive_id":"drive-42","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([r#"{"items":[],"next_marker":""}"#]);

    let root = AliyunClient::new(&store, &http).directory("root").unwrap();

    assert!(root.items.iter().any(|item| {
        item.id == "__versa_down_aliyun_recycle_bin__"
            && item.parent_id == "root"
            && item.name == "回收站"
            && item.kind == "folder"
    }));
}

#[test]
fn aliyun_browser_session_uses_the_current_web_bearer_before_refreshing() {
    // A just-completed official web login already supplies a valid bearer token.
    // The authorization handshake must use it directly, so a transient refresh
    // endpoint failure cannot keep the authorization window open.
    let store = MemoryCredentialStore::default();
    let http = RecordingHttp::sequence([r#"{"user_id":"u1","nick_name":"alice"}"#]);
    let client = AliyunClient::new(&store, &http);

    let account = client
        .authorize_browser_session(AliyunSession {
            refresh_token: "browser-refresh".into(),
            access_token: "browser-access".into(),
            drive_id: "browser-drive".into(),
            browser_access_ready: false,
            expires_at: None,
        })
        .unwrap();

    assert_eq!(account.id, "u1");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://api.aliyundrive.com/adrive/v2/user/get"
    );
    assert_eq!(
        requests[0].headers["Authorization"],
        "Bearer browser-access"
    );
    drop(requests);
    let persisted = String::from_utf8(store.load("aliyun").unwrap().unwrap()).unwrap();
    assert!(persisted.contains("browser-refresh"));
    assert!(persisted.contains("browser-access"));
    assert!(persisted.contains("browser-drive"));
    assert!(persisted.contains("browser_access_ready"));
}

#[test]
fn aliyun_browser_session_falls_back_when_nick_name_is_empty() {
    // The current official web account endpoint can return an empty nickname
    // alongside a populated user_name.  That is still a completed login.
    let store = MemoryCredentialStore::default();
    let http = RecordingHttp::sequence([r#"{"user_id":"u1","nick_name":"","user_name":"alice"}"#]);
    let client = AliyunClient::new(&store, &http);

    let account = client
        .authorize_browser_session(AliyunSession {
            refresh_token: "browser-refresh".into(),
            access_token: "browser-access".into(),
            drive_id: "browser-drive".into(),
            browser_access_ready: false,
            expires_at: None,
        })
        .unwrap();

    assert_eq!(account.id, "u1");
    assert_eq!(account.display_name, "alice");
}

#[test]
fn aliyun_browser_session_lists_the_root_with_the_same_current_bearer() {
    let store = MemoryCredentialStore::default();
    let http = RecordingHttp::sequence([
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[{"file_id":"f1","parent_file_id":"root","name":"from-web.txt","type":"file"}]}"#,
    ]);
    let client = AliyunClient::new(&store, &http);
    client
        .authorize_browser_session(AliyunSession {
            refresh_token: "browser-refresh".into(),
            access_token: "browser-access".into(),
            drive_id: "browser-drive".into(),
            browser_access_ready: false,
            expires_at: None,
        })
        .unwrap();

    let root = client.directory("root").unwrap();

    assert_eq!(root.items[0].name, "from-web.txt");
    let requests = http.requests.lock().unwrap();
    assert!(requests
        .iter()
        .all(|request| !request.url.ends_with("/token/refresh")));
    assert_eq!(requests.len(), 2);
    let lists = requests
        .iter()
        .filter(|request| request.url.ends_with("/adrive/v3/file/list"))
        .collect::<Vec<_>>();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].headers["Authorization"], "Bearer browser-access");
}

#[test]
fn aliyun_root_listing_refreshes_once_after_an_authentication_failure() {
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"expired","drive_id":"drive","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::responses([
        CloudHttpResponse {
            status: 401,
            body: r#"{"code":"AccessTokenInvalid"}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"access_token":"fresh","refresh_token":"rotated","default_drive_id":"drive"}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"items":[{"file_id":"f1","parent_file_id":"root","name":"after-refresh.txt","type":"file"}],"next_marker":""}"#.into(),
            ..Default::default()
        },
    ]);

    let directory = AliyunClient::new(&store, &http).directory("root").unwrap();

    assert_eq!(directory.items[0].name, "after-refresh.txt");
    let requests = http.requests.lock().unwrap();
    let lists = requests
        .iter()
        .filter(|request| request.url.ends_with("/adrive/v3/file/list"))
        .collect::<Vec<_>>();
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[0].headers["Authorization"], "Bearer expired");
    assert_eq!(lists[1].headers["Authorization"], "Bearer fresh");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.ends_with("/token/refresh"))
            .count(),
        1
    );
    drop(requests);
    let persisted = String::from_utf8(store.load("aliyun").unwrap().unwrap()).unwrap();
    assert!(persisted.contains("rotated"));
}

#[test]
fn aliyun_second_authentication_failure_after_refresh_stops_without_more_requests() {
    // Break caught: recursively refreshing/replaying a rejected retry instead
    // of returning the second authentication error to the caller.
    let store = MemoryCredentialStore::default();
    store.save("aliyun", br#"{"refresh_token":"refresh","access_token":"expired","drive_id":"drive","browser_access_ready":true}"#).unwrap();
    let rejected = || CloudHttpResponse {
        status: 401,
        body: r#"{"code":"AccessTokenInvalid"}"#.into(),
        ..Default::default()
    };
    let http = RecordingHttp::responses([
        rejected(),
        CloudHttpResponse {
            status: 200,
            body:
                r#"{"access_token":"fresh","refresh_token":"rotated","default_drive_id":"drive"}"#
                    .into(),
            ..Default::default()
        },
        rejected(),
    ]);
    assert!(matches!(
        AliyunClient::new(&store, &http).directory("root"),
        Err(QuarkError::AuthorizationExpired)
    ));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].url.ends_with("/adrive/v3/file/list"));
    assert!(requests[1].url.ends_with("/token/refresh"));
    assert!(requests[2].url.ends_with("/adrive/v3/file/list"));
    assert_eq!(requests[0].headers["Authorization"], "Bearer expired");
    assert_eq!(requests[2].headers["Authorization"], "Bearer fresh");
    let persisted: serde_json::Value =
        serde_json::from_slice(&store.load("aliyun").unwrap().unwrap()).unwrap();
    assert_eq!(persisted["refresh_token"], "rotated");
}

#[test]
fn aliyun_mkdir_accepts_current_file_name_response_field() {
    // The live /v2/file/create folder response uses `file_name`, not `name`.
    // Rejecting that response leaves a successfully created folder invisible to
    // the caller and strands every following operation in a real workflow.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"browser-refresh","access_token":"browser-access","drive_id":"browser-drive","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"file_id":"folder-1","file_name":"VersaDown-IT-folder","parent_file_id":"root","type":"folder"}"#,
    ]);

    let folder = AliyunClient::new(&store, &http)
        .mkdir("root", "VersaDown-IT-folder")
        .expect("the official file_name response must create a usable folder");

    assert_eq!(folder.id, "folder-1");
    assert_eq!(folder.name, "VersaDown-IT-folder");
    assert_eq!(folder.kind, "folder");
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
        r#"{"code":"AccessTokenInvalid","message":"expired"}"#,
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{"items":[{"file_id":"f1","parent_file_id":"root","name":"book.pdf","type":"file","size":5,"updated_at":"2026-09-09T00:00:00.000Z"}]}"#,
    ]);

    let directory = AliyunClient::new(&store, &http).directory("root").unwrap();

    assert_eq!(directory.items[0].name, "book.pdf");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].url,
        "https://api.aliyundrive.com/adrive/v3/file/list"
    );
    assert_eq!(requests[0].headers["Authorization"], "Bearer expired");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].url, "https://api.aliyundrive.com/token/refresh");
    assert_eq!(requests[1].headers["X-Requested-With"], "XMLHttpRequest");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[1].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"grant_type":"refresh_token","refresh_token":"refresh"})
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
        let http = RecordingHttp::responses([
            CloudHttpResponse {
                status: 401,
                body: r#"{"code":"AccessTokenInvalid"}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 400,
                body: serde_json::json!({"code":code}).to_string(),
                ..Default::default()
            },
        ]);

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
fn aliyun_upload_init_uses_public_https_url_when_internal_url_is_http() {
    // The official service returns a browser-reachable HTTPS upload_url and an
    // HTTP-only intranet internal_upload_url in the same part.  The client must
    // use only the public URL instead of rejecting a valid upload response.
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"r","access_token":"access","drive_id":"drive","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"part_info_list":[{"part_number":1,"upload_url":"https://upload.example/public","internal_upload_url":"http://oss.internal/private"}]}"#,
    ]);

    let init = AliyunClient::new(&store, &http)
        .upload_init("root", "fixture.txt", 7)
        .expect("a valid public HTTPS upload_url must not be rejected by an internal HTTP URL");

    assert_eq!(
        init["part_info_list"][0]["upload_url"],
        "https://upload.example/public"
    );
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
fn aliyun_upload_init_rejects_each_malformed_public_endpoint() {
    // Only the browser-reachable public endpoint can transfer bytes. A valid
    // internal sibling must not hide a malformed public upload_url.
    let valid = serde_json::json!("https://upload.example/part");
    let invalid = [
        serde_json::json!(""),
        serde_json::json!(false),
        serde_json::Value::Null,
        serde_json::json!(17),
        serde_json::json!("http://upload.example/part"),
        serde_json::json!("https://upload.example/part#fragment"),
    ];
    for upload_url in invalid {
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
                        "internal_upload_url": valid,
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
    assert_eq!(upload_support::bytes(&uploads[0].body), b"cloud-bytes");
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

#[test]
fn aliyun_upload_file_omits_content_type_when_the_signed_part_leaves_it_empty() {
    // A live browser session issued `content_type: ""`. Supplying an arbitrary
    // Content-Type changes the signed canonical request and is rejected by OSS.
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"cloud-bytes").unwrap();
    let store = MemoryCredentialStore::default();
    store
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"access","drive_id":"drive","browser_access_ready":true}"#,
        )
        .unwrap();
    let http = RecordingHttp::sequence([
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"file_id":"file-1","file_name":"upload.bin","parent_file_id":"root","type":"file","upload_id":"upload-1","part_info_list":[{"part_number":1,"content_type":"","upload_url":"https://upload.example/part?temporary=secret"}]}"#,
        r#"{}"#,
    ])
    .with_upload_responses([UploadHttpResponse {
        status: 200,
        headers: Default::default(),
        body: String::new(),
    }]);

    AliyunClient::new(&store, &http)
        .upload_file("root", local.path())
        .expect("an empty server content type must not add an unsigned header");

    let uploads = http.upload_requests.lock().unwrap();
    assert!(
        !uploads[0].headers.contains_key("Content-Type"),
        "empty signed content type must be omitted"
    );
}
