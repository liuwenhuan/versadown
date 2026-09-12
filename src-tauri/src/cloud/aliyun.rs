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
pub(crate) const ALIYUN_RECYCLE_BIN_ID: &str = "__all_downloader_aliyun_recycle_bin__";
const ALIYUN_RECYCLE_BIN_NAME: &str = "回收站";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{
        share::{test_support::*, *},
        MemoryCredentialStore,
    };
    fn listing() -> Value {
        json!({"items":[{"file_id":"f1","name":"verify.mp4","type":"file","size":7}],"next_marker":""})
    }
    #[test]
    fn aliyun_share_url_uses_share_token_not_personal_bearer() {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save("aliyun", br#"{"access_token":"personal-secret"}"#)
            .unwrap();
        let http = Http::new(vec![
            json!({"share_token":"share-secret"}),
            listing(),
            json!({"share_token":"fresh-secret"}),
            json!({"url":"https://cdn.example/file"}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Aliyun, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) =
            adapter.inspect(&candidate(CloudProvider::Aliyun)).unwrap()
        else {
            panic!()
        };
        let ShareResolution::Direct(direct) = adapter.resolve(&entry(plan)).unwrap() else {
            panic!()
        };
        assert!(direct
            .headers
            .contains(&("x-share-token".into(), "fresh-secret".into())));
        assert!(!direct
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("authorization")));
        let requests = http.requests.lock().unwrap();
        assert!(requests.iter().all(|r| !r
            .headers
            .keys()
            .any(|n| n.eq_ignore_ascii_case("authorization"))));
        assert!(requests[3].url.ends_with("/v2/file/get_download_url"));
        let download_body: Value =
            serde_json::from_str(requests[3].body.as_ref().unwrap()).unwrap();
        assert_eq!(download_body["share_id"], "share123");
        assert_eq!(download_body["file_id"], "f1");
        assert!(download_body.get("drive_id").is_none());
        assert_eq!(
            serde_json::from_str::<Value>(requests[0].body.as_ref().unwrap()).unwrap()["share_pwd"],
            "a123"
        );
    }
    #[test]
    fn aliyun_share_recurses_and_pages_and_rejects_cycles() {
        let credentials = MemoryCredentialStore::default();
        let http = Http::new(vec![
            json!({"share_token":"s"}),
            json!({"items":[{"file_id":"d","name":"dir","type":"folder"}],"next_marker":"page2"}),
            json!({"items":[],"next_marker":""}),
            listing(),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Aliyun, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) =
            adapter.inspect(&candidate(CloudProvider::Aliyun)).unwrap()
        else {
            panic!()
        };
        assert_eq!(plan.entries[0].relative_directory, vec!["dir"]);
        let requests = http.requests.lock().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(requests[2].body.as_ref().unwrap()).unwrap()["marker"],
            "page2"
        );
        drop(requests);
        let http = Http::new(vec![
            json!({"share_token":"s"}),
            json!({"items":[],"next_marker":"same"}),
            json!({"items":[],"next_marker":"same"}),
        ]);
        assert!(
            provider_share_adapter(CloudProvider::Aliyun, &credentials, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Aliyun))
                .is_err()
        );
    }
    #[test]
    fn aliyun_share_access_code_and_expiry_fail_closed() {
        let credentials = MemoryCredentialStore::default();
        let mut input = candidate(CloudProvider::Aliyun);
        input.access_code = None;
        let http = Http::new(vec![
            json!({"code":"ShareLink.PasswordRequired","message":"secret"}),
        ]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Aliyun, &credentials, &http)
                .unwrap()
                .inspect(&input),
            Ok(ShareInspection::AccessCodeRequired)
        ));
        for code in [
            "ShareLink.PasswordMismatch",
            "ShareLink.Cancelled",
            "ShareLink.Expired",
            "Forbidden",
            "TooManyRequests",
            "ShareLinkTokenInvalid",
        ] {
            let http = Http::new(vec![json!({"code":code,"message":"secret"})]);
            let result = provider_share_adapter(CloudProvider::Aliyun, &credentials, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Aliyun));
            assert!(result.is_err(), "accepted {code}");
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
        for value in [
            json!({}),
            json!({"download_url":""}),
            json!({"download_url":"https://www.alipan.com/s/other"}),
            json!({"code":"Forbidden","download_url":"https://cdn.example/f"}),
        ] {
            let http = Http::new(vec![
                json!({"share_token":"s"}),
                listing(),
                json!({"share_token":"s"}),
                value,
            ]);
            let adapter =
                provider_share_adapter(CloudProvider::Aliyun, &credentials, &http).unwrap();
            let ShareInspection::Plan(plan) =
                adapter.inspect(&candidate(CloudProvider::Aliyun)).unwrap()
            else {
                panic!()
            };
            assert!(adapter.resolve(&entry(plan)).is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 4);
        }
    }

    #[test]
    fn aliyun_share_invalid_resource_password_requests_an_access_code() {
        let credentials = MemoryCredentialStore::default();
        let missing_code = Http::new(vec![json!({"code":"InvalidResource.SharePwd"})]);
        let mut without_code = candidate(CloudProvider::Aliyun);
        without_code.access_code = None;
        assert!(matches!(
            provider_share_adapter(CloudProvider::Aliyun, &credentials, &missing_code)
                .unwrap()
                .inspect(&without_code),
            Ok(ShareInspection::AccessCodeRequired)
        ));

        let supplied_code = Http::new(vec![json!({"code":"InvalidResource.SharePwd"})]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Aliyun, &credentials, &supplied_code)
                .unwrap()
                .inspect(&candidate(CloudProvider::Aliyun)),
            Err(QuarkError::InvalidInput(_))
        ));
    }

    #[test]
    fn aliyun_share_rejects_malformed_error_fields_even_with_a_url() {
        let credentials = MemoryCredentialStore::default();
        for response in [
            json!({"error_code":"Forbidden","url":"https://cdn.example/f"}),
            json!({"error":{"code":"Forbidden"},"url":"https://cdn.example/f"}),
            json!({"status":"403","url":"https://cdn.example/f"}),
        ] {
            let http = Http::new(vec![
                json!({"share_token":"s"}),
                listing(),
                json!({"share_token":"s"}),
                response,
            ]);
            let adapter =
                provider_share_adapter(CloudProvider::Aliyun, &credentials, &http).unwrap();
            let ShareInspection::Plan(plan) =
                adapter.inspect(&candidate(CloudProvider::Aliyun)).unwrap()
            else {
                panic!()
            };
            assert!(adapter.resolve(&entry(plan)).is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 4);
        }
    }

    #[test]
    fn aliyun_share_refreshes_token_once_after_block_exception_while_listing() {
        struct StatusHttp {
            replies: std::sync::Mutex<std::collections::VecDeque<CloudHttpResponse>>,
            requests: std::sync::Mutex<Vec<CloudHttpRequest>>,
        }
        impl CloudHttpTransport for StatusHttp {
            fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
                self.requests.lock().unwrap().push(request);
                self.replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or(QuarkError::InvalidResponse)
            }
        }

        let credentials = MemoryCredentialStore::default();
        let http = StatusHttp {
            replies: std::sync::Mutex::new(
                vec![
                    CloudHttpResponse {
                        status: 200,
                        body: json!({"share_token":"first-token"}).to_string(),
                        set_cookies: vec![],
                    },
                    CloudHttpResponse {
                        status: 200,
                        body: json!({"items":[{"file_id":"dir","name":"folder","type":"folder"}],"next_marker":""}).to_string(),
                        set_cookies: vec![],
                    },
                    CloudHttpResponse {
                        status: 429,
                        body: json!({"code":"BlockException"}).to_string(),
                        set_cookies: vec![],
                    },
                    CloudHttpResponse {
                        status: 200,
                        body: json!({"share_token":"second-token"}).to_string(),
                        set_cookies: vec![],
                    },
                    CloudHttpResponse {
                        status: 200,
                        body: listing().to_string(),
                        set_cookies: vec![],
                    },
                ]
                .into(),
            ),
            requests: std::sync::Mutex::new(vec![]),
        };

        let ShareInspection::Plan(plan) =
            provider_share_adapter(CloudProvider::Aliyun, &credentials, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Aliyun))
                .unwrap()
        else {
            panic!();
        };
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].relative_directory, vec!["folder"]);
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[3].url.ends_with("/v2/share_link/get_share_token"));
        assert_eq!(
            serde_json::from_str::<Value>(requests[3].body.as_ref().unwrap()).unwrap()["share_pwd"],
            "a123"
        );
    }
}

