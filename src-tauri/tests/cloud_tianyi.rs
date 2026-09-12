use alldownloader_lib::cloud::{
    tianyi::{parse_tianyi_web_session, TianyiClient, TianyiSession, TIANYI_RECYCLE_BIN_ID},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport, QuarkError,
    UploadHttpRequest, UploadHttpResponse,
};
use std::{collections::BTreeMap, io::Write, sync::Mutex};
use tempfile::NamedTempFile;

const TIANYI_LOGIN_RSA_KEY: &str = concat!(
    "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDY7mpaUysvgQkbp0iIn2ezoUyh",
    "i1zPFn0HCXloLFWT7uoNkqtrphpQ/63LEcPz1VYzmDuDIf3iGxQKzeoHTiVMSmW6",
    "FlhDeqVOG094hFJvZeK4OzA6HVwzwnEW5vIZ7d+u61RV1bsFxmB68+8JXs3ycGcE",
    "4anY+YzZJcyOcEGKVQIDAQAB",
);

fn tianyi_app_login_page() -> String {
    format!(
        r#"<input id="captchaToken" value="captcha-token">
        <script>var lt = "login-ticket"; var returnUrl = 'https://m.cloud.189.cn/return'; var paramId = "param-id"; var reqId = "request-id";</script>
        <input id="j_rsaKey" value="{TIANYI_LOGIN_RSA_KEY}">"#
    )
}

struct RecordingHttp {
    replies: Mutex<Vec<CloudHttpResponse>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
}

impl RecordingHttp {
    fn sequence(bodies: &[&str]) -> Self {
        Self {
            replies: Mutex::new(
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
        Self::sequence(&[r#"{"result":-1,"msg":"需要短信验证码"}"#])
    }

    fn requests(&self) -> Vec<CloudHttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl CloudHttpTransport for RecordingHttp {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        self.replies
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| QuarkError::Transport("unexpected request".into()))
    }
}

fn form_field(body: &str, wanted: &str) -> String {
    url::form_urlencoded::parse(body.as_bytes())
        .find_map(|(name, value)| (name == wanted).then(|| value.into_owned()))
        .unwrap_or_default()
}

fn tianyi_recycle_page(first_id: u64) -> String {
    serde_json::json!({
        "res_code": 0,
        "fileList": (first_id..first_id + 100)
            .map(|id| serde_json::json!({
                "id": id,
                "name": format!("deleted-{id}.txt"),
                "isFolder": false,
            }))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

fn tianyi_directory_page(first_id: u64, count: usize) -> String {
    serde_json::json!({
        "res_code": 0,
        "fileListAO": {
            "fileList": (first_id..first_id + count as u64)
                .map(|id| serde_json::json!({
                    "id": format!("file-{id}"),
                    "name": format!("file-{id}.txt"),
                    "isFolder": false,
                }))
                .collect::<Vec<_>>(),
        },
    })
    .to_string()
}

struct UploadRecordingHttp {
    replies: Mutex<Vec<UploadHttpResponse>>,
    requests: Mutex<Vec<UploadHttpRequest>>,
}

impl UploadRecordingHttp {
    fn sequence(status: u16, bodies: &[&str]) -> Self {
        Self {
            replies: Mutex::new(
                bodies
                    .iter()
                    .map(|body| UploadHttpResponse {
                        status,
                        body: (*body).into(),
                        headers: BTreeMap::new(),
                    })
                    .rev()
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        }
    }
    fn requests(&self) -> Vec<UploadHttpRequest> {
        self.requests.lock().unwrap().drain(..).collect()
    }
}

impl CloudUploadTransport for UploadRecordingHttp {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        self.replies
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| QuarkError::Transport("unexpected upload request".into()))
    }
}

#[test]
fn tianyi_browser_identity_reaches_account_directory_and_download_requests() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"loginName":"alice@189.cn"}"#,
        r#"{"res_code":0,"fileListAO":{"fileList":[]}}"#,
        r#"{"res_code":0,"fileDownloadUrl":"https://download.example/photo.jpg","fileName":"photo.jpg"}"#,
    ]);
    let mut session = TianyiSession::cookie("SSON=local");
    session.apply_browser_context(&serde_json::json!({
        "browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"
    }));
    let client = TianyiClient::new(session, &http);
    client.account().unwrap();
    client.directory("-11").unwrap();
    let download = client.download("f1").unwrap();
    let requests = http.requests();
    assert_eq!(requests.len(), 3);
    for headers in requests
        .iter()
        .map(|request| &request.headers)
        .chain([&download.headers])
    {
        assert_eq!(headers["Browser-Id"], "1234567890abcdef1234567890abcdef");
        assert_eq!(headers["User-Agent"], "OfficialBrowser/140");
        assert_eq!(headers["Cookie"], "SSON=local");
    }
}

#[test]
fn tianyi_maps_files_and_passes_cookie_to_the_resolved_download() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"fileListAO":{"fileList":[{"id":"f1","name":"photo.jpg","isFolder":false,"size":9}]}}"#,
        r#"{"res_code":0,"fileDownloadUrl":"https://download.example/photo.jpg","fileName":"photo.jpg"}"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    assert_eq!(client.directory("-11").unwrap().items[0].id, "f1");
    let download = client.download("f1").unwrap();
    assert!(download.url.starts_with("https://"));
    assert!(download.headers.contains_key("Cookie"));

    let requests = http.requests();
    assert!(requests[0].url.contains("/api/open/file/listFiles.action"));
    assert!(requests[0].url.contains("folderId=-11"));
    assert!(requests[1]
        .url
        .contains("/api/open/file/getFileDownloadUrl.action"));
    assert_eq!(requests[1].headers["Referer"], "https://cloud.189.cn/");
}

