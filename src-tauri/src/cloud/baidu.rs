use super::{
    types::{encoded, required},
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudHttpRequest, CloudHttpResponse,
    CloudHttpTransport, CloudProvider, QuarkError,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

const PAN: &str = "https://pan.baidu.com";
const PASSPORT: &str = "https://passport.baidu.com";
const BAIDU_WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";
const REFERENCE_PREFIX: &str = "bdr_";

#[derive(Clone, Deserialize, Serialize)]
struct BaiduFileReference {
    fsid: String,
    path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BaiduSession {
    pub cookie: String,
    #[serde(default)]
    pub auth_method: CloudAuthMethod,
}

impl BaiduSession {
    pub fn cookie(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            auth_method: CloudAuthMethod::Web,
        }
    }
}

/// Extract only cookie pairs required by the official Pan web session.  Cookie
/// attributes (including expiry and domain) never enter the saved session.
pub fn parse_baidu_web_session(cookies: &[String]) -> Result<BaiduSession, QuarkError> {
    let mut pairs = Vec::new();
    for raw in cookies {
        let pair = raw.split(';').next().unwrap_or("").trim();
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if matches!(name, "BDUSS" | "STOKEN" | "PTOKEN" | "BDCLND" | "BAIDUID") && !value.is_empty()
        {
            pairs.push(format!("{name}={value}"));
        }
    }
    if !pairs.iter().any(|pair| pair.starts_with("BDUSS=")) {
        return Err(QuarkError::AuthorizationExpired);
    }
    Ok(BaiduSession::cookie(pairs.join("; ")))
}

#[derive(Clone, Debug)]
pub struct BaiduDownload {
    pub url: String,
    pub name: String,
    pub headers: BTreeMap<String, String>,
}

pub struct BaiduClient<'a, T: CloudHttpTransport> {
    session: BaiduSession,
    http: &'a T,
}

impl<'a, T: CloudHttpTransport> BaiduClient<'a, T> {
    pub fn new(session: BaiduSession, http: &'a T) -> Self {
        Self { session, http }
    }

