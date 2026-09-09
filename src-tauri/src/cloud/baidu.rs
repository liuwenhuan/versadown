use super::{
    types::{encoded, required},
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudHttpRequest, CloudHttpResponse,
    CloudHttpTransport, CloudProvider, QuarkError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

const PAN: &str = "https://pan.baidu.com";
const PASSPORT: &str = "https://passport.baidu.com";
const BAIDU_WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";

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
        let value = self.xpan(
            "GET",
            "list",
            &format!("&dir={}&limit=100&order=name&desc=0", encoded(parent)),
            None,
        )?;
        let files = value
            .get("list")
            .and_then(Value::as_array)
            .ok_or(QuarkError::InvalidResponse)?;
        let items = files
            .iter()
            .map(|file| {
                Ok(CloudFile {
                    id: file
                        .get("fs_id")
                        .or_else(|| file.get("fsid"))
                        .and_then(string_or_number)
                        .ok_or(QuarkError::InvalidResponse)?,
                    parent_id: parent.into(),
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
            id: parent.into(),
            items,
        })
    }

    pub fn mkdir(&self, parent: &str, name: &str) -> Result<(), QuarkError> {
        let path = join_path(parent, name)?;
        self.xpan(
            "POST",
            "create",
            &format!("&path={}&isdir=1", encoded(&path)),
            None,
        )
        .map(|_| ())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), QuarkError> {
        self.xpan(
            "POST",
            "update",
            "",
            Some(json!({"fsid": id, "newname": name})),
        )
        .map(|_| ())
    }

    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.file_manager("move", ids, parent)
    }
    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        self.file_manager("copy", ids, parent)
    }
    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.xpan(
            "POST",
            "filemanager",
            "&opera=delete",
            Some(json!({"filelist": ids})),
        )
        .map(|_| ())
    }

    fn file_manager(
        &self,
        operation: &str,
        ids: &[String],
        parent: &str,
    ) -> Result<(), QuarkError> {
        if ids.is_empty() || parent.is_empty() {
            return Err(QuarkError::InvalidInput("请选择文件和目标目录".into()));
        }
        self.xpan(
            "POST",
            "filemanager",
            &format!("&opera={operation}"),
            Some(json!({"filelist": ids, "dest": parent})),
        )
        .map(|_| ())
    }

    /// Starts the official precreate upload flow.  The command layer owns file
    /// bytes and therefore never puts local paths or payloads in this adapter.
    pub fn upload(
        &self,
        parent: &str,
        name: &str,
        size: u64,
        content_md5: &str,
    ) -> Result<Value, QuarkError> {
        let path = join_path(parent, name)?;
        self.request(
            "POST",
            format!("{PAN}/api/precreate?web=1"),
            Some(
                json!({"path":path,"size":size,"isdir":0,"autoinit":1,"block_list":[content_md5]}),
            ),
        )
    }

    pub fn download(&self, id: &str) -> Result<BaiduDownload, QuarkError> {
        let value = self.request(
            "GET",
            format!(
                "{PAN}/rest/2.0/xpan/multimedia?method=filemetas&web=1&dlink=1&fsids={}",
                encoded(&format!("[{id}]"))
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
                .unwrap_or(id)
                .into(),
            headers: self.headers(),
        })
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
fn join_path(parent: &str, name: &str) -> Result<String, QuarkError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(QuarkError::InvalidInput("文件名无效".into()));
    }
    Ok(if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    })
}
