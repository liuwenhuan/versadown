//! UC Drive's authenticated web protocol.
//!
//! UC issues browser-cookie sessions rather than a stable public OAuth token.
//! This adapter therefore retains only the minimal cookie pairs obtained from
//! the official `drive.uc.cn` web authorization flow.  Signed download URLs
//! and response cookies are kept in `UcDownload` for immediate use only.
use super::{
    types::required, CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudHttpRequest,
    CloudHttpResponse, CloudHttpTransport, CloudProvider, QuarkError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fmt};

const API: &str = "https://pc-api.uc.cn/1/clouddrive";
const WEB: &str = "https://drive.uc.cn";
pub const UC_DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) uc-cloud-drive/2.5.20 Chrome/100.0.4896.160 Electron/18.3.5.4-b478491100 Safari/537.36 Channel/pckk_other_ch";

#[derive(Clone, Deserialize, Serialize)]
pub struct UcSession {
    pub cookie: String,
    #[serde(default)]
    pub auth_method: CloudAuthMethod,
}

impl fmt::Debug for UcSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UcSession")
            .field("cookie", &"[redacted]")
            .field("auth_method", &self.auth_method)
            .finish()
    }
}

impl UcSession {
    pub fn cookie(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            auth_method: CloudAuthMethod::Web,
        }
    }
}

/// Convert official-WebView cookie records into the minimum restorable UC
/// session.  Cookie attributes, trackers, and non-UC cookies are excluded.
pub fn parse_uc_web_session(cookies: &[String]) -> Result<UcSession, QuarkError> {
    const SESSION_COOKIE_NAMES: &[&str] =
        &["__uid", "__pus", "__puus", "_UP_A4A_11_", "__kp", "__kps"];
    let mut pairs: Vec<(String, String)> = Vec::new();
    for raw in cookies {
        let pair = raw.split(';').next().unwrap_or("").trim();
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if value.is_empty()
            || !SESSION_COOKIE_NAMES.contains(&name)
            || name.chars().any(char::is_control)
            || value.contains(['\r', '\n'])
        {
            continue;
        }
        if let Some(existing) = pairs.iter_mut().find(|(current, _)| current == name) {
            existing.1 = value.to_owned();
        } else {
            pairs.push((name.to_owned(), value.to_owned()));
        }
    }
    if !pairs.iter().any(|(name, _)| name == "__pus")
        || !pairs.iter().any(|(name, _)| name == "__puus")
    {
        return Err(QuarkError::AuthorizationExpired);
    }
    Ok(UcSession::cookie(
        pairs
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; "),
    ))
}

#[derive(Clone, Debug)]
pub struct UcDownload {
    pub url: String,
    pub name: String,
    pub headers: BTreeMap<String, String>,
}

pub struct UcClient<'a, T: CloudHttpTransport> {
    session: UcSession,
    http: &'a T,
}

impl<'a, T: CloudHttpTransport> UcClient<'a, T> {
    pub fn new(session: UcSession, http: &'a T) -> Self {
        Self { session, http }
    }

    pub fn session(&self) -> &UcSession {
        &self.session
    }

    fn cookie(&self) -> Result<&str, QuarkError> {
        let has_identity = self
            .session
            .cookie
            .split(';')
            .filter_map(cookie_pair)
            .any(|(name, _)| name == "__pus");
        let has_session = self
            .session
            .cookie
            .split(';')
            .filter_map(cookie_pair)
            .any(|(name, _)| name == "__puus");
        if has_identity && has_session {
            Ok(&self.session.cookie)
        } else {
            Err(QuarkError::AuthorizationExpired)
        }
    }

    fn headers(&self) -> Result<BTreeMap<String, String>, QuarkError> {
        Ok(BTreeMap::from([
            ("Cookie".into(), self.cookie()?.to_owned()),
            ("Origin".into(), WEB.into()),
            ("Referer".into(), format!("{WEB}/")),
            ("User-Agent".into(), UC_DESKTOP_USER_AGENT.into()),
        ]))
    }

