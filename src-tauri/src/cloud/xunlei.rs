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

    /// Refreshing is deliberately stricter than using a browser-issued access
    /// token: native refresh requires the client secret, which the official
    /// web application does not place in storage.
    fn can_refresh(&self) -> bool {
        self.is_drive_complete() && !self.client_secret.is_empty()
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
        let mut session = Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at,
            profile,
            user_id: user_id.into(),
            security_token: security_token.into(),
            auth_method: CloudAuthMethod::Web,
        };
        if session.access_token.is_empty() || session.refresh_token.is_empty() {
            return Err(QuarkError::AuthorizationPending);
        }
        // The current official web client grants a bearer session via its
        // in-memory user state, but never reveals the native client profile
        // required by the token refresh endpoint.  Keep that bearer usable
        // for the current browser session instead of pretending to refresh it
        // with fabricated native fields.
        if !session.profile.is_drive_complete() {
            if session.user_id.is_empty() {
                return Err(QuarkError::AuthorizationPending);
            }
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
    pub fn new(session: XunleiSession, http: &'a T) -> Self {
        Self {
            session: RefCell::new(session),
            http,
        }
    }

    pub fn session(&self) -> XunleiSession {
        self.session.borrow().clone()
    }

    fn is_browser_bearer_session(session: &XunleiSession) -> bool {
        session.auth_method == CloudAuthMethod::Web
            && !session.access_token.is_empty()
            && !session.refresh_token.is_empty()
            && !session.user_id.is_empty()
            && !session.profile.is_drive_complete()
    }

    /// Builds the account used to finish an official browser authorization.
    ///
    /// This deliberately does not probe Xunlei or refresh the token: browser
    /// storage does not expose the native client secret required for refresh.
    /// The saved session can therefore create its workspace immediately, while
    /// ordinary requests fail closed with reauthorization once its bearer token
    /// has expired.
    pub fn web_account(&self) -> Result<CloudAccount, QuarkError> {
        let session = self.session();
        if session.access_token.is_empty() || session.user_id.is_empty() {
            return Err(QuarkError::AuthorizationPending);
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
        if previous.refresh_token.is_empty() || !previous.profile.can_refresh() {
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
        let mut headers = session
            .profile
            .is_drive_complete()
            .then(|| Self::profile_headers(&session.profile))
            .unwrap_or_default();
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
            Err(QuarkError::AuthorizationExpired) if session.profile.can_refresh() => {
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
        let upload = resumable_upload(&value)?;
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

impl<'a, T: CloudHttpTransport + CloudUploadTransport> XunleiClient<'a, T> {
    /// Computes the provider's GCID, obtains one-shot STS credentials, and
    /// completes an AWS Signature V4 multipart upload.  The STS credentials
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
        let upload = resumable_upload(&value)?;
        if upload.upload_type != "UPLOAD_TYPE_RESUMABLE"
            || upload.kind != "drive#resumable"
            || upload.provider != "aws"
        {
            return Err(QuarkError::InvalidResponse);
        }
        let file = Self::file(
            value.get("file").ok_or(QuarkError::InvalidResponse)?,
            parent,
        )?;
        AwsMultipart::from_upload(&upload)?.upload(self.http, path, size)?;
        Ok(file)
    }
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
        let mut endpoint =
            url::Url::parse(&upload.endpoint).map_err(|_| QuarkError::InvalidResponse)?;
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
        Ok(Self {
            endpoint,
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
        if size == 0 {
            let response = http.send_upload(self.request("PUT", "", "", Vec::new())?)?;
            return aws_checked(response).map(|_| ());
        }
        let started = aws_checked(http.send_upload(self.request(
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
            let response =
                aws_checked(http.send_upload(self.request("PUT", &query, &query, body)?)?)?;
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
        aws_checked(http.send_upload(self.request(
            "POST",
            &query,
            &query,
            complete.into_bytes(),
        )?)?)?;
        Ok(())
    }
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
    while size / block > 0x200 && block < 0x200000 {
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