    fn headers(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("Cookie".into(), self.session.cookie.clone()),
            ("Origin".into(), PAN.into()),
            ("Referer".into(), format!("{PAN}/")),
            ("User-Agent".into(), BAIDU_WEB_USER_AGENT.into()),
        ])
    }

    fn request(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method,
            url,
            headers: self.headers(),
            body: body.map(|value| value.to_string()),
        })?;
        baidu_json(&response)
    }

    fn xpan(
        &self,
        method: &'static str,
        operation: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        self.request(
            method,
            format!("{PAN}/rest/2.0/xpan/file?method={operation}&web=1{query}"),
            body,
        )
    }

    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let value = self.request(
            "GET",
            format!("{PAN}/rest/2.0/xpan/nas?method=uinfo&web=1"),
            None,
        )?;
        let user = value.get("user").unwrap_or(&value);
        let id = user
            .get("uk")
            .or_else(|| user.get("user_id"))
            .and_then(string_or_number)
            .ok_or(QuarkError::InvalidResponse)?;
        Ok(CloudAccount {
            provider: CloudProvider::Baidu,
            id,
            display_name: user
                .get("netdisk_name")
                .or_else(|| user.get("nickname"))
                .and_then(Value::as_str)
                .unwrap_or("百度网盘")
                .into(),
            auth_method: self.session.auth_method,
        })
    }

    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let parent = parse_parent_reference(parent)?;
        let value = self.xpan(
            "GET",
            "list",
            &format!("&dir={}&limit=100&order=name&desc=0", encoded(&parent.path)),
            None,
        )?;
        let files = value
            .get("list")
            .and_then(Value::as_array)
            .ok_or(QuarkError::InvalidResponse)?;
        let directory_id = if parent.path == "/" {
            "/".into()
        } else {
            encode_reference(parent.clone())?
        };
        let items = files
            .iter()
            .map(|file| {
                Ok(CloudFile {
                    id: encode_reference(BaiduFileReference {
                        fsid: file
                            .get("fs_id")
                            .or_else(|| file.get("fsid"))
                            .and_then(string_or_number)
                            .ok_or(QuarkError::InvalidResponse)?,
                        path: required(file, "path")?,
                    })?,
                    parent_id: directory_id.clone(),
                    name: required(file, "server_filename")
                        .or_else(|_| required(file, "name"))
                        .or_else(|_| {
                            file.get("path")
                                .and_then(Value::as_str)
                                .and_then(|path| path.rsplit('/').next())
                                .filter(|name| !name.is_empty())
                                .map(str::to_owned)
                                .ok_or(QuarkError::InvalidResponse)
                        })?,
                    kind: if file.get("isdir").and_then(Value::as_i64) == Some(1) {
                        "folder"
                    } else {
                        "file"
                    }
                    .into(),
                    size: file.get("size").and_then(Value::as_u64),
                    modified_at: file.get("server_mtime").and_then(string_or_number),
                })
            })
            .collect::<Result<Vec<_>, QuarkError>>()?;
        Ok(CloudDirectory {
            id: directory_id,
            items,
        })
    }

    pub fn mkdir(&self, parent: &str, name: &str) -> Result<(), QuarkError> {
        let parent = parse_parent_reference(parent)?;
        let path = join_path(&parent.path, name)?;
        let token = self.web_token()?;
        self.web_form(
            "/api/create",
            &write_query(&token, "a=commit"),
            form(&[
                ("path", path),
                ("isdir", "1".into()),
                ("block_list", "[]".into()),
            ]),
        )
        .map(|_| ())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        let file = parse_file_reference(id)?;
        validate_name(name)?;
        let token = self.web_token()?;
        self.web_file_manager(
            "rename",
            &token,
            json!([{ "path": file.path, "newname": name }]),
        )
    }

    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.file_manager_operation("move", ids, parent)
    }
    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.file_manager_operation("copy", ids, parent)
    }
    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        let paths = ids
            .iter()
            .map(|id| parse_file_reference(id).map(|reference| reference.path))
            .collect::<Result<Vec<_>, _>>()?;
        if paths.is_empty() {
            return Err(QuarkError::InvalidInput("请选择文件".into()));
        }
        let token = self.web_token()?;
        self.web_file_manager("delete", &token, json!(paths))
    }

    fn file_manager_operation(
        &self,
        operation: &str,
        ids: &[String],
        parent: &str,
    ) -> Result<(), QuarkError> {
        let parent = parse_parent_reference(parent)?;
        let files = ids
            .iter()
            .map(|id| parse_file_reference(id))
            .collect::<Result<Vec<_>, _>>()?;
        if files.is_empty() {
            return Err(QuarkError::InvalidInput("请选择文件".into()));
        }
        let filelist = files
            .iter()
            .map(|file| {
                Ok(json!({
                    "path": file.path,
                    "dest": parent.path,
                    "newname": basename(&file.path)?,
                }))
            })
            .collect::<Result<Vec<_>, QuarkError>>()?;
        let token = self.web_token()?;
        self.web_file_manager(operation, &token, json!(filelist))
    }

    /// Starts the official web precreate flow after obtaining a fresh token.
    pub fn upload(
        &self,
        parent: &str,
        name: &str,
        size: u64,
        content_md5: &str,
    ) -> Result<Value, QuarkError> {
        let parent = parse_parent_reference(parent)?;
        let path = join_path(&parent.path, name)?;
        if content_md5.is_empty() {
            return Err(QuarkError::InvalidInput("文件校验值无效".into()));
        }
        let token = self.web_token()?;
        self.web_form(
            "/api/precreate",
            &write_query(&token, ""),
            form(&[
                ("path", path),
                ("size", size.to_string()),
                ("isdir", "0".into()),
                ("autoinit", "1".into()),
                ("block_list", json!([content_md5]).to_string()),
            ]),
        )
    }

    pub fn download(&self, id: &str) -> Result<BaiduDownload, QuarkError> {
        let file = parse_file_reference(id)?;
        let value = self.request(
            "GET",
            format!(
                "{PAN}/rest/2.0/xpan/multimedia?method=filemetas&web=1&dlink=1&fsids={}",
                encoded(&format!("[{}]", file.fsid))
            ),
            None,
        )?;
        let item = value
            .get("list")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .ok_or(QuarkError::InvalidResponse)?;
        let url = required(item, "dlink")?;
        if !url.starts_with("https://") {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(BaiduDownload {
            url,
            name: item
                .get("server_filename")
                .and_then(Value::as_str)
                .unwrap_or(&file.path)
                .into(),
            headers: self.headers(),
        })
    }

    fn web_token(&self) -> Result<String, QuarkError> {
        let value = self.request(
            "GET",
            format!(
                "{PAN}/api/gettemplatevariable?clienttype=0&app_id=250528&web=1&fields=%5B%22bdstoken%22%5D"
            ),
            None,
        )?;
        required(
            value.get("result").ok_or(QuarkError::InvalidResponse)?,
            "bdstoken",
        )
    }

    fn web_form(&self, path: &str, query: &str, body: String) -> Result<Value, QuarkError> {
        let mut headers = self.headers();
        headers.insert(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        );
        let response = self.http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{PAN}{path}?{query}"),
            headers,
            body: Some(body),
        })?;
        baidu_json(&response)
    }

    fn web_file_manager(
        &self,
        operation: &str,
        token: &str,
        filelist: Value,
    ) -> Result<(), QuarkError> {
        self.web_form(
            "/api/filemanager",
            &write_query(token, &format!("opera={operation}&async=2&onnest=fail")),
            form(&[("filelist", filelist.to_string())]),
        )
        .map(|_| ())
    }

    /// Password login is deliberately single-shot: CAPTCHA, SMS and risk
    /// responses are handed back to the official browser rather than guessed.
    pub fn login_password(
        username: &str,
        password: &str,
        http: &'a T,
    ) -> Result<BaiduSession, QuarkError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(QuarkError::InvalidInput("请输入账号和密码".into()));
        }
        let password = Zeroizing::new(password.to_owned());
        let body = format!(
            "username={}&password={}&tpl=netdisk&logintype=basicLogin",
            encoded(username),
            encoded(&password)
        );
        let response = http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{PASSPORT}/v2/api/?login"),
            headers: BTreeMap::from([
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("Referer".into(), format!("{PAN}/")),
                ("User-Agent".into(), BAIDU_WEB_USER_AGENT.into()),
            ]),
            body: Some(body),
        })?;
        let value: Value = serde_json::from_str(&response.body)
            .map_err(|_| QuarkError::WebAuthorizationRequired)?;
        if challenge(&value) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        if value.get("err_no").and_then(Value::as_i64).unwrap_or(-1) != 0 {
            return Err(QuarkError::Provider("登录被服务拒绝".into()));
        }
        let session = parse_baidu_web_session(&response.set_cookies)?;
        let client = Self::new(session.clone(), http);
        client.account()?;
        client.directory("/")?;
        Ok(session)
    }
}

