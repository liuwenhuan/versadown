use super::{CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use regex::Regex;
use rsa::{pkcs8::DecodePublicKey, Pkcs1v15Encrypt, RsaPublicKey};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{collections::BTreeMap, fmt, time::SystemTime};
use url::Url;
use zeroize::Zeroizing;

const WEB_URL: &str = "https://cloud.189.cn";
const AUTH_URL: &str = "https://open.e.189.cn";
const API_URL: &str = "https://api.cloud.189.cn";
const APP_ID: &str = "8025431004";
const LOGIN_CLIENT_TYPE: &str = "10020";
const API_CLIENT_TYPE: &str = "TELEPC";
const API_VERSION: &str = "6.2";
const API_CHANNEL: &str = "web_cloud.189.cn";
const RETURN_URL: &str = "https://m.cloud.189.cn/zhuanti/2020/loginErrorPc/index.html";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TianyiAppSession {
    session_key: String,
    session_secret: String,
    #[serde(default)]
    family_session_key: String,
    #[serde(default)]
    family_session_secret: String,
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    login_name: String,
}

impl fmt::Debug for TianyiAppSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TianyiAppSession")
            .field("session_key", &"<redacted>")
            .field("session_secret", &"<redacted>")
            .field("family_session_key", &"<redacted>")
            .field("family_session_secret", &"<redacted>")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("login_name", &"<redacted>")
            .finish()
    }
}

pub(crate) struct TianyiAppLogin {
    pub(crate) session: TianyiAppSession,
    pub(crate) web_cookie: String,
}

#[derive(Deserialize)]
struct LoginReply {
    #[serde(default)]
    result: serde_json::Value,
    #[serde(default, rename = "toUrl")]
    to_url: String,
    #[serde(default)]
    msg: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionReply {
    #[serde(default, rename = "res_code")]
    res_code: serde_json::Value,
    #[serde(default)]
    session_key: String,
    #[serde(default)]
    session_secret: String,
    #[serde(default)]
    family_session_key: String,
    #[serde(default)]
    family_session_secret: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    login_name: String,
}

struct LoginParameters {
    captcha_token: String,
    lt: String,
    return_url: String,
    param_id: String,
    req_id: String,
    rsa_key: String,
}

impl TianyiAppSession {
    pub(crate) fn login_name(&self) -> &str {
        &self.login_name
    }

    pub(crate) fn request(
        &self,
        http: &impl CloudHttpTransport,
        method: &'static str,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<CloudHttpResponse, QuarkError> {
        if !path.starts_with('/') || path.contains('?') || !matches!(method, "GET" | "POST") {
            return Err(QuarkError::InvalidResponse);
        }
        let request_nonce = uuid::Uuid::new_v4().simple().to_string();
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.extend_pairs(query.iter().copied());
        serializer.extend_pairs([
            ("clientType", API_CLIENT_TYPE),
            ("version", API_VERSION),
            ("channelId", API_CHANNEL),
            ("rand", request_nonce.as_str()),
        ]);
        let query = serializer.finish();
        http.send(CloudHttpRequest {
            method,
            url: format!("{API_URL}{path}?{query}"),
            headers: self.signed_headers(method, path)?,
            body: None,
        })
    }

    pub(crate) fn signed_download(
        &self,
        download_url: &str,
    ) -> Result<(String, BTreeMap<String, String>), QuarkError> {
        let mut url = Url::parse(download_url).map_err(|_| QuarkError::InvalidResponse)?;
        if url.scheme() != "https" || url.host_str().is_none() {
            return Err(QuarkError::InvalidResponse);
        }
        let request_path = url.path().to_owned();
        let request_nonce = uuid::Uuid::new_v4().simple().to_string();
        url.query_pairs_mut()
            .append_pair("clientType", API_CLIENT_TYPE)
            .append_pair("version", API_VERSION)
            .append_pair("channelId", API_CHANNEL)
            .append_pair("rand", &request_nonce);
        Ok((url.into(), self.signed_headers("GET", &request_path)?))
    }