    fn request(
        &self,
        method: &'static str,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        let query = if query.is_empty() {
            "pr=UCBrowser&fr=pc".into()
        } else {
            format!("{query}&fr=pc&pr=UCBrowser")
        };
        let response = self.http.send(CloudHttpRequest {
            method,
            url: format!("{API}/{path}?{query}"),
            headers: self.headers()?,
            body: body.map(|value| value.to_string()),
        })?;
        uc_json(&response)?;
        Ok(response)
    }

    fn request_json(
        &self,
        method: &'static str,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.request(method, path, query, body)?;
        uc_json(&response)
    }

    /// Build the account shown immediately after the official browser has
    /// issued a UC session.  The browser session itself is the authorization
    /// proof; an undocumented `member` probe must not keep the authorization
    /// window open, because that endpoint can be rejected independently of
    /// normal directory access.
    pub fn web_account(&self) -> Result<CloudAccount, QuarkError> {
        let cookie = self.cookie()?;
        Ok(CloudAccount {
            provider: CloudProvider::Uc,
            id: cookie_value(cookie, "__uid")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("uc-{:x}", md5::compute(cookie))),
            display_name: "UC网盘".into(),
            auth_method: self.session.auth_method,
        })
    }

    /// UC's member endpoint verifies that the cookie can access the drive.
    /// It does not guarantee that a nickname is returned, so `__uid` supplies
    /// a stable account identity and the display name has a safe fallback.
    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let value = self.request_json(
            "GET",
            "member",
            "fetch_subscribe=true&_ch=home&fetch_identity=true",
            None,
        )?;
        let member = value.get("data").ok_or(QuarkError::InvalidResponse)?;
        let id = cookie_value(self.cookie()?, "__uid")
            .or_else(|| member.get("user_id").and_then(string_or_number))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("uc-{:x}", md5::compute(self.cookie().unwrap_or_default())));
        let display_name = member
            .get("nickname")
            .or_else(|| member.get("user_name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("UC网盘")
            .to_owned();
        Ok(CloudAccount {
            provider: CloudProvider::Uc,
            id,
            display_name,
            auth_method: self.session.auth_method,
        })
    }

    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        if parent.is_empty() || parent.contains(['?', '#', '\r', '\n']) {
            return Err(QuarkError::InvalidInput("目录 ID 无效".into()));
        }
        let mut items = Vec::new();
        for page in 1..=10_000 {
            let value = self.request_json(
                "GET",
                "file/sort",
                &format!(
                    "pdir_fid={}&_page={page}&_size=200&_sort=file_name%3Aasc",
                    encoded(parent)
                ),
                None,
            )?;
            let files = value
                .pointer("/data/list")
                .and_then(Value::as_array)
                .ok_or(QuarkError::InvalidResponse)?;
            let count = files.len();
            items.extend(
                files
                    .iter()
                    .map(|file| parse_file(file, parent))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let total = value.pointer("/metadata/_total").and_then(Value::as_u64);
            if count < 200 || total.is_some_and(|total| items.len() as u64 >= total) {
                return Ok(CloudDirectory {
                    id: parent.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }

    pub fn mkdir(&self, parent: &str, name: &str) -> Result<CloudHttpResponse, QuarkError> {
        validate_name(name)?;
        self.request(
            "POST",
            "file",
            "",
            Some(json!({"pdir_fid":parent,"file_name":name,"dir_path":"","dir_init_lock":false})),
        )
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<CloudHttpResponse, QuarkError> {
        validate_id(id)?;
        validate_name(name)?;
        self.request(
            "POST",
            "file/rename",
            "",
            Some(json!({"fid":id,"file_name":name})),
        )
    }

    pub fn move_files(
        &self,
        ids: &[String],
        parent: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.file_operation("file/move", ids, parent)
    }

    pub fn copy_files(
        &self,
        ids: &[String],
        parent: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.file_operation("file/copy", ids, parent)
    }

    fn file_operation(
        &self,
        path: &str,
        ids: &[String],
        parent: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        validate_ids(ids)?;
        validate_id(parent)?;
        self.request(
            "POST",
            path,
            "",
            Some(json!({"filelist":ids,"to_pdir_fid":parent,"action_type":1})),
        )
    }

    pub fn trash(&self, ids: &[String]) -> Result<CloudHttpResponse, QuarkError> {
        validate_ids(ids)?;
        self.request(
            "POST",
            "file/delete",
            "",
            Some(json!({"action_type":2,"filelist":ids,"exclude_fids":[]})),
        )
    }

    /// Resolve an immediate, signed URL.  It is intentionally returned rather
    /// than stored; callers must create a stable `cloud://uc/<fid>` task source.
    pub fn download(&self, id: &str) -> Result<UcDownload, QuarkError> {
        validate_id(id)?;
        let response = self.request(
            "POST",
            "file/download",
            "entry=ft",
            Some(json!({"fids":[id]})),
        )?;
        let value = uc_json(&response)?;
        let file = value
            .pointer("/data/0")
            .ok_or(QuarkError::InvalidResponse)?;
        let url = https_download_url(&required(file, "download_url")?)?;
        let mut headers = self.headers()?;
        headers.insert(
            "Cookie".into(),
            cookie_for_download(self.cookie()?, &response.set_cookies),
        );
        Ok(UcDownload {
            url,
            name: file
                .get("file_name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(id)
                .to_owned(),
            headers,
        })
    }
}

fn uc_json(response: &CloudHttpResponse) -> Result<Value, QuarkError> {
    if response.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    let status = value.get("status").and_then(Value::as_i64).unwrap_or(200);
    let message = value
        .get("message")
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if code == 401 || status == 401 || message.to_ascii_lowercase().contains("login") {
        return Err(QuarkError::AuthorizationExpired);
    }
    if ["captcha", "verify", "risk", "安全验证"]
        .iter()
        .any(|needle| {
            value
                .to_string()
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase())
        })
    {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    if !(200..300).contains(&response.status) || code != 0 || !matches!(status, 0 | 200) {
        return Err(QuarkError::Provider("UC网盘拒绝请求".into()));
    }
    Ok(value)
}

fn parse_file(file: &Value, parent: &str) -> Result<CloudFile, QuarkError> {
    Ok(CloudFile {
        id: required(file, "fid")?,
        parent_id: parent.into(),
        name: required(file, "file_name")?,
        kind: if file
            .get("dir")
            .and_then(Value::as_bool)
            .unwrap_or(file.get("file_type").and_then(Value::as_i64) == Some(0))
        {
            "folder"
        } else {
            "file"
        }
        .into(),
        size: file.get("size").and_then(Value::as_u64),
        modified_at: file.get("updated_at").and_then(string_or_number),
    })
}

fn encoded(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn string_or_number(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn validate_name(name: &str) -> Result<(), QuarkError> {
    if name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(QuarkError::InvalidInput("文件名无效".into()));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), QuarkError> {
    if id.is_empty() || id.contains(['?', '#', '\r', '\n']) {
        return Err(QuarkError::InvalidInput("文件 ID 无效".into()));
    }
    Ok(())
}

fn validate_ids(ids: &[String]) -> Result<(), QuarkError> {
    if ids.is_empty() {
        return Err(QuarkError::InvalidInput("请选择至少一个文件".into()));
    }
    ids.iter().try_for_each(|id| validate_id(id))
}

fn cookie_pair(value: &str) -> Option<(&str, &str)> {
    let pair = value.trim();
    let (name, value) = pair.split_once('=')?;
    (!name.is_empty()
        && !value.is_empty()
        && !pair.contains(['\r', '\n'])
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)))
    .then_some((name, value))
}

fn cookie_value(cookie: &str, wanted: &str) -> Option<String> {
    cookie
        .split(';')
        .filter_map(cookie_pair)
        .find_map(|(name, value)| (name == wanted).then(|| value.to_owned()))
}

fn cookie_for_download(web_cookie: &str, response_cookies: &[String]) -> String {
    let mut cookies: Vec<(String, String)> = web_cookie
        .split(';')
        .filter_map(cookie_pair)
        .map(|(name, value)| (name.to_owned(), format!("{name}={value}")))
        .collect();
    for response_cookie in response_cookies {
        let pair = response_cookie.split(';').next().unwrap_or("");
        if let Some((name, value)) = cookie_pair(pair) {
            cookies.retain(|(current, _)| current != name);
            cookies.push((name.to_owned(), format!("{name}={value}")));
        }
    }
    cookies
        .into_iter()
        .map(|(_, pair)| pair)
        .collect::<Vec<_>>()
        .join("; ")
}

fn https_download_url(value: &str) -> Result<String, QuarkError> {
    let url = url::Url::parse(value).map_err(|_| QuarkError::InvalidResponse)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(url.into())
}
