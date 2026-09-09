//! Native Xunlei cloud-drive protocol adapter.
//!
//! Interactive authorization intentionally stays in Xunlei's official web or
//! QR-code flow.  This module only consumes the resulting refresh token,
//! identity, device id, and short-lived security state; it never accepts or
//! stores an account password.

use super::{
    http::response_json,
    types::{encoded, required},
    *,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    fmt,
    io::Read,
    path::Path,
};

const AUTH: &str = "https://xluser-ssl.xunlei.com";
const DRIVE: &str = "https://api-pan.xunlei.com";
const WEB_VERSION_CODE: &str = "1.92.98";
const WEB_CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";
const WEB_PACKAGE: &str = "pan.xunlei.com";
// Verified 2026-09-10 against the official module1618 signer and module25's
// pub1iwjKbiXrwP9i86t19Ao0HtnZnkjCjZk chain, using independent JS vectors.
// Source: utils-async.413fd3b1f17c7388b322.js and
// utils-initial.38a2added9b0686c9067.js on static-pan.xunlei.com.
// This is the public `filters` algorithm, not the separate dynamic-salt branch.
const WEB_SIGNATURE_SALTS: [&str; 15] = [
    "rgui",
    "GBbHjL+W3+IJcgUgydd8zuV2hrhnID9yPI",
    "KRDkNzr29JqPNUq2mxw+YFtueKYZlzmWJIaHkj3NSh/ogclHVm",
    "JMGo+UYkPo6/I4s1enZg",
    "ZcfXzUZq+yx9VKk1fV4xAQhq+TwyE42MP7JXORDQy",
    "Z",
    "WCvfOrvOIFJWrKDen7WBq17dX",
    "3J07h/7",
    "Im3qdyHl9DZH",
    "Jg",
    "Y+KS",
    "/1lcxUF8",
    "C",
    "GVEo",
    "8k4v5dntCyPof87pZ0gc/EixX3qv5RxoaGzZ",
];
/// Non-secret identity values captured from the official browser/QR flow.
/// They are retained because Xunlei binds refresh and drive calls to that
/// profile; callers must never fabricate a replacement profile.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct XunleiClientProfile {
    pub client_id: String,
    /// Sensitive client credential captured by the official authorization flow.
    /// Task 7 persists this only in the encrypted session store.
    #[serde(default)]
    pub client_secret: String,
    pub device_id: String,
    pub client_version: String,
    pub package_name: String,
    pub user_agent: String,
}

/// Browser-only context selected from the official Xunlei web session. These
/// values remain inside the encrypted provider session and are never logged.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct XunleiWebContext {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub peer_id: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub credit_key: String,
}

impl XunleiWebContext {
    /// The official web bundle uses the `sessionid` cookie when it exists and
    /// otherwise uses the current browser bearer. This fallback is valid only
    /// for an otherwise complete, allowlisted browser context.
    pub(crate) fn use_access_token_for_missing_session_id(&mut self, access_token: &str) {
        if self.session_id.is_empty()
            && (!self.peer_id.is_empty() || !self.device_id.is_empty())
            && !self.credit_key.is_empty()
            && !access_token.is_empty()
        {
            self.session_id = access_token.into();
        }
    }

    fn is_complete(&self) -> bool {
        !self.session_id.is_empty()
            && (!self.peer_id.is_empty() || !self.device_id.is_empty())
            && !self.credit_key.is_empty()
    }
}

impl fmt::Debug for XunleiWebContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XunleiWebContext")
            .field("session_id", &"<redacted>")
            .field("peer_id", &"<redacted>")
            .field("device_id", &"<redacted>")
            .field("credit_key", &"<redacted>")
            .finish()
    }
}

impl XunleiClientProfile {
    pub fn is_modern_web(&self) -> bool {
        self.client_id == WEB_CLIENT_ID && self.package_name == WEB_PACKAGE
    }

    /// Returns the version-pinned official webpage signature, when supported.
    /// Unknown versions require the official bridge's fresh signing metadata.
    pub fn web_captcha_signature(&self, timestamp: &str) -> Option<String> {
        if !self.supports_web_signature()
            || timestamp.is_empty()
            || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let mut current = format!(
            "{}{}{}{}{}",
            self.client_id, self.client_version, WEB_PACKAGE, self.device_id, timestamp
        );
        for salt in WEB_SIGNATURE_SALTS {
            current.push_str(salt);
            current = format!("{:x}", md5::compute(current.as_bytes()));
        }
        Some(format!("1.{current}"))
    }

    fn supports_web_signature(&self) -> bool {
        self.is_modern_web()
            && self.client_version == WEB_VERSION_CODE
            && !self.device_id.is_empty()
    }
    /// Headers accepted by the drive APIs.  The official browser does not
    /// expose a client secret, but it does supply these identity fields with a
    /// current bearer token.
    fn is_drive_complete(&self) -> bool {
        !self.client_id.is_empty()
            && !self.device_id.is_empty()
            && !self.client_version.is_empty()
            && !self.package_name.is_empty()
            && !self.user_agent.is_empty()
    }

    /// The public web OAuth client omits client_secret; native identities
    /// retain their original confidential-client refresh requirement.
    fn can_refresh(&self) -> bool {
        self.is_drive_complete() && (self.is_modern_web() || !self.client_secret.is_empty())
    }
}

impl fmt::Debug for XunleiClientProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XunleiClientProfile")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("device_id", &self.device_id)
            .field("client_version", &self.client_version)
            .field("package_name", &self.package_name)
            .field("user_agent", &self.user_agent)
            .finish()
    }
}

