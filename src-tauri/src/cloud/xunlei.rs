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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    fmt,
};

const AUTH: &str = "https://xluser-ssl.xunlei.com";
const DRIVE: &str = "https://api-pan.xunlei.com";
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

impl XunleiClientProfile {
    fn is_complete(&self) -> bool {
        !self.client_id.is_empty()
            && !self.device_id.is_empty()
            && !self.client_version.is_empty()
            && !self.package_name.is_empty()
            && !self.user_agent.is_empty()
            && !self.client_secret.is_empty()
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
        let session = Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at,
            profile,
            user_id: user_id.into(),
            security_token: security_token.into(),
            auth_method: CloudAuthMethod::Web,
        };
        if session.refresh_token.is_empty() || !session.profile.is_complete() {
            return Err(QuarkError::AuthorizationPending);
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
    pub fn new(session: XunleiSession, http: &'a T) -> Self {
        Self {
            session: RefCell::new(session),
            http,
        }
    }

    pub fn session(&self) -> XunleiSession {
        self.session.borrow().clone()
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

    fn challenge(value: &Value, status: u16) -> bool {
        let error = value
            .get("error")
            .or_else(|| value.get("error_description"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        status == 403
            || value
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
        if previous.refresh_token.is_empty() || !previous.profile.is_complete() {
            return Err(QuarkError::AuthorizationExpired);
        }
        let mut headers = Self::profile_headers(&previous.profile);
        if !previous.security_token.is_empty() {
            headers.insert("X-Captcha-Token".into(), previous.security_token.clone());
        }
        let response = self.http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{AUTH}/v1/auth/token"),
            headers,
            body: Some(
                json!({
                    "grant_type": "refresh_token",
                    "refresh_token": previous.refresh_token,
                    "client_id": previous.profile.client_id,
                    "client_secret": previous.profile.client_secret,
                })
                .to_string(),
            ),
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
        if !session.access_token.is_empty() && session.expires_at > now().saturating_add(60) {
            return Ok(session);
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
        if session.access_token.is_empty() || !session.profile.is_complete() {
            return Err(QuarkError::AuthorizationExpired);
        }
        let mut headers = Self::profile_headers(&session.profile);
        headers.insert(
            "Authorization".into(),
            format!("Bearer {}", session.access_token),
        );
        if !session.security_token.is_empty() {
            headers.insert("X-Captcha-Token".into(), session.security_token.clone());
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
        let session = self.ready_session()?;
        match self.raw(method, url.clone(), body.clone(), &session) {
            Err(QuarkError::AuthorizationExpired) => {
                let refreshed = self.refresh(session)?;
                self.raw(method, url, body, &refreshed)
            }
            result => result,
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
        loop {
            let value = self.request(
                "GET",
                format!(
                    "{DRIVE}/drive/v1/files?space=&__type=drive&refresh=true&__sync=true&parent_id={}&page_token={}&with_audit=true&limit=100&filters={}",
                    encoded(parent),
                    encoded(&page_token),
                    encoded(r#"{"phase":{"eq":"PHASE_TYPE_COMPLETE"},"trashed":{"eq":false}}"#),
                ),
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
            parent_id: value["parent_id"].as_str().unwrap_or(parent).into(),
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
            format!("{DRIVE}/drive/v1/files"),
            Some(json!({"kind":"drive#folder","parent_id":parent,"name":name})),
        )?;
        Ok(())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        self.request(
            "PATCH",
            format!("{DRIVE}/drive/v1/files/{}", encoded(id)),
            Some(json!({"name":name})),
        )?;
        Ok(())
    }

    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchMove"),
            Some(json!({"ids":ids,"to":{"parent_id":parent}})),
        )?;
        Ok(())
    }

    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchCopy"),
            Some(json!({"ids":ids,"to":{"parent_id":parent}})),
        )?;
        Ok(())
    }

    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        for id in ids {
            self.request(
                "PATCH",
                format!("{DRIVE}/drive/v1/files/{}/trash", encoded(id)),
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
            format!("{DRIVE}/drive/v1/files"),
            Some(json!({
                "kind":"drive#file",
                "parent_id":parent,
                "name":name,
                "size":size,
                "hash":gcid,
                "upload_type":"UPLOAD_TYPE_RESUMABLE",
            })),
        )?;
        let resumable = &value["resumable"];
        let params = &resumable["params"];
        let upload = XunleiResumableUpload {
            upload_type: required(&value, "upload_type")?,
            kind: required(resumable, "kind")?,
            provider: required(resumable, "provider")?,
            access_key_id: required(params, "access_key_id")?,
            access_key_secret: required(params, "access_key_secret")?,
            bucket: required(params, "bucket")?,
            endpoint: required(params, "endpoint")?,
            expiration: required(params, "expiration")?,
            key: required(params, "key")?,
            security_token: required(params, "security_token")?,
        };
        if upload.upload_type != "UPLOAD_TYPE_RESUMABLE" || upload.kind != "drive#resumable" {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(upload)
    }

    /// Retrieves a short-lived direct link afresh for every download request.
    pub fn download(&self, id: &str) -> Result<(String, String), QuarkError> {
        let value = self.request(
            "GET",
            format!("{DRIVE}/drive/v1/files/{}", encoded(id)),
            None,
        )?;
        let file = value.get("data").unwrap_or(&value);
        let url = file
            .get("web_content_link")
            .or_else(|| file.get("download_url"))
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("https://"))
            .map(str::to_owned)
            .ok_or(QuarkError::InvalidResponse)?;
        Ok((url, required(file, "name")?))
    }
}
