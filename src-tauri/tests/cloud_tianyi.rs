use alldownloader_lib::cloud::{
    tianyi::{TianyiClient, TianyiSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError,
};
use std::sync::Mutex;

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
    assert!(matches!(
        TianyiClient::login_password("u", "p", &RecordingHttp::challenge()),
        Err(QuarkError::WebAuthorizationRequired)
    ));
}
