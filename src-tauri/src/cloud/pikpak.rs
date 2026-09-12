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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::share::{test_support::*, *};
    fn credentials() -> MemoryCredentialStore {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "pikpak",
                &serde_json::to_vec(&PikPakSession {
                    access_token: "current-user".into(),
                    refresh_token: "refresh".into(),
                    expires_at: u64::MAX,
                    device_id: "device".into(),
                    captcha_token: "personal-captcha".into(),
                    client_id: Some("YUMx5nI8ZU8Ap8pm".into()),
                    user_id: "u1".into(),
                    captcha_expires_at: u64::MAX,
                    auth_method: CloudAuthMethod::Web,
                })
                .unwrap(),
            )
            .unwrap();
        store
    }
    fn cap() -> Value {
        json!({"captcha_token":"share-captcha","expires_in":300,"url":""})
    }
    fn pass() -> Value {
        json!({"share_status":"OK","pass_code_token":"pass-secret"})
    }
    fn listing() -> Value {
        json!({"share_status":"OK","files":[{"id":"f1","kind":"drive#file","name":"verify.mp4","size":"7"}],"next_page_token":""})
    }
    #[test]
    fn pikpak_share_web_context_and_current_identity_reach_direct_download() {
        let store = credentials();
        let http = Http::new(vec![
            cap(),
            pass(),
            listing(),
            cap(),
            pass(),
            json!({"share_status":"OK","file_info":{"id":"f1","kind":"drive#file","web_content_link":"http://cdn.example/file"}}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Pikpak, &store, &http).unwrap();
        let ShareInspection::Plan(plan) =
            adapter.inspect(&candidate(CloudProvider::Pikpak)).unwrap()
        else {
            panic!()
        };
        assert!(plan.provider_context.to_string().contains("pass-secret"));
        let ShareResolution::Direct(direct) = adapter.resolve(&entry(plan)).unwrap() else {
            panic!()
        };
        assert_eq!(direct.url, "http://cdn.example/file");
        assert!(direct
            .headers
            .contains(&("Authorization".into(), "Bearer current-user".into())));
        assert!(direct
            .headers
            .contains(&("X-Captcha-Token".into(), "share-captcha".into())));
        let requests = http.requests.lock().unwrap();
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["client_id"], "YUMx5nI8ZU8Ap8pm");
        assert_eq!(body["action"], "GET:/drive/v1/share:batch_file_info");
        assert_eq!(body["meta"]["package_name"], "mypikpak.com");
        assert_eq!(body["meta"]["client_version"], "2.0.0");
        assert_eq!(body["captcha_token"], "");
        assert!(requests[5].url.contains("/drive/v1/share/file_info?"));
        assert!(requests[5].url.contains("pass_code_token=pass-secret"));
    }
    #[test]
    fn pikpak_share_uses_source_media_link_when_web_content_link_is_omitted() {
        let store = credentials();
        let http = Http::new(vec![
            cap(),
            pass(),
            listing(),
            cap(),
            pass(),
            json!({
                "share_status":"OK",
                "file_info":{
                    "id":"f1",
                    "kind":"drive#file",
                    "medias":[{"link":{"url":"https://cdn.example/source-file"}}]
                }
            }),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Pikpak, &store, &http).unwrap();
        let ShareInspection::Plan(plan) =
            adapter.inspect(&candidate(CloudProvider::Pikpak)).unwrap()
        else {
            panic!()
        };

        let ShareResolution::Direct(direct) = adapter.resolve(&entry(plan)).unwrap() else {
            panic!()
        };
        assert_eq!(direct.url, "https://cdn.example/source-file");
    }
    #[test]
    fn pikpak_share_preserves_not_found_and_deleted_failures_for_the_parse_ui() {
        for (share_status, expected) in [
            ("NOT_FOUND", "PikPak 分享的文件不存在"),
            ("DELETED", "PikPak 分享已删除"),
        ] {
            assert!(matches!(
                pikpak_share_status(&json!({"share_status":share_status})),
                Err(QuarkError::Provider(message)) if message == expected
            ));
        }
    }
    #[test]
    fn pikpak_share_pages_recursively_and_never_retries_challenges() {
        let store = credentials();
        let http = Http::new(vec![
            cap(),
            pass(),
            json!({"share_status":"OK","files":[{"id":"d","kind":"drive#folder","name":"dir"}],"next_page_token":"p2"}),
            json!({"share_status":"OK","files":[],"next_page_token":""}),
            listing(),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Pikpak, &store, &http).unwrap();
        let ShareInspection::Plan(plan) =
            adapter.inspect(&candidate(CloudProvider::Pikpak)).unwrap()
        else {
            panic!()
        };
        assert_eq!(plan.entries[0].relative_directory, vec!["dir"]);
        assert!(http.requests.lock().unwrap()[3]
            .url
            .contains("page_token=p2"));
        for reply in [
            json!({"error_code":9,"error":"captcha_required"}),
            json!({"error_code":4031}),
            json!({"error":"forbidden"}),
            json!({"share_status":"EXPIRED"}),
            json!({"share_status":"PASS_CODE_ERROR"}),
        ] {
            let http = Http::new(vec![cap(), reply]);
            assert!(provider_share_adapter(CloudProvider::Pikpak, &store, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Pikpak))
                .is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 2);
        }
        let http = Http::new(vec![
            cap(),
            json!({"error_code":10,"error_description":"Regional Premium restricted, upgrade to Global Premium"}),
        ]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Pikpak, &store, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Pikpak)),
            Err(QuarkError::PikpakRegionalMembershipRestricted)
        ));
        let http = Http::new(vec![cap(), json!({"share_status":"PASS_CODE_EMPTY"})]);
        let mut input = candidate(CloudProvider::Pikpak);
        input.access_code = None;
        assert!(matches!(
            provider_share_adapter(CloudProvider::Pikpak, &store, &http)
                .unwrap()
                .inspect(&input),
            Ok(ShareInspection::AccessCodeRequired)
        ));
    }

    #[test]
    fn pikpak_share_rejects_malformed_error_fields_even_with_a_url() {
        let store = credentials();
        for error in [
            json!({"error":{"code":"forbidden"}}),
            json!({"status":"403"}),
        ] {
            let mut response = json!({"share_status":"OK","file_info":{"id":"f1","kind":"drive#file","web_content_link":"https://cdn.example/f"}});
            response
                .as_object_mut()
                .unwrap()
                .extend(error.as_object().unwrap().clone());
            let http = Http::new(vec![cap(), pass(), listing(), cap(), pass(), response]);
            let adapter = provider_share_adapter(CloudProvider::Pikpak, &store, &http).unwrap();
            let ShareInspection::Plan(plan) =
                adapter.inspect(&candidate(CloudProvider::Pikpak)).unwrap()
            else {
                panic!()
            };
            assert!(adapter.resolve(&entry(plan)).is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 6);
        }
    }

    #[test]
    fn pikpak_share_missing_original_url_and_web_challenge_never_transfer() {
        let store = credentials();
        let original = store.load("pikpak").unwrap().unwrap();
        for file in [
            json!({"id":"f1","kind":"drive#file"}),
            json!({"id":"other","kind":"drive#file","web_content_link":"https://cdn.example/f"}),
            json!({"id":"f1","kind":"drive#file","web_content_link":"https://mypikpak.com/s/other"}),
        ] {
            let http = Http::new(vec![
                cap(),
                pass(),
                listing(),
                cap(),
                pass(),
                json!({"share_status":"OK","file_info":file}),
            ]);
            let adapter = provider_share_adapter(CloudProvider::Pikpak, &store, &http).unwrap();
            let ShareInspection::Plan(plan) =
                adapter.inspect(&candidate(CloudProvider::Pikpak)).unwrap()
            else {
                panic!()
            };
            assert!(adapter.resolve(&entry(plan)).is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 6);
            assert_eq!(store.load("pikpak").unwrap().unwrap(), original);
        }
        let http = Http::new(vec![
            json!({"captcha_token":"token","url":"https://verify.example/challenge"}),
        ]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Pikpak, &store, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Pikpak)),
            Err(QuarkError::WebAuthorizationRequired)
        ));
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        let empty = MemoryCredentialStore::default();
        let http = Http::new(vec![]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Pikpak, &empty, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Pikpak)),
            Err(QuarkError::NotAuthorized)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn pikpak_restore_only_mutates_entries_confirmed_as_trashed() {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "pikpak",
                br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device"}"#,
            )
            .unwrap();
        let http = Http::new(vec![json!({"id":"trashed-id","trashed":true}), json!({})]);

        PikPakClient::new(&http, &store)
            .restore_recycle_entries(&["trashed-id".into()])
            .unwrap();

        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert!(requests[0].url.ends_with("/drive/v1/files/trashed-id"));
        assert_eq!(requests[1].method, "POST");
        assert!(requests[1].url.ends_with("/drive/v1/files:batchUntrash"));
        assert_eq!(
            serde_json::from_str::<Value>(requests[1].body.as_deref().unwrap()).unwrap()["ids"],
            json!(["trashed-id"])
        );
    }

    #[test]
    fn pikpak_restore_rejects_live_entries_without_a_mutation() {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "pikpak",
                br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device"}"#,
            )
            .unwrap();
        let http = Http::new(vec![json!({"id":"live-id","trashed":false})]);

        let error = PikPakClient::new(&http, &store)
            .restore_recycle_entries(&["live-id".into()])
            .unwrap_err();

        assert!(matches!(error, QuarkError::InvalidInput(_)));
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert!(!requests
            .iter()
            .any(|request| request.url.contains("batchUntrash")));
    }
}
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
        let device_id = self
            .store
            .load("pikpak")?
            .map(|bytes| {
                serde_json::from_slice::<PikPakSession>(&bytes).map_err(|_| QuarkError::Storage)
            })
            .transpose()?
            .map(|session| session.device_id)
            .filter(|device| !device.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
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
    pub(crate) fn refresh_session(&self) -> Result<(), QuarkError> {
        self.refresh(self.stored()?).map(|_| ())
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
        if !self.store.automatic_session_recovery() {
            return self.raw(method, url, body, Some(&s));
        }
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
        reject_pikpak_recycle_bin_source(id)?;
        self.request(
            "PATCH",
            format!("{DRIVE}/drive/v1/files/{}", encoded(id)),
            Some(json!({"name":name})),
        )?;
        Ok(())
    }
    pub fn move_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        reject_pikpak_recycle_bin_sources(ids)?;
        reject_pikpak_recycle_bin_target(parent)?;
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchMove"),
            Some(json!({"ids":ids,"to":{"parent_id":root(parent)}})),
        )?;
        Ok(())
    }
    pub fn copy_files(&self, ids: &[String], parent: &str) -> Result<(), QuarkError> {
        reject_pikpak_recycle_bin_sources(ids)?;
        reject_pikpak_recycle_bin_target(parent)?;
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchCopy"),
            Some(json!({"ids":ids,"to":{"parent_id":root(parent)}})),
        )?;
        Ok(())
    }
    pub fn trash(&self, ids: &[String]) -> Result<(), QuarkError> {
        reject_pikpak_recycle_bin_sources(ids)?;
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchTrash"),
            Some(json!({"ids":ids})),
        )?;
        Ok(())
    }
    /// Permanently deletes items already present in PikPak's recycle bin.
    pub fn delete(&self, ids: &[String]) -> Result<(), QuarkError> {
        reject_pikpak_recycle_bin_sources(ids)?;
        for id in ids {
            // PikPak documents no conditional batch-delete request. Validate the
            // current state immediately before sending the permanent-delete call.
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
    /// Restores items only after the provider confirms that they remain trashed.
    pub fn restore_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        reject_pikpak_recycle_bin_sources(ids)?;
        for id in ids {
            let file = self.request(
                "GET",
                format!("{DRIVE}/drive/v1/files/{}", encoded(id)),
                None,
            )?;
            if file["trashed"].as_bool() != Some(true) {
                return Err(QuarkError::InvalidInput(
                    "只能恢复 PikPak 回收站中的项目".into(),
                ));
            }
        }
        self.request(
            "POST",
            format!("{DRIVE}/drive/v1/files:batchUntrash"),
            Some(json!({"ids":ids})),
        )?;
        Ok(())
    }
    pub fn download(&self, id: &str) -> Result<(String, String), QuarkError> {
        reject_pikpak_recycle_bin_source(id)?;
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

fn reject_pikpak_recycle_bin_source(id: &str) -> Result<(), QuarkError> {
    if id == PIKPAK_RECYCLE_BIN_ID {
        return Err(QuarkError::InvalidInput(
            "回收站仅可打开，不能作为文件操作对象".into(),
        ));
    }
    Ok(())
}

fn reject_pikpak_recycle_bin_sources(ids: &[String]) -> Result<(), QuarkError> {
    if ids.iter().any(|id| id == PIKPAK_RECYCLE_BIN_ID) {
        return Err(QuarkError::InvalidInput(
            "回收站仅可打开，不能作为文件操作对象".into(),
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
    let error = value["error"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let error_code = value["error_code"]
        .as_i64()
        .or_else(|| value["code"].as_i64());
    // Preserve the provider's structured risk/CAPTCHA handling in response_json.
    // Its text can include plan wording, but it is not proof of a membership-only
    // restriction and must continue to trigger web authorization.
    if matches!(error_code, Some(9 | 4031)) || error == "risklimited" {
        return false;
    }
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

use super::share::{
    CloudShareAdapter, CloudShareEntry, CloudSharePlan, CloudShareResolvedEntry, ShareInspection,
    ShareResolution,
};
const SHARE_DRIVE: &str = "https://api-drive.mypikpak.net";
const SHARE_AUTH: &str = "https://user.mypikpak.net";
const SHARE_WEB_CLIENT: &str = "YUMx5nI8ZU8Ap8pm";
const SHARE_WEB_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";
pub struct PikPakShareAdapter<'a> {
    pub credentials: &'a dyn CredentialStore,
    pub http: &'a dyn CloudHttpTransport,
}
struct PikPakShareSession<'a> {
    http: &'a dyn CloudHttpTransport,
    headers: BTreeMap<String, String>,
}
impl PikPakShareAdapter<'_> {
    fn session(&self) -> Result<PikPakShareSession<'_>, QuarkError> {
        let bytes = self
            .credentials
            .load("pikpak")?
            .ok_or(QuarkError::NotAuthorized)?;
        let session: PikPakSession =
            serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if session.access_token.is_empty() {
            return Err(QuarkError::NotAuthorized);
        }
        if session.expires_at <= now() {
            return Err(QuarkError::AuthorizationExpired);
        }
        if session.device_id.is_empty() {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let mut client = PikPakShareSession {
            http: self.http,
            headers: BTreeMap::from([
                (
                    "Authorization".into(),
                    format!("Bearer {}", session.access_token),
                ),
                ("X-Client-Id".into(), SHARE_WEB_CLIENT.into()),
                ("X-Device-Id".into(), session.device_id.clone()),
                ("User-Agent".into(), SHARE_WEB_UA.into()),
                ("Referer".into(), "https://mypikpak.com/".into()),
            ]),
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        // A separate web-share challenge is initialized once; do not reuse or overwrite
        // the personal-drive CAPTCHA or enter its automatic renewal loop.
        let response = client.request("POST",format!("{SHARE_AUTH}/v1/shield/captcha/init"),Some(json!({
            "client_id":SHARE_WEB_CLIENT,"device_id":session.device_id,"captcha_token":"",
            "action":"GET:/drive/v1/share:batch_file_info",
            "meta":{"client_version":"2.0.0","package_name":"mypikpak.com","user_id":session.user_id,
                "timestamp":timestamp,"captcha_sign":pikpak_share_captcha_sign(&session.device_id,&timestamp)}
        })))?;
        if response
            .get("url")
            .is_some_and(|value| value.as_str() != Some(""))
        {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        client.headers.insert(
            "X-Captcha-Token".into(),
            required(&response, "captcha_token")?,
        );
        Ok(client)
    }
}
fn pikpak_share_captcha_sign(device: &str, timestamp: &str) -> String {
    let mut value = format!("{SHARE_WEB_CLIENT}2.0.0mypikpak.com{device}{timestamp}");
    for salt in [
        "C9qPpZLN8ucRTaTiUMWYS9cQvWOE",
        "+r6CQVxjzJV6LCV",
        "F",
        "pFJRC",
        "9WXYIDGrwTCz2OiVlgZa90qpECPD6olt",
        "/750aCr4lm/Sly/c",
        "RB+DT/gZCrbV",
        "",
        "CyLsf7hdkIRxRm215hl",
        "7xHvLi2tOYP0Y92b",
        "ZGTXXxu8E/MIWaEDB+Sm/",
        "1UI3",
        "E7fP5Pfijd+7K+t6Tg/NhuLq0eEUVChpJSkrKxpO",
        "ihtqpG6FMt65+Xk+tWUH2",
        "NhXXU9rg4XXdzo7u5o",
    ] {
        value = format!("{:x}", md5::compute(format!("{value}{salt}")));
    }
    format!("1.{value}")
}
impl PikPakShareSession<'_> {
    fn request(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method,
            url,
            headers: self.headers.clone(),
            body: body.map(|v| v.to_string()),
        })?;
        let value: Value =
            serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
        if !value.is_object() {
            return Err(QuarkError::InvalidResponse);
        }
        // Numeric fields must not hide one another or default malformed errors to success.
        for key in ["code", "error_code", "status"] {
            if let Some(code) = value.get(key) {
                if code.as_i64().is_none() {
                    return Err(QuarkError::InvalidResponse);
                }
            }
        }
        if value
            .get("error")
            .is_some_and(|value| value.as_str().is_none())
        {
            return Err(QuarkError::InvalidResponse);
        }
        if let (Some(a), Some(b)) = (value.get("code"), value.get("error_code")) {
            if a != b {
                return Err(QuarkError::InvalidResponse);
            }
        }
        if pikpak_regional_membership_restriction(&response) {
            return Err(QuarkError::PikpakRegionalMembershipRestricted);
        }
        response_json(&response).map_err(|error| match error {
            QuarkError::Provider(_) => {
                QuarkError::Provider("PikPak 分享请求失败，请检查分享有效期及权限".into())
            }
            error => error,
        })
    }
    fn pass(&self, id: &str, code: Option<&str>) -> Result<Value, QuarkError> {
        self.request("GET",format!("{SHARE_DRIVE}/drive/v1/share?share_id={}&pass_code={}&thumbnail_size=SIZE_LARGE&limit=100",encoded(id),encoded(code.unwrap_or(""))),None)
    }
}
fn pikpak_share_status(value: &Value) -> Result<(), QuarkError> {
    match value["share_status"].as_str() {
        Some("OK") => Ok(()),
        Some("PASS_CODE_EMPTY" | "PASS_CODE_ERROR") => Err(QuarkError::InvalidInput(
            "PikPak 分享访问码错误或缺失".into(),
        )),
        Some("NOT_FOUND") => Err(QuarkError::Provider("PikPak 分享的文件不存在".into())),
        Some("DELETED") => Err(QuarkError::Provider("PikPak 分享已删除".into())),
        Some(_) => Err(QuarkError::Provider("PikPak 分享已失效或无权访问".into())),
        None => Err(QuarkError::InvalidResponse),
    }
}
impl CloudShareAdapter for PikPakShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        let id = super::share::share_id(candidate, CloudProvider::Pikpak)?;
        let session = self.session()?;
        let pass = session.pass(&id, candidate.access_code.as_deref())?;
        if candidate.access_code.is_none()
            && matches!(
                pass["share_status"].as_str(),
                Some("PASS_CODE_EMPTY" | "PASS_CODE_ERROR")
            )
        {
            return Ok(ShareInspection::AccessCodeRequired);
        }
        pikpak_share_status(&pass)?;
        let token = pass
            .get("pass_code_token")
            .map(|value| value.as_str().ok_or(QuarkError::InvalidResponse))
            .transpose()?
            .unwrap_or("");
        if candidate.access_code.is_some() && token.is_empty() {
            return Err(QuarkError::InvalidResponse);
        }
        let mut queue = std::collections::VecDeque::from([(String::new(), vec![])]);
        let mut seen = HashSet::new();
        let mut entries = vec![];
        let mut requests = 0usize;
        while let Some((parent, directory)) = queue.pop_front() {
            let mut page = String::new();
            let mut pages = HashSet::new();
            loop {
                requests += 1;
                if requests > 10_000 || directory.len() > 128 {
                    return Err(QuarkError::InvalidResponse);
                }
                let value = session.request("GET",format!("{SHARE_DRIVE}/drive/v1/share/detail?share_id={}&parent_id={}&pass_code_token={}&page_token={}&limit=100&thumbnail_size=SIZE_LARGE&with_audit=true",encoded(&id),encoded(&parent),encoded(token),encoded(&page)),None)?;
                pikpak_share_status(&value)?;
                for raw in value["files"]
                    .as_array()
                    .ok_or(QuarkError::InvalidResponse)?
                {
                    let file = parse_pikpak_file(raw, &parent)?;
                    if !seen.insert(file.id.clone()) {
                        return Err(QuarkError::InvalidResponse);
                    }
                    match raw["kind"].as_str() {
                        Some("drive#folder") => {
                            let mut path = directory.clone();
                            path.push(file.name);
                            queue.push_back((file.id, path));
                        }
                        Some("drive#file") => entries.push(CloudShareEntry::new(
                            &id,
                            file.id,
                            file.name,
                            directory.clone(),
                            file.size,
                        )),
                        _ => return Err(QuarkError::InvalidResponse),
                    }
                }
                page = value
                    .get("next_page_token")
                    .map(|v| v.as_str().ok_or(QuarkError::InvalidResponse))
                    .transpose()?
                    .unwrap_or("")
                    .into();
                if page.is_empty() {
                    break;
                }
                if !pages.insert(page.clone()) {
                    return Err(QuarkError::InvalidResponse);
                }
            }
        }
        Ok(ShareInspection::Plan(CloudSharePlan::new(
            CloudProvider::Pikpak,
            id,
            candidate.access_code.clone(),
            entries,
            json!({"pass_code_token":token}),
        )))
    }
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        resolve_pikpak_share(self.credentials, self.http, entry)
    }
}
pub fn resolve_pikpak_share(
    credentials: &dyn CredentialStore,
    http: &dyn CloudHttpTransport,
    entry: &CloudShareResolvedEntry,
) -> Result<ShareResolution, QuarkError> {
    if entry.provider != CloudProvider::Pikpak || entry.plan.provider != CloudProvider::Pikpak {
        return Err(QuarkError::InvalidResponse);
    }
    let adapter = PikPakShareAdapter { credentials, http };
    let session = adapter.session()?;
    let pass = session.pass(&entry.plan.canonical_share_id, entry.access_code())?;
    pikpak_share_status(&pass)?;
    let token = pass
        .get("pass_code_token")
        .map(|value| value.as_str().ok_or(QuarkError::InvalidResponse))
        .transpose()?
        .unwrap_or("");
    if entry.access_code().is_some() && token.is_empty() {
        return Err(QuarkError::InvalidResponse);
    }
    let value = session.request(
        "GET",
        format!(
            "{SHARE_DRIVE}/drive/v1/share/file_info?share_id={}&file_id={}&pass_code_token={}",
            encoded(&entry.plan.canonical_share_id),
            encoded(&entry.entry.entry_id),
            encoded(token)
        ),
        None,
    )?;
    pikpak_share_status(&value)?;
    let file = &value["file_info"];
    if file["id"].as_str() != Some(entry.entry.entry_id.as_str()) || file["kind"] != "drive#file" {
        return Err(QuarkError::InvalidResponse);
    }
    // Newer share responses can omit `web_content_link` while retaining the
    // provider-issued source-media link.  Use only that first-party direct
    // link; do not save the shared item into the user's personal drive.
    let url = pikpak_share_content_link(file)?;
    super::share::validate_direct_url(&url)?;
    Ok(ShareResolution::Direct(
        super::commands::ResolvedCloudDownload {
            url,
            name: entry.entry.name.clone(),
            headers: session.headers.into_iter().collect(),
        },
    ))
}
fn pikpak_share_content_link(file: &Value) -> Result<String, QuarkError> {
    match file.get("web_content_link") {
        Some(Value::String(url)) if !url.is_empty() => return Ok(url.clone()),
        Some(Value::Null | Value::String(_)) | None => {}
        Some(_) => return Err(QuarkError::InvalidResponse),
    }

    let media = file
        .get("medias")
        .and_then(Value::as_array)
        .and_then(|medias| medias.first())
        .ok_or(QuarkError::InvalidResponse)?;
    let link = media
        .get("link")
        .and_then(Value::as_object)
        .ok_or(QuarkError::InvalidResponse)?;
    match link.get("url") {
        Some(Value::String(url)) if !url.is_empty() => Ok(url.clone()),
        _ => Err(QuarkError::InvalidResponse),
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
