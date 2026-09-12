use super::QuarkError;
use reqwest::cookie::CookieStore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Instant,
};

const MAX_EXPLICIT_COOKIE_SESSIONS: usize = 64;
#[derive(Clone)]
pub struct CloudHttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct CloudHttpResponse {
    pub status: u16,
    pub body: String,
    /// Raw cookies issued by the response host. Attributes remain transient;
    /// only a provider-validated, URL-scoped snapshot may be persisted.
    pub set_cookies: Vec<String>,
}
pub trait CloudHttpTransport: Send + Sync {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError>;

    /// Upload workers can prevent snapshot publication after cooperative cancellation.
    fn check_transfer(&self) -> Result<(), QuarkError> {
        Ok(())
    }

    /// Creates a clean cookie context for account login. Stateless test
    /// transports can keep the default and receive the requests directly.
    fn isolated_session(&self) -> Result<Option<Box<dyn CloudHttpTransport>>, QuarkError> {
        Ok(None)
    }

    /// Stateful transports scope, expire and replace cookies themselves.
    fn manages_cookie_session(&self) -> bool {
        false
    }

    /// Export only cookies the jar would send to this exact URL. This is used
    /// after provider validation so the persisted snapshot includes cookies
    /// rotated by the validation responses.
    fn cookies_for_url(&self, _url: &str) -> Result<Vec<String>, QuarkError> {
        Ok(Vec::new())
    }
}

pub(crate) struct ControlledUploadHttp<'a, T> {
    pub inner: &'a T,
    pub control: super::UploadControl,
}
impl<T: CloudHttpTransport> CloudHttpTransport for ControlledUploadHttp<'_, T> {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.control.check()?;
        self.inner.send(request)
    }
    fn check_transfer(&self) -> Result<(), QuarkError> {
        self.control.check()
    }
    fn manages_cookie_session(&self) -> bool {
        self.inner.manages_cookie_session()
    }
    fn cookies_for_url(&self, url: &str) -> Result<Vec<String>, QuarkError> {
        self.inner.cookies_for_url(url)
    }
}
impl<T: super::CloudUploadTransport> super::CloudUploadTransport for ControlledUploadHttp<'_, T> {
    fn send_upload(
        &self,
        mut request: super::UploadHttpRequest,
    ) -> Result<super::UploadHttpResponse, QuarkError> {
        self.control.check()?;
        request.body = super::UploadBody::Controlled {
            body: Box::new(request.body),
            control: self.control.clone(),
        };
        let response = self.inner.send_upload(request)?;
        self.control.check()?;
        Ok(response)
    }
}
pub struct ReqwestTransport {
    pub(super) client: reqwest::blocking::Client,
    jar: Arc<reqwest::cookie::Jar>,
    explicit_cookie_sessions: Mutex<HashMap<[u8; 32], ExplicitCookieSession>>,
}

struct ExplicitCookieSession {
    client: reqwest::blocking::Client,
    _jar: Arc<reqwest::cookie::Jar>,
    last_used: Instant,
}

impl ReqwestTransport {
    fn client_with_jar(
        jar: Arc<reqwest::cookie::Jar>,
    ) -> Result<reqwest::blocking::Client, QuarkError> {
        reqwest::blocking::Client::builder()
            .cookie_provider(jar)
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
            .build()
            .map_err(|_| QuarkError::Transport("无法初始化 HTTPS 客户端".into()))
    }