/// Captured from the official captcha service for the same account/device.
/// Rust can renew signatures for the verified webpage version. Other versions
/// retain the bridge's signature with its original timestamp for bounded reuse.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct XunleiWebSecurity {
    pub captcha_token: String,
    /// Unix seconds; the bridge converts the official storage date to seconds.
    pub expires_at: u64,
    /// Empty for an official globally cached token, otherwise METHOD:/path.
    pub action: String,
    pub captcha_sign: String,
    pub timestamp: String,
    pub client_version: String,
    pub package_name: String,
    pub verification_url: String,
}

impl fmt::Debug for XunleiWebSecurity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XunleiWebSecurity")
            .field("captcha_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("action", &self.action)
            .field("captcha_sign", &"<redacted>")
            .field("timestamp", &self.timestamp)
            .field("verification_url", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct XunleiSession {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub profile: XunleiClientProfile,
    #[serde(default)]
    pub user_id: String,
    /// A security token issued during the official web/QR authorization flow.
    /// It is sent only to Xunlei and is never logged or placed in task sources.
    #[serde(default)]
    pub security_token: String,
    /// The narrow cookie allowlist required by the official web drive API.
    #[serde(default)]
    pub web_context: XunleiWebContext,
    #[serde(default)]
    pub web_security: XunleiWebSecurity,
    #[serde(default)]
    pub auth_method: CloudAuthMethod,
}

impl fmt::Debug for XunleiSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XunleiSession")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("profile", &self.profile)
            .field("user_id", &self.user_id)
            .field("security_token", &"<redacted>")
            .field("web_context", &self.web_context)
            .field("web_security", &self.web_security)
            .field("auth_method", &self.auth_method)
            .finish()
    }
}

impl XunleiSession {
    pub fn refresh(refresh_token: impl Into<String>, profile: XunleiClientProfile) -> Self {
        Self {
            access_token: String::new(),
            refresh_token: refresh_token.into(),
            expires_at: 0,
            profile,
            user_id: String::new(),
            security_token: String::new(),
            web_context: XunleiWebContext::default(),
            web_security: XunleiWebSecurity::default(),
            auth_method: CloudAuthMethod::Web,
        }
    }

    pub fn with_access(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        profile: XunleiClientProfile,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at: u64::MAX,
            profile,
            user_id: String::new(),
            security_token: String::new(),
            web_context: XunleiWebContext::default(),
            web_security: XunleiWebSecurity::default(),
            auth_method: CloudAuthMethod::Web,
        }
    }

    /// Creates a session from data returned by the official web/QR flow.
    pub fn from_web_authorization(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        user_id: impl Into<String>,
        security_token: impl Into<String>,
        profile: XunleiClientProfile,
        expires_at: u64,
    ) -> Result<Self, QuarkError> {
        let mut session = Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at,
            profile,
            user_id: user_id.into(),
            security_token: security_token.into(),
            web_context: XunleiWebContext::default(),
            web_security: XunleiWebSecurity::default(),
            auth_method: CloudAuthMethod::Web,
        };
        if session.access_token.is_empty() || session.user_id.is_empty() {
            return Err(QuarkError::AuthorizationPending);
        }
        // Preserve historical incomplete browser sessions for migration. The
        // explicit modern profile keeps its actual expiry and can refresh.
        if !session.profile.is_modern_web()
            && (session.refresh_token.is_empty() || !session.profile.is_drive_complete())
        {
            session.expires_at = u64::MAX;
        }
        Ok(session)
    }
}

/// Credentials required to start the provider's resumable object upload.
/// This is deliberately not serializable: it contains short-lived upload
/// credentials and must be consumed immediately by the upload executor.
#[derive(Clone, PartialEq, Eq)]
pub struct XunleiResumableUpload {
    pub upload_type: String,
    pub kind: String,
    pub provider: String,
    pub access_key_id: String,
    pub access_key_secret: String,
    pub bucket: String,
    pub endpoint: String,
    pub expiration: String,
    pub key: String,
    pub security_token: String,
}

impl fmt::Debug for XunleiResumableUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XunleiResumableUpload")
            .field("upload_type", &self.upload_type)
            .field("kind", &self.kind)
            .field("provider", &self.provider)
            .field("access_key_id", &"<redacted>")
            .field("access_key_secret", &"<redacted>")
            .field("bucket", &self.bucket)
            .field("endpoint", &self.endpoint)
            .field("expiration", &self.expiration)
            .field("key", &self.key)
            .field("security_token", &"<redacted>")
            .finish()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A blocking client whose refreshed session can be persisted by the command
/// layer with [`Self::session`].
pub struct XunleiClient<'a, T: CloudHttpTransport> {
    session: RefCell<XunleiSession>,
    http: &'a T,
}

impl<'a, T: CloudHttpTransport> XunleiClient<'a, T> {
    pub fn new(mut session: XunleiSession, http: &'a T) -> Self {
        let access_token = session.access_token.clone();
        if !session.profile.is_modern_web() {
            session
                .web_context
                .use_access_token_for_missing_session_id(&access_token);
        }
        Self {
            session: RefCell::new(session),
            http,
        }
    }

    pub fn session(&self) -> XunleiSession {
        self.session.borrow().clone()
    }

    fn is_browser_bearer_session(session: &XunleiSession) -> bool {
        !session.profile.is_modern_web()
            && session.auth_method == CloudAuthMethod::Web
            && !session.access_token.is_empty()
            && !session.user_id.is_empty()
            // A web session can expose its genuine browser device/profile
            // while deliberately withholding the native-only refresh secret.
            // It is usable until the provider rejects that current bearer;
            // never turn it into an impossible native refresh attempt.
            && (session.refresh_token.is_empty() || !session.profile.can_refresh())
    }

