use super::{
    types::{encoded, required},
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudHttpRequest, CloudHttpResponse,
    CloudHttpTransport, CloudProvider, CloudUploadTransport, QuarkError, UploadHttpRequest,
    UploadHttpResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};
use zeroize::Zeroizing;

const CLOUD: &str = "https://cloud.189.cn";
const LOGIN: &str = "https://open.e.189.cn";
const TIANYI_WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TianyiSession {
    pub cookie: String,
    #[serde(default)]
    pub auth_method: CloudAuthMethod,
}
impl TianyiSession {
    pub fn cookie(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            auth_method: CloudAuthMethod::Web,
        }
    }
}

pub fn parse_tianyi_web_session(cookies: &[String]) -> Result<TianyiSession, QuarkError> {
    let pairs = cookies
        .iter()
        .filter_map(|raw| {
            let pair = raw.split(';').next()?.trim();
            let (name, value) = pair.split_once('=')?;
            (!value.is_empty() && matches!(name, "COOKIE_LOGIN_USER" | "SSON" | "SESSION" | "LT"))
                .then(|| format!("{name}={value}"))
        })
        .collect::<Vec<_>>();
    if !pairs
        .iter()
        .any(|pair| pair.starts_with("COOKIE_LOGIN_USER=") || pair.starts_with("SSON="))
    {
        return Err(QuarkError::AuthorizationExpired);
    }
    Ok(TianyiSession::cookie(pairs.join("; ")))
}

#[derive(Clone, Debug)]
pub struct TianyiDownload {
    pub url: String,
    pub name: String,
    pub headers: BTreeMap<String, String>,
}
pub struct TianyiClient<'a, T: CloudHttpTransport> {
    session: TianyiSession,
    http: &'a T,
}

