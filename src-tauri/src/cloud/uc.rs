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
            message
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

use super::share::{
    CloudShareAdapter, CloudShareEntry, CloudSharePlan, CloudSharePlanStore,
    CloudShareResolvedEntry, ShareInspection, ShareResolution, TransferRequest,
};

pub struct UcShareAdapter<'a> {
    pub credentials: &'a dyn super::CredentialStore,
    pub http: &'a dyn CloudHttpTransport,
}
struct UcShareSession<'a> {
    http: &'a dyn CloudHttpTransport,
    headers: BTreeMap<String, String>,
}
impl UcShareAdapter<'_> {
    fn session(&self) -> Result<UcShareSession<'_>, QuarkError> {
        let bytes = self
            .credentials
            .load("uc")?
            .ok_or(QuarkError::NotAuthorized)?;
        let session: UcSession = serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if session.cookie.is_empty() {
            return Err(QuarkError::NotAuthorized);
        }
        // Use the same authenticated cookie snapshot for signing and downloading.
        let valid = ["__pus", "__puus"]
            .iter()
            .all(|name| cookie_value(&session.cookie, name).is_some());
        if !valid {
            return Err(QuarkError::AuthorizationExpired);
        }
        let mut headers = BTreeMap::from([
            ("Cookie".into(), session.cookie.clone()),
            ("User-Agent".into(), UC_DESKTOP_USER_AGENT.into()),
            ("Origin".into(), WEB.into()),
            ("Referer".into(), format!("{WEB}/")),
        ]);
        if let Some(csrf) = cookie_value(&session.cookie, "csrf_token") {
            headers.insert("x-csrf-token".into(), csrf);
        }
        Ok(UcShareSession {
            http: self.http,
            headers,
        })
    }
}
impl UcShareSession<'_> {
    fn request(
        &mut self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method,
            url: format!(
                "{API}/{path}{}pr=UCBrowser&fr=pc",
                if path.contains('?') { "&" } else { "?" }
            ),
            headers: self.headers.clone(),
            body: body.map(|v| v.to_string()),
        })?;
        if response.status == 401 {
            return Err(QuarkError::AuthorizationExpired);
        }
        if response.status == 412 {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let value: Value =
            serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
        let code = value["code"].as_i64().ok_or(QuarkError::InvalidResponse)?;
        for key in ["error_code", "status"] {
            if value.get(key).is_some_and(|v| v.as_i64().is_none()) {
                return Err(QuarkError::InvalidResponse);
            }
        }
        if value
            .get("error_code")
            .is_some_and(|v| v.as_i64() != Some(code))
        {
            return Err(QuarkError::InvalidResponse);
        }
        if value.get("error").is_some_and(|v| v.as_str().is_none()) {
            return Err(QuarkError::InvalidResponse);
        }
        if matches!(code, 9 | 4031) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        if matches!(
            value["message"].as_str(),
            Some("提取码错误" | "提取码不正确" | "请输入提取码" | "提取码不能为空")
        ) && code != 0
        {
            return Err(QuarkError::InvalidInput("UC 分享访问码错误或缺失".into()));
        }
        uc_json(&response)?;
        if value
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
        {
            return Err(QuarkError::Provider("UC 分享请求失败".into()));
        }
        let cookie = cookie_for_download(&self.headers["Cookie"], &response.set_cookies);
        // Only a newly issued CSRF cookie can replace a newer token-body value.
        // The merged cookie may still contain an older authenticated-session value.
        let issued_cookie = cookie_for_download("", &response.set_cookies);
        if let Some(csrf) = cookie_value(&issued_cookie, "csrf_token") {
            self.headers.insert("x-csrf-token".into(), csrf);
        }
        self.headers.insert("Cookie".into(), cookie);
        Ok(value)
    }
    fn token(&mut self, id: &str, code: Option<&str>) -> Result<String, QuarkError> {
        let value = self.request(
            "POST",
            "share/sharepage/token",
            Some(json!({"pwd_id":id,"passcode":code.unwrap_or("")})),
        )?;
        if let Some(csrf) = value.pointer("/data/csrf_token") {
            let csrf = csrf
                .as_str()
                .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
                .ok_or(QuarkError::InvalidResponse)?;
            self.headers.insert("x-csrf-token".into(), csrf.into());
        }
        required(&value["data"], "stoken")
    }
    fn require_csrf(&self) -> Result<(), QuarkError> {
        if self
            .headers
            .get("x-csrf-token")
            .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_control))
        {
            Ok(())
        } else {
            Err(QuarkError::WebAuthorizationRequired)
        }
    }
}
impl CloudShareAdapter for UcShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        let id = super::share::share_id(candidate, CloudProvider::Uc)?;
        let mut session = self.session()?;
        let token = match session.token(&id, candidate.access_code.as_deref()) {
            Err(QuarkError::InvalidInput(_)) if candidate.access_code.is_none() => {
                return Ok(ShareInspection::AccessCodeRequired)
            }
            result => result?,
        };
        let mut context = json!({"stoken":token,"files":{}});
        let mut queue = std::collections::VecDeque::from([("0".to_owned(), Vec::<String>::new())]);
        let mut seen = std::collections::HashSet::new();
        let mut entries = vec![];
        let mut requests = 0;
        while let Some((parent, directory)) = queue.pop_front() {
            let mut count = 0u64;
            for page in 1..=10_000 {
                requests += 1;
                if requests > 10_000 || directory.len() > 128 {
                    return Err(QuarkError::InvalidResponse);
                }
                let value=session.request("GET",&format!("share/sharepage/detail?pwd_id={}&stoken={}&pdir_fid={}&_page={page}&_size=200&_fetch_total=1",encoded(&id),encoded(&token),encoded(&parent)),None)?;
                let files = value["data"]["list"]
                    .as_array()
                    .ok_or(QuarkError::InvalidResponse)?;
                count += files.len() as u64;
                for raw in files {
                    let file = parse_file(raw, &parent)?;
                    if !seen.insert(file.id.clone()) {
                        return Err(QuarkError::InvalidResponse);
                    }
                    if file.kind == "folder" {
                        let mut path = directory.clone();
                        path.push(file.name);
                        queue.push_back((file.id, path));
                    } else {
                        context["files"][&file.id] =
                            json!({"token":required(raw,"share_fid_token")?,"parent":parent});
                        entries.push(CloudShareEntry::new(
                            &id,
                            file.id,
                            file.name,
                            directory.clone(),
                            file.size,
                        ));
                    }
                }
                let more = value
                    .pointer("/metadata/_total")
                    .and_then(Value::as_u64)
                    .map(|total| count < total)
                    .unwrap_or(files.len() == 200);
                if !more {
                    break;
                }
                if files.is_empty() || page == 10_000 {
                    return Err(QuarkError::InvalidResponse);
                }
            }
        }
        // This internal context is serializable only through the DPAPI plan store.
        context["headers"] =
            serde_json::to_value(session.headers).map_err(|_| QuarkError::InvalidResponse)?;
        Ok(ShareInspection::Plan(CloudSharePlan::new(
            CloudProvider::Uc,
            id,
            candidate.access_code.clone(),
            entries,
            context,
        )))
    }
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        resolve_uc_share(self.credentials, self.http, entry)
    }
    fn transfer(
        &self,
        entry: &CloudShareResolvedEntry,
        store: &CloudSharePlanStore,
    ) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
        if entry.provider != CloudProvider::Uc || entry.plan.provider != CloudProvider::Uc {
            return Err(QuarkError::InvalidResponse);
        }
        let mut session = self.session()?;
        let token = session.token(&entry.plan.canonical_share_id, entry.access_code())?;
        session.require_csrf()?;
        let file = &entry.plan.provider_context["files"][&entry.entry.entry_id];
        let value=session.request("POST","share/sharepage/save",Some(json!({"fid_list":[entry.entry.entry_id],"fid_token_list":[required(file,"token")?],"to_pdir_fid":"0","pwd_id":entry.plan.canonical_share_id,"stoken":token,"pdir_fid":required(file,"parent")?,"scene":"link","pdir_save_all":false,"exclude_fids":[]})))?;
        let task = required(&value["data"], "task_id")?;
        for retry in 0..20 {
            let value = session.request(
                "GET",
                &format!("task?task_id={}&retry_index={retry}", encoded(&task)),
                None,
            )?;
            match value["data"]["status"].as_i64() {
                Some(2) => {
                    let ids = value
                        .pointer("/data/save_as/save_as_top_fids")
                        .and_then(Value::as_array)
                        .filter(|v| v.len() == 1)
                        .ok_or(QuarkError::InvalidResponse)?;
                    let id = ids[0]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or(QuarkError::InvalidResponse)?;
                    store.record_transfer(&entry.plan.source_for(&entry.entry.entry_id), id)?;
                    let headers = session.headers.clone();
                    let value =
                        session.request("POST", "file/download", Some(json!({"fids":[id]})))?;
                    return uc_share_download(&value, id, &entry.entry.name, headers);
                }
                Some(0 | 1) => std::thread::sleep(std::time::Duration::from_millis(250)),
                _ => return Err(QuarkError::Provider("UC 分享转存失败".into())),
            }
        }
        Err(QuarkError::Provider("UC 分享转存尚未完成".into()))
    }
}
fn uc_share_download(
    value: &Value,
    id: &str,
    name: &str,
    headers: BTreeMap<String, String>,
) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
    let files = value["data"]
        .as_array()
        .filter(|v| v.len() == 1)
        .ok_or(QuarkError::InvalidResponse)?;
    let file = &files[0];
    if file["fid"].as_str() != Some(id) {
        return Err(QuarkError::InvalidResponse);
    }
    let url = required(file, "download_url")?;
    super::share::validate_direct_url(&url)?;
    Ok(super::commands::ResolvedCloudDownload {
        url,
        name: name.into(),
        headers: headers.into_iter().collect(),
    })
}
pub fn resolve_uc_share(
    credentials: &dyn super::CredentialStore,
    http: &dyn CloudHttpTransport,
    entry: &CloudShareResolvedEntry,
) -> Result<ShareResolution, QuarkError> {
    if entry.provider != CloudProvider::Uc || entry.plan.provider != CloudProvider::Uc {
        return Err(QuarkError::InvalidResponse);
    }
    let adapter = UcShareAdapter { credentials, http };
    let mut session = adapter.session()?;
    let token = session.token(&entry.plan.canonical_share_id, entry.access_code())?;
    session.require_csrf()?;
    let headers = session.headers.clone();
    let value=session.request("POST","share/sharepage/download",Some(json!({"pwd_id":entry.plan.canonical_share_id,"stoken":token,"fids":[entry.entry.entry_id],"fids_token":[required(&entry.plan.provider_context["files"][&entry.entry.entry_id],"token")?]})))?;
    if value["data"].as_array().is_some_and(|v| v.is_empty()) {
        return Ok(ShareResolution::TransferRequired(TransferRequest {
            source: entry.plan.source_for(&entry.entry.entry_id),
        }));
    }
    Ok(ShareResolution::Direct(uc_share_download(
        &value,
        &entry.entry.entry_id,
        &entry.entry.name,
        headers,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::share::{
        test_support::{entry, Http},
        *,
    };
    use crate::cloud::{CredentialStore, MemoryCredentialStore};

    fn share_credentials() -> MemoryCredentialStore {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save(
                "uc",
                &serde_json::to_vec(&UcSession::cookie(
                    "__pus=identity; __puus=session; csrf_token=csrf-secret",
                ))
                .unwrap(),
            )
            .unwrap();
        credentials
    }
    fn share_entry() -> CloudShareResolvedEntry {
        entry(CloudSharePlan::new(
            CloudProvider::Uc,
            "share123".into(),
            Some("a123".into()),
            vec![CloudShareEntry::new(
                "share123",
                "f1".into(),
                "file.txt".into(),
                vec![],
                Some(7),
            )],
            json!({"stoken":"old-token","files":{"f1":{"token":"file-token","parent":"0"}}}),
        ))
    }
    #[test]
    fn uc_share_direct_url_keeps_identity_headers_out_of_source() {
        let credentials = share_credentials();
        let http = Http::new(vec![
            json!({"code":0,"data":{"stoken":"fresh-token"}}),
            json!({"code":0,"data":[{"fid":"f1","download_url":"https://cdn.example/file?signature=secret"}]}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Uc, &credentials, &http)
            .expect("UC share adapter registered");
        let entry = share_entry();
        let ShareResolution::Direct(direct) = adapter.resolve(&entry).unwrap() else {
            panic!("direct URL expected")
        };
        for (name, value) in [
            (
                "Cookie",
                "__pus=identity; __puus=session; csrf_token=csrf-secret; SESSION=issued",
            ),
            ("x-csrf-token", "csrf-secret"),
            ("User-Agent", UC_DESKTOP_USER_AGENT),
            ("Origin", WEB),
            ("Referer", "https://drive.uc.cn/"),
        ] {
            assert!(direct
                .headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case(name) && v == value));
        }
        assert_eq!(direct.url, "https://cdn.example/file?signature=secret");
        for secret in [
            "cookie",
            "csrf-secret",
            "fresh-token",
            "signature",
            "share123",
        ] {
            assert!(!entry.plan.source_for("f1").contains(secret));
        }
    }
    #[test]
    fn uc_share_pages_recursively_and_retains_only_protected_token_context() {
        let credentials = share_credentials();
        let http = Http::new(vec![
            json!({"code":0,"data":{"stoken":"fresh-token"}}),
            json!({"code":0,"data":{"list":[{"fid":"d","file_name":"dir","dir":true}]},"metadata":{"_total":2}}),
            json!({"code":0,"data":{"list":[{"fid":"f1","file_name":"one","dir":false,"share_fid_token":"f-token"}]},"metadata":{"_total":2}}),
            json!({"code":0,"data":{"list":[{"fid":"f2","file_name":"two","dir":false,"share_fid_token":"nested-token"}]},"metadata":{"_total":1}}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Uc, &credentials, &http).unwrap();
        let candidate = crate::parser::CloudShareCandidate {
            provider: CloudProvider::Uc,
            url: "https://drive.uc.cn/s/share123".into(),
            access_code: Some("a123".into()),
        };
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate).unwrap() else {
            panic!()
        };
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[1].relative_directory, vec!["dir"]);
        assert_eq!(
            plan.provider_context["files"]["f2"]["token"],
            "nested-token"
        );
        let requests = http.requests.lock().unwrap();
        assert!(requests[2].url.contains("_page=2"));
        assert!(requests[3].url.contains("pdir_fid=d"));
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["passcode"], "a123");
    }
    #[test]
    fn uc_share_errors_missing_csrf_and_no_login_never_transfer_or_retry() {
        let empty = MemoryCredentialStore::default();
        let http = Http::new(vec![]);
        let adapter = provider_share_adapter(CloudProvider::Uc, &empty, &http).unwrap();
        assert!(matches!(
            adapter.resolve(&share_entry()),
            Err(QuarkError::NotAuthorized)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
        let credentials = share_credentials();
        for value in [
            json!({"code":401}),
            json!({"code":4031,"message":"risk verification"}),
            json!({"code":1,"message":"expired share secret"}),
            json!({"code":"0","data":[]}),
            json!({"code":0,"error_code":4031}),
        ] {
            let http = Http::new(vec![
                json!({"code":0,"data":{"stoken":"fresh"}}),
                value.clone(),
            ]);
            let adapter = provider_share_adapter(CloudProvider::Uc, &credentials, &http).unwrap();
            let error = adapter.resolve(&share_entry()).err().unwrap();
            if value["code"] == 4031 {
                assert!(matches!(error, QuarkError::WebAuthorizationRequired));
            }
            assert!(!error.to_string().contains("expired share secret"));
            assert_eq!(http.requests.lock().unwrap().len(), 2);
        }
        let no_csrf = MemoryCredentialStore::default();
        no_csrf
            .save(
                "uc",
                &serde_json::to_vec(&UcSession::cookie("__pus=i; __puus=s")).unwrap(),
            )
            .unwrap();
        let http = Http::new(vec![json!({"code":0,"data":{"stoken":"fresh"}})]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Uc, &no_csrf, &http)
                .unwrap()
                .resolve(&share_entry()),
            Err(QuarkError::WebAuthorizationRequired)
        ));
    }
    #[test]
    fn uc_share_issued_csrf_is_used_and_no_direct_journals_exact_transfer() {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save(
                "uc",
                &serde_json::to_vec(&UcSession::cookie("__pus=i; __puus=s")).unwrap(),
            )
            .unwrap();
        let token = json!({"code":0,"data":{"stoken":"fresh","csrf_token":"issued-csrf"}});
        let http = Http::new(vec![
            token.clone(),
            json!({"code":0,"data":[]}),
            token,
            json!({"code":0,"data":{"task_id":"t"}}),
            json!({"code":0,"data":{"status":2,"save_as":{"save_as_top_fids":["personal-id"]}}}),
            json!({"code":401}),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let store = CloudSharePlanStore::new(
            crate::cloud::DpapiCredentialStore::new(directory.path().into()).unwrap(),
        );
        let entry = share_entry();
        store.upsert(&entry.plan).unwrap();
        let source = entry.plan.source_for("f1");
        let adapter = provider_share_adapter(CloudProvider::Uc, &credentials, &http).unwrap();
        assert!(matches!(
            resolve_current_share_download(&store, adapter.as_ref(), &source),
            Err(QuarkError::AuthorizationExpired)
        ));
        assert_eq!(
            store.resolve_entry(&source).unwrap().plan.transfers[0].created_file_id,
            "personal-id"
        );
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests[1].headers["x-csrf-token"], "issued-csrf");
        assert!(requests[5].body.as_ref().unwrap().contains("personal-id"));
    }

    struct DirectoryTransport;

    impl CloudHttpTransport for DirectoryTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{
                    "code": 0,
                    "status": 200,
                    "data": {
                        "list": [{
                            "fid": "file-1",
                            "file_name": "normal-file.txt",
                            "file_type": 1,
                            "risk_type": 0
                        }]
                    },
                    "metadata": {"_total": 1}
                }"#
                .into(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn directory_accepts_successful_file_metadata_that_includes_risk_type() {
        let directory = UcClient::new(
            UcSession::cookie("__pus=identity; __puus=session"),
            &DirectoryTransport,
        )
        .directory("0")
        .expect("a successful UC directory response is not a web-verification response");

        assert_eq!(directory.id, "0");
        assert_eq!(directory.items.len(), 1);
        assert_eq!(directory.items[0].id, "file-1");
    }

    #[test]
    fn uc_share_newer_json_csrf_survives_followup_responses_without_csrf_cookies() {
        let credentials = share_credentials();
        let http = Http::new(vec![
            json!({"code":0,"data":{"stoken":"fresh","csrf_token":"newer-csrf"}}),
            json!({"code":0,"data":{"task_id":"t"}}),
            json!({"code":0,"data":{"status":2,"save_as":{"save_as_top_fids":["personal-id"]}}}),
            json!({"code":0,"data":[{"fid":"personal-id","download_url":"https://cdn.example/personal"}]}),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let store = CloudSharePlanStore::new(
            crate::cloud::DpapiCredentialStore::new(directory.path().into()).unwrap(),
        );
        let entry = share_entry();
        store.upsert(&entry.plan).unwrap();
        let adapter = provider_share_adapter(CloudProvider::Uc, &credentials, &http).unwrap();
        let direct = adapter.transfer(&entry, &store).unwrap();
        assert!(direct
            .headers
            .contains(&("x-csrf-token".into(), "newer-csrf".into())));
        assert_eq!(
            http.requests.lock().unwrap()[3].headers["x-csrf-token"],
            "newer-csrf"
        );
    }
    #[test]
    fn uc_share_set_cookie_csrf_rotates_before_signing_and_keeps_download_snapshot() {
        struct CookieHttp(std::sync::Mutex<Vec<CloudHttpRequest>>);
        impl CloudHttpTransport for CookieHttp {
            fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
                let mut requests = self.0.lock().unwrap();
                requests.push(request);
                let first = requests.len() == 1;
                Ok(CloudHttpResponse{status:200,body:if first {json!({"code":0,"data":{"stoken":"fresh"}})}else{json!({"code":0,"data":[{"fid":"f1","download_url":"https://cdn.example/f"}]})}.to_string(),set_cookies:vec![if first{"csrf_token=issued-cookie; Path=/"}else{"csrf_token=after-signing; Path=/"}.into()]})
            }
        }
        let credentials = share_credentials();
        let http = CookieHttp(std::sync::Mutex::new(vec![]));
        let adapter = provider_share_adapter(CloudProvider::Uc, &credentials, &http).unwrap();
        let ShareResolution::Direct(direct) = adapter.resolve(&share_entry()).unwrap() else {
            panic!()
        };
        assert!(direct
            .headers
            .contains(&("x-csrf-token".into(), "issued-cookie".into())));
        assert_eq!(
            http.0.lock().unwrap()[1].headers["x-csrf-token"],
            "issued-cookie"
        );
        assert!(!direct
            .headers
            .iter()
            .any(|(_, value)| value.contains("after-signing")));
    }
    #[test]
    fn uc_share_http_challenge_without_json_preserves_authorization_error() {
        struct StatusHttp(u16);
        impl CloudHttpTransport for StatusHttp {
            fn send(&self,_:CloudHttpRequest)->Result<CloudHttpResponse,QuarkError>{Ok(CloudHttpResponse{status:self.0,body:"<html>secret</html>".into(),..Default::default()})}
        }
        let credentials=share_credentials();
        for status in [401,412] {
            let http=StatusHttp(status);let result=provider_share_adapter(CloudProvider::Uc,&credentials,&http).unwrap().resolve(&share_entry());
            assert!(if status==401 {matches!(result,Err(QuarkError::AuthorizationExpired))} else {matches!(result,Err(QuarkError::WebAuthorizationRequired))});
        }
    }
}
