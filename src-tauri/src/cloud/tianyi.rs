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

struct TianyiEntryInfo {
    id: String,
    name: String,
    is_folder: bool,
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
            ("Accept".into(), "application/json;charset=UTF-8".into()),
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

    /// The official Tianyi browser window is the authentication boundary for
    /// web authorization.  Its session cookie is sufficient to establish the
    /// workspace; probing a separate private account endpoint here can fail
    /// independently and must not strand a completed login in that window.
    pub fn web_account(&self) -> Result<CloudAccount, QuarkError> {
        if self.session.cookie.trim().is_empty() {
            return Err(QuarkError::AuthorizationPending);
        }
        Ok(CloudAccount {
            provider: CloudProvider::Tianyi,
            id: "tianyi-web".into(),
            display_name: "天翼云盘".into(),
            auth_method: self.session.auth_method,
        })
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
        let files = tianyi_file_list(&value)?;
        let items = files
            .iter()
            .map(|(file, listed_as_folder)| {
                Ok(CloudFile {
                    id: file
                        .get("id")
                        .or_else(|| file.get("fileId"))
                        .and_then(string_or_number)
                        .ok_or(QuarkError::InvalidResponse)?,
                    parent_id: parent.into(),
                    name: required(file, "name").or_else(|_| required(file, "fileName"))?,
                    kind: if *listed_as_folder
                        || bool_value(file.get("isFolder").or_else(|| file.get("folder")))
                    {
                        "folder"
                    } else {
                        "file"
                    }
                    .into(),
                    size: u64_value(file.get("size").or_else(|| file.get("fileSize"))),
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
        self.form_request(
            "POST",
            "/api/open/file/createFolder.action",
            &[("parentFolderId", parent), ("folderName", name)],
        )
        .map(|_| ())
    }
    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        let entry = self.entry_info(id)?;
        let (path, id_key, name_key) = if entry.is_folder {
            (
                "/api/open/file/renameFolder.action",
                "folderId",
                "destFolderName",
            )
        } else {
            (
                "/api/open/file/renameFile.action",
                "fileId",
                "destFileName",
            )
        };
        self.form_request("POST", path, &[(id_key, id), (name_key, name)])
            .map(|_| ())
    }
    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.batch_operation("MOVE", ids, parent)
    }
    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.batch_operation("COPY", ids, parent)
    }
    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.batch_operation("DELETE", ids, "")
    }
    fn batch_operation(
        &self,
        operation: &str,
        ids: &[String],
        parent: &str,
    ) -> Result<(), QuarkError> {
        if ids.is_empty() || (operation != "DELETE" && parent.is_empty()) {
            return Err(QuarkError::InvalidInput("请选择文件和目标目录".into()));
        }
        let task_infos = ids
            .iter()
            .map(|id| self.entry_info(id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| {
                json!({
                    "fileId": entry.id,
                    "fileName": entry.name,
                    "isFolder": i64::from(entry.is_folder),
                })
            })
            .collect::<Vec<_>>();
        let task_infos = serde_json::to_string(&task_infos)
            .map_err(|_| QuarkError::InvalidResponse)?;
        self.form_request(
            "POST",
            "/api/open/batch/createBatchTask.action",
            &[
                ("type", operation),
                ("targetFolderId", parent),
                ("taskInfos", &task_infos),
            ],
        )
        .map(|_| ())
    }

    fn form_request(
        &self,
        method: &'static str,
        path: &str,
        fields: &[(&str, &str)],
    ) -> Result<Value, QuarkError> {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in fields {
            serializer.append_pair(name, value);
        }
        let mut headers = self.headers();
        headers.insert(
            "Content-Type".into(),
            "application/x-www-form-urlencoded; charset=UTF-8".into(),
        );
        let response = self.http.send(CloudHttpRequest {
            method,
            url: format!("{CLOUD}{path}"),
            headers,
            body: Some(serializer.finish()),
        })?;
        tianyi_json(&response)
    }

    fn entry_info(&self, id: &str) -> Result<TianyiEntryInfo, QuarkError> {
        let value = self.request(
            "GET",
            "/api/portal/getFileInfo.action",
            &format!("fileId={}", encoded(id)),
            None,
        )?;
        let file = value
            .get("fileInfo")
            .or_else(|| value.get("data"))
            .unwrap_or(&value);
        Ok(TianyiEntryInfo {
            id: file
                .get("fileId")
                .or_else(|| file.get("folderId"))
                .or_else(|| file.get("id"))
                .and_then(string_or_number)
                .unwrap_or_else(|| id.into()),
            name: required(file, "fileName").or_else(|_| required(file, "name"))?,
            is_folder: bool_value(file.get("isFolder").or_else(|| file.get("folder")))
                || file.get("fileCata").and_then(i64_value) == Some(1),
        })
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
    let value = tianyi_response_value(&response.body)?;
    let code = value
        .get("res_code")
        .or_else(|| value.get("result"))
        .and_then(i64_value)
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
    let value = tianyi_response_value(&response.body)?;
    if challenge(&value) {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let code = value
        .get("res_code")
        .or_else(|| value.get("result"))
        .and_then(i64_value)
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
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn i64_value(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
}

fn u64_value(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
    })
}

fn bool_value(value: Option<&Value>) -> bool {
    value
        .and_then(|value| {
            value.as_bool().or_else(|| {
                value.as_str().and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => Some(true),
                    "false" | "0" | "no" => Some(false),
                    _ => None,
                })
            })
        })
        .unwrap_or(false)
}

