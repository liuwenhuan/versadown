//! Aliyun Drive's browser-granted refresh-token protocol.
//!
//! The credential store deliberately contains only the renewable session fields.
//! Signed download URLs are requested immediately before use and never written here.
use super::{
    http::response_json, types::required, CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile,
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider, CloudUploadTransport,
    CredentialStore, QuarkError, UploadHttpRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Read, path::Path};

// The current official web client renews the browser-granted `token` entry
// through this endpoint.  The legacy auth.aliyundrive.com endpoint rejects
// otherwise valid refresh tokens created by the contemporary web login flow.
const TOKEN_URL: &str = "https://api.aliyundrive.com/token/refresh";
const API: &str = "https://api.aliyundrive.com";
const WEB_REFERER: &str = "https://www.aliyundrive.com/";

/// The minimum durable state obtained after the user finishes the official web login.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AliyunSession {
    pub refresh_token: String,
    pub access_token: String,
    pub drive_id: String,
}

pub struct AliyunClient<'a, S: CredentialStore, T: CloudHttpTransport> {
    store: &'a S,
    http: &'a T,
}

impl<'a, S: CredentialStore, T: CloudHttpTransport> AliyunClient<'a, S, T> {
    pub fn new(store: &'a S, http: &'a T) -> Self {
        Self { store, http }
    }

    fn stored(&self) -> Result<AliyunSession, QuarkError> {
        let bytes = self
            .store
            .load("aliyun")?
            .ok_or(QuarkError::AuthorizationExpired)?;
        let session: AliyunSession =
            serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if session.refresh_token.is_empty() || session.drive_id.is_empty() {
            return Err(QuarkError::AuthorizationExpired);
        }
        Ok(session)
    }

    fn save(&self, session: &AliyunSession) -> Result<(), QuarkError> {
        if session.refresh_token.is_empty()
            || session.access_token.is_empty()
            || session.drive_id.is_empty()
        {
            return Err(QuarkError::AuthorizationExpired);
        }
        self.store.save(
            "aliyun",
            &serde_json::to_vec(session).map_err(|_| QuarkError::Storage)?,
        )
    }

    fn response(&self, response: &CloudHttpResponse) -> Result<Value, QuarkError> {
        // Token refresh failures use a 400 body with string error codes. Parse it
        // before the shared numeric-code mapper turns it into a generic provider
        // error, so callers can return to the official web authorization flow.
        if response.status == 400 {
            if let Ok(value) = serde_json::from_str::<Value>(&response.body) {
                if matches!(
                    value.get("code").and_then(Value::as_str),
                    Some("RefreshTokenExpired" | "InvalidParameter.RefreshToken")
                ) {
                    return Err(QuarkError::AuthorizationExpired);
                }
            }
        }
        let value = response_json(response)?;
        let code = value.get("code").and_then(Value::as_str).unwrap_or("");
        if matches!(
            code,
            "AccessTokenInvalid"
                | "AccessTokenExpired"
                | "InvalidParameter.AccessToken"
                | "InvalidToken"
        ) {
            return Err(QuarkError::AuthorizationExpired);
        }
        if !code.is_empty() {
            return Err(QuarkError::Provider("阿里云盘拒绝请求".into()));
        }
        Ok(value)
    }

