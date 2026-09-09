use alldownloader_lib::cloud::{
    tianyi::{parse_tianyi_web_session, TianyiClient, TianyiSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport, QuarkError,
    UploadHttpRequest, UploadHttpResponse,
};
use std::{collections::BTreeMap, io::Write, sync::Mutex};
use tempfile::NamedTempFile;

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
fn tianyi_rejects_non_https_dlinks_and_uses_native_mutation_endpoints() {
    let http = RecordingHttp::sequence(&[
        r#"{"res_code":0,"fileDownloadUrl":"http://download.example/photo.jpg"}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0}"#,
        r#"{"res_code":0}"#,
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
    assert!(requests[2].url.contains("/api/open/file/rename.action"));
    assert!(requests[3].url.contains("/api/open/file/moveFiles.action"));
    assert!(requests[4].url.contains("/api/open/file/copyFiles.action"));
    assert!(requests[5]
        .url
        .contains("/api/open/file/deleteFiles.action"));
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
