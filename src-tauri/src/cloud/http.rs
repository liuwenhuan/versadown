use super::QuarkError;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
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
    /// Cookies issued by the response's host. These are transient and must
    /// never be written to the credential store.
    pub set_cookies: Vec<String>,
}
pub trait CloudHttpTransport: Send + Sync {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError>;
}
pub struct ReqwestTransport {
    pub(super) client: reqwest::blocking::Client,
    jar: Arc<reqwest::cookie::Jar>,
}
impl ReqwestTransport {
    pub fn new() -> Result<Self, QuarkError> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .cookie_provider(Arc::clone(&jar))
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
                .build()
                .map_err(|_| QuarkError::Transport("无法初始化 HTTPS 客户端".into()))?,
            jar,
        })
    }
}
impl CloudHttpTransport for ReqwestTransport {
    fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let has_content_type = r
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("Content-Type"));
        let mut b = self.client.request(
            r.method.parse().map_err(|_| QuarkError::InvalidResponse)?,
            &r.url,
        );
        let request_url = reqwest::Url::parse(&r.url).map_err(|_| QuarkError::InvalidResponse)?;
        for (k, v) in r.headers {
            if k.eq_ignore_ascii_case("Cookie") {
                // Providers give us an encrypted session as a Cookie header.
                // Seed the jar for this origin, then let reqwest retain any
                // server-issued context cookies for subsequent requests.
                for cookie in v
                    .split(';')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    self.jar.add_cookie_str(cookie, &request_url);
                }
            } else {
                b = b.header(k, v)
            }
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