    /// Builds the account used to finish an official browser authorization.
    ///
    /// This only formats identity. The command layer must successfully read
    /// the root directory before considering the authorization complete.
    pub fn web_account(&self) -> Result<CloudAccount, QuarkError> {
        let session = self.session();
        if session.access_token.is_empty() || session.user_id.is_empty() {
            return Err(QuarkError::AuthorizationPending);
        }
        if session.profile.is_modern_web() && !session.profile.is_drive_complete() {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        if Self::is_browser_bearer_session(&session) && !session.web_context.is_complete() {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let id = (!session.user_id.is_empty())
            .then_some(session.user_id)
            .ok_or(QuarkError::AuthorizationPending)?;
        Ok(CloudAccount {
            provider: CloudProvider::Xunlei,
            display_name: id.clone(),
            id,
            auth_method: CloudAuthMethod::Web,
        })
    }

    fn profile_headers(profile: &XunleiClientProfile) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("X-Device-Id".into(), profile.device_id.clone()),
            ("X-Client-Id".into(), profile.client_id.clone()),
            ("X-Client-Version".into(), profile.client_version.clone()),
            ("User-Agent".into(), profile.user_agent.clone()),
            ("Accept".into(), "application/json;charset=UTF-8".into()),
        ])
    }

    fn browser_protocol_headers(&self, session: &XunleiSession) -> Vec<(String, String)> {
        let peer_id = if session.web_context.peer_id.is_empty() {
            session.web_context.device_id.clone()
        } else {
            session.web_context.peer_id.clone()
        };
        vec![
            ("Peer-Id".into(), peer_id.clone()),
            (
                "Guid".into(),
                format!("{:x}", md5::compute(peer_id.as_bytes())),
            ),
            ("User-Id".into(), session.user_id.clone()),
            ("Session-Id".into(), session.web_context.session_id.clone()),
            ("Version-Code".into(), WEB_VERSION_CODE.into()),
            ("Credit-Key".into(), session.web_context.credit_key.clone()),
        ]
    }

    fn drive_url(&self, path: &str) -> String {
        let session = self.session();
        let raw = format!("{DRIVE}{path}");
        if !Self::is_browser_bearer_session(&session) {
            return raw;
        }
        let mut url = url::Url::parse(&raw).expect("Xunlei drive URL is constant");
        let mut query = url.query_pairs_mut();
        query.append_pair("_sessid", &session.web_context.session_id);
        query.append_pair("_uid", &session.user_id);
        for (name, value) in self.browser_protocol_headers(&session) {
            query.append_pair("_h[]", &format!("{name}: {value}"));
        }
        drop(query);
        url.into()
    }

    fn challenge(value: &Value, _status: u16) -> bool {
        let error = value
            .get("error")
            .or_else(|| value.get("error_description"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        value
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| !url.is_empty())
            || value
                .get("captcha_url")
                .and_then(Value::as_str)
                .is_some_and(|url| !url.is_empty())
            || value
                .get("verification_url")
                .and_then(Value::as_str)
                .is_some_and(|url| !url.is_empty())
            || error.contains("captcha")
            || error.contains("verify")
            || error.contains("risk")
    }

    fn value(&self, response: CloudHttpResponse) -> Result<Value, QuarkError> {
        let parsed: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        let code = parsed
            .get("error_code")
            .or_else(|| parsed.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if self.session.borrow().profile.is_modern_web() {
            let error = parsed["error"].as_str().unwrap_or("");
            if response.status == 401
                || code == 16
                || matches!(
                    error,
                    "unauthenticated" | "invalid_grant" | "invalid_token" | "unauthorized_client"
                )
            {
                return Err(QuarkError::AuthorizationExpired);
            }
            if matches!(code, 4001 | 4002)
                || matches!(error, "captcha_required" | "captcha_invalid")
            {
                return Err(QuarkError::WebAuthorizationRequired);
            }
            if Self::challenge(&parsed, response.status) && error.is_empty() {
                if let Some(url) = parsed["url"].as_str() {
                    self.session.borrow_mut().web_security.verification_url = url.into();
                }
                return Err(QuarkError::WebAuthorizationRequired);
            }
            if !(200..300).contains(&response.status) || code != 0 || !error.is_empty() {
                return Err(QuarkError::Provider(format!(
                    "迅雷服务拒绝请求（HTTP {} / code {}）",
                    response.status, code
                )));
            }
            return if parsed.is_null() {
                if response.body.is_empty() {
                    Ok(json!({}))
                } else {
                    Err(QuarkError::InvalidResponse)
                }
            } else {
                Ok(parsed)
            };
        }
        if matches!(code, 4121 | 10) {
            return Err(QuarkError::AuthorizationExpired);
        }
        if Self::challenge(&parsed, response.status) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        response_json(&response).map_err(|error| match error {
            QuarkError::CaptchaRequired => QuarkError::WebAuthorizationRequired,
            error => error,
        })
    }

    fn refresh(&self, previous: XunleiSession) -> Result<XunleiSession, QuarkError> {
        if previous.refresh_token.is_empty() || !previous.profile.can_refresh() {
            return Err(QuarkError::AuthorizationExpired);
        }
        let mut headers = Self::profile_headers(&previous.profile);
        if previous.profile.is_modern_web() {
            headers.insert("X-Sdk-Version".into(), "3.4.20".into());
            headers.insert("X-Protocol-Version".into(), "301".into());
            headers.insert("X-Action".into(), "401".into());
        } else if !previous.security_token.is_empty() {
            headers.insert("X-Captcha-Token".into(), previous.security_token.clone());
        }
        let mut body = json!({
            "grant_type": "refresh_token",
            "refresh_token": previous.refresh_token,
            "client_id": previous.profile.client_id,
        });
        if !previous.profile.is_modern_web() {
            body["client_secret"] = previous.profile.client_secret.clone().into();
        }
        let response = self.http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{AUTH}/v1/auth/token"),
            headers,
            body: Some(body.to_string()),
        })?;
        let value = self.value(response)?;
        let mut refreshed = previous;
        refreshed.access_token = required(&value, "access_token")?;
        if let Ok(token) = required(&value, "refresh_token") {
            refreshed.refresh_token = token;
        }
        if let Some(id) = value
            .get("sub")
            .or_else(|| value.get("user_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            if refreshed.profile.is_modern_web()
                && !refreshed.user_id.is_empty()
                && refreshed.user_id != id
            {
                return Err(QuarkError::AuthorizationExpired);
            }
            refreshed.user_id = id.into();
        }
        if let Some(token) = value
            .get("captcha_token")
            .or_else(|| value.get("security_token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
        {
            refreshed.security_token = token.into();
        }
        refreshed.expires_at = now().saturating_add(value["expires_in"].as_u64().unwrap_or(0));
        *self.session.borrow_mut() = refreshed.clone();
        Ok(refreshed)
    }

    fn ready_session(&self) -> Result<XunleiSession, QuarkError> {
        let session = self.session();
        if (!session.access_token.is_empty() && session.expires_at > now().saturating_add(60))
            || Self::is_browser_bearer_session(&session)
        {
            return Ok(session);
        }
        if !session.profile.can_refresh() {
            return Err(QuarkError::AuthorizationExpired);
        }
        self.refresh(session)
    }

    fn raw(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
        session: &XunleiSession,
    ) -> Result<Value, QuarkError> {
        if session.access_token.is_empty()
            || (!session.profile.is_drive_complete() && !Self::is_browser_bearer_session(session))
        {
            return Err(QuarkError::AuthorizationExpired);
        }
        if Self::is_browser_bearer_session(session) && !session.web_context.is_complete() {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let mut headers = session
            .profile
            .is_drive_complete()
            .then(|| Self::profile_headers(&session.profile))
            .unwrap_or_default();
        headers.insert(
            "Authorization".into(),
            format!("Bearer {}", session.access_token),
        );
        if session.profile.is_modern_web() {
            if !session.web_security.captcha_token.is_empty()
                && session.web_security.expires_at > now()
            {
                headers.insert(
                    "X-Captcha-Token".into(),
                    session.web_security.captcha_token.clone(),
                );
            }
            headers.insert("Origin".into(), "https://pan.xunlei.com".into());
            headers.insert("Referer".into(), "https://pan.xunlei.com/".into());
        } else if !session.security_token.is_empty() {
            headers.insert("X-Captcha-Token".into(), session.security_token.clone());
        }
        if Self::is_browser_bearer_session(session) {
            headers.extend(self.browser_protocol_headers(session));
            headers.insert("Origin".into(), "https://pan.xunlei.com".into());
            headers.insert("Referer".into(), "https://pan.xunlei.com/".into());
            headers
                .entry("Accept".into())
                .or_insert_with(|| "application/json;charset=UTF-8".into());
            if !session.profile.user_agent.is_empty() {
                headers.insert("User-Agent".into(), session.profile.user_agent.clone());
            }
        }
        self.value(self.http.send(CloudHttpRequest {
            method,
            url,
            headers,
            body: body.map(|value| value.to_string()),
        })?)
    }

    fn request(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let mut session = self.ready_session()?;
        if session.profile.is_modern_web() {
            let parsed = url::Url::parse(&url).map_err(|_| QuarkError::InvalidResponse)?;
            let action = format!("{method}:{}", parsed.path());
            let security = &session.web_security;
            let mut renewed = false;
            if (session.profile.supports_web_signature()
                || !security.captcha_token.is_empty()
                || !security.captcha_sign.is_empty())
                && (security.expires_at <= now().saturating_add(30)
                    || (!security.action.is_empty() && security.action != action))
            {
                session = self.refresh_web_captcha(&action)?;
                renewed = true;
            }
            let mut refreshed = false;
            loop {
                match self.raw(method, url.clone(), body.clone(), &session) {
                    Err(QuarkError::AuthorizationExpired)
                        if !refreshed && session.profile.can_refresh() =>
                    {
                        session = self.refresh(session)?;
                        refreshed = true;
                    }
                    Err(QuarkError::WebAuthorizationRequired) if !renewed => {
                        session = self.refresh_web_captcha(&action)?;
                        renewed = true;
                    }
                    result => return result,
                }
            }
        }
        match self.raw(method, url.clone(), body.clone(), &session) {
            Err(QuarkError::AuthorizationExpired) if session.profile.can_refresh() => {
                let refreshed = self.refresh(session)?;
                self.raw(method, url, body, &refreshed)
            }
            result => result,
        }
    }

    fn refresh_web_captcha(&self, action: &str) -> Result<XunleiSession, QuarkError> {
        let mut session = self.session();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        if let Some(signature) = session.profile.web_captcha_signature(&timestamp) {
            session.web_security.captcha_sign = signature;
            session.web_security.timestamp = timestamp;
            session.web_security.client_version = session.profile.client_version.clone();
            session.web_security.package_name = session.profile.package_name.clone();
        }
        let security = &session.web_security;
        if security.captcha_sign.is_empty()
            || security.timestamp.is_empty()
            || security.client_version.is_empty()
            || security.package_name != session.profile.package_name
        {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        // A local conservative reuse bound for unknown versions, not a claim
        // about the provider's TTL. Supported profiles just generated a fresh
        // pair; other profiles require a recent pair from the official bridge.
        let signed_at = security
            .timestamp
            .parse::<u64>()
            .map(|millis| millis / 1000)
            .map_err(|_| QuarkError::WebAuthorizationRequired)?;
        if signed_at > now().saturating_add(30) || now().saturating_sub(signed_at) > 300 {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let response = self.http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{AUTH}/v1/shield/captcha/init"),
            headers: BTreeMap::from([
                ("Content-Type".into(), "application/json".into()),
                ("User-Agent".into(), session.profile.user_agent.clone()),
                ("Origin".into(), "https://pan.xunlei.com".into()),
            ]),
            body: Some(
                json!({
                    "client_id": session.profile.client_id,
                    "device_id": session.profile.device_id,
                    "action": action,
                    "captcha_token": security.captcha_token,
                    "meta": {
                        "package_name": security.package_name,
                        "client_version": security.client_version,
                        "captcha_sign": security.captcha_sign,
                        "timestamp": security.timestamp,
                        "user_id": session.user_id,
                        "username": "", "phone_number": "", "email": ""
                    }
                })
                .to_string(),
            ),
        })?;
        let parsed: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        if let Some(url) = parsed["url"].as_str().filter(|url| !url.is_empty()) {
            session.web_security.verification_url = url.into();
            *self.session.borrow_mut() = session;
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let value = self.value(response)?;
        session.web_security.captcha_token = required(&value, "captcha_token")?;
        let expires_in = value["expires_in"]
            .as_u64()
            .filter(|value| *value > 30)
            .ok_or(QuarkError::InvalidResponse)?;
        session.web_security.expires_at = now().saturating_add(expires_in);
        session.web_security.action = action.into();
        session.web_security.verification_url.clear();
        *self.session.borrow_mut() = session.clone();
        Ok(session)
    }

    fn request_parent<'b>(&self, parent: &'b str) -> &'b str {
        if parent == "0"
            && (self.session.borrow().profile.is_modern_web()
                || Self::is_browser_bearer_session(&self.session()))
        {
            ""
        } else {
            parent
        }
    }

    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let value = self.request("GET", format!("{AUTH}/v1/user/me"), None)?;
        let session = self.session();
        let id = value
            .get("sub")
            .or_else(|| value.get("id"))
            .or_else(|| value.get("user_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .or_else(|| (!session.user_id.is_empty()).then_some(session.user_id))
            .ok_or(QuarkError::InvalidResponse)?;
        let display_name = value
            .get("nickname")
            .or_else(|| value.get("name"))
            .or_else(|| value.get("username"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| id.clone());
        Ok(CloudAccount {
            provider: CloudProvider::Xunlei,
            id,
            display_name,
            auth_method: CloudAuthMethod::Web,
        })
    }

    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        let mut page_token = String::new();
        let mut pages = HashSet::new();
        let request_parent = self.request_parent(parent);
        let extras = if self.session.borrow().profile.is_modern_web() {
            ""
        } else {
            "__type=drive&refresh=true&__sync=true&"
        };
        loop {
            let value = self.request(
                "GET",
                self.drive_url(&format!(
                    "/drive/v1/files?space=&{extras}parent_id={}&page_token={}&with_audit=true&limit=100&filters={}",
                    encoded(request_parent),
                    encoded(&page_token),
                    encoded(r#"{"phase":{"eq":"PHASE_TYPE_COMPLETE"},"trashed":{"eq":false}}"#),
                )),
                None,
            )?;
            for file in value["files"]
                .as_array()
                .ok_or(QuarkError::InvalidResponse)?
            {
                if file["phase"].as_str() != Some("PHASE_TYPE_COMPLETE") {
                    continue;
                }
                items.push(Self::file(file, parent)?);
            }
            page_token = value["next_page_token"].as_str().unwrap_or("").into();
            if page_token.is_empty() {
                return Ok(CloudDirectory {
                    id: parent.into(),
                    items,
                });
            }
            if !pages.insert(page_token.clone()) || pages.len() > 10_000 {
                return Err(QuarkError::InvalidResponse);
            }
        }
    }

    fn file(value: &Value, parent: &str) -> Result<CloudFile, QuarkError> {
        let kind = required(value, "kind")?;
        Ok(CloudFile {
            id: required(value, "id")?,
            parent_id: value["parent_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or(parent)
                .into(),
            name: required(value, "name")?,
            kind: if matches!(kind.as_str(), "drive#folder" | "FOLDER") {
                "folder"
            } else {
                "file"
            }
            .into(),
            size: value["size"]
                .as_u64()
                .or_else(|| value["size"].as_str().and_then(|size| size.parse().ok())),
            modified_at: value["modified_time"]
                .as_str()
                .or_else(|| value["modified_at"].as_str())
                .map(str::to_owned),
        })
    }

    pub fn mkdir(&self, parent: &str, name: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            self.drive_url("/drive/v1/files"),
            Some(
                json!({"kind":"drive#folder","parent_id":self.request_parent(parent),"name":name}),
            ),
        )?;
        Ok(())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        self.request(
            "PATCH",
            self.drive_url(&format!("/drive/v1/files/{}", encoded(id))),
            Some(json!({"name":name})),
        )?;
        Ok(())
    }

    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            self.drive_url("/drive/v1/files:batchMove"),
            Some(json!({"ids":ids,"to":{"parent_id":self.request_parent(parent)}})),
        )?;
        Ok(())
    }

    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            self.drive_url("/drive/v1/files:batchCopy"),
            Some(json!({"ids":ids,"to":{"parent_id":self.request_parent(parent)}})),
        )?;
        Ok(())
    }

    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        for id in ids {
            self.request(
                "PATCH",
                self.drive_url(&format!("/drive/v1/files/{}/trash", encoded(id))),
                Some(json!({})),
            )?;
        }
        Ok(())
    }

    /// Initiates an upload and returns the provider-issued resumable contract.
    /// Its short-lived credentials must be consumed immediately and never
    /// stored with the stable cloud source.
    pub fn upload_init(
        &self,
        parent: &str,
        name: &str,
        size: u64,
        gcid: &str,
    ) -> Result<XunleiResumableUpload, QuarkError> {
        if gcid.is_empty() {
            return Err(QuarkError::InvalidInput("缺少上传内容 GCID".into()));
        }
        let value = self.request(
            "POST",
            self.drive_url("/drive/v1/files"),
            Some(upload_request_body(
                self.request_parent(parent),
                name,
                size,
                gcid,
                self.session().profile.is_modern_web(),
            )),
        )?;
        let upload = resumable_upload(&value)?;
        if upload.upload_type != "UPLOAD_TYPE_RESUMABLE" || upload.kind != "drive#resumable" {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(upload)
    }

    /// Retrieves a short-lived direct link afresh for every download request.
    pub fn download(&self, id: &str) -> Result<(String, String), QuarkError> {
        let modern_web = self.session().profile.is_modern_web();
        let started = std::time::Instant::now();
        loop {
            if modern_web {
                download_poll_delay(started.elapsed())?;
            }
            let value = self.request(
                "GET",
                self.drive_url(&format!("/drive/v1/files/{}", encoded(id))),
                None,
            )?;
            let file = value.get("data").unwrap_or(&value);
            let name = required(file, "name")?;
            let link = file.get("web_content_link").or_else(|| file.get("download_url"));
            match link {
                Some(Value::String(link)) if !link.is_empty() => {
                    if !link.starts_with("https://") {
                        return Err(QuarkError::InvalidResponse);
                    }
                    return Ok((link.clone(), name));
                }
                None | Some(Value::Null | Value::String(_)) => {}
                _ => return Err(QuarkError::InvalidResponse),
            }
            // Modern web metadata can report COMPLETE before (or between)
            // requests with a generated link. Only this known empty-link
            // state is transient; protocol, auth and transport errors escape.
            if !modern_web
                || !matches!(file["phase"].as_str(), Some(
                    "PHASE_TYPE_COMPLETE" | "PHASE_TYPE_PENDING" | "PHASE_TYPE_RUNNING"
                ))
            {
                return Err(QuarkError::InvalidResponse);
            }
            std::thread::sleep(download_poll_delay(started.elapsed())?);
        }
    }
}

