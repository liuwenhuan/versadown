//! The loopback listener behind the browser sign-in: it answers exactly one
//! request, the redirect carrying the right `state`, and ignores everything
//! else (favicon, port scanners, stale tabs, forged redirects).

use super::FlowError;
use crate::cloud::QuarkError;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const REQUEST_LIMIT: usize = 8 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_INTERVAL: Duration = Duration::from_millis(10);

/// Bind a free port on `127.0.0.1`; for `localhost` also on `::1` (best
/// effort) since the name may resolve to either address.
pub(super) fn bind(host: &str) -> Result<(Vec<TcpListener>, u16), QuarkError> {
    let v4 = TcpListener::bind("127.0.0.1:0")
        .map_err(|_| QuarkError::Transport("无法启动本机授权回调".into()))?;
    let port = v4.local_addr().map_err(|_| QuarkError::Storage)?.port();
    v4.set_nonblocking(true).map_err(|_| QuarkError::Storage)?;
    let mut listeners = vec![v4];
    if host == "localhost" {
        if let Ok(v6) = TcpListener::bind(("::1", port)) {
            if v6.set_nonblocking(true).is_ok() {
                listeners.push(v6);
            }
        }
    }
    Ok((listeners, port))
}

/// What one HTTP request on the loopback port means.
enum Callback {
    /// Not the redirect (favicon, probes): ignored.
    Unrelated,
    /// The redirect, but not ours (missing or wrong `state`): ignored.
    Forged,
    Code(String),
    Failed(FlowError),
}

fn interpret(request: &str, expected_state: &str) -> Callback {
    let Some(target) = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split_whitespace().next())
    else {
        return Callback::Unrelated;
    };
    let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
        return Callback::Unrelated;
    };
    if url.path() != "/" {
        return Callback::Unrelated;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
    };
    // A foreign page must not be able to complete (or cancel) the flow.
    if param("state").as_deref() != Some(expected_state) {
        return Callback::Forged;
    }
    match (
        param("code").filter(|code| !code.is_empty()),
        param("error"),
    ) {
        (_, Some(error)) if error == "access_denied" => Callback::Failed(FlowError::Denied),
        (_, Some(_)) => Callback::Failed(FlowError::Failed),
        (Some(code), None) => Callback::Code(code),
        (None, None) => Callback::Failed(FlowError::Failed),
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let mut buffer = [0u8; REQUEST_LIMIT];
    let mut length = 0;
    while length < buffer.len() {
        match stream.read(&mut buffer[length..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                length += read;
                if buffer[..length]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n")
                {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// The page is served outside the app's UI, so it follows the browser's
/// language and shows both supported languages when it does not say.
fn landing_page(accept_language: &str, success: bool) -> String {
    let (zh, en) = if success {
        (
            "授权已完成，可以关闭此页面并返回 VersaDown。",
            "Authorization complete. You can close this page and return to VersaDown.",
        )
    } else {
        (
            "授权未完成，请返回 VersaDown 重试。",
            "Authorization was not completed. Return to VersaDown and try again.",
        )
    };
    let first = accept_language
        .split(',')
        .next()
        .map(|language| language.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let message = if first.is_empty() {
        format!("<p>{zh}</p><p>{en}</p>")
    } else if first.starts_with("zh") {
        format!("<p>{zh}</p>")
    } else {
        format!("<p>{en}</p>")
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>VersaDown</title></head><body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\">{message}</body></html>"
    )
}

fn accepted(listeners: &[TcpListener]) -> Option<TcpStream> {
    listeners
        .iter()
        .find_map(|listener| listener.accept().ok().map(|(stream, _)| stream))
}

/// Wait for the redirect until `timeout` (or cancellation).
pub(super) fn serve(
    listeners: &[TcpListener],
    expected_state: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<String, FlowError> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err(FlowError::Timeout);
        }
        let Some(mut stream) = accepted(listeners) else {
            std::thread::sleep(ACCEPT_INTERVAL);
            continue;
        };
        let _ = stream.set_nonblocking(false);
        let request = read_request(&mut stream);
        let language = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("accept-language")
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default();
        match interpret(&request, expected_state) {
            Callback::Unrelated => respond(&mut stream, "404 Not Found", ""),
            Callback::Forged => respond(&mut stream, "400 Bad Request", ""),
            Callback::Code(code) => {
                respond(&mut stream, "200 OK", &landing_page(&language, true));
                return Ok(code);
            }
            Callback::Failed(failure) => {
                respond(&mut stream, "200 OK", &landing_page(&language, false));
                return Err(failure);
            }
        }
    }
}