    fn signed_headers(
        &self,
        method: &str,
        path: &str,
    ) -> Result<BTreeMap<String, String>, QuarkError> {
        let date = httpdate::fmt_http_date(SystemTime::now());
        let signature =
            app_signature(&self.session_secret, &self.session_key, method, path, &date)?;
        Ok(BTreeMap::from([
            ("Date".into(), date),
            ("SessionKey".into(), self.session_key.clone()),
            ("Signature".into(), signature),
            (
                "X-Request-ID".into(),
                uuid::Uuid::new_v4().to_string().to_uppercase(),
            ),
        ]))
    }
}

pub(crate) fn login_password(
    username: &str,
    password: &str,
    http: &impl CloudHttpTransport,
) -> Result<TianyiAppLogin, QuarkError> {
    if username.trim().is_empty() || password.is_empty() {
        return Err(QuarkError::InvalidInput("请输入账号和密码".into()));
    }
    let password = Zeroizing::new(password.to_owned());
    let login_page_url = format!(
        "{WEB_URL}/unifyLoginForPC.action?appId={APP_ID}&clientType={LOGIN_CLIENT_TYPE}&returnURL={}&timeStamp={}",
        encoded(RETURN_URL),
        unix_millis(),
    );
    let login_page = http.send(CloudHttpRequest {
        method: "GET",
        url: login_page_url,
        headers: BTreeMap::from([(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )]),
        body: None,
    })?;
    if !(200..300).contains(&login_page.status) {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let parameters = parse_login_parameters(&login_page.body)?;
    let encrypted_username = rsa_encrypt(&parameters.rsa_key, username)?;
    let encrypted_password = rsa_encrypt(&parameters.rsa_key, &password)?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("appKey", APP_ID)
        .append_pair("accountType", "02")
        .append_pair("userName", &format!("{{RSA}}{encrypted_username}"))
        .append_pair("password", &format!("{{RSA}}{encrypted_password}"))
        .append_pair("validateCode", "")
        .append_pair("captchaToken", &parameters.captcha_token)
        .append_pair("returnUrl", &parameters.return_url)
        .append_pair("mailSuffix", "@189.cn")
        .append_pair("dynamicCheck", "FALSE")
        .append_pair("clientType", LOGIN_CLIENT_TYPE)
        .append_pair("cb_SaveName", "1")
        .append_pair("isOauth2", "false")
        .append_pair("state", "")
        .append_pair("paramId", &parameters.param_id)
        .finish();
    let login = http.send(CloudHttpRequest {
        method: "POST",
        url: format!("{AUTH_URL}/api/logbox/oauth2/loginSubmit.do"),
        headers: BTreeMap::from([
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            (
                "Referer".into(),
                format!("{AUTH_URL}/api/logbox/oauth2/unifyAccountLogin.do"),
            ),
            ("Cookie".into(), format!("LT={}", parameters.lt)),
            ("X-Requested-With".into(), "XMLHttpRequest".into()),
            ("REQID".into(), parameters.req_id),
            ("lt".into(), parameters.lt),
        ]),
        body: Some(body),
    })?;
    let reply: LoginReply =
        serde_json::from_str(&login.body).map_err(|_| QuarkError::WebAuthorizationRequired)?;
    if challenge_text(&reply.msg) {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    if protocol_code(&reply.result).as_deref() != Some("0") || reply.to_url.is_empty() {
        return Err(QuarkError::Provider(format!(
            "天翼桌面登录被拒绝（错误码 {}）",
            protocol_code(&reply.result).unwrap_or_else(|| "Unknown".into())
        )));
    }
    let redirect = Url::parse(&reply.to_url).map_err(|_| QuarkError::InvalidResponse)?;
    if redirect.scheme() != "https"
        || !redirect
            .host_str()
            .is_some_and(|host| host == "189.cn" || host.ends_with(".189.cn"))
    {
        return Err(QuarkError::InvalidResponse);
    }
    let session_url = format!(
        "{API_URL}/getSessionForPC.action?clientType=TELEMAC&version=1.0.0&channelId={API_CHANNEL}&redirectURL={}",
        encoded(&reply.to_url),
    );
    let response = http.send(CloudHttpRequest {
        method: "GET",
        url: session_url,
        headers: BTreeMap::from([("Accept".into(), "application/json;charset=UTF-8".into())]),
        body: None,
    })?;
    let reply: SessionReply =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    if protocol_code(&reply.res_code).as_deref() != Some("0") {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    if reply.session_key.is_empty()
        || reply.session_secret.is_empty()
        || reply.access_token.is_empty()
    {
        return Err(QuarkError::InvalidResponse);
    }
    let session = TianyiAppSession {
        session_key: reply.session_key,
        session_secret: reply.session_secret,
        family_session_key: reply.family_session_key,
        family_session_secret: reply.family_session_secret,
        access_token: reply.access_token,
        refresh_token: reply.refresh_token,
        login_name: if reply.login_name.is_empty() {
            username.trim().to_owned()
        } else {
            reply.login_name
        },
    };
    // MaurUppi's fork correctly treats web-Cookie conversion as optional: the
    // signed App session remains valid even when this compatibility step fails.
    let web_cookie = refresh_web_cookie(&session, http).unwrap_or_default();
    Ok(TianyiAppLogin {
        session,
        web_cookie,
    })
}

fn refresh_web_cookie(
    session: &TianyiAppSession,
    http: &impl CloudHttpTransport,
) -> Result<String, QuarkError> {
    let response = http.send(CloudHttpRequest {
        method: "GET",
        url: format!(
            "{WEB_URL}/ssoLogin.action?sessionKey={}&redirectUrl=main.action%23recycle",
            encoded(&session.session_key)
        ),
        headers: BTreeMap::from([("Accept-Language".into(), "zh-CN,zh;q=0.9,en;q=0.8".into())]),
        body: None,
    })?;
    Ok(response
        .set_cookies
        .iter()
        .filter_map(|cookie| cookie.split(';').next())
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(name, value)| {
            matches!(*name, "COOKIE_LOGIN_USER" | "SSON")
                && !value.is_empty()
                && !value.chars().any(char::is_control)
        })
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

fn parse_login_parameters(html: &str) -> Result<LoginParameters, QuarkError> {
    let required = |name: &str| capture_login_field(html, name);
    Ok(LoginParameters {
        captcha_token: required("captchaToken")?,
        lt: required("lt")?,
        return_url: html_unescape(&required("returnUrl")?),
        param_id: required("paramId")?,
        req_id: required("reqId")?,
        rsa_key: required("j_rsaKey")?,
    })
}

fn capture_login_field(html: &str, name: &str) -> Result<String, QuarkError> {
    let escaped = regex::escape(name);
    let patterns = [
        format!(
            r#"(?is)<input[^>]*(?:id|name)\s*=\s*["']{escaped}["'][^>]*value\s*=\s*["']([^"']+)["']"#
        ),
        format!(
            r#"(?is)<input[^>]*value\s*=\s*["']([^"']+)["'][^>]*(?:id|name)\s*=\s*["']{escaped}["']"#
        ),
        format!(r#"(?m)\b{escaped}\s*=\s*["']([^"']+)["']"#),
    ];
    patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .find_map(|regex| {
            regex
                .captures(html)
                .and_then(|capture| capture.get(1))
                .map(|value| value.as_str().trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .ok_or(QuarkError::WebAuthorizationRequired)
}

fn rsa_encrypt(public_key: &str, plaintext: &str) -> Result<String, QuarkError> {
    let der = STANDARD
        .decode(
            public_key
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>(),
        )
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    let key = RsaPublicKey::from_public_key_der(&der)
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    let encrypted = key
        .encrypt(
            &mut rand::thread_rng(),
            Pkcs1v15Encrypt,
            plaintext.as_bytes(),
        )
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    Ok(lower_hex(&encrypted))
}

fn app_signature(
    secret: &str,
    session_key: &str,
    method: &str,
    path: &str,
    date: &str,
) -> Result<String, QuarkError> {
    let mut mac =
        Hmac::<Sha1>::new_from_slice(secret.as_bytes()).map_err(|_| QuarkError::InvalidResponse)?;
    mac.update(
        format!("SessionKey={session_key}&Operate={method}&RequestURI={path}&Date={date}")
            .as_bytes(),
    );
    Ok(upper_hex(&mac.finalize().into_bytes()))
}

fn protocol_code(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    }
}

fn challenge_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "captcha",
        "sms",
        "verify",
        "risk",
        "验证码",
        "短信",
        "设备",
        "风控",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
}

fn encoded(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn upper_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}