#[test]
fn tianyi_browser_directory_collects_following_pages() {
    let first = tianyi_directory_page(0, 100);
    let second = tianyi_directory_page(100, 1);
    let http = RecordingHttp::sequence(&[&first, &second]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.directory("folder-browser").unwrap();

    assert_eq!(directory.items.len(), 101);
    assert_eq!(
        directory
            .items
            .iter()
            .map(|item| &item.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        101
    );
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].url.contains("pageNum=2"));
}

#[test]
fn tianyi_app_directory_collects_following_pages() {
    let first = tianyi_directory_page(0, 100);
    let second = tianyi_directory_page(100, 1);
    let http = RecordingHttp::sequence(&[&first, &second]);
    let session: TianyiSession = serde_json::from_value(serde_json::json!({
        "cookie": "",
        "auth_method": "password",
        "app": {
            "sessionKey": "app-session-key",
            "sessionSecret": "app-session-secret",
            "accessToken": "access-token",
            "loginName": "alice@189.cn"
        }
    }))
    .unwrap();
    let client = TianyiClient::new(session, &http);

    let directory = client.directory("folder-app").unwrap();

    assert_eq!(directory.items.len(), 101);
    assert_eq!(
        directory
            .items
            .iter()
            .map(|item| &item.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        101
    );
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].url.contains("pageNum=2"));
}

