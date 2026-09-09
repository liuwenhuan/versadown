use alldownloader_lib::cloud::{
    baidu::{BaiduClient, BaiduSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError, ReqwestTransport,
};
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
fn baidu_rejects_non_https_dlinks_and_unsafe_mutations_before_transport() {
    let http = RecordingHttp::sequence(&[
        r#"{"errno":0,"list":[{"dlink":"http://download.example/file"}]}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local"), &http);

    assert!(matches!(
        client.download("7"),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(matches!(
        client.mkdir("/", "new folder"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(matches!(
        client.rename("7", "renamed.mkv"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(matches!(
        client.move_files(&["7".into()], "/target"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(matches!(
        client.copy_files(&["7".into()], "/target"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(matches!(
        client.trash(&["7".into()]),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(matches!(
        client.upload("/", "movie.mkv", 12, "md5"),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert_eq!(
        http.requests().len(),
        1,
        "unsafe writes must not reach the transport"
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
    assert_eq!(
        http.requests()[0].headers["Content-Type"],
        "application/x-www-form-urlencoded"
    );
}
