use alldownloader_lib::cloud::{
    baidu::{BaiduClient, BaiduSession},
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
fn baidu_lists_with_session_cookie_and_resolves_an_https_dlink() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"fs_id":7,"path":"/movie.mkv","isdir":0,"size":12}]}"#,
        r#"{"errno":0,"list":[{"dlink":"https://download.example/file","server_filename":"movie.mkv"}]}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local; STOKEN=token"), &http);

    assert_eq!(client.directory("/").unwrap().items[0].name, "movie.mkv");
    let download = client.download("7").unwrap();
    assert!(download.url.starts_with("https://"));
    assert!(download.headers.contains_key("Cookie"));

    let requests = http.requests();
    assert!(requests[0].url.contains("/rest/2.0/xpan/file?method=list"));
    assert!(requests[1]
        .url
        .contains("/rest/2.0/xpan/multimedia?method=filemetas"));
    assert!(requests[0].headers["Cookie"].contains("BDUSS=local"));
    assert_eq!(requests[1].headers["Referer"], "https://pan.baidu.com/");
}

#[test]
fn baidu_rejects_non_https_dlinks_and_uses_native_mutation_endpoints() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"dlink":"http://download.example/file"}]}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);

    assert!(matches!(
        client.download("7"),
        Err(QuarkError::InvalidResponse)
    ));
    client.mkdir("/", "new folder").unwrap();
    client.rename("7", "renamed.mkv").unwrap();
    client.move_files(&["7".into()], "/target").unwrap();
    client.copy_files(&["7".into()], "/target").unwrap();
    client.trash(&["7".into()]).unwrap();

    let requests = http.requests();
    assert!(requests[1].url.contains("method=create"));
    assert!(requests[2].url.contains("method=update"));
    assert!(
        requests[3].url.contains("method=filemanager") && requests[3].url.contains("opera=move")
    );
    assert!(
        requests[4].url.contains("method=filemanager") && requests[4].url.contains("opera=copy")
    );
    assert!(
        requests[5].url.contains("method=filemanager") && requests[5].url.contains("opera=delete")
    );
}

#[test]
fn baidu_password_challenge_requires_web_authorization_without_a_session() {
    let http = RecordingHttp::sequence(&[r#"{"err_no":257,"codeString":"captcha"}"#]);

    assert!(matches!(
        BaiduClient::login_password("alice", "password", &http),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(http.requests()[0]
        .body
        .as_deref()
        .unwrap()
        .contains("username=alice"));
}