#[test]
fn tianyi_maps_the_official_xml_directory_response() {
    // The authenticated browser endpoint currently returns XML for file
    // listing even though mutation responses can still be JSON.  The client
    // must preserve the existing directory model across both formats.
    let http = RecordingHttp::sequence(&[
        r#"<?xml version="1.0" encoding="UTF-8"?><fileListAO><fileList><file><id>f1</id><name>photo.jpg</name><folder>false</folder><size>9</size><lastOpTime>2026-09-09</lastOpTime></file></fileList></fileListAO>"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.directory("-11").unwrap();

    assert_eq!(directory.items.len(), 2);
    assert_eq!(directory.items[0].id, "f1");
    assert_eq!(directory.items[0].name, "photo.jpg");
    assert_eq!(directory.items[0].kind, "file");
    assert_eq!(directory.items[0].size, Some(9));
    assert_eq!(directory.items[1].id, TIANYI_RECYCLE_BIN_ID);
    assert_eq!(directory.items[1].kind, "folder");
}

#[test]
fn tianyi_preserves_ampersands_in_xml_download_urls() {
    let http = RecordingHttp::sequence(&[
        r#"<fileDownloadUrl>https://download.cloud.189.cn/native.txt?token=opaque&amp;part=1</fileDownloadUrl>"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let download = client.download("f1").expect("XML download URL");

    assert_eq!(
        download.url,
        "https://download.cloud.189.cn/native.txt?token=opaque&part=1"
    );
}

#[test]
fn tianyi_maps_the_current_list_files_xml_root() {
    // The current browser endpoint wraps the same file list in `listFiles`
    // rather than the older `fileListAO` JSON envelope.
    let http = RecordingHttp::sequence(&[
        r#"<?xml version="1.0" encoding="UTF-8"?><listFiles><fileList><file><id>f2</id><name>current.jpg</name><folder>false</folder><size>10</size></file></fileList></listFiles>"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.directory("-11").unwrap();

    assert_eq!(directory.items.len(), 2);
    assert_eq!(directory.items[0].id, "f2");
    assert_eq!(directory.items[0].size, Some(10));
    assert_eq!(directory.items[1].id, TIANYI_RECYCLE_BIN_ID);
}

#[test]
fn tianyi_accepts_a_successful_empty_root_directory() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"fileListAO":{"fileList":[],"folderList":[]}}"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.directory("-11").unwrap();

    assert_eq!(directory.items.len(), 1);
    assert_eq!(directory.items[0].id, TIANYI_RECYCLE_BIN_ID);
}

#[test]
fn tianyi_rejects_null_or_scalar_directory_listings() {
    for body in [
        r#"{"res_code":0,"fileListAO":{"fileList":null,"folderList":[]}}"#,
        r#"{"res_code":0,"fileListAO":{"fileList":"","folderList":[]}}"#,
        r#"{"res_code":0,"fileListAO":{"fileList":"not-a-list","folderList":[]}}"#,
    ] {
        let http = RecordingHttp::sequence(&[body]);
        let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

        assert!(matches!(
            client.directory("-11"),
            Err(QuarkError::InvalidResponse)
        ));
    }
}

#[test]
fn tianyi_accepts_an_empty_xml_directory_listing() {
    let http = RecordingHttp::sequence(&[r#"<listFiles><fileList/></listFiles>"#]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.directory("-11").unwrap();
    assert_eq!(directory.items.len(), 1);
    assert_eq!(directory.items[0].id, TIANYI_RECYCLE_BIN_ID);
}

#[test]
fn tianyi_maps_xml_folder_entries_as_folders() {
    let http = RecordingHttp::sequence(&[
        r#"<listFiles><folderList><folder><id>d1</id><name>folder</name></folder></folderList></listFiles>"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.directory("-11").unwrap();

    assert_eq!(directory.items.len(), 2);
    assert_eq!(directory.items[0].id, "d1");
    assert_eq!(directory.items[0].kind, "folder");
    assert_eq!(directory.items[1].id, TIANYI_RECYCLE_BIN_ID);
}

#[test]
fn tianyi_recycle_list_collects_following_pages_and_stops_after_a_short_page() {
    let first = serde_json::json!({
        "res_code": 0,
        "fileList": (1..=100)
            .map(|id| serde_json::json!({
                "id": id,
                "name": format!("deleted-{id}.txt"),
                "isFolder": false,
                "size": id,
                "lastOpTime": "2026-09-10",
            }))
            .collect::<Vec<_>>(),
    })
    .to_string();
    let second = r#"{"res_code":0,"fileList":[{"id":201,"name":"deleted-201.txt","isFolder":false,"size":201,"lastOpTime":"2026-09-10"},{"id":202,"name":"deleted-202.txt","isFolder":false,"size":202,"lastOpTime":"2026-09-10"}]}"#;
    let http = RecordingHttp::sequence(&[&first, second]);
    let client = TianyiClient::new(
        TianyiSession::cookie("COOKIE_LOGIN_USER=paged-session"),
        &http,
    );

    let directory = client.list_recycle_bin().unwrap();

    assert_eq!(directory.items.len(), 102);
    assert_eq!(directory.items[0].id, "1");
    assert_eq!(directory.items[99].id, "100");
    assert_eq!(directory.items[100].id, "201");
    assert_eq!(directory.items[101].id, "202");
    let requests = http.requests();
    assert_eq!(requests.len(), 2, "a short page must end pagination");
    assert!(requests[0].url.contains(
        "/api/open/file/listRecycleBinFiles.action?pageNum=1&pageSize=100&iconOption=1&family=false"
    ));
    assert!(requests[1].url.contains(
        "/api/open/file/listRecycleBinFiles.action?pageNum=2&pageSize=100&iconOption=1&family=false"
    ));
    for request in requests {
        assert_eq!(request.headers["Cookie"], "COOKIE_LOGIN_USER=paged-session");
    }
}

#[test]
fn tianyi_recycle_list_stops_after_an_empty_following_page() {
    let first = tianyi_recycle_page(1);
    let http = RecordingHttp::sequence(&[&first, r#"{"res_code":0,"fileList":[]}"#]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=empty-page"), &http);

    let directory = client.list_recycle_bin().unwrap();

    assert_eq!(directory.items.len(), 100);
    assert_eq!(directory.items[99].id, "100");
    let requests = http.requests();
    assert_eq!(requests.len(), 2, "an empty page must end pagination");
    assert!(requests[1].url.contains(
        "/api/open/file/listRecycleBinFiles.action?pageNum=2&pageSize=100&iconOption=1&family=false"
    ));
    assert_eq!(
        requests[1].headers["Cookie"],
        "COOKIE_LOGIN_USER=empty-page"
    );
}

#[test]
fn tianyi_recycle_list_rejects_a_duplicate_record_on_a_following_page() {
    let first = tianyi_recycle_page(1);
    let http = RecordingHttp::sequence(&[
        &first,
        r#"{"res_code":0,"fileList":[{"id":100,"name":"deleted-100.txt","isFolder":false}]}"#,
    ]);
    let client = TianyiClient::new(
        TianyiSession::cookie("COOKIE_LOGIN_USER=duplicate-page"),
        &http,
    );

    assert!(matches!(
        client.list_recycle_bin(),
        Err(QuarkError::InvalidResponse)
    ));
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].headers["Cookie"],
        "COOKIE_LOGIN_USER=duplicate-page"
    );
}

#[test]
fn tianyi_recycle_list_rejects_ten_thousand_full_pages() {
    let pages = (0..10_000)
        .map(|page| tianyi_recycle_page(page * 100 + 1))
        .collect::<Vec<_>>();
    let replies = pages.iter().map(String::as_str).collect::<Vec<_>>();
    let http = RecordingHttp::sequence(&replies);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=page-limit"), &http);

    assert!(matches!(
        client.list_recycle_bin(),
        Err(QuarkError::InvalidResponse)
    ));
    assert_eq!(http.requests().len(), 10_000);
}

#[test]
fn tianyi_recycle_operations_use_the_authenticated_web_session() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"fileList":[{"id":"456","fileId":"live-file","name":"deleted.jpg","isFolder":false,"size":9,"lastOpTime":"2026-09-10"}]}"#,
        r#"{"res_code":0,"taskId":"restore-task"}"#,
        r#"{"res_code":0,"success":true}"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let directory = client.list_recycle_bin().unwrap();

    assert_eq!(directory.id, TIANYI_RECYCLE_BIN_ID);
    assert_eq!(directory.items.len(), 1);
    assert_eq!(directory.items[0].id, "456");
    assert_eq!(directory.items[0].parent_id, TIANYI_RECYCLE_BIN_ID);
    client
        .restore_recycle_entries(&[directory.items[0].id.clone()])
        .unwrap();
    client
        .delete_recycle_entries(&[directory.items[0].id.clone()])
        .unwrap();

    let requests = http.requests();
    assert!(requests[0]
        .url
        .contains("/api/open/file/listRecycleBinFiles.action?pageNum=1&pageSize=100&iconOption=1&family=false"));
    assert!(requests[1]
        .url
        .contains("/api/open/batch/createBatchTask.action"));
    assert_eq!(
        form_field(requests[1].body.as_deref().unwrap(), "type"),
        "RESTORE"
    );
    assert_eq!(
        form_field(requests[1].body.as_deref().unwrap(), "taskInfos"),
        r#"[{"fileId":"456"}]"#
    );
    assert!(requests[2]
        .url
        .contains("/v2/deleteFile.action?fileIdList=456"));
    for request in &requests {
        assert_eq!(request.headers["Cookie"], "COOKIE_LOGIN_USER=local");
        assert_eq!(request.headers["Origin"], "https://cloud.189.cn");
        assert_eq!(request.headers["Referer"], "https://cloud.189.cn/");
    }
    assert_eq!(
        requests[1].headers["Content-Type"],
        "application/x-www-form-urlencoded; charset=UTF-8"
    );
}

#[test]
fn tianyi_recycle_mutations_reject_non_record_ids_before_transport() {
    let http = RecordingHttp::sequence(&[]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    for malformed in ["", "live-file", "456,457", "456/457"] {
        assert!(matches!(
            client.restore_recycle_entries(&[malformed.into()]),
            Err(QuarkError::InvalidInput(_))
        ));
        assert!(matches!(
            client.delete_recycle_entries(&[malformed.into()]),
            Err(QuarkError::InvalidInput(_))
        ));
    }
    assert!(http.requests().is_empty());
}

#[test]
fn tianyi_rejects_non_https_dlinks_and_uses_native_mutation_endpoints() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"fileDownloadUrl":"http://download.example/photo.jpg"}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0,"fileInfo":{"fileId":"f1","fileName":"photo.jpg","isFolder":false}}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0,"fileInfo":{"fileId":"f1","fileName":"renamed.jpg","isFolder":false}}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0,"fileInfo":{"fileId":"f1","fileName":"renamed.jpg","isFolder":false}}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0,"fileInfo":{"fileId":"f1","fileName":"renamed.jpg","isFolder":false}}"#,
        r#"{"res_code":0}"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    assert!(matches!(
        client.download("f1"),
        Err(QuarkError::InvalidResponse)
    ));
    client.mkdir("-11", "new folder").unwrap();
    client.rename("f1", "renamed.jpg").unwrap();
    client.move_files(&["f1".into()], "target").unwrap();
    client.copy_files(&["f1".into()], "target").unwrap();
    client.trash(&["f1".into()]).unwrap();

    let requests = http.requests();
    assert!(requests[1]
        .url
        .contains("/api/open/file/createFolder.action"));
    assert!(requests[2].url.contains("/api/portal/getFileInfo.action"));
    assert!(requests[3].url.contains("/api/open/file/renameFile.action"));
    assert!(requests[4].url.contains("/api/portal/getFileInfo.action"));
    assert!(requests[5]
        .url
        .contains("/api/open/batch/createBatchTask.action"));
    assert!(requests[6].url.contains("/api/portal/getFileInfo.action"));
    assert!(requests[7]
        .url
        .contains("/api/open/batch/createBatchTask.action"));
    assert!(requests[8].url.contains("/api/portal/getFileInfo.action"));
    assert!(requests[9]
        .url
        .contains("/api/open/batch/createBatchTask.action"));
    assert_eq!(
        requests[1].headers["Content-Type"],
        "application/x-www-form-urlencoded; charset=UTF-8"
    );
    assert!(requests[1]
        .body
        .as_deref()
        .is_some_and(|body| body.contains("parentFolderId=-11")));
    assert!(requests[5]
        .body
        .as_deref()
        .is_some_and(|body| body.contains("type=MOVE") && body.contains("taskInfos=")));
    assert!(requests[9]
        .body
        .as_deref()
        .is_some_and(|body| body.contains("type=DELETE") && body.contains("taskInfos=")));
}

