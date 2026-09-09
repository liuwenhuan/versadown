use alldownloader_lib::cloud::{
    baidu::{parse_baidu_web_session, BaiduClient, BaiduSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport, QuarkError,
    ReqwestTransport, UploadHttpRequest, UploadHttpResponse,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{mpsc, Mutex},
    thread,
};

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

struct UploadRecordingHttp {
    replies: Mutex<Vec<UploadHttpResponse>>,
    requests: Mutex<Vec<UploadHttpRequest>>,
}

impl UploadRecordingHttp {
    fn sequence(bodies: &[&str]) -> Self {
        Self {
            replies: Mutex::new(
                bodies
                    .iter()
                    .map(|body| UploadHttpResponse {
                        status: 200,
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

fn form_field(body: &str, wanted: &str) -> String {
    url::form_urlencoded::parse(body.as_bytes())
        .find_map(|(name, value)| (name == wanted).then(|| value.into_owned()))
        .unwrap_or_default()
}

fn opaque_reference(path: &str) -> String {
    format!(
        "bdr_{}",
        URL_SAFE_NO_PAD.encode(serde_json::json!({"fsid":"7","path":path}).to_string())
    )
}

#[test]
fn baidu_accepts_the_secure_webview_session_cookie_after_browser_authorization() {
    let session = parse_baidu_web_session(&[
        "BAIDUID=browser-identity".into(),
        "BDUSS_BFESS=secure-webview-session".into(),
        "STOKEN=netdisk-token".into(),
    ])
    .unwrap();

    assert_eq!(
        session.cookie,
        "BAIDUID=browser-identity; BDUSS_BFESS=secure-webview-session; STOKEN=netdisk-token"
    );
}

#[test]
fn baidu_web_session_retains_the_complete_browser_cookie_jar() {
    let session = parse_baidu_web_session(&[
        "BAIDUID=browser-identity".into(),
        "BDUSS_BFESS=secure-webview-session".into(),
        "PANPSC=netdisk-session".into(),
        "PANWEB=web-context".into(),
        "SCRC=anti-replay-context".into(),
        "SBOXTKN=download-context".into(),
    ])
    .unwrap();

    for pair in [
        "BAIDUID=browser-identity",
        "BDUSS_BFESS=secure-webview-session",
        "PANPSC=netdisk-session",
        "PANWEB=web-context",
        "SCRC=anti-replay-context",
        "SBOXTKN=download-context",
    ] {
        assert!(
            session.cookie.split("; ").any(|saved| saved == pair),
            "missing browser session cookie {pair}"
        );
    }
}

#[test]
fn baidu_web_session_rejects_cookie_header_injection() {
    let session = parse_baidu_web_session(&[
        "BDUSS=authorized-session".into(),
        "invalid\r\nX-Injected=value".into(),
    ])
    .unwrap();

    assert_eq!(session.cookie, "BDUSS=authorized-session");
}

#[test]
fn baidu_file_requests_alias_a_persisted_secure_webview_session_for_the_native_api() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"fs_id":7,"path":"/movie.mkv","isdir":0,"size":12}]}"#,
    ]);
    let client = BaiduClient::new(
        BaiduSession::cookie(
            "BAIDUID=browser-identity; BDUSS_BFESS=secure-webview-session; STOKEN=netdisk-token",
        ),
        &http,
    );

    client.directory("/").unwrap();

    let request = http.requests().into_iter().next().unwrap();
    assert!(request.headers["Cookie"].contains("BDUSS_BFESS=secure-webview-session"));
    assert!(request.headers["Cookie"].contains("BDUSS=secure-webview-session"));
}

#[test]
fn reqwest_transport_preserves_the_explicit_form_content_type() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (headers_tx, headers_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        headers_tx
            .send(String::from_utf8(request).unwrap())
            .unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .unwrap();
    });

    ReqwestTransport::new()
        .unwrap()
        .send(CloudHttpRequest {
            method: "POST",
            url: format!("http://{address}/login"),
            headers: BTreeMap::from([(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )]),
            body: Some("username=alice".into()),
        })
        .unwrap();
    let request = headers_rx.recv().unwrap().to_ascii_lowercase();
    server.join().unwrap();

    assert!(request.contains("content-type: application/x-www-form-urlencoded"));
    assert!(
        !request.contains("application/json"),
        "an explicit form type must not be accompanied by a JSON type"
    );
}