impl<'a, T: CloudHttpTransport> TianyiClient<'a, T> {
    pub fn new(session: TianyiSession, http: &'a T) -> Self {
        Self { session, http }
    }
    fn headers(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("Cookie".into(), self.session.cookie.clone()),
            ("Origin".into(), CLOUD.into()),
            ("Referer".into(), format!("{CLOUD}/")),
            ("User-Agent".into(), TIANYI_WEB_USER_AGENT.into()),
        ])
    }
    fn request(
        &self,
        method: &'static str,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method,
            url: format!("{CLOUD}{path}?{query}"),
            headers: self.headers(),
            body: body.map(|value| value.to_string()),
        })?;
        tianyi_json(&response)
    }
    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let value = self.request(
            "GET",
            "/api/open/user/getUserInfoForPortal.action",
            "",
            None,
        )?;
        let user = value
            .get("userInfo")
            .or_else(|| value.get("data"))
            .unwrap_or(&value);
        Ok(CloudAccount {
            provider: CloudProvider::Tianyi,
            id: user
                .get("userId")
                .or_else(|| user.get("id"))
                .or_else(|| user.get("loginName"))
                .and_then(string_or_number)
                .ok_or(QuarkError::InvalidResponse)?,
            display_name: user
                .get("nickName")
                .or_else(|| user.get("userName"))
                .or_else(|| user.get("loginName"))
                .and_then(Value::as_str)
                .unwrap_or("天翼云盘")
                .into(),
            auth_method: self.session.auth_method,
        })
    }
    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let value = self.request(
            "GET",
            "/api/open/file/listFiles.action",
            &format!(
                "folderId={}&pageNum=1&pageSize=100&orderBy=1&order=ASC",
                encoded(parent)
            ),
            None,
        )?;
        let files = value
            .pointer("/fileListAO/fileList")
            .or_else(|| value.get("data"))
            .and_then(Value::as_array)
            .ok_or(QuarkError::InvalidResponse)?;
        let items = files
            .iter()
            .map(|file| {
                Ok(CloudFile {
                    id: file
                        .get("id")
                        .or_else(|| file.get("fileId"))
                        .and_then(string_or_number)
                        .ok_or(QuarkError::InvalidResponse)?,
                    parent_id: parent.into(),
                    name: required(file, "name").or_else(|_| required(file, "fileName"))?,
                    kind: if file
                        .get("isFolder")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        "folder"
                    } else {
                        "file"
                    }
                    .into(),
                    size: file
                        .get("size")
                        .or_else(|| file.get("fileSize"))
                        .and_then(Value::as_u64),
                    modified_at: file.get("lastOpTime").and_then(string_or_number),
                })
            })
            .collect::<Result<Vec<_>, QuarkError>>()?;
        Ok(CloudDirectory {
            id: parent.into(),
            items,
        })
    }
    pub fn mkdir(&self, parent: &str, name: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            "/api/open/file/createFolder.action",
            "",
            Some(json!({"folderId":parent,"folderName":name})),
        )
        .map(|_| ())
    }
    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        self.request(
            "POST",
            "/api/open/file/rename.action",
            "",
            Some(json!({"fileId":id,"fileName":name})),
        )
        .map(|_| ())
    }
    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.file_operation("moveFiles", ids, parent)
    }
    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.file_operation("copyFiles", ids, parent)
    }
    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.request(
            "POST",
            "/api/open/file/deleteFiles.action",
            "",
            Some(json!({"fileIds":ids,"toRecycle":true})),
        )
        .map(|_| ())
    }
    fn file_operation(
        &self,
        operation: &str,
        ids: &[String],
        parent: &str,
    ) -> Result<(), QuarkError> {
        if ids.is_empty() || parent.is_empty() {
            return Err(QuarkError::InvalidInput("请选择文件和目标目录".into()));
        }
        self.request(
            "POST",
            &format!("/api/open/file/{operation}.action"),
            "",
            Some(json!({"fileIds":ids,"destFolderId":parent})),
        )
        .map(|_| ())
    }
    /// Obtains the upload host; bytes are submitted later by the command layer.
    pub fn upload(&self) -> Result<Value, QuarkError> {
        self.request("POST", "/v2/getUserUploadUrl.action", "", None)
    }

    /// Uploads a regular file with the authenticated web session. The page
    /// session key and upload URL are consumed in this stack frame only.
    pub fn upload_file<U: CloudUploadTransport>(
        &self,
        parent: &str,
        local_path: &Path,
        upload_http: &U,
    ) -> Result<CloudFile, QuarkError> {
        if parent.is_empty() {
            return Err(QuarkError::InvalidInput("请选择目标目录".into()));
        }
        let metadata = std::fs::symlink_metadata(local_path)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(QuarkError::InvalidInput("请选择普通文件上传".into()));
        }
        let name = local_path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty() && !value.contains(['\r', '\n']))
            .ok_or_else(|| QuarkError::InvalidInput("文件名无效".into()))?
            .to_owned();
        let bytes = std::fs::read(local_path)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        if bytes.len() as u64 != metadata.len() {
            return Err(QuarkError::InvalidInput("本地文件在上传前发生变化".into()));
        }
        let session_key = self.page_session_key()?;
        let endpoint = required(&self.upload()?, "uploadUrl")?;
        let endpoint = super::https_transfer_url(&endpoint)?;
        let (content_type, body) = super::multipart_file(
            &[
                ("sessionKey", session_key),
                ("parentId", parent.into()),
                ("albumId", "undefined".into()),
                ("opertype", "1".into()),
                ("fname", name.clone()),
            ],
            "Filedata",
            &name,
            bytes,
        )?;
        let mut headers = self.headers();
        headers.insert("Content-Type".into(), content_type);
        let response = upload_http.send_upload(UploadHttpRequest {
            method: "POST",
            url: endpoint,
            headers,
            body,
        })?;
        tianyi_upload_response(&response, parent, &name, metadata.len())
    }
    pub fn download(&self, id: &str) -> Result<TianyiDownload, QuarkError> {
        let value = self.request(
            "GET",
            "/api/open/file/getFileDownloadUrl.action",
            &format!("fileId={}", encoded(id)),
            None,
        )?;
        let url = required(&value, "fileDownloadUrl")?;
        if !url.starts_with("https://") {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(TianyiDownload {
            url,
            name: value
                .get("fileName")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .into(),
            headers: self.headers(),
        })
    }
    pub fn login_password(
        username: &str,
        password: &str,
        http: &'a T,
    ) -> Result<TianyiSession, QuarkError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(QuarkError::InvalidInput("请输入账号和密码".into()));
        }
        let password = Zeroizing::new(password.to_owned());
        let response = http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{LOGIN}/api/logbox/oauth2/loginSubmit.do"),
            headers: BTreeMap::from([
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("Referer".into(), format!("{LOGIN}/")),
                ("User-Agent".into(), TIANYI_WEB_USER_AGENT.into()),
            ]),
            body: Some(format!(
                "appKey=cloud&accountType=01&userName={}&password={}",
                encoded(username),
                encoded(&password)
            )),
        })?;
        let value: Value = serde_json::from_str(&response.body)
            .map_err(|_| QuarkError::WebAuthorizationRequired)?;
        if challenge(&value) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        if value.get("result").and_then(Value::as_i64).unwrap_or(-1) != 0 {
            return Err(QuarkError::Provider("登录被服务拒绝".into()));
        }
        let mut session = parse_tianyi_web_session(&response.set_cookies)?;
        session.auth_method = CloudAuthMethod::Password;
        let client = Self::new(session.clone(), http);
        client.account()?;
        client.directory("-11")?;
        Ok(session)
    }

    fn page_session_key(&self) -> Result<String, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method: "GET",
            url: format!("{CLOUD}/main.action"),
            headers: self.headers(),
            body: None,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(QuarkError::Provider("天翼云盘无法建立上传会话".into()));
        }
        let capture = regex::Regex::new(r#"sessionKey\s*=\s*['\"]([^'\"]+)['\"]"#)
            .map_err(|_| QuarkError::InvalidResponse)?
            .captures(&response.body)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        Ok(capture)
    }
}