#[test]
fn tianyi_password_login_prefers_the_signed_desktop_session_without_a_web_cookie() {
    // Break caught: treating Tianyi password login as a browser-cookie flow
    // makes the valid desktop SessionKey unusable whenever ssoLogin no longer
    // emits COOKIE_LOGIN_USER. The App session must remain sufficient for a
    // signed root listing, as in the maintained cloudpan189-go fork.
    let login_page = tianyi_app_login_page();
    let http = RecordingHttp::sequence(&[
        &login_page,
        r#"{"result":0,"msg":"登录成功","toUrl":"https://open.e.189.cn/callback?ticket=opaque"}"#,
        r#"{"res_code":0,"loginName":"alice@189.cn","sessionKey":"app-session-key","sessionSecret":"app-session-secret","familySessionKey":"family-key","familySessionSecret":"family-secret","accessToken":"access-token","refreshToken":"refresh-token"}"#,
        "",
        r#"<listFiles><fileList><file><id>f-app</id><name>native.txt</name><size>12</size></file></fileList></listFiles>"#,
    ]);

    let login = TianyiClient::login_password("alice@189.cn", "private-password", &http);
    assert!(
        login.is_ok(),
        "desktop login should not require a synthetic web cookie; safe error={:?}, request_count={}",
        login.as_ref().err(),
        http.requests().len()
    );
    let session = login.unwrap();
    assert_eq!(
        session.auth_method,
        alldownloader_lib::cloud::CloudAuthMethod::Password
    );
    let restored: TianyiSession =
        serde_json::from_slice(&serde_json::to_vec(&session).expect("serialize App session"))
            .expect("restore App session");
    assert_eq!(
        TianyiClient::new(restored, &http)
            .account()
            .expect("restored App account")
            .id,
        "alice@189.cn"
    );
    let requests = http.requests();
    assert_eq!(requests.len(), 5);
    assert!(requests[0].url.contains("/unifyLoginForPC.action?"));
    assert!(requests[0].url.contains("appId=8025431004"));
    assert!(requests[1]
        .url
        .ends_with("/api/logbox/oauth2/loginSubmit.do"));
    let login_body = requests[1].body.as_deref().unwrap_or_default();
    assert_eq!(form_field(login_body, "appKey"), "8025431004");
    assert_eq!(form_field(login_body, "clientType"), "10020");
    assert!(form_field(login_body, "userName").starts_with("{RSA}"));
    assert!(form_field(login_body, "password").starts_with("{RSA}"));
    assert!(!login_body.contains("alice@189.cn"));
    assert!(!login_body.contains("private-password"));
    assert!(requests[2]
        .url
        .contains("api.cloud.189.cn/getSessionForPC.action?"));
    assert!(requests[3].url.contains("cloud.189.cn/ssoLogin.action?"));
    assert!(requests[4]
        .url
        .contains("api.cloud.189.cn/listFiles.action?"));
    for header in ["Date", "SessionKey", "Signature", "X-Request-ID"] {
        assert!(requests[4].headers.contains_key(header), "missing {header}");
    }
    let signature = &requests[4].headers["Signature"];
    assert_eq!(signature.len(), 40);
    assert!(signature
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)));
}

