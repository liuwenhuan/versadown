use super::{
    types::{encoded, required},
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudHttpRequest, CloudHttpResponse,
    CloudHttpTransport, CloudProvider, CloudUploadTransport, QuarkError, UploadHttpRequest,
    UploadHttpResponse,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use zeroize::Zeroizing;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{share::*, CredentialStore, MemoryCredentialStore};
    use std::sync::Mutex;
    struct Http {
        replies: Mutex<std::collections::VecDeque<Value>>,
        requests: Mutex<Vec<CloudHttpRequest>>,
    }
    impl Http {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(vec![]),
            }
        }
    }
    impl CloudHttpTransport for Http {
        fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            self.requests.lock().unwrap().push(request);
            Ok(CloudHttpResponse {
                status: 200,
                body: self
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected HTTP request")
                    .to_string(),
                set_cookies: vec![],
            })
        }
    }
    fn candidate() -> crate::parser::CloudShareCandidate {
        crate::parser::CloudShareCandidate {
            provider: CloudProvider::Baidu,
            url: "https://pan.baidu.com/s/1share123".into(),
            access_code: Some("a123".into()),
        }
    }
    fn listing() -> Value {
        json!({"errno":0,"data":{"shareid":22,"uk":33,"seckey":"secret-key","has_more":false,"list":[{"fs_id":11,"server_filename":"movie.mp4","path":"/movie.mp4","isdir":0,"size":123}]}})
    }
    #[test]
    fn share_cleanup_resolves_only_exact_recorded_ids_before_deleting() {
        let http = Http::new(vec![
            json!({"errno":0,"list":[{"fs_id":99,"path":"/created.txt","isdir":0}]}),
            json!({"errno":0,"result":{"bdstoken":"token"}}),
            json!({"errno":0}),
        ]);
        BaiduClient::new(BaiduSession::cookie("BDUSS=current"), &http)
            .trash_transferred_files(&["99".into()])
            .unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let url = url::Url::parse(&requests[0].url).unwrap();
        assert!(url.query_pairs().any(|(k, v)| k == "fsids" && v == "[99]"));
        assert!(requests[2].url.contains("opera=delete"));
        assert!(
            url::form_urlencoded::parse(requests[2].body.as_ref().unwrap().as_bytes())
                .any(|(k, v)| k == "filelist" && v == "[\"/created.txt\"]")
        );
    }

    #[test]
    fn share_cleanup_rejects_missing_extra_directory_and_invalid_path_metadata() {
        for files in [
            json!([]),
            json!([{"fs_id":100,"path":"/other","isdir":0}]),
            json!([{"fs_id":99,"path":"/created","isdir":0},{"fs_id":100,"path":"/other","isdir":0}]),
            json!([{"fs_id":99,"path":"/folder","isdir":1}]),
            json!([{"fs_id":99,"path":"/","isdir":0}]),
            json!([{"fs_id":99,"path":"/../other","isdir":0}]),
        ] {
            let http = Http::new(vec![json!({"errno":0,"list":files})]);
            assert!(
                BaiduClient::new(BaiduSession::cookie("BDUSS=current"), &http)
                    .trash_transferred_files(&["99".into()])
                    .is_err()
            );
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
    }
    #[test]
    fn baidu_transfer_fallback_requires_current_login() {
        let credentials = MemoryCredentialStore::default();
        let http = Http::new(vec![
            listing(),
            json!({"errno":0,"data":{"sign":"secret-sign","timestamp":1234}}),
            json!({"errno":0,"list":[]}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        assert!(matches!(
            adapter.inspect(&candidate()),
            Err(QuarkError::NotAuthorized)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
        credentials
            .save(
                "baidu",
                &serde_json::to_vec(&BaiduSession::cookie("BDUSS=current")).unwrap(),
            )
            .unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!("plan required")
        };
        let entry = CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        };
        assert!(matches!(
            adapter.resolve(&entry).unwrap(),
            ShareResolution::TransferRequired(_)
        ));
        let requests = http.requests.lock().unwrap();
        assert!(requests[2].url.contains("api/sharedownload"));
        assert_eq!(requests[2].headers["User-Agent"], BAIDU_PAN_USER_AGENT);
        assert!(requests[2].headers["Cookie"].contains("BDUSS=current"));
    }
    fn credentials() -> MemoryCredentialStore {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save(
                "baidu",
                &serde_json::to_vec(&BaiduSession::cookie("BDUSS_BFESS=current")).unwrap(),
            )
            .unwrap();
        credentials
    }
    #[test]
    fn baidu_share_direct_keeps_native_identity_and_secret_out_of_credentials() {
        let credentials = credentials();
        let http = Http::new(vec![
            listing(),
            json!({"errno":0,"data":{"sign":"secret-sign","timestamp":1234}}),
            json!({"errno":0,"list":[{"fs_id":11,"dlink":"https://d.pcs.baidu.com/file?sign=secret"}]}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let entry = CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        };
        let ShareResolution::Direct(download) = adapter.resolve(&entry).unwrap() else {
            panic!()
        };
        assert_eq!(download.url, "https://d.pcs.baidu.com/file?sign=secret");
        assert!(download
            .headers
            .iter()
            .any(|(k, v)| k == "Cookie" && v.contains("BDUSS=current")));
        assert_eq!(http.requests.lock().unwrap().len(), 3);
        let saved = String::from_utf8(credentials.load("baidu").unwrap().unwrap()).unwrap();
        assert!(!saved.contains("secret"));
        let requests = http.requests.lock().unwrap();
        let form: BTreeMap<_, _> =
            url::form_urlencoded::parse(requests[2].body.as_ref().unwrap().as_bytes())
                .into_owned()
                .collect();
        assert_eq!(form["fid_list"], "[11]");
        assert_eq!(form["extra"], "{\"sekey\":\"secret-key\"}");
    }
    #[test]
    fn baidu_share_access_code_and_expiry_never_transfer() {
        let credentials = credentials();
        let http = Http::new(vec![
            json!({"errno":-9}),
            json!({"errno":-9}),
            json!({"errno":-6}),
            json!({"errno":-20}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let mut missing = candidate();
        missing.access_code = None;
        assert!(matches!(
            adapter.inspect(&missing),
            Ok(ShareInspection::AccessCodeRequired)
        ));
        assert!(matches!(
            adapter.inspect(&candidate()),
            Err(QuarkError::InvalidInput(_))
        ));
        assert!(matches!(
            adapter.inspect(&candidate()),
            Err(QuarkError::AuthorizationExpired)
        ));
        assert!(matches!(
            adapter.inspect(&candidate()),
            Err(QuarkError::WebAuthorizationRequired)
        ));
        assert_eq!(http.requests.lock().unwrap().len(), 4);
    }
    #[test]
    fn baidu_share_pages_nested_paths_and_empty_folder_stays_empty() {
        let credentials = credentials();
        let mut first = listing();
        first["data"]["has_more"] = json!(true);
        first["data"]["list"] =
            json!([{"fs_id":12,"server_filename":"season","path":"/season","isdir":1}]);
        let mut nested = listing();
        nested["data"]["list"][0]["fs_id"] = json!(13);
        nested["data"]["list"][0]["path"] = json!("/season/verify.mp4");
        nested["data"]["list"][0]["server_filename"] = json!("verify.mp4");
        let http = Http::new(vec![first, listing(), nested]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[1].relative_directory, vec!["season"]);
        assert!(http.requests.lock().unwrap()[1]
            .body
            .as_ref()
            .unwrap()
            .contains("page=2"));
        let mut empty = listing();
        empty["data"]["list"] = json!([]);
        let http = Http::new(vec![empty]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        assert!(plan.entries.is_empty());
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
    #[test]
    fn baidu_share_transfer_records_exact_id_and_downloads_personal_file() {
        let credentials = credentials();
        let http = Http::new(vec![
            listing(),
            json!({"errno":0,"data":{"sign":"sign","timestamp":123}}),
            json!({"errno":0,"list":[]}),
            json!({"user":{"uk":"account-7"}}),
            json!({"errno":0,"result":{"bdstoken":"write-token"}}),
            json!({"errno":0,"extra":{"list":[{"from_fs_id":11,"to_fs_id":99,"from":"/movie.mp4","to":"/movie (1).mp4"}]}}),
            json!({"errno":0,"list":[{"fs_id":99,"server_filename":"movie (1).mp4","dlink":"https://d.pcs.baidu.com/file?sign=personal"}]}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let dir = tempfile::tempdir().unwrap();
        let store = CloudSharePlanStore::new(
            crate::cloud::DpapiCredentialStore::new(dir.path().into()).unwrap(),
        );
        store.upsert(&plan).unwrap();
        let source = plan.source_for("11");
        let download = resolve_current_share_download(&store, adapter.as_ref(), &source).unwrap();
        assert_eq!(download.name, "movie.mp4");
        assert_eq!(
            store.resolve_entry(&source).unwrap().plan.transfers[0].created_file_id,
            "99"
        );
        let requests = http.requests.lock().unwrap();
        assert!(requests[5].url.contains("ondup=newcopy"));
        assert!(requests[6].url.contains("fsids=%5B99%5D"));
    }
    #[test]
    fn baidu_recorded_transfer_refreshes_personal_file_without_share_transfer() {
        let credentials = credentials();
        let http = Http::new(vec![
            listing(),
            json!({"errno":0,"list":[{"fs_id":99,"server_filename":"movie (1).mp4","dlink":"https://d.pcs.baidu.com/file?sign=personal"}]}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let entry = CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        };

        let refreshed = adapter.refresh_recorded_transfer(&entry, "99").unwrap();

        let Ok(RecordedTransferDownload::Available(download)) = refreshed.result else {
            panic!("a personal direct link is required")
        };
        assert_eq!(download.url, "https://d.pcs.baidu.com/file?sign=personal");
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].url.contains("filemetas"));
        assert!(!requests.iter().any(|request| request.url.contains("/share/transfer")));
    }
    #[test]
    fn baidu_transfer_owner_survives_cookie_rotation_for_the_same_account() {
        let credentials = credentials();
        let http = Http::new(vec![
            listing(),
            json!({"user":{"uk":"account-7"}}),
            json!({"user":{"uk":"account-7"}}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let entry = CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        };

        let before = adapter.transfer_owner_fingerprint(&entry).unwrap();
        credentials
            .save(
                "baidu",
                &serde_json::to_vec(&BaiduSession::cookie("BDUSS_BFESS=renewed")).unwrap(),
            )
            .unwrap();
        let after = adapter.transfer_owner_fingerprint(&entry).unwrap();

        assert_eq!(before, after);
    }
    #[test]
    fn baidu_share_transfer_accepts_verify_filename_and_http_download() {
        let credentials = credentials();
        let mut listing = listing();
        listing["data"]["list"][0]["server_filename"] = json!("verify.mp4");
        listing["data"]["list"][0]["path"] = json!("/verify.mp4");
        let http = Http::new(vec![
            listing,
            json!({"errno":0,"data":{"sign":"sign","timestamp":123}}),
            json!({"errno":0,"list":[]}),
            json!({"user":{"uk":"account-7"}}),
            json!({"errno":0,"result":{"bdstoken":"write-token"}}),
            json!({"errno":0,"extra":{"list":[{"from_fs_id":11,"to_fs_id":99,"from":"/verify.mp4","to":"/verify.mp4"}]}}),
            json!({"errno":0,"list":[{"fs_id":99,"server_filename":"verify.mp4","dlink":"http://d.pcs.baidu.com/file?sign=personal"}]}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let dir = tempfile::tempdir().unwrap();
        let store = CloudSharePlanStore::new(
            crate::cloud::DpapiCredentialStore::new(dir.path().into()).unwrap(),
        );
        store.upsert(&plan).unwrap();
        let source = plan.source_for("11");
        let download = resolve_current_share_download(&store, adapter.as_ref(), &source).unwrap();
        assert_eq!(download.name, "verify.mp4");
        assert_eq!(download.url, "http://d.pcs.baidu.com/file?sign=personal");
        let transfers = store.resolve_entry(&source).unwrap().plan.transfers;
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].created_file_id, "99");
        assert_eq!(transfers[0].cleanup_state, CloudShareCleanupState::Pending);
        assert_eq!(http.requests.lock().unwrap().len(), 7);
    }
    #[test]
    fn baidu_share_errors_never_become_transfer_requests() {
        for error in [
            json!({"errno":-20}),
            json!({"errno":-6}),
            json!({"errno":112}),
            json!({"errno":0,"list":{}}),
        ] {
            let credentials = credentials();
            let http = Http::new(vec![
                listing(),
                json!({"errno":0,"data":{"sign":"sign","timestamp":123}}),
                error,
            ]);
            let adapter =
                provider_share_adapter(CloudProvider::Baidu, &credentials, &http).unwrap();
            let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
                panic!()
            };
            let entry = CloudShareResolvedEntry {
                provider: plan.provider,
                entry: plan.entries[0].clone(),
                plan,
            };
            assert!(adapter.resolve(&entry).is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 3);
        }
    }
}

const PAN: &str = "https://pan.baidu.com";
const PASSPORT: &str = "https://passport.baidu.com";
const BAIDU_WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";
const BAIDU_PAN_USER_AGENT: &str =
    "netdisk;P2SP;3.0.0.8;netdisk;11.12.3;ANG-AN00;android-android;10.0;JSbridge4.4.0;jointBridge;1.1.0;";
const REFERENCE_PREFIX: &str = "bdr_";
pub const BAIDU_RECYCLE_BIN_ID: &str = "__all_downloader_baidu_recycle_bin__";
const BAIDU_RECYCLE_BIN_NAME: &str = "回收站";
const BAIDU_UPLOAD_HOST: &str = "https://d.pcs.baidu.com/rest/2.0/pcs/superfile2";
const BAIDU_UPLOAD_BLOCK_SIZE: usize = 4 * 1024 * 1024;

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

fn native_api_cookie(cookie: String) -> String {
    let mut has_legacy_bduss = false;
    let mut secure_webview_bduss = None;
    for pair in cookie.split(';').map(str::trim) {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        match name {
            "BDUSS" if !value.is_empty() => has_legacy_bduss = true,
            "BDUSS_BFESS" if !value.is_empty() => secure_webview_bduss = Some(value),
            _ => {}
        }
    }
    if has_legacy_bduss {
        return cookie;
    }
    let Some(value) = secure_webview_bduss else {
        return cookie;
    };
    format!("{}; BDUSS={value}", cookie.trim_end_matches(';').trim())
}

/// Normalize the complete official Pan web session into a Cookie header.
/// Cookie attributes (including expiry and domain) never enter the saved
/// session.  The browser has already scoped this list to Baidu's domains; do
/// not reduce it to a hand-maintained allowlist because Pan also relies on
/// session-bound cookies such as `PANPSC` and `SCRC` for file operations.
/// Chromium/WebView2 keeps the secure Baidu login credential under
/// `BDUSS_BFESS`, so it must remain intact instead of being rewritten here.
pub fn parse_baidu_web_session(cookies: &[String]) -> Result<BaiduSession, QuarkError> {
    let mut pairs = BTreeMap::new();
    for raw in cookies {
        let pair = raw.split(';').next().unwrap_or("").trim();
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if valid_cookie_name(name) && !value.is_empty() && !value.chars().any(char::is_control) {
            // Cookie names are unique in a Cookie header.  Later values are
            // the current values when multiple browser URLs expose a pair.
            pairs.insert(name, value);
        }
    }
    if !pairs.contains_key("BDUSS") && !pairs.contains_key("BDUSS_BFESS") {
        return Err(QuarkError::AuthorizationExpired);
    }
    Ok(BaiduSession::cookie(
        pairs
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; "),
    ))
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

#[derive(Clone, Debug)]
pub struct BaiduDownload {
    pub url: String,
    pub name: String,
    pub headers: BTreeMap<String, String>,
}

pub struct BaiduClient<'a, T: CloudHttpTransport + ?Sized> {
    session: BaiduSession,
    http: &'a T,
}

impl<'a, T: CloudHttpTransport + ?Sized> BaiduClient<'a, T> {
    pub fn new(mut session: BaiduSession, http: &'a T) -> Self {
        // WebView2 stores the current QR-login credential as BDUSS_BFESS,
        // while Baidu's native xpan file API still expects the legacy BDUSS
        // key. Keep the secure cookie and add a request-only compatibility
        // alias so sessions saved by earlier releases keep working.
        session.cookie = native_api_cookie(session.cookie);
        Self { session, http }
    }

    /// Native Pan endpoints reject a browser origin paired with the Netdisk
    /// client signature. Keep this profile aligned with BaiduPCS-Go: its Pan
    /// requests provide the Netdisk UA and authenticated cookie jar only.
    fn pan_headers(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("Cookie".into(), self.session.cookie.clone()),
            ("User-Agent".into(), BAIDU_PAN_USER_AGENT.into()),
        ])
    }

    fn web_headers(&self) -> BTreeMap<String, String> {
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
            headers: self.pan_headers(),
            body: body.map(|value| value.to_string()),
        })?;
        baidu_json(&response)
    }

    fn web_request(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method,
            url,
            headers: self.web_headers(),
            body: body.map(|value| value.to_string()),
        })?;
        baidu_json(&response)
    }

    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let value = self.account_value()?;
        let user = value.get("user").unwrap_or(&value);
        let id = user
            .get("uk")
            .or_else(|| user.get("user_id"))
            .and_then(string_or_number)
            .filter(|id| !id.is_empty())
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

    fn stable_account_id(&self) -> Result<Option<String>, QuarkError> {
        let value = self.account_value()?;
        let user = value.get("user").unwrap_or(&value);
        Ok(user
            .get("uk")
            .or_else(|| user.get("user_id"))
            .and_then(string_or_number)
            .filter(|id| !id.is_empty()))
    }

    fn account_value(&self) -> Result<Value, QuarkError> {
        self.request(
            "GET",
            format!("{PAN}/rest/2.0/xpan/nas?method=uinfo&web=1"),
            None,
        )
    }

    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let parent = parse_parent_reference(parent)?;
        // BaiduPCS-Go's current directory implementation uses the Pan web
        // listing route (not xpan/file) with this small, fixed query shape.
        // Keeping the native Netdisk header profile is required for the
        // browser-issued cookie jar to be accepted by that route.
        let value = self.request(
            "GET",
            format!(
                "{PAN}/api/list?dir={}&order=name&desc=0&clienttype=0&num=100&page=1",
                encoded(&parent.path)
            ),
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
        let mut items = files
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
        if parent.path == "/" {
            items.push(CloudFile {
                id: BAIDU_RECYCLE_BIN_ID.into(),
                parent_id: directory_id.clone(),
                name: BAIDU_RECYCLE_BIN_NAME.into(),
                kind: "folder".into(),
                size: None,
                modified_at: None,
            });
        }
        Ok(CloudDirectory {
            id: directory_id,
            items,
        })
    }

    pub fn list_recycle_bin(&self) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        let mut seen = BTreeSet::new();
        for page in 1..=10_000 {
            let value = self.web_request(
                "GET",
                format!("{PAN}/api/recycle/list/?num=100&page={page}&web=1"),
                None,
            )?;
            let files = value
                .get("list")
                .and_then(Value::as_array)
                .ok_or(QuarkError::InvalidResponse)?;
            let length = files.len();
            for file in files {
                let id = file
                    .get("fs_id")
                    .or_else(|| file.get("fsid"))
                    .and_then(string_or_number)
                    .filter(|id| valid_baidu_recycle_record_id(id))
                    .ok_or(QuarkError::InvalidResponse)?;
                if !seen.insert(id.clone()) {
                    return Err(QuarkError::InvalidResponse);
                }
                items.push(CloudFile {
                    id,
                    parent_id: BAIDU_RECYCLE_BIN_ID.into(),
                    name: required(file, "server_filename").or_else(|_| required(file, "name"))?,
                    kind: if file.get("isdir").and_then(Value::as_i64) == Some(1) {
                        "folder"
                    } else {
                        "file"
                    }
                    .into(),
                    size: file.get("size").and_then(Value::as_u64),
                    modified_at: file.get("server_mtime").and_then(string_or_number),
                });
            }
            if length < 100 {
                return Ok(CloudDirectory {
                    id: BAIDU_RECYCLE_BIN_ID.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }

    pub fn restore_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.recycle_operation("restore", ids)
    }

    pub fn delete_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        self.recycle_operation("delete", ids)
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

    fn recycle_operation(&self, action: &str, ids: &[String]) -> Result<(), QuarkError> {
        let fidlist = baidu_recycle_fidlist(ids)?;
        let token = self.web_token()?;
        self.web_form(
            &format!("/api/recycle/{action}"),
            &format!("channel=chunlei&async=1&bdstoken={}", encoded(&token)),
            form(&[("fidlist", fidlist)]),
        )
        .map(|_| ())
    }

    pub(crate) fn trash_transferred_files(&self, ids: &[String]) -> Result<(), QuarkError> {
        let expected: std::collections::BTreeSet<_> = ids.iter().cloned().collect();
        if expected.is_empty()
            || expected.len() != ids.len()
            || ids
                .iter()
                .any(|id| id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(QuarkError::InvalidResponse);
        }
        let response = self.share_http(
            "GET",
            &format!(
                "/rest/2.0/xpan/multimedia?method=filemetas&web=1&fsids={}",
                encoded(&format!("[{}]", ids.join(",")))
            ),
            None,
            "",
        )?;
        let value = share_json(&response)?;
        let files = value["list"]
            .as_array()
            .filter(|files| files.len() == expected.len())
            .ok_or(QuarkError::InvalidResponse)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut paths = Vec::new();
        for file in files {
            let id = numeric_field(file, "fs_id")?;
            let path = required(file, "path")?;
            if !expected.contains(&id)
                || !seen.insert(id)
                || file["isdir"].as_u64() != Some(0)
                || path == "/"
            {
                return Err(QuarkError::InvalidResponse);
            }
            validate_path(&path)?;
            paths.push(path);
        }
        if seen != expected {
            return Err(QuarkError::InvalidResponse);
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

    /// Uploads a regular local file through the official precreate, slice and
    /// commit sequence. Upload IDs and transfer requests stay entirely local
    /// to this call and are never stored with the session.
    pub fn upload_file<U: CloudUploadTransport>(
        &self,
        parent: &str,
        local_path: &Path,
        upload_http: &U,
    ) -> Result<CloudFile, QuarkError> {
        let parent = parse_parent_reference(parent)?;
        let (name, size) = local_file_metadata(local_path)?;
        let path = join_path(&parent.path, &name)?;
        let blocks = baidu_block_hashes(local_path, size)?;
        let token = self.web_token()?;
        let precreate = self.web_form(
            "/api/precreate",
            &write_query(&token, ""),
            form(&[
                ("path", path.clone()),
                ("size", size.to_string()),
                ("isdir", "0".into()),
                ("autoinit", "1".into()),
                ("rtype", "3".into()),
                ("block_list", json!(blocks).to_string()),
            ]),
        )?;
        // A zero-slice/rapid-upload response still needs the normal create
        // commit unless the service has already returned a concrete file.
        // Treating precreate itself as a completed file loses valid rapid
        // uploads because that response normally contains only an upload id.
        if precreate.get("return_type").and_then(Value::as_i64) == Some(2)
            && precreate.get("info").is_some()
        {
            return upload_file_result(&precreate, &parent, &name, size);
        }
        let upload_id = required(&precreate, "uploadid")?;
        let requested_blocks = precreate
            .get("block_list")
            .and_then(Value::as_array)
            .ok_or(QuarkError::InvalidResponse)?
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < blocks.len())
                    .ok_or(QuarkError::InvalidResponse)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for part in requested_blocks {
            let bytes = baidu_block(local_path, size, part)?;
            let response = upload_http
                .send_upload(self.slice_request(&path, &upload_id, part, &token, &name, bytes)?)?;
            validate_slice_response(&response, &blocks[part])?;
        }
        let committed = self.web_form(
            "/api/create",
            &write_query(&token, "a=commit"),
            form(&[
                ("path", path),
                ("size", size.to_string()),
                ("isdir", "0".into()),
                ("uploadid", upload_id),
                ("block_list", json!(blocks).to_string()),
            ]),
        )?;
        upload_file_result(&committed, &parent, &name, size)
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
            headers: self.pan_headers(),
        })
    }

    fn web_token(&self) -> Result<String, QuarkError> {
        let value = self.web_request(
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

    fn slice_request(
        &self,
        path: &str,
        upload_id: &str,
        part: usize,
        token: &str,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<UploadHttpRequest, QuarkError> {
        let mut url =
            url::Url::parse(BAIDU_UPLOAD_HOST).map_err(|_| QuarkError::InvalidResponse)?;
        url.query_pairs_mut()
            .append_pair("method", "upload")
            .append_pair("type", "tmpfile")
            .append_pair("app_id", "250528")
            .append_pair("bdstoken", token)
            .append_pair("path", path)
            .append_pair("uploadid", upload_id)
            .append_pair("partseq", &part.to_string());
        let url = super::https_transfer_url(url.as_str())?;
        let (content_type, body) = super::multipart_file(&[], "file", name, bytes)?;
        let mut headers = self.web_headers();
        headers.insert("Content-Type".into(), content_type);
        Ok(UploadHttpRequest {
            method: "POST",
            url,
            headers,
            body,
        })
    }

    fn web_form(&self, path: &str, query: &str, body: String) -> Result<Value, QuarkError> {
        let mut headers = self.web_headers();
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
        let mut session = parse_baidu_web_session(&response.set_cookies)?;
        session.auth_method = CloudAuthMethod::Password;
        let client = Self::new(session.clone(), http);
        client.account()?;
        client.directory("/")?;
        Ok(session)
    }
}

fn local_file_metadata(path: &Path) -> Result<(String, u64), QuarkError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(QuarkError::InvalidInput("请选择普通文件上传".into()));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| QuarkError::InvalidInput("文件名无效".into()))?
        .to_owned();
    validate_name(&name)?;
    Ok((name, metadata.len()))
}

fn baidu_block_hashes(path: &Path, size: u64) -> Result<Vec<String>, QuarkError> {
    let mut file = std::fs::File::open(path)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
    let block_count = usize::try_from(
        (size + BAIDU_UPLOAD_BLOCK_SIZE as u64 - 1) / BAIDU_UPLOAD_BLOCK_SIZE as u64,
    )
    .map_err(|_| QuarkError::InvalidInput("文件过大，无法上传".into()))?;
    if block_count == 0 {
        return Err(QuarkError::InvalidInput("不能上传空文件".into()));
    }
    let mut hashes = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let mut bytes = vec![0; BAIDU_UPLOAD_BLOCK_SIZE];
        let read = file
            .read(&mut bytes)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
        if read == 0 {
            return Err(QuarkError::InvalidInput("本地文件在上传前发生变化".into()));
        }
        bytes.truncate(read);
        hashes.push(format!("{:x}", md5::compute(bytes)));
    }
    Ok(hashes)
}

fn baidu_block(path: &Path, size: u64, index: usize) -> Result<Vec<u8>, QuarkError> {
    let offset = u64::try_from(index)
        .ok()
        .and_then(|index| index.checked_mul(BAIDU_UPLOAD_BLOCK_SIZE as u64))
        .ok_or(QuarkError::InvalidResponse)?;
    if offset >= size {
        return Err(QuarkError::InvalidResponse);
    }
    let expected = (size - offset).min(BAIDU_UPLOAD_BLOCK_SIZE as u64) as usize;
    let mut file = std::fs::File::open(path)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的本地文件".into()))?;
    let mut bytes = vec![0; expected];
    file.read_exact(&mut bytes)
        .map_err(|_| QuarkError::InvalidInput("本地文件在上传过程中发生变化".into()))?;
    Ok(bytes)
}

fn validate_slice_response(
    response: &UploadHttpResponse,
    expected_md5: &str,
) -> Result<(), QuarkError> {
    if !(200..300).contains(&response.status) {
        return Err(QuarkError::Provider("百度网盘拒绝上传分片".into()));
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    if required(&value, "md5")? != expected_md5 {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(())
}

fn upload_file_result(
    value: &Value,
    parent: &BaiduFileReference,
    name: &str,
    size: u64,
) -> Result<CloudFile, QuarkError> {
    let value = value.get("info").unwrap_or(value);
    let fsid = value
        .get("fs_id")
        .or_else(|| value.get("fsid"))
        .and_then(string_or_number)
        .ok_or(QuarkError::InvalidResponse)?;
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or(join_path(&parent.path, name)?);
    let id = encode_reference(BaiduFileReference { fsid, path })?;
    Ok(CloudFile {
        id,
        parent_id: if parent.path == "/" {
            "/".into()
        } else {
            encode_reference(parent.clone())?
        },
        name: value
            .get("server_filename")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .into(),
        kind: "file".into(),
        size: value.get("size").and_then(Value::as_u64).or(Some(size)),
        modified_at: value.get("server_mtime").and_then(string_or_number),
    })
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
            if std::env::var("ALLDOWNLOADER_BAIDU_DIAGNOSTIC").as_deref() == Ok("1") {
                return Err(QuarkError::Provider(format!(
                    "百度授权拒绝（errno {errno}，HTTP {}）",
                    response.status
                )));
            }
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

fn valid_baidu_recycle_record_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn baidu_recycle_fidlist(ids: &[String]) -> Result<String, QuarkError> {
    if ids.is_empty()
        || ids.iter().any(|id| !valid_baidu_recycle_record_id(id))
        || ids
            .iter()
            .enumerate()
            .any(|(index, id)| ids[..index].contains(id))
    {
        return Err(QuarkError::InvalidInput("回收站记录 ID 无效".into()));
    }
    let records = ids
        .iter()
        .map(|id| {
            id.parse::<u64>()
                .map(Value::from)
                .map_err(|_| QuarkError::InvalidInput("回收站记录 ID 无效".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_string(&records).map_err(|_| QuarkError::InvalidResponse)
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

use super::share::{
    CloudShareAdapter, CloudShareEntry, CloudSharePlan, CloudSharePlanStore,
    CloudShareResolvedEntry, RecordedTransferDownload, ShareInspection, ShareOperationResult,
    ShareResolution, TransferRequest,
};

pub struct BaiduShareAdapter<'a> {
    pub credentials: &'a dyn super::CredentialStore,
    pub http: &'a dyn CloudHttpTransport,
}
impl BaiduShareAdapter<'_> {
    fn client(&self) -> Result<BaiduClient<'_, dyn CloudHttpTransport + '_>, QuarkError> {
        let bytes = self
            .credentials
            .load("baidu")?
            .ok_or(QuarkError::NotAuthorized)?;
        let session: BaiduSession =
            serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if session.cookie.trim().is_empty() {
            return Err(QuarkError::NotAuthorized);
        }
        Ok(BaiduClient::new(session, self.http))
    }
}
impl<T: CloudHttpTransport + ?Sized> BaiduClient<'_, T> {
    fn share_http(
        &self,
        method: &'static str,
        path: &str,
        body: Option<String>,
        bdclnd: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        let mut headers = self.pan_headers();
        if !bdclnd.is_empty() {
            headers.insert(
                "Cookie".into(),
                format!("{}; BDCLND={bdclnd}", self.session.cookie),
            );
        }
        if body.is_some() {
            headers.insert(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            );
        }
        self.http.send(CloudHttpRequest {
            method,
            url: format!("{PAN}{path}"),
            headers,
            body,
        })
    }
}
fn share_json(response: &CloudHttpResponse) -> Result<Value, QuarkError> {
    if response.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    let code = value["errno"].as_i64().ok_or(QuarkError::InvalidResponse)?;
    if code == -20
        || matches!(
            value["err_no"].as_i64(),
            Some(6 | 16 | 257 | 120021 | 400031)
        )
        || ["captcha", "vcode", "vcode_str", "codeString"]
            .iter()
            .any(|key| {
                value
                    .get(key)
                    .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            })
    {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    if matches!(code, -6 | -7 | 111) {
        return Err(QuarkError::AuthorizationExpired);
    }
    if code != 0 || !(200..300).contains(&response.status) {
        return Err(QuarkError::Provider("百度分享无效或请求被拒绝".into()));
    }
    Ok(value)
}
fn numeric_field(value: &Value, field: &str) -> Result<String, QuarkError> {
    value
        .get(field)
        .and_then(string_or_number)
        .filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
        .ok_or(QuarkError::InvalidResponse)
}
impl CloudShareAdapter for BaiduShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        let client = self.client()?;
        let id = super::share::share_id(candidate, CloudProvider::Baidu)?;
        let short = id.strip_prefix('1').ok_or(QuarkError::InvalidResponse)?;
        let mut entries = vec![];
        let mut context = json!({"shorturl":short,"bdclnd":"","paths":{}});
        let mut queue =
            std::collections::VecDeque::from([("/".to_owned(), Vec::<String>::new(), true)]);
        let mut visited = std::collections::BTreeSet::new();
        while let Some((parent, directory, root)) = queue.pop_front() {
            if directory.len() > 128 || !visited.insert(parent.clone()) || visited.len() > 10000 {
                return Err(QuarkError::InvalidResponse);
            }
            for page in 1..=10000 {
                let response = client.share_http(
                    "POST",
                    "/share/wxlist?channel=weixin&version=2.2.2&clienttype=25&web=1",
                    Some(form(&[
                        ("shorturl", short.into()),
                        ("pwd", candidate.access_code.clone().unwrap_or_default()),
                        ("root", if root { "1" } else { "0" }.into()),
                        ("dir", parent.clone()),
                        ("num", "100".into()),
                        ("page", page.to_string()),
                        ("order", "time".into()),
                    ])),
                    context["bdclnd"].as_str().unwrap_or(""),
                )?;
                let raw: Value = serde_json::from_str(&response.body)
                    .map_err(|_| QuarkError::InvalidResponse)?;
                if response.status == 200 && raw["errno"].as_i64() == Some(-9) {
                    return if candidate.access_code.is_none() {
                        Ok(ShareInspection::AccessCodeRequired)
                    } else {
                        Err(QuarkError::InvalidInput("百度分享访问码错误".into()))
                    };
                }
                let value = share_json(&response)?;
                let data = &value["data"];
                if root && page == 1 {
                    context["shareid"] = json!(numeric_field(data, "shareid")?);
                    context["uk"] = json!(numeric_field(data, "uk")?);
                    context["seckey"] = json!(data["seckey"].as_str().unwrap_or(""));
                }
                for cookie in &response.set_cookies {
                    if let Some(value) = cookie
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .strip_prefix("BDCLND=")
                    {
                        context["bdclnd"] = json!(value);
                    }
                }
                let files = data["list"].as_array().ok_or(QuarkError::InvalidResponse)?;
                for file in files {
                    let fid = numeric_field(file, "fs_id")?;
                    let name = required(file, "server_filename")?;
                    let path = required(file, "path")?;
                    validate_path(&path)?;
                    if file["isdir"].as_i64() == Some(1) {
                        let mut relative = directory.clone();
                        relative.push(name);
                        queue.push_back((path, relative, false));
                    } else {
                        context["paths"][&fid] = json!(path);
                        entries.push(CloudShareEntry::new(
                            &id,
                            fid,
                            name,
                            directory.clone(),
                            file["size"].as_u64(),
                        ));
                    }
                }
                let more = data["has_more"]
                    .as_bool()
                    .or_else(|| data["has_more"].as_u64().map(|v| v != 0))
                    .unwrap_or(files.len() == 100);
                if !more {
                    break;
                }
                if files.is_empty() || page == 10000 {
                    return Err(QuarkError::InvalidResponse);
                }
            }
        }
        Ok(ShareInspection::Plan(CloudSharePlan::new(
            CloudProvider::Baidu,
            id,
            candidate.access_code.clone(),
            entries,
            context,
        )))
    }
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        resolve_baidu_share(self.credentials, self.http, entry)
    }
    fn transfer_owner_fingerprint(
        &self,
        _: &CloudShareResolvedEntry,
    ) -> Result<Option<String>, QuarkError> {
        self.client()?
            .stable_account_id()?
            .map(|id| super::share::stable_owner_fingerprint(CloudProvider::Baidu, &id))
            .transpose()
    }
    fn refresh_recorded_transfer(
        &self,
        entry: &CloudShareResolvedEntry,
        created_file_id: &str,
    ) -> Result<ShareOperationResult<RecordedTransferDownload>, QuarkError> {
        if entry.provider != CloudProvider::Baidu
            || entry.plan.provider != CloudProvider::Baidu
            || created_file_id.is_empty()
            || !created_file_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(QuarkError::InvalidResponse);
        }
        let client = self.client()?;
        let response = client.share_http(
            "GET",
            &format!(
                "/rest/2.0/xpan/multimedia?method=filemetas&web=1&dlink=1&fsids={}",
                encoded(&format!("[{created_file_id}]"))
            ),
            None,
            "",
        )?;
        let value = share_json(&response)?;
        let files = value["list"]
            .as_array()
            .filter(|files| files.len() == 1)
            .ok_or(QuarkError::InvalidResponse)?;
        if numeric_field(&files[0], "fs_id")? != created_file_id {
            return Err(QuarkError::InvalidResponse);
        }
        let url = required(&files[0], "dlink")?;
        super::share::validate_direct_url(&url)?;
        Ok(ShareOperationResult {
            result: Ok(RecordedTransferDownload::Available(
                super::commands::ResolvedCloudDownload {
                    url,
                    name: entry.entry.name.clone(),
                    headers: super::share::download_headers(
                        &client.session.cookie,
                        &response.set_cookies,
                        BAIDU_PAN_USER_AGENT,
                        None,
                    ),
                },
            )),
            updated_context: None,
        })
    }
    fn transfer(
        &self,
        entry: &CloudShareResolvedEntry,
        store: &CloudSharePlanStore,
    ) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
        let owner = self.transfer_owner_fingerprint(entry)?;
        let client = self.client()?;
        let context = &entry.plan.provider_context;
        let token = client.web_token()?;
        let response = client.share_http(
            "POST",
            &format!(
                "/share/transfer?{}&shareid={}&from={}&ondup=newcopy&async=0",
                write_query(&token, ""),
                numeric_field(context, "shareid")?,
                numeric_field(context, "uk")?
            ),
            Some(form(&[
                (
                    "fsidlist",
                    format!(
                        "[{}]",
                        numeric_field(&json!({"id":entry.entry.entry_id}), "id")?
                    ),
                ),
                ("path", "/".into()),
            ])),
            context["bdclnd"].as_str().unwrap_or(""),
        )?;
        let value = share_json(&response)?;
        let files = value
            .pointer("/extra/list")
            .and_then(Value::as_array)
            .filter(|v| v.len() == 1)
            .ok_or(QuarkError::InvalidResponse)?;
        let file = &files[0];
        if numeric_field(file, "from_fs_id")? != entry.entry.entry_id {
            return Err(QuarkError::InvalidResponse);
        }
        let id = numeric_field(file, "to_fs_id")?;
        if let Some(owner) = owner {
            store.record_transfer_with_owner(
                &entry.plan.source_for(&entry.entry.entry_id),
                &id,
                &owner,
            )?;
        } else {
            store.record_transfer(&entry.plan.source_for(&entry.entry.entry_id), &id)?;
        }
        validate_path(&required(file, "to")?)?;
        // Share fallback must not pass the personal file metadata through
        // baidu_json's legacy body-wide challenge scan: filenames are data.
        let response = client.share_http(
            "GET",
            &format!(
                "/rest/2.0/xpan/multimedia?method=filemetas&web=1&dlink=1&fsids={}",
                encoded(&format!("[{id}]"))
            ),
            None,
            "",
        )?;
        let value = share_json(&response)?;
        let files = value["list"]
            .as_array()
            .filter(|files| files.len() == 1)
            .ok_or(QuarkError::InvalidResponse)?;
        if numeric_field(&files[0], "fs_id")? != id {
            return Err(QuarkError::InvalidResponse);
        }
        let url = required(&files[0], "dlink")?;
        super::share::validate_direct_url(&url)?;
        Ok(super::commands::ResolvedCloudDownload {
            url,
            name: entry.entry.name.clone(),
            headers: super::share::download_headers(
                &client.session.cookie,
                &response.set_cookies,
                BAIDU_PAN_USER_AGENT,
                None,
            ),
        })
    }
}
pub fn resolve_baidu_share(
    credentials: &dyn super::CredentialStore,
    http: &dyn CloudHttpTransport,
    entry: &CloudShareResolvedEntry,
) -> Result<ShareResolution, QuarkError> {
    let adapter = BaiduShareAdapter { credentials, http };
    let client = adapter.client()?;
    if entry.provider != CloudProvider::Baidu {
        return Err(QuarkError::InvalidResponse);
    }
    let context = &entry.plan.provider_context;
    let bdclnd = context["bdclnd"].as_str().unwrap_or("");
    let response = client.share_http("GET", &format!("/share/tplconfig?surl={}&fields=sign,timestamp&channel=chunlei&web=1&app_id=250528&clienttype=0", encoded(&required(context,"shorturl")?)), None, bdclnd)?;
    let value = share_json(&response)?;
    let sign = required(&value["data"], "sign")?;
    let timestamp = numeric_field(&value["data"], "timestamp")?;
    let response = client.share_http("POST", &format!("/api/sharedownload?app_id=250528&channel=chunlei&clienttype=12&web=1&sign={}&timestamp={timestamp}", encoded(&sign)), Some(form(&[("encrypt","0".into()),("extra",json!({"sekey":context["seckey"].as_str().unwrap_or("")}).to_string()),("fid_list",format!("[{}]",numeric_field(&json!({"id":entry.entry.entry_id}),"id")?)),("primaryid",numeric_field(context,"shareid")?),("uk",numeric_field(context,"uk")?),("product","share".into()),("type","nolimit".into())])), bdclnd)?;
    let value = share_json(&response)?;
    let files = value["list"]
        .as_array()
        .ok_or(QuarkError::InvalidResponse)?;
    // Only an explicit successful download response without a link can request
    // transfer. Empty listings never reach this selected-file operation.
    if files.is_empty() {
        return Ok(ShareResolution::TransferRequired(TransferRequest {
            source: entry.plan.source_for(&entry.entry.entry_id),
        }));
    }
    let url = required(&files[0], "dlink")?;
    super::share::validate_direct_url(&url)?;
    let cookie = if bdclnd.is_empty() {
        client.session.cookie.clone()
    } else {
        format!("{}; BDCLND={bdclnd}", client.session.cookie)
    };
    Ok(ShareResolution::Direct(
        super::commands::ResolvedCloudDownload {
            url,
            name: entry.entry.name.clone(),
            headers: super::share::download_headers(
                &cookie,
                &response.set_cookies,
                BAIDU_PAN_USER_AGENT,
                None,
            ),
        },
    ))
}