    fn send(
        &self,
        method: &'static str,
        url: String,
        headers: BTreeMap<String, String>,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method,
            url,
            headers,
            body: body.map(|body| body.to_string()),
        })?;
        self.response(&response)
    }

    fn refresh(&self, previous: &AliyunSession) -> Result<AliyunSession, QuarkError> {
        let value = self.send(
            "POST",
            TOKEN_URL.into(),
            BTreeMap::from([
                ("Referer".into(), WEB_REFERER.into()),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
            ]),
            Some(json!({
                "grant_type": "refresh_token",
                "refresh_token": previous.refresh_token,
            })),
        )?;
        Ok(AliyunSession {
            refresh_token: required(&value, "refresh_token")?,
            access_token: required(&value, "access_token")?,
            drive_id: value
                .get("default_drive_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .unwrap_or(&previous.drive_id)
                .to_owned(),
        })
    }

    fn bearer_headers(session: &AliyunSession) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "Authorization".into(),
                format!("Bearer {}", session.access_token),
            ),
            ("Referer".into(), WEB_REFERER.into()),
        ])
    }

    fn api(&self, session: &AliyunSession, path: &str, body: Value) -> Result<Value, QuarkError> {
        self.send(
            "POST",
            format!("{API}{path}"),
            Self::bearer_headers(session),
            Some(body),
        )
    }

    fn directory_with(
        &self,
        session: &AliyunSession,
        parent: &str,
    ) -> Result<CloudDirectory, QuarkError> {
        let mut marker = String::new();
        let mut items = Vec::new();
        for _ in 0..10_000 {
            let value = self.api(
                session,
                "/adrive/v3/file/list",
                json!({
                    "drive_id": session.drive_id,
                    "parent_file_id": parent,
                    "limit": 200,
                    "marker": marker,
                }),
            )?;
            let page = value
                .get("items")
                .and_then(Value::as_array)
                .ok_or(QuarkError::InvalidResponse)?;
            for file in page {
                items.push(parse_file(file, parent)?);
            }
            marker = value
                .get("next_marker")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if marker.is_empty() {
                return Ok(CloudDirectory {
                    id: parent.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }

    /// Validate an official browser refresh token before it replaces the old session.
    /// There is intentionally no username/password login API in this adapter.
    pub fn authorize_browser_refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<CloudAccount, QuarkError> {
        if refresh_token.trim().is_empty() {
            return Err(QuarkError::InvalidInput("缺少网页登录刷新令牌".into()));
        }
        let session = self.refresh(&AliyunSession {
            refresh_token: refresh_token.into(),
            access_token: String::new(),
            drive_id: String::new(),
        })?;
        let (account, _) = self.validate(&session)?;
        self.save(&session)?;
        Ok(account)
    }

    /// A refresh token may be single-use. Validate the refreshed replacement
    /// completely and persist it exactly once before any caller-visible operation.
    fn refreshed_validated_session(
        &self,
    ) -> Result<(AliyunSession, CloudAccount, CloudDirectory), QuarkError> {
        let session = self.refresh(&self.stored()?)?;
        let (account, root) = self.validate(&session)?;
        self.save(&session)?;
        Ok((session, account, root))
    }

    fn validate(
        &self,
        session: &AliyunSession,
    ) -> Result<(CloudAccount, CloudDirectory), QuarkError> {
        let account = self.account_with(session)?;
        let root = self.directory_with(session, "root")?;
        Ok((account, root))
    }

    fn account_with(&self, session: &AliyunSession) -> Result<CloudAccount, QuarkError> {
        let value = self.api(session, "/adrive/v2/user/get", json!({}))?;
        let id = value
            .get("user_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        let display_name = value
            .get("nick_name")
            .or_else(|| value.get("name"))
            .or_else(|| value.get("user_name"))
            .or_else(|| value.get("phone"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        Ok(CloudAccount {
            provider: CloudProvider::Aliyun,
            id: id.into(),
            display_name: display_name.into(),
            auth_method: CloudAuthMethod::Web,
        })
    }

    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let (_, account, _) = self.refreshed_validated_session()?;
        Ok(account)
    }

    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let (session, _, root) = self.refreshed_validated_session()?;
        if parent == "root" {
            Ok(root)
        } else {
            self.directory_with(&session, parent)
        }
    }

    pub fn mkdir(&self, parent: &str, name: &str) -> Result<CloudFile, QuarkError> {
        let (session, _, _) = self.refreshed_validated_session()?;
        let value = self.api(
            &session,
            "/v2/file/create",
            json!({
                "drive_id": session.drive_id,
                "parent_file_id": parent,
                "name": name,
                "type": "folder",
                "check_name_mode": "refuse",
            }),
        )?;
        parse_file(&value, parent)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        let (session, _, _) = self.refreshed_validated_session()?;
        self.api(
            &session,
            "/v3/file/update",
            json!({"drive_id":session.drive_id,"file_id":id,"name":name,"check_name_mode":"refuse"}),
        )?;
        Ok(())
    }

    fn files_action(
        &self,
        path: &str,
        ids: &[String],
        parent: Option<&str>,
    ) -> Result<(), QuarkError> {
        if ids.is_empty() {
            return Err(QuarkError::InvalidInput("请选择至少一个文件".into()));
        }
        let (session, _, _) = self.refreshed_validated_session()?;
        for id in ids {
            let mut body = json!({"drive_id":session.drive_id,"file_id":id});
            if let Some(parent) = parent {
                body["to_parent_file_id"] = Value::String(parent.into());
            }
            self.api(&session, path, body)?;
        }
        Ok(())
    }

    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.files_action("/v2/file/move", ids, Some(parent))
    }

    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.files_action("/v2/file/copy", ids, Some(parent))
    }

    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.files_action("/v2/recyclebin/trash", ids, None)
    }

    /// Initialize a one-part upload; the caller transfers returned upload URLs and
    /// completes the upload separately, keeping those transient URLs out of storage.
    pub fn upload_init(&self, parent: &str, name: &str, size: u64) -> Result<Value, QuarkError> {
        let (session, _, _) = self.refreshed_validated_session()?;
        let value = self.api(
            &session,
            "/v2/file/create",
            json!({
                "drive_id": session.drive_id,
                "parent_file_id": parent,
                "name": name,
                "type": "file",
                "size": size,
                "check_name_mode": "refuse",
                "part_info_list": [{"part_number": 1}],
            }),
        )?;
        validate_upload_urls(&value)?;
        Ok(value)
    }

    pub fn upload(&self, parent: &str, name: &str, size: u64) -> Result<Value, QuarkError> {
        self.upload_init(parent, name, size)
    }

    /// Always asks the service for a fresh short-lived download URL.
    pub fn download(&self, id: &str) -> Result<(String, String), QuarkError> {
        let (session, _, _) = self.refreshed_validated_session()?;
        let metadata = self.api(
            &session,
            "/v2/file/get",
            json!({"drive_id":session.drive_id,"file_id":id}),
        )?;
        let name = required(&metadata, "name")?;
        let value = self.api(
            &session,
            "/v2/file/get_download_url",
            json!({"drive_id":session.drive_id,"file_id":id}),
        )?;
        value
            .get("url")
            .or_else(|| value.get("download_url"))
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("https://"))
            .map(str::to_owned)
            .map(|url| (url, name))
            .ok_or(QuarkError::InvalidResponse)
    }
}