#[test]
fn tianyi_app_download_signs_the_final_cdn_request() {
    // AppGetFileDownloadUrl is only half of Tianyi's desktop protocol. The
    // actual CDN request also needs the desktop client suffix and an HMAC
    // header set; returning browser Cookie headers makes listing work while
    // every real download fails.
    let login_page = tianyi_app_login_page();
    let http = RecordingHttp::sequence(&[
        &login_page,
        r#"{"result":0,"toUrl":"https://open.e.189.cn/callback?ticket=opaque"}"#,
        r#"{"res_code":0,"loginName":"alice@189.cn","sessionKey":"app-session-key","sessionSecret":"app-session-secret","accessToken":"access-token"}"#,
        "",
        r#"<listFiles><fileList><file><id>f-app</id><name>native.txt</name><size>12</size></file></fileList></listFiles>"#,
        r#"<fileDownloadUrl>https://download.cloud.189.cn/native.txt?token=opaque&amp;part=1</fileDownloadUrl>"#,
    ]);
    let session = TianyiClient::login_password("alice@189.cn", "private-password", &http)
        .expect("native App login");

    let download = TianyiClient::new(session, &http)
        .download("f-app")
        .expect("native App download resolution");

    assert!(
        download.url.starts_with(
            "https://download.cloud.189.cn/native.txt?token=opaque&part=1&clientType=TELEPC"
        ),
        "unexpected fixture URL: {}",
        download.url
    );
    assert!(download.url.contains("version=6.2"));
    assert!(download.url.contains("channelId=web_cloud.189.cn"));
    assert!(download.url.contains("rand="));
    for header in ["Date", "SessionKey", "Signature", "X-Request-ID"] {
        assert!(download.headers.contains_key(header), "missing {header}");
    }
    assert!(!download.headers.contains_key("Cookie"));
}