#[test]
fn baidu_lists_with_session_cookie_and_resolves_an_https_dlink() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"fs_id":7,"path":"/movie.mkv","isdir":0,"size":12}]}"#,
        r#"{"errno":0,"list":[{"dlink":"https://download.example/file","server_filename":"movie.mkv"}]}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local; STOKEN=token"), &http);

    let directory = client.directory("/").unwrap();
    assert_eq!(directory.items[0].name, "movie.mkv");
    assert_ne!(directory.items[0].id, "7", "ids must remain opaque");
    let download = client.download(&directory.items[0].id).unwrap();
    assert!(download.url.starts_with("https://"));
    assert!(download.headers.contains_key("Cookie"));

    let requests = http.requests();
    assert!(requests[0]
        .url
        .starts_with("https://pan.baidu.com/api/list?"));
    assert!(requests[1]
        .url
        .contains("/rest/2.0/xpan/multimedia?method=filemetas"));
    assert!(requests[1].url.contains("fsids=%5B7%5D"));
    assert!(requests[0].headers["Cookie"].contains("BDUSS=local"));
    assert_eq!(requests[1].headers["Referer"], "https://pan.baidu.com/");
}

#[test]
fn baidu_directory_uses_the_web_disk_listing_endpoint_for_a_browser_session() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"fs_id":7,"path":"/movie.mkv","isdir":0,"size":12}]}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS_BFESS=browser-session"), &http);

    client.directory("/").unwrap();

    let request = http.requests().into_iter().next().unwrap();
    assert!(request.url.starts_with("https://pan.baidu.com/api/list?"));
    assert!(request.url.contains("dir=%2F"));
    assert!(request.url.contains("channel=chunlei"));
}