fn tianyi_file_list(value: &Value) -> Result<Vec<(&Value, bool)>, QuarkError> {
    let list = value
        .pointer("/fileListAO/fileList")
        .or_else(|| value.pointer("/listFiles/fileList"))
        .or_else(|| value.get("data"));
    let container = value
        .get("fileListAO")
        .or_else(|| value.get("listFiles"))
        .or_else(|| value.get("data"));
    let mut items = Vec::new();
    if let Some(container) = container {
        let has_file_listing = container.get("fileList").is_some();
        let has_folder_listing = container.get("folderList").is_some();
        append_tianyi_entries(&mut items, container.get("fileList"), "file", false);
        append_tianyi_entries(&mut items, container.get("folderList"), "folder", true);
        // An authenticated but brand-new drive can validly contain neither
        // files nor folders. The explicit list fields distinguish that from
        // an unknown response shape.
        if !items.is_empty() || has_file_listing || has_folder_listing {
            return Ok(items);
        }
    }
    let list = list.ok_or(QuarkError::InvalidResponse)?;
    append_tianyi_entries(&mut items, Some(list), "file", false);
    (!items.is_empty())
        .then_some(items)
        .ok_or(QuarkError::InvalidResponse)
}

fn append_tianyi_entries<'a>(
    output: &mut Vec<(&'a Value, bool)>,
    candidate: Option<&'a Value>,
    nested_name: &str,
    folder: bool,
) {
    let Some(candidate) = candidate else {
        return;
    };
    let candidate = candidate.get(nested_name).unwrap_or(candidate);
    if let Some(entries) = candidate.as_array() {
        output.extend(entries.iter().map(|entry| (entry, folder)));
    } else if candidate.is_object() {
        output.push((candidate, folder));
    }
}

/// Tianyi's authenticated file endpoints can return either JSON or XML.  XML
/// is converted into the small JSON-like tree consumed by the rest of this
/// provider, without exposing a general XML execution surface.  Attributes,
/// DTDs and processing instructions are intentionally ignored.
fn tianyi_response_value(body: &str) -> Result<Value, QuarkError> {
    if let Ok(value) = serde_json::from_str(body) {
        return Ok(value);
    }
    tianyi_xml_value(body)
}

struct XmlNode {
    name: String,
    children: serde_json::Map<String, Value>,
    text: String,
}

impl XmlNode {
    fn new(name: String) -> Self {
        Self {
            name,
            children: serde_json::Map::new(),
            text: String::new(),
        }
    }

    fn into_value(mut self) -> Value {
        let text = self.text.trim();
        if self.children.is_empty() {
            return Value::String(text.into());
        }
        if !text.is_empty() {
            self.children.insert("#text".into(), Value::String(text.into()));
        }
        Value::Object(self.children)
    }
}

fn insert_xml_child(children: &mut serde_json::Map<String, Value>, name: String, value: Value) {
    match children.entry(name) {
        serde_json::map::Entry::Vacant(entry) => {
            entry.insert(value);
        }
        serde_json::map::Entry::Occupied(mut entry) => match entry.get_mut() {
            Value::Array(values) => values.push(value),
            previous => {
                let previous = std::mem::replace(previous, Value::Null);
                *entry.get_mut() = Value::Array(vec![previous, value]);
            }
        },
    }
}

fn xml_name(name: &str) -> Result<String, QuarkError> {
    (!name.is_empty())
        .then(|| name.to_owned())
        .ok_or(QuarkError::InvalidResponse)
}

fn xml_text(raw: &str) -> Result<String, QuarkError> {
    quick_xml::escape::unescape(raw)
        .map(|value| value.into_owned())
        .map_err(|_| QuarkError::InvalidResponse)
}

fn tianyi_xml_value(body: &str) -> Result<Value, QuarkError> {
    use quick_xml::{events::Event, Reader};

    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut stack = Vec::<XmlNode>::new();
    let mut root: Option<(String, Value)> = None;
    loop {
        match reader.read_event().map_err(|_| QuarkError::InvalidResponse)? {
            Event::Start(event) => stack.push(XmlNode::new(xml_name(event.name().as_ref())?)),
            Event::Empty(event) => {
                let node = XmlNode::new(xml_name(event.name().as_ref())?);
                let name = node.name.clone();
                let value = node.into_value();
                if let Some(parent) = stack.last_mut() {
                    insert_xml_child(&mut parent.children, name, value);
                } else if root.is_none() {
                    root = Some((name, value));
                } else {
                    return Err(QuarkError::InvalidResponse);
                }
            }
            Event::Text(event) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&xml_text(event.as_ref())?);
                }
            }
            Event::CData(event) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(event.as_ref());
                }
            }
            Event::End(_) => {
                let node = stack.pop().ok_or(QuarkError::InvalidResponse)?;
                let name = node.name.clone();
                let value = node.into_value();
                if let Some(parent) = stack.last_mut() {
                    insert_xml_child(&mut parent.children, name, value);
                } else if root.is_none() {
                    root = Some((name, value));
                } else {
                    return Err(QuarkError::InvalidResponse);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(QuarkError::InvalidResponse);
    }
    let (name, value) = root.ok_or(QuarkError::InvalidResponse)?;
    Ok(json!({ name: value }))
}