#[test]
fn tianyi_malformed_desktop_login_page_falls_back_without_panicking() {
    let http = RecordingHttp::sequence(&["<html><body>official login changed</body></html>"]);

    let result = TianyiClient::login_password("alice@189.cn", "private-password", &http);

    assert!(matches!(result, Err(QuarkError::WebAuthorizationRequired)));
    assert_eq!(http.requests().len(), 1);
}

#[test]
fn tianyi_app_xml_expiry_enters_session_recovery() {
    let login_page = tianyi_app_login_page();
    let http = RecordingHttp::sequence(&[
        &login_page,
        r#"{"result":0,"toUrl":"https://open.e.189.cn/callback?ticket=opaque"}"#,
        r#"{"res_code":0,"loginName":"alice@189.cn","sessionKey":"app-session-key","sessionSecret":"app-session-secret","accessToken":"access-token"}"#,
        "",
        r#"<listFiles><fileList><file><id>f-app</id><name>native.txt</name><size>12</size></file></fileList></listFiles>"#,
        r#"<error><code>InvalidSessionKey</code><message>session expired</message></error>"#,
    ]);
    let session = TianyiClient::login_password("alice@189.cn", "private-password", &http)
        .expect("native App login");

    let result = TianyiClient::new(session, &http).directory("-11");

    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
}