fn download_poll_delay(elapsed: std::time::Duration) -> Result<std::time::Duration, QuarkError> {
    let remaining = std::time::Duration::from_secs(30).saturating_sub(elapsed);
    if remaining.is_zero() {
        return Err(QuarkError::Provider(
            "文件可见但下载链接仍在生成，请稍后重试。".into(),
        ));
    }
    Ok(remaining.min(std::time::Duration::from_millis(250)))
}

#[cfg(test)]
mod download_poll_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn xunlei_download_poll_delay_rejects_deadline_and_caps_last_sleep() {
        assert_eq!(download_poll_delay(Duration::ZERO).unwrap(), Duration::from_millis(250));
        assert_eq!(download_poll_delay(Duration::from_millis(29_999)).unwrap(), Duration::from_millis(1));
        for elapsed in [Duration::from_secs(30), Duration::from_secs(31)] {
            assert!(matches!(download_poll_delay(elapsed), Err(QuarkError::Provider(message))
                if message == "文件可见但下载链接仍在生成，请稍后重试。"));
        }
    }
}

impl<'a, T: CloudHttpTransport + CloudUploadTransport> XunleiClient<'a, T> {
    /// Computes the provider's GCID, obtains one-shot STS credentials, and
    /// completes the negotiated OSS or AWS multipart upload. The STS credentials
    /// and upload URLs live only on this stack and are never serialized.
    pub fn upload_file(&self, parent: &str, path: &Path) -> Result<CloudFile, QuarkError> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(QuarkError::InvalidInput("请选择普通文件上传".into()));
        }
        let name = path
            .file_name()
            .ok_or_else(|| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?
            .to_string_lossy()
            .into_owned();
        let size = metadata.len();
        let gcid = xunlei_gcid(path, size)?;
        let value = self.request(
            "POST",
            self.drive_url("/drive/v1/files"),
            Some(upload_request_body(
                self.request_parent(parent),
                &name,
                size,
                &gcid,
                self.session().profile.is_modern_web(),
            )),
        )?;
        let file_value = value.get("file").ok_or(QuarkError::InvalidResponse)?;
        let file = Self::file(file_value, parent)?;
        // The official web upload flow returns a completed file before
        // examining upload_type or any object-storage credentials.
        if file_value["phase"] == "PHASE_TYPE_COMPLETE" {
            return Ok(file);
        }
        let upload = resumable_upload(&value)?;
        if upload.upload_type != "UPLOAD_TYPE_RESUMABLE" || upload.kind != "drive#resumable" {
            return Err(QuarkError::InvalidResponse);
        }
        match upload.provider.as_str() {
            "aws" => AwsMultipart::from_upload(&upload)?.upload(self.http, path, size)?,
            "PROVIDER_ALIYUN" => upload_oss(&upload, self.http, path, size)?,
            _ => return Err(QuarkError::InvalidResponse),
        }
        if self.session().profile.is_modern_web() {
            return self.wait_for_uploaded_file(&file.id, parent);
        }
        Ok(file)
    }

    fn wait_for_uploaded_file(&self, id: &str, parent: &str) -> Result<CloudFile, QuarkError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(QuarkError::Provider(
                    "数据已上传但云盘仍在处理，请稍后刷新目录。".into(),
                ));
            }
            let value = self.request(
                "GET",
                self.drive_url(&format!("/drive/v1/files/{}", encoded(id))),
                None,
            )?;
            match value["phase"].as_str() {
                Some("PHASE_TYPE_COMPLETE") => return Self::file(&value, parent),
                Some("PHASE_TYPE_ERROR") => {
                    return Err(QuarkError::Provider(
                        "上传数据的云盘处理失败，请在迅雷网页中查看详情。".into(),
                    ));
                }
                Some("PHASE_TYPE_PENDING" | "PHASE_TYPE_RUNNING") => {}
                _ => return Err(QuarkError::InvalidResponse),
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            std::thread::sleep(remaining.min(std::time::Duration::from_millis(250)));
        }
    }
}