impl<'a, S: CredentialStore, T: CloudHttpTransport + CloudUploadTransport> AliyunClient<'a, S, T> {
    /// Upload a regular local file through the short-lived HTTPS part URLs, then
    /// tell Aliyun Drive to publish the completed object.  The temporary URLs are
    /// intentionally scoped to this call and never enter the credential store.
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
        let parts = upload_part_count(size)?;
        let (session, _, _) = self.refreshed_validated_session()?;
        let init = self.api(
            &session,
            "/v2/file/create",
            json!({
                "drive_id": session.drive_id,
                "parent_file_id": parent,
                "name": name,
                "type": "file",
                "size": size,
                "check_name_mode": "refuse",
                "part_info_list": (1..=parts).map(|part_number| json!({"part_number": part_number})).collect::<Vec<_>>(),
            }),
        )?;
        validate_upload_urls(&init)?;
        let file_id = required(&init, "file_id")?;
        let upload_id = required(&init, "upload_id")?;
        let urls = upload_urls(&init, parts)?;

        let mut input = std::fs::File::open(path)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        let mut remaining = size;
        for url in urls {
            let length = remaining.min(ALIYUN_PART_SIZE) as usize;
            let mut body = vec![0; length];
            input
                .read_exact(&mut body)
                .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
            remaining = remaining.saturating_sub(length as u64);
            let response = self.http.send_upload(UploadHttpRequest {
                method: "PUT",
                url,
                headers: BTreeMap::from([(
                    "Content-Type".into(),
                    "application/octet-stream".into(),
                )]),
                body,
            })?;
            if !(200..300).contains(&response.status) {
                return Err(QuarkError::Provider(format!(
                    "阿里云盘上传分片失败（HTTP {}），请重试",
                    response.status
                )));
            }
        }
        if remaining != 0 {
            return Err(QuarkError::InvalidInput("无法读取选中的本地文件".into()));
        }
        self.api(
            &session,
            "/v2/file/complete",
            json!({"drive_id": session.drive_id, "file_id": file_id, "upload_id": upload_id}),
        )?;
        parse_file(&init, parent)
    }
}

const ALIYUN_PART_SIZE: u64 = 10 * 1024 * 1024;

fn upload_part_count(size: u64) -> Result<u64, QuarkError> {
    let count = size.saturating_add(ALIYUN_PART_SIZE - 1) / ALIYUN_PART_SIZE;
    let count = count.max(1);
    if count > 10_000 {
        return Err(QuarkError::InvalidInput(
            "文件过大，无法安全分片上传".into(),
        ));
    }
    Ok(count)
}

fn upload_urls(value: &Value, expected_parts: u64) -> Result<Vec<String>, QuarkError> {
    let parts = value
        .get("part_info_list")
        .and_then(Value::as_array)
        .filter(|parts| parts.len() == expected_parts as usize)
        .ok_or(QuarkError::InvalidResponse)?;
    let mut urls = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        if part.get("part_number").and_then(Value::as_u64) != Some(index as u64 + 1) {
            return Err(QuarkError::InvalidResponse);
        }
        let url = part
            .get("upload_url")
            .and_then(Value::as_str)
            .filter(|url| secure_temporary_url(url))
            .ok_or(QuarkError::InvalidResponse)?;
        urls.push(url.into());
    }
    Ok(urls)
}

fn secure_temporary_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn validate_upload_urls(value: &Value) -> Result<(), QuarkError> {
    match value {
        Value::Array(values) => values.iter().try_for_each(validate_upload_urls),
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(key.as_str(), "upload_url" | "internal_upload_url") {
                    match value.as_str() {
                        Some(url) if secure_temporary_url(url) => {}
                        _ => return Err(QuarkError::InvalidResponse),
                    }
                }
                validate_upload_urls(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn parse_file(value: &Value, parent: &str) -> Result<CloudFile, QuarkError> {
    Ok(CloudFile {
        id: required(value, "file_id")?,
        parent_id: value
            .get("parent_file_id")
            .and_then(Value::as_str)
            .unwrap_or(parent)
            .into(),
        name: required(value, "name")?,
        kind: required(value, "type")?,
        size: value
            .get("size")
            .and_then(|size| size.as_u64().or_else(|| size.as_str()?.parse().ok())),
        modified_at: value
            .get("updated_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