#[test]
fn tianyi_renames_a_folder_with_the_official_form_endpoint() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"fileInfo":{"fileId":"d1","fileName":"old","isFolder":true}}"#,
        r#"{"res_code":0}"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    client.rename("d1", "new").unwrap();

    let requests = http.requests();
    assert!(requests[1]
        .url
        .contains("/api/open/file/renameFolder.action"));
    assert!(requests[1]
        .body
        .as_deref()
        .is_some_and(|body| body.contains("folderId=d1") && body.contains("destFolderName=new")));
}

#[test]
fn tianyi_captcha_requests_web_authorization() {
    let http = RecordingHttp::challenge();
    assert!(matches!(
        TianyiClient::login_password("u", "p", &http),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert_eq!(
        http.requests()[0].headers["Content-Type"],
        "application/x-www-form-urlencoded"
    );
}

#[test]
fn tianyi_rejects_an_empty_login_cookie() {
    assert!(matches!(
        parse_tianyi_web_session(&["COOKIE_LOGIN_USER=; Path=/".into()]),
        Err(QuarkError::AuthorizationExpired)
    ));
}

#[test]
fn tianyi_accepts_the_current_browser_sso_cookie_after_web_authorization() {
    // The current official browser sign-in can issue SSON without the older
    // COOKIE_LOGIN_USER name.  Treating that successful login as expired
    // leaves the native authorization window open forever.
    let session = parse_tianyi_web_session(&[
        "SSON=browser-sso-session; Path=/; Domain=.cloud.189.cn".into(),
        "SESSION=portal-session; Path=/".into(),
    ])
    .expect("SSON is a valid official Tianyi browser session");

    assert!(session.cookie.contains("SSON=browser-sso-session"));
    assert!(session.cookie.contains("SESSION=portal-session"));
}

#[test]
fn tianyi_accepts_the_portal_login_name_returned_after_web_authorization() {
    // The browser portal confirms a valid web session with `loginName` rather
    // than a numeric `userId`. Rejecting this response prevents the command
    // layer from persisting the verified session and closing the auth window.
    let http = RecordingHttp::sequence(&[r#"{"res_code":0,"loginName":"alice@189.cn"}"#]);
    let account = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http)
        .account()
        .unwrap();

    assert_eq!(account.id, "alice@189.cn");
    assert_eq!(account.display_name, "alice@189.cn");
    assert!(http.requests()[0]
        .url
        .contains("/api/open/user/getUserInfoForPortal.action"));
}

#[test]
fn tianyi_account_rejection_reports_only_the_safe_protocol_code() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":"InvalidDeviceStatus","message":"private account details must not escape"}"#,
    ]);
    let error = TianyiClient::new(TianyiSession::cookie("SSON=local"), &http)
        .account()
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "网盘请求失败：天翼云盘拒绝请求（错误码 InvalidDeviceStatus）"
    );
    assert!(!error.to_string().contains("private account details"));
}