fn upload_request_body(parent: &str, name: &str, size: u64, gcid: &str, modern_web: bool) -> Value {
    let mut body = json!({
        "kind":"drive#file", "parent_id":parent, "name":name,
        "size":size, "hash":gcid, "upload_type":"UPLOAD_TYPE_RESUMABLE",
    });
    if modern_web {
        body["resumable"] = json!({"supported_providers":["PROVIDER_ALIYUN"]});
    }
    body
}

fn resumable_upload(value: &Value) -> Result<XunleiResumableUpload, QuarkError> {
    let resumable = &value["resumable"];
    let params = &resumable["params"];
    Ok(XunleiResumableUpload {
        upload_type: required(value, "upload_type")?,
        kind: required(resumable, "kind")?,
        provider: required(resumable, "provider")?,
        access_key_id: required(params, "access_key_id")?,
        access_key_secret: required(params, "access_key_secret")?,
        bucket: required(params, "bucket")?,
        endpoint: required(params, "endpoint")?,
        expiration: required(params, "expiration")?,
        key: required(params, "key")?,
        security_token: required(params, "security_token")?,
    })
}

const AWS_PART_SIZE: u64 = 8 * 1024 * 1024;

struct AwsMultipart {
    endpoint: url::Url,
    bucket: String,
    key: String,
    access_key_id: String,
    access_key_secret: String,
    security_token: String,
}