fn tianyi_upload_response(
    response: &UploadHttpResponse,
    parent: &str,
    name: &str,
    size: u64,
) -> Result<CloudFile, QuarkError> {
    if !(200..300).contains(&response.status) {
        return Err(QuarkError::Provider("天翼云盘拒绝上传文件".into()));
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    let code = value
        .get("res_code")
        .or_else(|| value.get("result"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if code != 0 {
        return Err(QuarkError::Provider("天翼云盘拒绝上传文件".into()));
    }
    Ok(CloudFile {
        id: value
            .get("id")
            .or_else(|| value.get("fileId"))
            .and_then(string_or_number)
            .ok_or(QuarkError::InvalidResponse)?,
        parent_id: parent.into(),
        name: value
            .get("fileName")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(name)
            .into(),
        kind: "file".into(),
        size: value
            .get("size")
            .or_else(|| value.get("fileSize"))
            .and_then(Value::as_u64)
            .or(Some(size)),
        modified_at: value.get("createDate").and_then(string_or_number),
    })
}
fn tianyi_json(response: &CloudHttpResponse) -> Result<Value, QuarkError> {
    if response.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    if challenge(&value) {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let code = value
        .get("res_code")
        .or_else(|| value.get("result"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if code != 0 {
        return Err(if matches!(code, -1 | 401) {
            QuarkError::AuthorizationExpired
        } else {
            QuarkError::Provider("天翼云盘拒绝请求".into())
        });
    }
    if !(200..300).contains(&response.status) {
        return Err(QuarkError::Provider("天翼云盘请求失败".into()));
    }
    Ok(value)
}
fn challenge(value: &Value) -> bool {
    ["captcha", "短信", "sms", "verify", "risk", "安全验证"]
        .iter()
        .any(|needle| {
            value
                .to_string()
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase())
        })
}
fn string_or_number(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}
