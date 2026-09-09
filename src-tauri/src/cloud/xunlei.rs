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
};

const AUTH: &str = "https://xluser-ssl.xunlei.com";
const DRIVE: &str = "https://api-pan.xunlei.com";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct XunleiSession {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: u64,
    pub device_id: String,
    #[serde(default)]
    pub user_id: String,
    /// A security token issued during the official web/QR authorization flow.
    /// It is sent only to Xunlei and is never logged or placed in task sources.
    #[serde(default)]
    pub security_token: String,
    #[serde(default)]
    pub auth_method: CloudAuthMethod,
}

impl XunleiSession {
    pub fn refresh(refresh_token: impl Into<String>, device_id: impl Into<String>) -> Self {
        Self {
            access_token: String::new(),
            refresh_token: refresh_token.into(),
            expires_at: 0,
            device_id: device_id.into(),
            user_id: String::new(),
            security_token: String::new(),
            auth_method: CloudAuthMethod::Web,
        }
    }

    pub fn with_access(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at: u64::MAX,
            device_id: device_id.into(),
            user_id: String::new(),
            security_token: String::new(),
            auth_method: CloudAuthMethod::Web,
        }
    }

    /// Creates a session from data returned by the official web/QR flow.
    pub fn from_web_authorization(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        device_id: impl Into<String>,
        user_id: impl Into<String>,
        security_token: impl Into<String>,
        expires_at: u64,
    ) -> Result<Self, QuarkError> {
        let session = Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at,
            device_id: device_id.into(),
            user_id: user_id.into(),
            security_token: security_token.into(),
            auth_method: CloudAuthMethod::Web,
        };
        if session.refresh_token.is_empty() || session.device_id.is_empty() {
            return Err(QuarkError::AuthorizationPending);
        }
        Ok(session)
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
        if Self::challenge(&parsed, response.status) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        response_json(&response).map_err(|error| match error {
            QuarkError::CaptchaRequired => QuarkError::WebAuthorizationRequired,
            error => error,
        })
    }

    fn refresh(&self, previous: XunleiSession) -> Result<XunleiSession, QuarkError> {
        if previous.refresh_token.is_empty() || previous.device_id.is_empty() {
            return Err(QuarkError::AuthorizationExpired);
        }
        let mut headers = BTreeMap::from([
            ("X-Device-Id".into(), previous.device_id.clone()),
            ("User-Agent".into(), USER_AGENT.into()),
        ]);
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
        if session.access_token.is_empty() || session.device_id.is_empty() {
            return Err(QuarkError::AuthorizationExpired);
        }
        let mut headers = BTreeMap::from([
            (
                "Authorization".into(),
                format!("Bearer {}", session.access_token),
            ),
            ("X-Device-Id".into(), session.device_id.clone()),
            ("User-Agent".into(), USER_AGENT.into()),
        ]);
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
                    "{DRIVE}/drive/v1/files?parent_id={}&page_token={}&limit=100&filters={}",
                    encoded(parent),
                    encoded(&page_token),
                    encoded(r#"{"trashed":{"eq":false}}"#),
                ),
                None,
            )?;
            for file in value["files"]
                .as_array()
                .ok_or(QuarkError::InvalidResponse)?
            {
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
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchTrash"),
            Some(json!({"ids":ids})),
        )?;
        Ok(())
    }

    /// Initiates an upload; callers consume any provider-provided upload URLs
    /// immediately and never store them with the stable cloud source.
    pub fn upload_init(&self, parent: &str, name: &str, size: u64) -> Result<Value, QuarkError> {
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files"),
            Some(json!({"kind":"drive#file","parent_id":parent,"name":name,"size":size})),
        )
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