impl AwsMultipart {
    fn from_upload(upload: &XunleiResumableUpload) -> Result<Self, QuarkError> {
        Ok(Self {
            endpoint: object_storage_endpoint(upload)?,
            bucket: upload.bucket.clone(),
            key: upload.key.clone(),
            access_key_id: upload.access_key_id.clone(),
            access_key_secret: upload.access_key_secret.clone(),
            security_token: upload.security_token.clone(),
        })
    }

    fn object_path(&self) -> String {
        format!(
            "/{}",
            self.key
                .split('/')
                .map(aws_percent)
                .collect::<Vec<_>>()
                .join("/")
        )
    }

    fn object_host(&self) -> Result<String, QuarkError> {
        let host = self
            .endpoint
            .host_str()
            .ok_or(QuarkError::InvalidResponse)?;
        if host.contains(':') {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(match self.endpoint.port() {
            Some(port) => format!("{}.{}:{port}", self.bucket, host),
            None => format!("{}.{}", self.bucket, host),
        })
    }

    fn request(
        &self,
        method: &'static str,
        raw_query: &str,
        canonical_query: &str,
        body: Vec<u8>,
    ) -> Result<UploadHttpRequest, QuarkError> {
        let timestamp = aws_timestamp();
        let date = &timestamp[..8];
        let payload_hash = hex(&Sha256::digest(&body));
        let host = self.object_host()?;
        let canonical_headers = format!(
            "host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{timestamp}\nx-amz-security-token:{}\n",
            self.security_token
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date;x-amz-security-token";
        let canonical = format!(
            "{method}\n{}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
            self.object_path()
        );
        let scope = format!("{date}/xunlei/s3/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let date_key = hmac_sha256(
            format!("AWS4{}", self.access_key_secret).as_bytes(),
            date.as_bytes(),
        )?;
        let region_key = hmac_sha256(&date_key, b"xunlei")?;
        let service_key = hmac_sha256(&region_key, b"s3")?;
        let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
        let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        let mut url = format!("https://{host}{}", self.object_path());
        if !raw_query.is_empty() {
            url.push('?');
            url.push_str(raw_query);
        }
        Ok(UploadHttpRequest {
            method,
            url,
            headers: BTreeMap::from([
                ("Host".into(), host),
                ("Content-Type".into(), "application/octet-stream".into()),
                ("x-amz-content-sha256".into(), payload_hash),
                ("x-amz-date".into(), timestamp),
                ("x-amz-security-token".into(), self.security_token.clone()),
                (
                    "Authorization".into(),
                    format!(
                        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                        self.access_key_id
                    ),
                ),
            ]),
            body,
        })
    }

    fn upload(
        &self,
        http: &impl CloudUploadTransport,
        path: &Path,
        size: u64,
    ) -> Result<(), QuarkError> {
        upload_multipart(http, path, size, |method, raw_query, canonical_query, body| {
            self.request(method, raw_query, canonical_query, body)
        })
    }
}

fn object_storage_endpoint(upload: &XunleiResumableUpload) -> Result<url::Url, QuarkError> {
    // Xunlei's official Aliyun contract can return a bare DNS endpoint,
    // including a custom Xunlei host. The web SDK's secure option uses HTTPS.
    // Preserve the existing strict absolute-HTTPS contract for other providers.
    let bare_dns = !upload.endpoint.is_empty()
        && upload.endpoint.len() <= 253
        && upload.endpoint.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    let endpoint_value = if upload.provider == "PROVIDER_ALIYUN" && bare_dns {
        format!("https://{}", upload.endpoint)
    } else {
        upload.endpoint.clone()
    };
    let mut endpoint = url::Url::parse(&endpoint_value).map_err(|_| QuarkError::InvalidResponse)?;
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
        || upload.bucket.is_empty()
        || upload.key.is_empty()
        || !upload
            .bucket
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(QuarkError::InvalidResponse);
    }
    let host = endpoint
        .host_str()
        .ok_or(QuarkError::InvalidResponse)?
        .to_owned();
    if let Some(host) = host.strip_prefix(&format!("{}.", upload.bucket)) {
        endpoint
            .set_host(Some(host))
            .map_err(|_| QuarkError::InvalidResponse)?;
    }
    Ok(endpoint)
}

fn upload_oss(
    upload: &XunleiResumableUpload,
    http: &impl CloudUploadTransport,
    path: &Path,
    size: u64,
) -> Result<(), QuarkError> {
    let mut endpoint = object_storage_endpoint(upload)?;
    let host = endpoint.host_str().ok_or(QuarkError::InvalidResponse)?;
    endpoint
        .set_host(Some(&format!("{}.{host}", upload.bucket)))
        .map_err(|_| QuarkError::InvalidResponse)?;
    let oss = super::upload::Oss {
        endpoint: endpoint.into(),
        bucket: upload.bucket.clone(),
        key: upload.key.clone(),
        id: upload.access_key_id.clone(),
        secret: upload.access_key_secret.clone(),
        token: upload.security_token.clone(),
    };
    upload_multipart(http, path, size, |method, raw_query, _, body| {
        let query = if raw_query.is_empty() {
            String::new()
        } else {
            format!("?{raw_query}")
        };
        oss.request(method, &query, body)
    })
}

fn upload_multipart(
    http: &impl CloudUploadTransport,
    path: &Path,
    size: u64,
    request: impl Fn(&'static str, &str, &str, Vec<u8>) -> Result<UploadHttpRequest, QuarkError>,
) -> Result<(), QuarkError> {
    if size == 0 {
        let response = http.send_upload(request("PUT", "", "", Vec::new())?)?;
        return aws_checked(response).map(|_| ());
    }
    let started = aws_checked(http.send_upload(request(
        "POST",
        "uploads",
        "uploads=",
        Vec::new(),
    )?)?)?;
    let upload_id = xml_tag(&started.body, "UploadId")?;
    let mut file = std::fs::File::open(path)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
    let mut remaining = size;
    let mut parts = Vec::new();
    let mut number = 1_u64;
    while remaining > 0 {
        let length = remaining.min(AWS_PART_SIZE) as usize;
        let mut body = vec![0; length];
        file.read_exact(&mut body)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        remaining = remaining.saturating_sub(length as u64);
        let encoded_upload = aws_percent(&upload_id);
        let query = format!("partNumber={number}&uploadId={encoded_upload}");
        let response = aws_checked(http.send_upload(request("PUT", &query, &query, body)?)?)?;
        let etag = response
            .headers
            .iter()
            .find(|(key, value)| key.eq_ignore_ascii_case("etag") && !value.is_empty())
            .map(|(_, value)| value.clone())
            .ok_or(QuarkError::InvalidResponse)?;
        parts.push((number, etag));
        number += 1;
    }
    let mut complete = String::from("<CompleteMultipartUpload>");
    for (number, etag) in parts {
        complete.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>",
            xml_escape(&etag)
        ));
    }
    complete.push_str("</CompleteMultipartUpload>");
    let encoded_upload = aws_percent(&upload_id);
    let query = format!("uploadId={encoded_upload}");
    aws_checked(http.send_upload(request(
        "POST",
        &query,
        &query,
        complete.into_bytes(),
    )?)?)?;
    Ok(())
}