/// The minimum durable state obtained after the user finishes the official web login.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AliyunSession {
    pub refresh_token: String,
    pub access_token: String,
    pub drive_id: String,
    /// The current bearer was captured immediately after the official web login.
    #[serde(default)]
    pub browser_access_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
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
        if session.refresh_token.is_empty()
            || session.access_token.is_empty()
            || session.drive_id.is_empty()
        {
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
            browser_access_ready: false,
            expires_at: value["expires_in"]
                .as_u64()
                .filter(|ttl| *ttl > 0)
                .map(|ttl| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_add(ttl)
                }),
        })
    }

    fn refresh_and_persist(&self, previous: &AliyunSession) -> Result<AliyunSession, QuarkError> {
        let refreshed = self.refresh(previous)?;
        self.save(&refreshed)?;
        Ok(refreshed)
    }
    pub(crate) fn refresh_session(&self) -> Result<(), QuarkError> {
        self.refresh_and_persist(&self.stored()?).map(|_| ())
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
                if parent == "root" {
                    items.push(CloudFile {
                        id: ALIYUN_RECYCLE_BIN_ID.into(),
                        parent_id: "root".into(),
                        name: ALIYUN_RECYCLE_BIN_NAME.into(),
                        kind: "folder".into(),
                        size: None,
                        modified_at: None,
                    });
                }
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
            browser_access_ready: false,
            expires_at: None,
        })?;
        let (account, _) = self.validate(&session)?;
        self.save(&session)?;
        Ok(account)
    }

    /// Accept an official browser session before rotating its refresh token.
    /// The access token was just issued by the authenticated web page, and
    /// validating it directly prevents a needless first refresh from trapping a
    /// successful login in its authorization window.
    pub fn authorize_browser_session(
        &self,
        mut session: AliyunSession,
    ) -> Result<CloudAccount, QuarkError> {
        if session.refresh_token.trim().is_empty()
            || session.access_token.trim().is_empty()
            || session.drive_id.trim().is_empty()
        {
            return Err(QuarkError::InvalidInput("网页登录会话不完整".into()));
        }
        let account = self.account_with(&session)?;
        session.browser_access_ready = true;
        self.save(&session)?;
        Ok(account)
    }

    /// A refresh token may be single-use. Validate the refreshed replacement
    /// completely and persist it exactly once before any caller-visible operation.
    fn refreshed_validated_session(
        &self,
    ) -> Result<(AliyunSession, CloudAccount, CloudDirectory), QuarkError> {
        let previous = self.stored()?;
        if !self.store.automatic_session_recovery() {
            let (account, root) = self.validate(&previous)?;
            return Ok((previous, account, root));
        }
        if previous.browser_access_ready {
            match self.validate(&previous) {
                Ok((account, root)) => return Ok((previous, account, root)),
                Err(QuarkError::AuthorizationExpired) => {}
                Err(error) => return Err(error),
            }
        }
        let session = self.refresh(&previous)?;
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
        let non_empty = |keys: &[&str]| {
            keys.iter().find_map(|key| {
                value
                    .get(*key)
                    .and_then(Value::as_str)
                    .filter(|field| !field.is_empty())
            })
        };
        let id = non_empty(&["user_id", "id"]).ok_or(QuarkError::InvalidResponse)?;
        let display_name = non_empty(&["nick_name", "name", "user_name", "phone"])
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
        if parent == ALIYUN_RECYCLE_BIN_ID {
            return self.list_recycle_bin();
        }
        let session = self.stored()?;
        let first = self.directory_with(&session, parent);
        match first {
            Err(QuarkError::AuthorizationExpired) if self.store.automatic_session_recovery() => {
                let refreshed = self.refresh_and_persist(&session)?;
                self.directory_with(&refreshed, parent)
            }
            result => result,
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

    pub fn list_recycle_bin(&self) -> Result<CloudDirectory, QuarkError> {
        let (session, _, _) = self.refreshed_validated_session()?;
        let mut marker = String::new();
        let mut items = Vec::new();
        for _ in 0..10_000 {
            let value = self.api(
                &session,
                "/v2/recyclebin/list",
                json!({
                    "drive_id": session.drive_id,
                    "limit": 200,
                    "marker": marker,
                }),
            )?;
            let page = value
                .get("items")
                .and_then(Value::as_array)
                .ok_or(QuarkError::InvalidResponse)?;
            for file in page {
                let mut file = parse_file(file, ALIYUN_RECYCLE_BIN_ID)?;
                file.parent_id = ALIYUN_RECYCLE_BIN_ID.into();
                items.push(file);
            }
            marker = value
                .get("next_marker")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if marker.is_empty() {
                return Ok(CloudDirectory {
                    id: ALIYUN_RECYCLE_BIN_ID.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }

    pub fn restore_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.files_action("/v2/recyclebin/restore", ids, None)
    }

    pub fn delete_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.files_action("/v2/file/delete", ids, None)
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
        let upload_parts = upload_parts(&init, parts)?;

        let mut input = std::fs::File::open(path)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        let mut remaining = size;
        for part in upload_parts {
            let length = remaining.min(ALIYUN_PART_SIZE) as usize;
            let mut body = vec![0; length];
            input
                .read_exact(&mut body)
                .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
            remaining = remaining.saturating_sub(length as u64);
            let mut headers = BTreeMap::new();
            if let Some(content_type) = part.content_type {
                headers.insert("Content-Type".into(), content_type);
            }
            let response = self.http.send_upload(UploadHttpRequest {
                method: "PUT",
                url: part.url,
                headers,
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

struct UploadPart {
    url: String,
    content_type: Option<String>,
}

fn upload_parts(value: &Value, expected_parts: u64) -> Result<Vec<UploadPart>, QuarkError> {
    let parts = value
        .get("part_info_list")
        .and_then(Value::as_array)
        .filter(|parts| parts.len() == expected_parts as usize)
        .ok_or(QuarkError::InvalidResponse)?;
    let mut upload_parts = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        if part.get("part_number").and_then(Value::as_u64) != Some(index as u64 + 1) {
            return Err(QuarkError::InvalidResponse);
        }
        let url = part
            .get("upload_url")
            .and_then(Value::as_str)
            .filter(|url| secure_temporary_url(url))
            .ok_or(QuarkError::InvalidResponse)?;
        let content_type = match part.get("content_type") {
            None => None,
            Some(Value::String(value)) if value.is_empty() => None,
            Some(Value::String(value)) if !value.contains(['\r', '\n']) => Some(value.to_owned()),
            _ => return Err(QuarkError::InvalidResponse),
        };
        upload_parts.push(UploadPart {
            url: url.into(),
            content_type,
        });
    }
    Ok(upload_parts)
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
                // Only the public upload URL is ever transferred by this
                // client.  Aliyun also returns an HTTP-only intranet endpoint
                // (`internal_upload_url`) for its own infrastructure; treating
                // that as a browser transfer target rejects a valid public URL.
                if key == "upload_url" {
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
        name: value
            .get("name")
            .or_else(|| value.get("file_name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or(QuarkError::InvalidResponse)?,
        kind: required(value, "type")?.parse()?,
        size: value
            .get("size")
            .and_then(|size| size.as_u64().or_else(|| size.as_str()?.parse().ok())),
        modified_at: value
            .get("updated_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

use super::share::{
    CloudShareAdapter, CloudShareEntry, CloudSharePlan, CloudShareResolvedEntry, ShareInspection,
    ShareResolution,
};

/// Share-token authorization is deliberately separate from personal-drive sessions.
pub struct AliyunShareAdapter<'a> {
    pub http: &'a dyn CloudHttpTransport,
}
impl AliyunShareAdapter<'_> {
    fn headers(token: Option<&str>) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::from([
            ("Referer".into(), WEB_REFERER.into()),
            ("User-Agent".into(), "Mozilla/5.0".into()),
        ]);
        if let Some(token) = token {
            headers.insert("x-share-token".into(), token.into());
        }
        headers
    }
    fn raw(
        &self,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.http.send(CloudHttpRequest {
            method: "POST",
            url: format!("{API}{path}"),
            headers: Self::headers(token),
            body: Some(body.to_string()),
        })
    }
    fn request(&self, path: &str, token: Option<&str>, body: Value) -> Result<Value, QuarkError> {
        aliyun_share_json(&self.raw(path, token, body)?)
    }
    fn token(&self, id: &str, code: Option<&str>) -> Result<CloudHttpResponse, QuarkError> {
        self.raw(
            "/v2/share_link/get_share_token",
            None,
            json!({"share_id":id,"share_pwd":code.unwrap_or("")}),
        )
    }
}
fn aliyun_share_json(response: &CloudHttpResponse) -> Result<Value, QuarkError> {
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    if !value.is_object() {
        return Err(QuarkError::InvalidResponse);
    }
    for key in ["error_code", "status"] {
        if value.get(key).is_some_and(|value| value.as_i64().is_none()) {
            return Err(QuarkError::InvalidResponse);
        }
    }
    if value
        .get("error")
        .is_some_and(|value| value.as_str().is_none())
    {
        return Err(QuarkError::InvalidResponse);
    }
    if response.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    if let Some(code) = value.get("code") {
        let code = code.as_str().ok_or(QuarkError::InvalidResponse)?;
        if !code.is_empty() {
            return Err(match code {
                "ShareLink.PasswordMismatch"
                | "ShareLink.PasswordRequired"
                | "InvalidResource.SharePwd" => {
                    QuarkError::InvalidInput("阿里云盘分享访问码错误或缺失".into())
                }
                "ShareLinkTokenInvalid"
                | "ShareLinkTokenExpired"
                | "AccessTokenInvalid"
                | "AccessTokenExpired" => QuarkError::AuthorizationExpired,
                "TooManyRequests" | "Forbidden.RiskControl" | "CaptchaRequired" => {
                    QuarkError::WebAuthorizationRequired
                }
                "BlockException" => QuarkError::Provider("阿里云盘分享请求频繁，请稍后重试".into()),
                _ => QuarkError::Provider("阿里云盘分享不可用，请检查分享有效期及访问权限".into()),
            });
        }
    }
    // The common decoder catches numeric error_code and HTTP errors; redact its text.
    response_json(response).map_err(|error| match error {
        QuarkError::Provider(_) => QuarkError::Provider("阿里云盘分享请求失败".into()),
        error => error,
    })?;
    Ok(value)
}

fn aliyun_share_rate_limited(response: &CloudHttpResponse) -> bool {
    response.status == 429
        && serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|value| value.get("code").and_then(Value::as_str).map(str::to_owned))
            .as_deref()
            == Some("BlockException")
}

impl CloudShareAdapter for AliyunShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        let id = super::share::share_id(candidate, CloudProvider::Aliyun)?;
        let response = self.token(&id, candidate.access_code.as_deref())?;
        let value: Value =
            serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
        if (200..500).contains(&response.status)
            && candidate.access_code.is_none()
            && matches!(
                value["code"].as_str(),
                Some(
                    "ShareLink.PasswordRequired"
                        | "ShareLink.PasswordMismatch"
                        | "InvalidResource.SharePwd"
                )
            )
        {
            return Ok(ShareInspection::AccessCodeRequired);
        }
        let mut token = required(&aliyun_share_json(&response)?, "share_token")?;
        let mut queue = std::collections::VecDeque::from([("root".to_string(), vec![])]);
        let mut seen = std::collections::BTreeSet::from(["root".to_string()]);
        let mut entries = vec![];
        let mut requests = 0usize;
        while let Some((parent, directory)) = queue.pop_front() {
            let mut marker = String::new();
            let mut markers = std::collections::BTreeSet::new();
            loop {
                requests += 1;
                if requests > 10_000 || directory.len() > 128 {
                    return Err(QuarkError::InvalidResponse);
                }
                let body =
                    json!({"share_id":id,"parent_file_id":parent,"limit":200,"marker":marker});
                let response = self.raw("/adrive/v3/file/list", Some(&token), body.clone())?;
                let value = if aliyun_share_rate_limited(&response) {
                    let refreshed = self.token(&id, candidate.access_code.as_deref())?;
                    token = required(&aliyun_share_json(&refreshed)?, "share_token")?;
                    self.request("/adrive/v3/file/list", Some(&token), body)?
                } else {
                    aliyun_share_json(&response)?
                };
                for raw in value["items"]
                    .as_array()
                    .ok_or(QuarkError::InvalidResponse)?
                {
                    let file = parse_file(raw, &parent)?;
                    if !seen.insert(file.id.clone()) {
                        return Err(QuarkError::InvalidResponse);
                    }
                    match file.kind.as_str() {
                        "folder" => {
                            let mut path = directory.clone();
                            path.push(file.name);
                            queue.push_back((file.id, path));
                        }
                        "file" => entries.push(CloudShareEntry::new(
                            &id,
                            file.id,
                            file.name,
                            directory.clone(),
                            file.size,
                        )),
                        _ => return Err(QuarkError::InvalidResponse),
                    }
                }
                marker = match value.get("next_marker") {
                    None => String::new(),
                    Some(value) => value.as_str().ok_or(QuarkError::InvalidResponse)?.into(),
                };
                if marker.is_empty() {
                    break;
                }
                if !markers.insert(marker.clone()) {
                    return Err(QuarkError::InvalidResponse);
                }
            }
        }
        Ok(ShareInspection::Plan(CloudSharePlan::new(
            CloudProvider::Aliyun,
            id,
            candidate.access_code.clone(),
            entries,
            json!({"share_token":token}),
        )))
    }
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        resolve_aliyun_share(self.http, entry)
    }
}
pub fn resolve_aliyun_share(
    http: &dyn CloudHttpTransport,
    entry: &CloudShareResolvedEntry,
) -> Result<ShareResolution, QuarkError> {
    if entry.provider != CloudProvider::Aliyun || entry.plan.provider != CloudProvider::Aliyun {
        return Err(QuarkError::InvalidResponse);
    }
    let adapter = AliyunShareAdapter { http };
    let response = adapter.token(&entry.plan.canonical_share_id, entry.access_code())?;
    let token = required(&aliyun_share_json(&response)?, "share_token")?;
    // Official GetDownloadUrl: share_id + x-share-token, no drive_id or personal bearer.
    let value = adapter.request("/v2/file/get_download_url",Some(&token),json!({"share_id":entry.plan.canonical_share_id,"file_id":entry.entry.entry_id,"expire_sec":900}))?;
    let url = required(&value, "url").or_else(|_| required(&value, "download_url"))?;
    super::share::validate_direct_url(&url)?;
    Ok(ShareResolution::Direct(
        super::commands::ResolvedCloudDownload {
            url,
            name: entry.entry.name.clone(),
            headers: AliyunShareAdapter::headers(Some(&token))
                .into_iter()
                .collect(),
        },
    ))
}