#[test]
fn tianyi_uploads_a_local_file_to_the_ephemeral_https_host() {
    let mut input = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    input.write_all(b"hello tianyi").unwrap();
    let name = input
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let http = RecordingHttp::sequence(&[
        "<script>var sessionKey = 'temporary-session-key';</script>",
        r#"{"res_code":0,"uploadUrl":"https://upload.example/receiver"}"#,
    ]);
    let upload = UploadRecordingHttp::sequence(200, &[r#"{"res_code":0,"id":"f2","size":12}"#]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    let file = client.upload_file("-11", input.path(), &upload).unwrap();

    assert_eq!(file.id, "f2");
    assert_eq!(file.name, name);
    let requests = upload.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://upload.example/receiver");
    assert!(requests[0]
        .body
        .windows(b"hello tianyi".len())
        .any(|part| part == b"hello tianyi"));
    assert!(requests[0]
        .headers
        .get("Content-Type")
        .is_some_and(|value| value.starts_with("multipart/form-data; boundary=")));
    assert!(http
        .requests()
        .iter()
        .all(|request| !request.url.contains("temporary-session-key")));
}

#[test]
fn tianyi_rejects_a_non_https_upload_host_before_sending_file_bytes() {
    let mut input = NamedTempFile::new().unwrap();
    input.write_all(b"hello tianyi").unwrap();
    let http = RecordingHttp::sequence(&[
        "<script>var sessionKey = 'temporary-session-key';</script>",
        r#"{"res_code":0,"uploadUrl":"http://upload.example/receiver"}"#,
    ]);
    let upload = UploadRecordingHttp::sequence(200, &[]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    assert!(matches!(
        client.upload_file("-11", input.path(), &upload),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(upload.requests().is_empty());
}

#[test]
fn tianyi_fails_when_the_upload_host_rejects_the_file() {
    let mut input = NamedTempFile::new().unwrap();
    input.write_all(b"hello tianyi").unwrap();
    let http = RecordingHttp::sequence(&[
        "<script>var sessionKey = 'temporary-session-key';</script>",
        r#"{"res_code":0,"uploadUrl":"https://upload.example/receiver"}"#,
    ]);
    let upload = UploadRecordingHttp::sequence(500, &[r#"{"res_code":500,"msg":"rejected"}"#]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);

    assert!(matches!(
        client.upload_file("-11", input.path(), &upload),
        Err(QuarkError::Provider(_))
    ));
}