fn baidu_json(response: &CloudHttpResponse) -> Result<Value, QuarkError> {
    if response.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    if challenge(&value) {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let errno = value.get("errno").and_then(Value::as_i64).unwrap_or(0);
    if errno != 0 {
        if matches!(errno, -6 | -7 | -9 | 111) {
            return Err(QuarkError::AuthorizationExpired);
        }
        return Err(QuarkError::Provider("百度网盘拒绝请求".into()));
    }
    if !(200..300).contains(&response.status) {
        return Err(QuarkError::Provider("百度网盘请求失败".into()));
    }
    Ok(value)
}

fn challenge(value: &Value) -> bool {
    matches!(
        value.get("err_no").and_then(Value::as_i64),
        Some(6 | 16 | 257 | 120021 | 400031)
    ) || ["captcha", "sms", "verify", "risk", "codeString"]
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

fn encode_reference(reference: BaiduFileReference) -> Result<String, QuarkError> {
    if reference.fsid.is_empty()
        || !reference.fsid.bytes().all(|byte| byte.is_ascii_digit())
        || reference.path == "/"
    {
        return Err(QuarkError::InvalidResponse);
    }
    validate_path(&reference.path)?;
    let bytes = serde_json::to_vec(&reference).map_err(|_| QuarkError::InvalidResponse)?;
    Ok(format!(
        "{REFERENCE_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

fn parse_parent_reference(value: &str) -> Result<BaiduFileReference, QuarkError> {
    if value == "/" {
        return Ok(BaiduFileReference {
            fsid: String::new(),
            path: "/".into(),
        });
    }
    parse_file_reference(value)
}

fn parse_file_reference(value: &str) -> Result<BaiduFileReference, QuarkError> {
    let encoded = value
        .strip_prefix(REFERENCE_PREFIX)
        .ok_or(QuarkError::InvalidResponse)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| QuarkError::InvalidResponse)?;
    let reference: BaiduFileReference =
        serde_json::from_slice(&bytes).map_err(|_| QuarkError::InvalidResponse)?;
    if reference.fsid.is_empty() || !reference.fsid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(QuarkError::InvalidResponse);
    }
    validate_path(&reference.path)?;
    if reference.path == "/" {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(reference)
}

fn validate_path(path: &str) -> Result<(), QuarkError> {
    if !path.starts_with('/') || path.contains(['?', '#']) || path.chars().any(char::is_control) {
        return Err(QuarkError::InvalidResponse);
    }
    if path != "/" {
        if path.ends_with('/') {
            return Err(QuarkError::InvalidResponse);
        }
        for segment in path.split('/').skip(1) {
            if segment.is_empty() || matches!(segment, "." | "..") {
                return Err(QuarkError::InvalidResponse);
            }
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), QuarkError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return Err(QuarkError::InvalidInput("文件名无效".into()));
    }
    Ok(())
}

fn basename(path: &str) -> Result<&str, QuarkError> {
    validate_path(path)?;
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or(QuarkError::InvalidResponse)
}

fn join_path(parent: &str, name: &str) -> Result<String, QuarkError> {
    validate_name(name)?;
    Ok(if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    })
}

fn form(fields: &[(&str, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

fn write_query(token: &str, operation: &str) -> String {
    let mut query = format!(
        "bdstoken={}&clienttype=0&app_id=250528&web=1",
        encoded(token)
    );
    if !operation.is_empty() {
        query.push('&');
        query.push_str(operation);
    }
    query
}