#[test]
fn baidu_rejects_malformed_references_before_transport() {
    let http = RecordingHttp::sequence(&[]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);

    assert!(matches!(
        client.download("bad-reference"),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(matches!(
        client.rename("bad-reference", "renamed.mkv"),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(matches!(
        client.mkdir("bad-reference", "new folder"),
        Err(QuarkError::InvalidResponse)
    ));
    for path in [
        "/./movie.mkv",
        "/dir/../movie.mkv",
        "/dir//movie.mkv",
        "/dir/\u{0007}movie.mkv",
    ] {
        let reference = opaque_reference(path);
        assert!(matches!(
            client.download(&reference),
            Err(QuarkError::InvalidResponse)
        ));
        assert!(matches!(
            client.mkdir(&reference, "new folder"),
            Err(QuarkError::InvalidResponse)
        ));
    }
    assert!(matches!(
        client.move_files(&[opaque_reference("/")], "/"),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(matches!(
        client.copy_files(&[opaque_reference("/")], "/"),
        Err(QuarkError::InvalidResponse)
    ));
    let forged_root = opaque_reference("/");
    assert!(matches!(
        client.rename(&forged_root, "renamed.mkv"),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(matches!(
        client.trash(&[forged_root.clone()]),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(matches!(
        client.download(&forged_root),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(
        http.requests().is_empty(),
        "malformed references must not reach the transport"
    );
}

#[test]
fn baidu_uses_a_fresh_web_token_and_path_payload_for_every_mutation() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"fs_id":7,"path":"/movie.mkv","isdir":0},{"fs_id":8,"path":"/target","isdir":1}]}"#,
        r#"{"errno":0,"result":{"bdstoken":"token-1"}}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0,"result":{"bdstoken":"token-2"}}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0,"result":{"bdstoken":"token-3"}}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0,"result":{"bdstoken":"token-4"}}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0,"result":{"bdstoken":"token-5"}}"#,
        r#"{"errno":0}"#,
        r#"{"errno":0,"result":{"bdstoken":"token-6"}}"#,
        r#"{"errno":0}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);
    let directory = client.directory("/").unwrap();
    let file = directory
        .items
        .iter()
        .find(|item| item.name == "movie.mkv")
        .unwrap()
        .id
        .clone();
    let target = directory
        .items
        .iter()
        .find(|item| item.name == "target")
        .unwrap()
        .id
        .clone();

    client.mkdir(&target, "child").unwrap();
    client.rename(&file, "renamed.mkv").unwrap();
    client.move_files(&[file.clone()], &target).unwrap();
    client.copy_files(&[file.clone()], &target).unwrap();
    client.trash(&[file.clone()]).unwrap();
    client.upload(&target, "upload.mkv", 12, "md5").unwrap();

    let requests = http.requests();
    for index in (1..requests.len()).step_by(2) {
        assert!(requests[index].url.contains("/api/gettemplatevariable"));
        assert!(requests[index + 1]
            .url
            .contains(&format!("bdstoken=token-{}", (index + 1) / 2)));
        assert_eq!(
            requests[index + 1].headers["Content-Type"],
            "application/x-www-form-urlencoded"
        );
    }
    assert!(requests[2]
        .body
        .as_deref()
        .unwrap()
        .contains("path=%2Ftarget%2Fchild"));
    assert!(requests[2].url.contains("a=commit"));
    assert_eq!(
        form_field(requests[2].body.as_deref().unwrap(), "path"),
        "/target/child"
    );
    assert_eq!(
        form_field(requests[2].body.as_deref().unwrap(), "isdir"),
        "1"
    );
    assert_eq!(
        form_field(requests[2].body.as_deref().unwrap(), "block_list"),
        "[]"
    );
    assert!(requests[4]
        .body
        .as_deref()
        .unwrap()
        .contains("%2Fmovie.mkv"));
    assert!(
        requests[6]
            .body
            .as_deref()
            .unwrap()
            .contains("%2Fmovie.mkv")
            && requests[6].body.as_deref().unwrap().contains("%2Ftarget")
    );
    let moved: serde_json::Value = serde_json::from_str(&form_field(
        requests[6].body.as_deref().unwrap(),
        "filelist",
    ))
    .unwrap();
    assert_eq!(moved[0]["path"], "/movie.mkv");
    assert_eq!(moved[0]["dest"], "/target");
    assert_eq!(moved[0]["newname"], "movie.mkv");
    assert!(
        requests[8]
            .body
            .as_deref()
            .unwrap()
            .contains("%2Fmovie.mkv")
            && requests[8].body.as_deref().unwrap().contains("%2Ftarget")
    );
    let copied: serde_json::Value = serde_json::from_str(&form_field(
        requests[8].body.as_deref().unwrap(),
        "filelist",
    ))
    .unwrap();
    assert_eq!(copied[0]["path"], "/movie.mkv");
    assert_eq!(copied[0]["dest"], "/target");
    assert_eq!(copied[0]["newname"], "movie.mkv");
    assert!(requests[10]
        .body
        .as_deref()
        .unwrap()
        .contains("%2Fmovie.mkv"));
    assert!(requests[12]
        .body
        .as_deref()
        .unwrap()
        .contains("path=%2Ftarget%2Fupload.mkv"));
    assert!(!requests.iter().skip(1).any(|request| request
        .body
        .as_deref()
        .unwrap_or("")
        .contains("%227%22")));
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
    assert_eq!(
        http.requests()[0].headers["Content-Type"],
        "application/x-www-form-urlencoded"
    );
}

#[test]
fn baidu_uploads_a_local_file_then_commits_without_retaining_the_transfer_url() {
    let mut input = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    input.write_all(b"hello baidu").unwrap();
    let name = input
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let hash = format!("{:x}", md5::compute(b"hello baidu"));
    let precreate = format!(
        r#"{{"errno":0,"return_type":1,"uploadid":"upload-1","block_list":[0],"expected":"{hash}"}}"#
    );
    let slice = format!(r#"{{"md5":"{hash}"}}"#);
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"result":{"bdstoken":"fresh-token"}}"#,
        &precreate,
        r#"{"errno":0,"fs_id":7,"path":"/hello.txt"}"#,
    ]);
    let upload = UploadRecordingHttp::sequence(&[&slice]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);

    let file = client.upload_file("/", input.path(), &upload).unwrap();

    assert_eq!(file.name, name);
    assert_eq!(file.size, Some(11));
    let requests = upload.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.starts_with("https://"));
    assert!(requests[0].url.contains("method=upload"));
    assert!(requests[0]
        .body
        .windows(b"hello baidu".len())
        .any(|part| part == b"hello baidu"));
    assert!(requests[0]
        .headers
        .get("Content-Type")
        .is_some_and(|value| value.starts_with("multipart/form-data; boundary=")));
    assert!(
        http.requests()
            .iter()
            .all(|request| !request.url.contains("upload-1")),
        "upload id must never be persisted in a follow-up URL outside the in-flight transfer"
    );
}

#[test]
fn baidu_upload_rejects_a_non_regular_file_before_starting_a_transfer() {
    let directory = tempfile::tempdir().unwrap();
    let http = RecordingHttp::sequence(&[]);
    let upload = UploadRecordingHttp::sequence(&[]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);

    assert!(matches!(
        client.upload_file("/", directory.path(), &upload),
        Err(QuarkError::InvalidInput(_))
    ));
    assert!(http.requests().is_empty());
    assert!(upload.requests().is_empty());
}

#[test]
fn baidu_commits_a_zero_slice_upload_instead_of_treating_precreate_as_a_file() {
    let mut input = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    input.write_all(b"rapid upload").unwrap();
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"result":{"bdstoken":"fresh-token"}}"#,
        r#"{"errno":0,"return_type":2,"uploadid":"rapid-1","block_list":[]}"#,
        r#"{"errno":0,"fs_id":8,"path":"/rapid.txt","server_filename":"rapid.txt"}"#,
    ]);
    let upload = UploadRecordingHttp::sequence(&[]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);

    let file = client.upload_file("/", input.path(), &upload).unwrap();

    assert_eq!(file.name, "rapid.txt");
    assert!(upload.requests().is_empty());
    assert_eq!(http.requests().len(), 3);
    assert!(http.requests()[2].url.contains("/api/create"));
}