fn aws_checked(response: UploadHttpResponse) -> Result<UploadHttpResponse, QuarkError> {
    if !(200..300).contains(&response.status) || response.body.contains("<Error>") {
        Err(QuarkError::Provider(format!(
            "迅雷云盘上传服务拒绝请求（HTTP {}），请重试",
            response.status
        )))
    } else {
        Ok(response)
    }
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<Vec<u8>, QuarkError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| QuarkError::InvalidResponse)?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn aws_percent(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn xml_tag(body: &str, tag: &str) -> Result<String, QuarkError> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open).ok_or(QuarkError::InvalidResponse)? + open.len();
    let end = body[start..]
        .find(&close)
        .ok_or(QuarkError::InvalidResponse)?
        + start;
    let value = &body[start..end];
    if value.is_empty() {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(value.into())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xunlei_gcid(path: &Path, size: u64) -> Result<String, QuarkError> {
    let mut block = 0x40000_u64;
    while size > block * 0x200 && block < 0x200000 {
        block <<= 1;
    }
    let mut file = std::fs::File::open(path)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
    let mut total = Sha1::new();
    let mut remaining = size;
    while remaining > 0 {
        let length = remaining.min(block) as usize;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        total.update(Sha1::digest(&bytes));
        remaining = remaining.saturating_sub(length as u64);
    }
    Ok(hex(&total.finalize()).to_uppercase())
}

fn aws_timestamp() -> String {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (elapsed / 86_400) as i64;
    let seconds = elapsed % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}
