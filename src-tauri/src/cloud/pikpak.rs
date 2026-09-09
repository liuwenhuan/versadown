use super::{
    http::response_json,
    types::{encoded, required},
    *,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
const AUTH: &str = "https://user.mypikpak.com";
const DRIVE: &str = "https://api-drive.mypikpak.com";
pub(crate) const PIKPAK_RECYCLE_BIN_ID: &str = "__all_downloader_pikpak_recycle_bin__";
const PIKPAK_RECYCLE_BIN_NAME: &str = "回收站";
const CAPTCHA_SALTS: &[&str] = &[
    "SOP04dGzk0TNO7t7t9ekDbAmx+eq0OI1ovEx",
    "nVBjhYiND4hZ2NCGyV5beamIr7k6ifAsAbl",
    "Ddjpt5B/Cit6EDq2a6cXgxY9lkEIOw4yC1GDF28KrA",
    "VVCogcmSNIVvgV6U+AochorydiSymi68YVNGiz",
    "u5ujk5sM62gpJOsB/1Gu/zsfgfZO",
    "dXYIiBOAHZgzSruaQ2Nhrqc2im",
    "z5jUTBSIpBN9g4qSJGlidNAutX6",
    "KJE2oveZ34du/g1tiimm",
];
/// Public Android 1.53.2 protocol signature; timestamp is supplied so test vectors are stable.
pub fn pikpak_captcha_sign(client_id: &str, device_id: &str, timestamp: &str) -> String {
    let mut value = format!("{client_id}1.53.2com.pikcloud.pikpak{device_id}{timestamp}");
    for salt in CAPTCHA_SALTS {
        value = format!("{:x}", md5::compute(format!("{value}{salt}")));
    }
    format!("1.{value}")
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PikPakSession {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub captcha_token: String,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub captcha_expires_at: u64,
    #[serde(default)]
    pub auth_method: CloudAuthMethod,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub struct PikPakClient<'a, T: CloudHttpTransport, S: CredentialStore> {
    pub(super) http: &'a T,
    store: &'a S,
    client_id: String,
    client_secret: String,
}
impl<'a, T: CloudHttpTransport, S: CredentialStore> PikPakClient<'a, T, S> {
    pub fn new(http: &'a T, store: &'a S) -> Self {
        Self {
            http,
            store,
            client_id: std::env::var("ALLDOWNLOADER_PIKPAK_CLIENT_ID")
                .unwrap_or_else(|_| "YNxT9w7GMdWvEOKa".into()),
            client_secret: std::env::var("ALLDOWNLOADER_PIKPAK_CLIENT_SECRET")
                .unwrap_or_else(|_| "dbw2OtmVEeuUvIptb1Coyg".into()),
        }
    }
    fn raw(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
        session: Option<&PikPakSession>,
    ) -> Result<Value, QuarkError> {
        let mut headers = BTreeMap::from([(
            "User-Agent".into(),
            "ANDROID-com.pikcloud.pikpak/1.53.2".into(),
        )]);
        if let Some(s) = session {
            if !s.access_token.is_empty() {
                headers.insert("Authorization".into(), format!("Bearer {}", s.access_token));
            }
            headers.insert("X-Device-Id".into(), s.device_id.clone());
            if let Some(id) = &s.client_id {
                headers.insert("X-Client-Id".into(), id.clone());
            }
            if !s.captcha_token.is_empty() {
                headers.insert("X-Captcha-Token".into(), s.captcha_token.clone());
            }
        }
        let response = self.http.send(CloudHttpRequest {
            method,
            url,
            headers,
            body: body.map(|b| b.to_string()),
        })?;
        if pikpak_regional_membership_restriction(&response) {
            return Err(QuarkError::PikpakRegionalMembershipRestricted);
        }
        response_json(&response)
    }
    pub fn save_session(&self, s: &PikPakSession) -> Result<(), QuarkError> {
        if s.access_token.is_empty() || s.refresh_token.is_empty() {
            return Err(QuarkError::AuthorizationExpired);
        }
        self.store.save(
            "pikpak",
            &serde_json::to_vec(s).map_err(|_| QuarkError::Storage)?,
        )
    }
    pub fn login(&self, username: &str, password: &str) -> Result<(), QuarkError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(QuarkError::InvalidInput("请输入账号和密码".into()));
        }
        let device_id = uuid::Uuid::new_v4().simple().to_string();
        let pending = PikPakSession {
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: 0,
            device_id: device_id.clone(),
            captcha_token: String::new(),
            client_id: Some(self.client_id.clone()),
            user_id: String::new(),
            captcha_expires_at: 0,
            auth_method: CloudAuthMethod::Password,
        };
        let cap=self.raw("POST",format!("{AUTH}/v1/shield/captcha/init"),Some(json!({"client_id":self.client_id,"device_id":device_id,"action":format!("POST:{AUTH}/v1/auth/signin"),"meta":{"username":username}})),Some(&pending))?;
        if cap["url"].as_str().is_some_and(|u| !u.is_empty()) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let captcha_token = required(&cap, "captcha_token")?;
        let v=self.raw("POST",format!("{AUTH}/v1/auth/signin"),Some(json!({"client_id":self.client_id,"client_secret":self.client_secret,"grant_type":"password","username":username,"password":password,"captcha_token":captcha_token})),Some(&pending))?;
        self.save_session(&PikPakSession {
            access_token: required(&v, "access_token")?,
            refresh_token: required(&v, "refresh_token")?,
            expires_at: now() + v["expires_in"].as_u64().unwrap_or(0),
            device_id,
            captcha_token,
            client_id: Some(self.client_id.clone()),
            user_id: v["sub"].as_str().unwrap_or("").into(),
            captcha_expires_at: now().saturating_add(cap["expires_in"].as_u64().unwrap_or(0)),
            auth_method: CloudAuthMethod::Password,
        })
    }
    fn stored(&self) -> Result<PikPakSession, QuarkError> {
        let bytes = self
            .store
            .load("pikpak")?
            .ok_or(QuarkError::AuthorizationExpired)?;
        let legacy = serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| QuarkError::Storage)?
            .get("auth_method")
            .is_none();
        let mut session: PikPakSession =
            serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if legacy
            && session.client_id.as_deref() == Some(self.client_id.as_str())
            && !session.device_id.is_empty()
        {
            session.auth_method = CloudAuthMethod::Password;
        }
        Ok(session)
    }
    /// Caller holds the shared session coordinator only around this credential operation.
    pub(crate) fn session_snapshot(&self) -> Result<PikPakSession, QuarkError> {
        let session = self.stored()?;
        if session.expires_at <= now() + 60 {
            self.refresh(session)
        } else {
            Ok(session)
        }
    }
    /// Upload initialization does no credential writes and can safely run without the provider lock.
    pub(crate) fn send_snapshot(
        &self,
        method: &'static str,
        url: &str,
        body: Value,
        session: &PikPakSession,
    ) -> Result<Value, QuarkError> {
        self.raw(method, url.into(), Some(body), Some(session))
    }
    /// Re-read current credentials under the shared lock, so logout or another refresh wins over a stale upload snapshot.
    pub(crate) fn recover_snapshot(
        &self,
        previous: &PikPakSession,
        captcha: bool,
        method: &str,
        url: &str,
    ) -> Result<PikPakSession, QuarkError> {
        let current = self.stored()?;
        if current.device_id != previous.device_id
            || (!current.user_id.is_empty()
                && !previous.user_id.is_empty()
                && current.user_id != previous.user_id)
        {
            return Err(QuarkError::AuthorizationExpired);
        }
        if captcha {
            if current.captcha_token != previous.captcha_token {
                return Ok(current);
            }
            self.refresh_action_captcha(method, url, current)
        } else {
            if current.access_token != previous.access_token {
                return self.session_snapshot();
            }
            self.refresh(current)
        }
    }
    fn refresh(&self, mut s: PikPakSession) -> Result<PikPakSession, QuarkError> {
        let client_id = s.client_id.as_deref().unwrap_or(&self.client_id);
        let mut payload = json!({"client_id":client_id,"grant_type":"refresh_token","refresh_token":s.refresh_token});
        // Browser OAuth refresh tokens belong to their public browser client, not the Android client.
        if client_id == self.client_id {
            payload["client_secret"] = json!(self.client_secret);
        }
        let v = self.raw(
            "POST",
            format!("{AUTH}/v1/auth/token"),
            Some(payload),
            Some(&s),
        )?;
        s.access_token = required(&v, "access_token")?;
        if let Some(user_id) = v["sub"].as_str() {
            s.user_id = user_id.into();
        }
        if let Ok(r) = required(&v, "refresh_token") {
            s.refresh_token = r
        }
        s.expires_at = now() + v["expires_in"].as_u64().unwrap_or(0);
        self.save_session(&s)?;
        Ok(s)
    }
    pub fn request(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let s = self.stored()?;
        let mut access_refreshed = s.expires_at <= now() + 60;
        let mut s = if access_refreshed {
            self.refresh(s)?
        } else {
            s
        };
        let mut captcha_refreshed = false;
        loop {
            match self.raw(method, url.clone(), body.clone(), Some(&s)) {
                Err(QuarkError::AuthorizationExpired) if !access_refreshed => {
                    s = self.refresh(s)?;
                    access_refreshed = true;
                }
                Err(QuarkError::CaptchaRequired) if !captcha_refreshed => {
                    s = self.refresh_action_captcha(method, &url, s)?;
                    captcha_refreshed = true;
                }
                Err(QuarkError::CaptchaRequired) => {
                    return Err(QuarkError::WebAuthorizationRequired)
                }
                r => return r,
            }
        }
    }
    fn refresh_action_captcha(
        &self,
        method: &str,
        url: &str,
        mut s: PikPakSession,
    ) -> Result<PikPakSession, QuarkError> {
        let action_url = url::Url::parse(url).map_err(|_| QuarkError::InvalidResponse)?;
        // Browser clients have a different public signature contract; use interactive authorization there.
        if action_url.host_str() != Some("api-drive.mypikpak.com")
            || s.device_id.is_empty()
            || s.client_id
                .as_deref()
                .is_some_and(|id| id != self.client_id)
        {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        let mut meta = json!({"client_version":"1.53.2","package_name":"com.pikcloud.pikpak","timestamp":timestamp,"captcha_sign":pikpak_captcha_sign(&self.client_id,&s.device_id,&timestamp)});
        if s.user_id.is_empty() {
            // This claim is only metadata for the provider, never used as a local authorization decision.
            if let Some(payload) = s
                .access_token
                .split('.')
                .nth(1)
                .and_then(|p| {
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(p)
                        .ok()
                })
                .and_then(|p| serde_json::from_slice::<Value>(&p).ok())
            {
                s.user_id = payload["sub"].as_str().unwrap_or("").into();
            }
        }
        if !s.user_id.is_empty() {
            meta["user_id"] = json!(s.user_id);
        }
        // PikPak scopes a GET for an individual drive file to the collection
        // captcha action. Sending the concrete id here yields an interactive
        // challenge even with a freshly completed browser authorization.
        let captcha_path = if matches!(method, "GET" | "PATCH")
            && action_url
                .path()
                .strip_prefix("/drive/v1/files/")
                .is_some_and(|id| !id.is_empty() && !id.contains(['/', ':']))
        {
            "/drive/v1/files/"
        } else {
            action_url.path()
        };
        let payload = json!({"action":format!("{method}:{captcha_path}"),"captcha_token":s.captcha_token,"client_id":self.client_id,"device_id":s.device_id,"meta":meta,"redirect_uri":"xlaccsdk01://xbase.cloud/callback?state=harbor"});
        let v = self
            .raw(
                "POST",
                format!(
                    "{AUTH}/v1/shield/captcha/init?client_id={}",
                    encoded(&self.client_id)
                ),
                Some(payload),
                Some(&s),
            )
            .map_err(|e| {
                if matches!(e, QuarkError::CaptchaRequired) {
                    QuarkError::WebAuthorizationRequired
                } else {
                    e
                }
            })?;
        if v["url"].as_str().is_some_and(|u| !u.is_empty()) {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        s.captcha_token =
            required(&v, "captcha_token").map_err(|_| QuarkError::WebAuthorizationRequired)?;
        s.captcha_expires_at = now().saturating_add(v["expires_in"].as_u64().unwrap_or(0));
        self.save_session(&s)?;
        Ok(s)
    }
    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let v = self.request("GET", format!("{AUTH}/v1/user/me"), None)?;
        let auth_method = self.stored()?.auth_method;
        let id = required(&v, "sub").or_else(|_| required(&v, "id"))?;
        let display_name = required(&v, "name")
            .or_else(|_| required(&v, "username"))
            .or_else(|_| required(&v, "email"))?;
        Ok(CloudAccount {
            provider: CloudProvider::Pikpak,
            id,
            display_name,
            auth_method,
        })
    }
    pub fn list(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let trashed = parent == PIKPAK_RECYCLE_BIN_ID;
        let p = if trashed { "" } else { root(parent) };
        let mut items = Vec::new();
        let mut token = String::new();
        let mut seen = HashSet::new();
        loop {
            let v = self.request(
                "GET",
                format!(
                    "{DRIVE}/drive/v1/files?parent_id={}&limit=500&filters={}&page_token={}",
                    encoded(p),
                    encoded(if trashed {
                        r#"{"trashed":{"eq":true}}"#
                    } else {
                        r#"{"trashed":{"eq":false}}"#
                    }),
                    encoded(&token)
                ),
                None,
            )?;
            for f in v["files"].as_array().ok_or(QuarkError::InvalidResponse)? {
                items.push(parse_pikpak_file(f, parent)?)
            }
            token = v["next_page_token"].as_str().unwrap_or("").into();
            if token.is_empty() {
                break;
            }
            if !seen.insert(token.clone()) || seen.len() > 10000 {
                return Err(QuarkError::InvalidResponse);
            }
        }
        if parent == "0" {
            items.push(CloudFile {
                id: PIKPAK_RECYCLE_BIN_ID.into(),
                name: PIKPAK_RECYCLE_BIN_NAME.into(),
                parent_id: "0".into(),
                kind: "folder".into(),
                size: None,
                modified_at: None,
            });
        }
        Ok(CloudDirectory {
            id: parent.into(),
            items,
        })
    }
    pub fn mkdir(&self, parent: &str, name: &str) -> Result<CloudFile, QuarkError> {
        reject_pikpak_recycle_bin_target(parent)?;
        let v = self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files"),
            Some(json!({"kind":"drive#folder","parent_id":root(parent),"name":name})),
        )?;
        parse_pikpak_file(v.get("file").unwrap_or(&v), parent)
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
        reject_pikpak_recycle_bin_target(parent)?;
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchMove"),
            Some(json!({"ids":ids,"to":{"parent_id":root(parent)}})),
        )?;
        Ok(())
    }
    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        reject_pikpak_recycle_bin_target(parent)?;
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchCopy"),
            Some(json!({"ids":ids,"to":{"parent_id":root(parent)}})),
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
    /// Permanently deletes items already present in PikPak's recycle bin.
    pub fn delete(&self, ids: &[String]) -> Result<(), QuarkError> {
        for id in ids {
            let file = self.request(
                "GET",
                format!("{DRIVE}/drive/v1/files/{}", encoded(id)),
                None,
            )?;
            if file["trashed"].as_bool() != Some(true) {
                return Err(QuarkError::InvalidInput(
                    "只能永久删除 PikPak 回收站中的项目".into(),
                ));
            }
        }
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchDelete"),
            Some(json!({"ids":ids})),
        )?;
        Ok(())
    }
    pub fn download(&self, id: &str) -> Result<(String, String), QuarkError> {
        let cached = self.request(
            "GET",
            format!("{DRIVE}/drive/v1/files/{}?usage=CACHE", encoded(id)),
            None,
        )?;
        if let Some(url) = cached["web_content_link"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
        {
            return Ok((url.into(), required(&cached, "name")?));
        }
        let fetched = self.request(
            "GET",
            format!("{DRIVE}/drive/v1/files/{}?usage=FETCH", encoded(id)),
            None,
        )?;
        let url = fetched["web_content_link"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
            .ok_or(QuarkError::InvalidResponse)?;
        Ok((url.into(), required(&fetched, "name")?))
    }
}

pub(crate) fn reject_pikpak_recycle_bin_target(parent: &str) -> Result<(), QuarkError> {
    if parent == PIKPAK_RECYCLE_BIN_ID {
        return Err(QuarkError::InvalidInput(
            "回收站不能作为创建、移动、复制或上传的目标目录".into(),
        ));
    }
    Ok(())
}

/// Maps only the provider's explicit Regional Premium → Global Premium
/// restriction wording from fixed JSON error fields. It never infers this
/// from HTTP status, numeric codes, generic region wording, CAPTCHA, or
/// authorization errors.
fn pikpak_regional_membership_restriction(response: &CloudHttpResponse) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(&response.body) else {
        return false;
    };
    let mut texts = Vec::new();
    for key in ["error", "error_description", "message"] {
        if let Some(text) = value[key].as_str() {
            texts.push(text);
        }
    }
    for key in ["details", "error_details"] {
        if let Some(details) = value[key].as_array() {
            for detail in details {
                for key in ["reason", "message"] {
                    if let Some(text) = detail[key].as_str() {
                        texts.push(text);
                    }
                }
            }
        }
    }
    let normalized = texts
        .iter()
        .map(|text| text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let is_captcha_or_authorization = [
        "captcha",
        "unauthenticated",
        "invalid_grant",
        "invalid_token",
        "验证码",
        "授权失效",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    let is_regional_membership =
        normalized.contains("regional premium") || normalized.contains("区域会员");
    let is_global_membership =
        normalized.contains("global premium") || normalized.contains("全球会员");
    let is_restricted_or_upgrade = ["restrict", "upgrade", "受限", "升级"]
        .iter()
        .any(|marker| normalized.contains(marker));
    !is_captcha_or_authorization
        && is_regional_membership
        && is_global_membership
        && is_restricted_or_upgrade
}

fn root(p: &str) -> &str {
    if p == "0" {
        ""
    } else {
        p
    }
}
pub fn parse_pikpak_file(v: &Value, parent: &str) -> Result<CloudFile, QuarkError> {
    Ok(CloudFile {
        id: required(v, "id")?,
        name: required(v, "name")?,
        parent_id: parent.into(),
        kind: if v["kind"] == "drive#folder" {
            "folder"
        } else {
            "file"
        }
        .into(),
        size: v["size"]
            .as_u64()
            .or_else(|| v["size"].as_str().and_then(|s| s.parse().ok())),
        modified_at: v["modified_time"].as_str().map(str::to_owned),
    })
}