    fn explicit_cookie_client(
        &self,
        url: &reqwest::Url,
        cookie_header: &str,
    ) -> Result<reqwest::blocking::Client, QuarkError> {
        let origin = url.origin().ascii_serialization();
        let mut hasher = Sha256::new();
        hasher.update(origin.as_bytes());
        hasher.update([0]);
        hasher.update(cookie_header.as_bytes());
        let key: [u8; 32] = hasher.finalize().into();
        let now = Instant::now();
        let mut sessions = self
            .explicit_cookie_sessions
            .lock()
            .map_err(|_| QuarkError::Transport("HTTP 会话状态不可用".into()))?;
        if let Some(session) = sessions.get_mut(&key) {
            session.last_used = now;
            return Ok(session.client.clone());
        }
        if sessions.len() >= MAX_EXPLICIT_COOKIE_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.last_used)
                .map(|(key, _)| *key)
            {
                sessions.remove(&oldest);
            }
        }
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let root_url =
            reqwest::Url::parse(&format!("{origin}/")).map_err(|_| QuarkError::InvalidResponse)?;
        for cookie in cookie_header
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            // Persisted provider sessions intentionally contain only cookie
            // pairs, not browser-only attributes. Scope those pairs to the
            // provider origin root so auth survives switches between API
            // paths while response Set-Cookie still observes its real scope.
            jar.add_cookie_str(cookie, &root_url);
        }
        let client = Self::client_with_jar(Arc::clone(&jar))?;
        sessions.insert(
            key,
            ExplicitCookieSession {
                client: client.clone(),
                _jar: jar,
                last_used: now,
            },
        );
        Ok(client)
    }

    pub fn new() -> Result<Self, QuarkError> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        Ok(Self {
            client: Self::client_with_jar(Arc::clone(&jar))?,
            jar,
            explicit_cookie_sessions: Mutex::new(HashMap::new()),
        })
    }
}
impl CloudHttpTransport for ReqwestTransport {
    fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let has_content_type = r
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("Content-Type"));
        let mut explicit_cookie = None;
        let mut headers = Vec::with_capacity(r.headers.len());
        for (name, value) in r.headers {
            if name.eq_ignore_ascii_case("Cookie") {
                explicit_cookie = Some(value);
            } else {
                headers.push((name, value));
            }
        }
        let client = if let Some(cookie) = explicit_cookie.as_deref() {
            let url = reqwest::Url::parse(&r.url).map_err(|_| QuarkError::InvalidResponse)?;
            self.explicit_cookie_client(&url, cookie)?
        } else {
            self.client.clone()
        };
        let mut b = client.request(
            r.method.parse().map_err(|_| QuarkError::InvalidResponse)?,
            &r.url,
        );
        for (k, v) in headers {
            b = b.header(k, v)
        }
        if let Some(body) = r.body {
            if !has_content_type {
                b = b.header("Content-Type", "application/json");
            }
            b = b.body(body)
        }
        // URL-bearing errors may disclose signed credentials.
        let response = b.send().map_err(|e| {
            QuarkError::Transport(
                if e.is_timeout() {
                    "连接超时"
                } else if e.is_connect() {
                    "连接失败"
                } else {
                    "HTTPS 请求失败"
                }
                .into(),
            )
        })?;
        let status = response.status().as_u16();
        let set_cookies = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect();
        let body = response.text().map_err(|_| QuarkError::InvalidResponse)?;
        Ok(CloudHttpResponse {
            status,
            body,
            set_cookies,
        })
    }

    fn isolated_session(&self) -> Result<Option<Box<dyn CloudHttpTransport>>, QuarkError> {
        Ok(Some(Box::new(Self::new()?)))
    }

    fn manages_cookie_session(&self) -> bool {
        true
    }

    fn cookies_for_url(&self, url: &str) -> Result<Vec<String>, QuarkError> {
        let url = reqwest::Url::parse(url).map_err(|_| QuarkError::InvalidResponse)?;
        let Some(header) = self.jar.cookies(&url) else {
            return Ok(Vec::new());
        };
        let header = header.to_str().map_err(|_| QuarkError::InvalidResponse)?;
        Ok(header
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
            .map(str::to_owned)
            .collect())
    }
}
pub(crate) fn response_json(r: &CloudHttpResponse) -> Result<Value, QuarkError> {
    if r.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    let v: Value = if r.body.is_empty() && (200..300).contains(&r.status) {
        serde_json::json!({})
    } else {
        serde_json::from_str(&r.body).map_err(|_| QuarkError::InvalidResponse)?
    };
    let code = v
        .get("error_code")
        .or_else(|| v.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let status = v.get("status").and_then(Value::as_i64).unwrap_or(200);
    let error = v.get("error").and_then(Value::as_str).unwrap_or("");
    if matches!(code, 31001 | 401 | 16 | 4122)
        || status == 401
        || matches!(error, "unauthenticated" | "invalid_grant" | "invalid_token")
    {
        return Err(QuarkError::AuthorizationExpired);
    }
    if code == 9 || error == "riskLimited" {
        return Err(QuarkError::CaptchaRequired);
    }
    if code == 4031 || error.to_ascii_lowercase().contains("captcha") {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    if !(200..300).contains(&r.status)
        || code != 0
        || !matches!(status, 0 | 200 | 2000000)
        || !error.is_empty()
    {
        return Err(QuarkError::Provider(format!(
            "HTTP {} / code {} / {}",
            r.status,
            code,
            if error.is_empty() {
                "服务端拒绝操作"
            } else {
                error
            }
        )));
    }
    Ok(v)
}

#[cfg(test)]
mod upload_tests {
    use super::*;
    use crate::cloud::{
        CloudUploadTransport, UploadBody, UploadControl, UploadFileSnapshot, UploadHttpRequest,
    };
    use std::io::{Read, Write};

    #[test]
    fn reqwest_streams_exact_content_length_and_file_slice() {
        let source = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(source.path(), b"prefix-PAYLOAD-suffix").unwrap();
        let snapshot = UploadFileSnapshot::capture(source.path()).unwrap();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/upload", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.contains("content-length: 7\r\n"));
            assert!(!headers.contains("transfer-encoding:"));
            let mut body = [0; 7];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"PAYLOAD");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let control = UploadControl::default();
        let http = ReqwestTransport::new().unwrap();
        let controlled = ControlledUploadHttp {
            inner: &http,
            control: control.clone(),
        };
        let result = controlled.send_upload(UploadHttpRequest {
            method: "PUT",
            url,
            headers: BTreeMap::new(),
            body: snapshot.slice(7, 7).unwrap(),
        });
        worker.join().unwrap();
        assert_eq!(result.unwrap().status, 200);
        assert_eq!(control.bytes_read(), 7);
    }

    #[test]
    fn cancelled_transport_sends_neither_metadata_nor_mutation_requests() {
        struct Never;
        impl CloudHttpTransport for Never {
            fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
                panic!("cancelled metadata request started")
            }
        }
        impl CloudUploadTransport for Never {
            fn send_upload(
                &self,
                _: UploadHttpRequest,
            ) -> Result<crate::cloud::UploadHttpResponse, QuarkError> {
                panic!("cancelled transfer started")
            }
        }
        let control = UploadControl::default();
        let http = ControlledUploadHttp {
            inner: &Never,
            control: control.clone(),
        };
        control.cancel();
        assert!(http.check_transfer().is_err());
        assert!(http
            .send(CloudHttpRequest {
                method: "POST",
                url: "https://example.com/commit".into(),
                headers: BTreeMap::new(),
                body: None
            })
            .is_err());
        assert!(http
            .send_upload(UploadHttpRequest {
                method: "POST",
                url: "https://example.com/upload".into(),
                headers: BTreeMap::new(),
                body: UploadBody::Bytes(vec![])
            })
            .is_err());
    }
}
