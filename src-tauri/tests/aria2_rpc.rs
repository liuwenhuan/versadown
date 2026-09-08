use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::mpsc,
};

use alldownloader_lib::{Aria2Client, ProxySettings};
use serde_json::Value;

fn read_http_body(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    let mut buffer = [0u8; 2048];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        received.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = received.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&received[..header_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .unwrap();
            let body_start = header_end + 4;
            if received.len() >= body_start + length {
                return received[body_start..body_start + length].to_vec();
            }
        }
    }
}

#[test]
fn add_uri_sends_authenticated_json_rpc_and_returns_gid() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let body = read_http_body(&mut stream);
        sender.send(body).unwrap();
        let response = r#"{"jsonrpc":"2.0","id":"test","result":"2089b05ecca3d829"}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
    });

    let client = Aria2Client::new(address, "local-secret".into());
    let gid = client
        .add_uri(
            "https://example.com/game.zip",
            Path::new(r"D:\Downloads"),
            &ProxySettings::default(),
            None,
        )
        .unwrap();

    assert_eq!(gid, "2089b05ecca3d829");
    let request: Value = serde_json::from_slice(&receiver.recv().unwrap()).unwrap();
    assert_eq!(request["method"], "aria2.addUri");
    assert_eq!(request["params"][0], "token:local-secret");
    assert_eq!(request["params"][1][0], "https://example.com/game.zip");
    assert_eq!(request["params"][2]["dir"], r"D:\Downloads");
    assert!(request["params"][2].get("allow-overwrite").is_none());
}

#[test]
fn explicit_overwrite_option_is_sent_only_after_duplicate_confirmation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request: Value = serde_json::from_slice(&read_http_body(&mut stream)).unwrap();
        let response = r#"{"jsonrpc":"2.0","id":"test","result":"2089b05ecca3d829"}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        request
    });
    let client = Aria2Client::new(address, "local-secret".into());

    client
        .add_uri_with_options_and_overwrite(
            "https://example.com/game.zip",
            Path::new(r"D:\Downloads"),
            &ProxySettings::default(),
            None,
            &[],
            None,
            true,
        )
        .unwrap();

    assert_eq!(server.join().unwrap()["params"][2]["allow-overwrite"], "true");
}

#[test]
fn cloud_download_preserves_provider_headers_and_filename_in_rpc_options() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request: Value = serde_json::from_slice(&read_http_body(&mut stream)).unwrap();
        let response = r#"{"jsonrpc":"2.0","id":"test","result":"2089b05ecca3d829"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .unwrap();
        request
    });
    let client = Aria2Client::new(address, "local-secret".into());
    let headers = vec![
        ("Referer".into(), "https://pan.quark.cn/".into()),
        ("User-Agent".into(), "Cloud Download Test".into()),
        ("Cookie".into(), "test_session=fake-fixture".into()),
    ];
    let gid = client
        .add_uri_with_options(
            "https://example.com/signed?token=fixture",
            Path::new(r"D:\Downloads"),
            &ProxySettings::default(),
            None,
            &headers,
            Some("报告.pdf"),
        )
        .unwrap();
    assert_eq!(gid, "2089b05ecca3d829");
    let request = server.join().unwrap();
    assert_eq!(
        request["params"][1][0],
        "https://example.com/signed?token=fixture"
    );
    assert_eq!(
        request["params"][2]["header"],
        serde_json::json!([
            "Referer: https://pan.quark.cn/",
            "User-Agent: Cloud Download Test",
            "Cookie: test_session=fake-fixture",
        ])
    );
    assert_eq!(request["params"][2]["out"], "报告.pdf");
}

#[test]
fn cloud_download_rejects_header_injection_and_names_that_escape_the_directory() {
    let client = Aria2Client::new("127.0.0.1:1".parse().unwrap(), "test".into());
    let headers = vec![("Cookie".into(), "secret\r\nInjected: value".into())];
    let error = client
        .add_uri_with_options(
            "https://example.com/file",
            Path::new(r"D:\Downloads"),
            &ProxySettings::default(),
            None,
            &headers,
            None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("invalid download header"));
    assert!(!error.to_string().contains("secret"));
    let error = client
        .add_uri_with_options(
            "https://example.com/file",
            Path::new(r"D:\Downloads"),
            &ProxySettings::default(),
            None,
            &[],
            Some("../outside.txt"),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("filename must not contain a path"));
}
