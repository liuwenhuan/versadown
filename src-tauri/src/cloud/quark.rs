use super::{
    http::response_json,
    types::{encoded, required},
    *,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::share::*;
    use std::sync::Mutex;

    struct Http {
        replies: Mutex<std::collections::VecDeque<Value>>,
        requests: Mutex<Vec<CloudHttpRequest>>,
    }
    impl Http {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
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
            provider: CloudProvider::Quark,
            url: "https://pan.quark.cn/s/share123".into(),
            access_code: Some("a123".into()),
        }
    }
    fn listing() -> Vec<Value> {
        vec![
            json!({"status":200,"code":0,"data":{"stoken":"secret-token"}}),
            json!({"status":200,"code":0,"data":{"list":[{"fid":"11","file_name":"movie.mp4","dir":false,"size":123,"share_fid_token":"file-secret"}]},"metadata":{"_total":1}}),
        ]
    }
    #[test]
    fn quark_share_direct_requests_file_download_with_share_context() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let mut replies = listing();
        replies.push(json!({"status":200,"code":0,"data":[{"fid":"11","download_url":"https://download.quark.cn/movie?sign=secret"}]}));
        let http = Http::new(replies);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!("plan required")
        };
        let entry = CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        };
        let ShareResolution::Direct(download) = adapter.resolve(&entry).unwrap() else {
            panic!("direct required")
        };
        assert_eq!(download.url, "https://download.quark.cn/movie?sign=secret");
        assert!(download
            .headers
            .contains(&("Cookie".into(), "sid=current".into())));
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[2].method, "POST");
        assert_eq!(
            requests[2].url,
            "https://drive-pc.quark.cn/1/clouddrive/file/download?pr=ucpro&fr=pc"
        );
        assert_eq!(requests[2].headers["User-Agent"], QUARK_DESKTOP_USER_AGENT);
        let token_body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(token_body["passcode"], "a123");
        let download_body: Value =
            serde_json::from_str(requests[2].body.as_ref().unwrap()).unwrap();
        assert_eq!(download_body["fids"], json!(["11"]));
        assert_eq!(download_body["pwd_id"], "share123");
        assert_eq!(download_body["stoken"], "secret-token");
        assert_eq!(download_body["fids_token"], json!(["file-secret"]));
    }
    #[test]
    fn quark_share_download_cookie_rotation_persists_for_following_directory_entry() {
        struct CookieHttp(std::sync::Mutex<std::collections::VecDeque<CloudHttpResponse>>);
        impl CloudHttpTransport for CookieHttp {
            fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
                self.0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or(QuarkError::InvalidResponse)
            }
        }
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let http = CookieHttp(std::sync::Mutex::new(
            [
                CloudHttpResponse {
                    status: 200,
                    body: json!({"code":0,"data":[{"fid":"f1","download_url":"https://download.quark.cn/first"}]}).to_string(),
                    set_cookies: vec!["__puus=rotated; Path=/; Secure".into()],
                },
                CloudHttpResponse {
                    status: 200,
                    body: json!({"code":0,"data":[{"fid":"f2","download_url":"https://download.quark.cn/second"}]}).to_string(),
                    set_cookies: vec![],
                },
            ]
            .into(),
        ));
        let entries = vec![
            CloudShareEntry::new("share123", "f1".into(), "first.bin".into(), vec![], Some(1)),
            CloudShareEntry::new(
                "share123",
                "f2".into(),
                "second.bin".into(),
                vec![],
                Some(1),
            ),
        ];
        let plan = CloudSharePlan::new(
            CloudProvider::Quark,
            "share123".into(),
            None,
            entries.clone(),
            json!({"stoken":"share-token","files":{"f1":{"token":"first-token"},"f2":{"token":"second-token"}}}),
        );
        let first = CloudShareResolvedEntry {
            provider: CloudProvider::Quark,
            plan: plan.clone(),
            entry: entries[0].clone(),
        };
        let second = CloudShareResolvedEntry {
            provider: CloudProvider::Quark,
            plan,
            entry: entries[1].clone(),
        };

        assert!(matches!(
            resolve_quark_share(&credentials, &http, &first),
            Ok(ShareResolution::Direct(_))
        ));
        let Ok(ShareResolution::Direct(second_download)) =
            resolve_quark_share(&credentials, &http, &second)
        else {
            panic!("the second directory entry must resolve directly")
        };
        assert!(second_download
            .headers
            .iter()
            .any(|(name, value)| name == "Cookie" && value.contains("__puus=rotated")));
    }
    #[test]
    fn quark_share_without_login_does_not_request_http() {
        let credentials = MemoryCredentialStore::default();
        let http = Http::new(vec![]);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
        assert!(matches!(
            adapter.inspect(&candidate()),
            Err(QuarkError::NotAuthorized)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
    }
    #[test]
    fn quark_share_access_code_required_and_wrong_code_are_distinct() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let http = Http::new(vec![
            json!({"status":400,"code":41003,"message":"提取码错误"}),
            json!({"status":400,"code":41003,"message":"提取码错误"}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
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
    }
    #[test]
    fn quark_share_requires_an_access_code_even_when_the_server_uses_http_400() {
        struct Http400;
        impl CloudHttpTransport for Http400 {
            fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
                Ok(CloudHttpResponse {
                    status: 400,
                    body: json!({"code":41008,"message":"请输入提取码"}).to_string(),
                    set_cookies: vec![],
                })
            }
        }
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let http = Http400;
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
        let mut missing_code = candidate();
        missing_code.access_code = None;

        assert!(matches!(
            adapter.inspect(&missing_code),
            Ok(ShareInspection::AccessCodeRequired)
        ));
    }
    #[test]
    fn quark_share_preserves_cancelled_and_blocked_failures_for_the_parse_ui() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        for (reply, expected) in [
            (
                json!({"code":41012,"message":"好友已取消了分享"}),
                "夸克分享已被取消",
            ),
            (
                json!({"code":41031,"message":"分享者用户封禁链接查看受限"}),
                "夸克分享因账号封禁而无法访问",
            ),
        ] {
            let http = Http::new(vec![reply]);
            let adapter =
                provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();

            assert!(matches!(
                adapter.inspect(&candidate()),
                Err(QuarkError::Provider(message)) if message == expected
            ));
        }
    }
    #[test]
    fn quark_share_pages_nested_paths_and_empty_folder_stays_empty() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let http = Http::new(vec![
            listing()[0].clone(),
            json!({"code":0,"data":{"list":[{"fid":"folder","file_name":"season","dir":true}]},"metadata":{"_total":2}}),
            listing()[1].clone(),
            json!({"code":0,"data":{"list":[{"fid":"12","file_name":"episode.mp4","dir":false,"share_fid_token":"nested"}]},"metadata":{"_total":1}}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[1].relative_directory, vec!["season"]);
        assert!(http.requests.lock().unwrap()[2].url.contains("_page=2"));
        let empty = Http::new(vec![
            listing()[0].clone(),
            json!({"code":0,"data":{"list":[]},"metadata":{"_total":0}}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &empty).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        assert!(plan.entries.is_empty());
        assert_eq!(empty.requests.lock().unwrap().len(), 2);
    }
    #[test]
    fn quark_share_transfer_records_returned_personal_id_before_download_failure() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let mut replies = listing();
        replies.extend([
            json!({"code":0,"data":[]}),
            json!({"code":0,"data":{"uid":"account-7","nickname":"owner"}}),
            json!({"code":0,"data":{"task_id":"task1"}}),
            json!({"code":0,"data":{"status":2,"save_as":{"save_as_top_fids":["personal99"]}}}),
            json!({"code":9,"message":"secret-server-detail"}),
        ]);
        let http = Http::new(replies);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let dir = tempfile::tempdir().unwrap();
        let store = CloudSharePlanStore::new(DpapiCredentialStore::new(dir.path().into()).unwrap());
        store.upsert(&plan).unwrap();
        let source = plan.source_for("11");
        assert!(matches!(
            resolve_current_share_download(&store, adapter.as_ref(), &source),
            Err(QuarkError::CaptchaRequired)
        ));
        assert_eq!(
            store.resolve_entry(&source).unwrap().plan.transfers[0].created_file_id,
            "personal99"
        );
        let requests = http.requests.lock().unwrap();
        let body: Value = serde_json::from_str(requests[6].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["fids"], json!(["personal99"]));
        assert!(requests[4].url.contains("share/sharepage/save"));
    }
    #[test]
    fn quark_recorded_transfer_refreshes_personal_file_without_share_save() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let mut replies = listing();
        replies.push(json!({"code":0,"data":[{"fid":"personal99","download_url":"https://download.quark.cn/personal"}]}));
        let http = Http::new(replies);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
            panic!()
        };
        let entry = CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        };

        let refreshed = adapter
            .refresh_recorded_transfer(&entry, "personal99")
            .unwrap();

        let Ok(RecordedTransferDownload::Available(download)) = refreshed.result else {
            panic!("a personal direct link is required")
        };
        assert_eq!(download.url, "https://download.quark.cn/personal");
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].url.contains("file/download"));
        assert!(!requests
            .iter()
            .any(|request| request.url.contains("sharepage/save")));
    }
    #[test]
    fn quark_transfer_owner_uses_uid_not_rotating_session_cookie() {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save("quark", b"__uid=account-7; sid=before")
            .unwrap();
        let http = Http::new(listing());
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
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
            .save("quark", b"__uid=account-7; sid=renewed")
            .unwrap();
        let after = adapter.transfer_owner_fingerprint(&entry).unwrap();

        assert_eq!(before, after);
    }
    #[test]
    fn quark_share_errors_never_become_transfer_requests() {
        for error in [
            json!({"code":9}),
            json!({"code":401}),
            json!({"code":500,"error":"https://secret.invalid/?token=secret"}),
            json!({"code":0,"data":{}}),
        ] {
            let credentials = MemoryCredentialStore::default();
            credentials.save("quark", b"sid=current").unwrap();
            let mut replies = listing();
            replies.push(error);
            let http = Http::new(replies);
            let adapter =
                provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
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
    #[test]
    fn quark_share_conflicting_error_codes_never_trigger_save() {
        for error in [
            json!({"error_code":0,"code":401,"data":[]}),
            json!({"error_code":0,"code":500,"data":[]}),
            json!({"error_code":"invalid","code":0,"data":[]}),
            json!({"error_code":0,"code":"invalid","data":[]}),
        ] {
            let credentials = MemoryCredentialStore::default();
            credentials.save("quark", b"sid=current").unwrap();
            let mut replies = listing();
            replies.push(error);
            let http = Http::new(replies);
            let adapter =
                provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
            let ShareInspection::Plan(plan) = adapter.inspect(&candidate()).unwrap() else {
                panic!()
            };
            let dir = tempfile::tempdir().unwrap();
            let store =
                CloudSharePlanStore::new(DpapiCredentialStore::new(dir.path().into()).unwrap());
            store.upsert(&plan).unwrap();
            let source = plan.source_for("11");
            assert!(matches!(
                resolve_current_share_download(&store, adapter.as_ref(), &source),
                Err(QuarkError::InvalidResponse | QuarkError::Provider(_))
            ));
            assert!(store
                .resolve_entry(&source)
                .unwrap()
                .plan
                .transfers
                .is_empty());
            assert_eq!(http.requests.lock().unwrap().len(), 3);
        }
    }
    #[test]
    fn quark_share_direct_accepts_valid_http_url() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"sid=current").unwrap();
        let mut replies = listing();
        replies.push(json!({"code":0,"data":[{"fid":"11","download_url":"http://download.quark.cn/movie?sign=secret"}]}));
        let http = Http::new(replies);
        let adapter = provider_share_adapter(CloudProvider::Quark, &credentials, &http).unwrap();
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
        assert_eq!(download.url, "http://download.quark.cn/movie?sign=secret");
    }
    #[test]
    fn quark_download_parser_accepts_http_and_rejects_unsafe_urls() {
        for url in [
            "http://download.quark.cn/file?sign=secret",
            "https://download.quark.cn/file",
            "https://download.quark.cn/path@part?email=user@example.com",
        ] {
            let body = json!({"data":[{"download_url":url}]}).to_string();
            assert_eq!(parse_quark_download_url(&body).unwrap(), url);
        }
        for url in [
            "ftp://download.quark.cn/file",
            "file:///tmp/file",
            "https://user:secret@download.quark.cn/file",
            "https://@download.quark.cn/file",
            "https:\n//@download.quark.cn/file",
            "http://user@download.quark.cn/file",
            "not a URL",
            "https://pan.quark.cn/s/share123",
        ] {
            let body = json!({"data":[{"download_url":url}]}).to_string();
            assert!(
                matches!(
                    parse_quark_download_url(&body),
                    Err(QuarkError::InvalidResponse)
                ),
                "unsafe URL accepted"
            );
        }
    }
}

pub const QUARK_DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) quark-cloud-drive/2.5.56 Chrome/100.0.4896.160 Electron/18.3.5.12-a038f7b798 Safari/537.36 Channel/pckk_other_ch";
pub(crate) const QUARK_RECYCLE_BIN_ID: &str = "__all_downloader_quark_recycle_bin__";
const QUARK_RECYCLE_BIN_NAME: &str = "回收站";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct QuarkFile {
    pub id: String,
    pub name: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub updated_at: Option<String>,
}
pub fn parse_quark_files(body: &str) -> Result<Vec<QuarkFile>, QuarkError> {
    let v: Value = serde_json::from_str(body).map_err(|_| QuarkError::InvalidResponse)?;
    v.pointer("/data/list")
        .and_then(Value::as_array)
        .ok_or(QuarkError::InvalidResponse)?
        .iter()
        .map(|x| {
            Ok(QuarkFile {
                id: required(x, "fid")?,
                name: required(x, "file_name")?,
                is_folder: x["dir"]
                    .as_bool()
                    .unwrap_or(x["file_type"].as_i64() == Some(0)),
                size: x["size"].as_u64(),
                updated_at: x.get("updated_at").and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| value.is_number().then(|| value.to_string()))
                }),
            })
        })
        .collect()
}
pub fn parse_quark_download_url(body: &str) -> Result<String, QuarkError> {
    let v: Value = serde_json::from_str(body).map_err(|_| QuarkError::InvalidResponse)?;
    let url = v
        .pointer("/data/0/download_url")
        .and_then(Value::as_str)
        .ok_or(QuarkError::InvalidResponse)?;
    super::share::validate_direct_url(url)?;
    Ok(url.to_owned())
}
pub struct QuarkClient<'a, T: CloudHttpTransport + ?Sized> {
    cookie: String,
    http: &'a T,
}
impl<'a, T: CloudHttpTransport + ?Sized> QuarkClient<'a, T> {
    pub fn new(cookie: String, http: &'a T) -> Self {
        Self { cookie, http }
    }
    fn request_url(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        let headers = BTreeMap::from([
            ("Cookie".into(), self.cookie.clone()),
            ("Origin".into(), "https://pan.quark.cn".into()),
            ("Referer".into(), "https://pan.quark.cn/".into()),
            ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
        ]);
        let r = self.http.send(CloudHttpRequest {
            method,
            url,
            headers,
            body: body.map(|v| v.to_string()),
        })?;
        response_json(&r)?;
        Ok(r)
    }
    fn request(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.request_url(
            method,
            format!(
                "https://drive-pc.quark.cn/1/clouddrive/{path}{}pr=ucpro&fr=pc",
                if path.contains('?') { "&" } else { "?" }
            ),
            body,
        )
    }
    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let r = self.request_url(
            "GET",
            "https://pan.quark.cn/account/info?fr=pc&platform=pc".into(),
            None,
        )?;
        let v = response_json(&r)?;
        let d = &v["data"];
        let id = d
            .get("uid")
            .or_else(|| d.get("user_id"))
            .and_then(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .or_else(|| x.as_u64().map(|n| n.to_string()))
            })
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.cookie.split(';').find_map(|part| {
                    let (name, value) = part.trim().split_once('=')?;
                    (name == "__uid" && !value.is_empty()).then(|| value.to_owned())
                })
            })
            .unwrap_or_else(|| format!("quark-{:x}", md5::compute(self.cookie.as_bytes())));
        Ok(CloudAccount {
            provider: CloudProvider::Quark,
            id,
            display_name: required(d, "nickname")?,
            auth_method: CloudAuthMethod::Web,
        })
    }
    pub fn list(&self, parent: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.list_page(parent, 1)
    }
    fn list_page(&self, parent: &str, page: usize) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "GET",
            &format!(
                "file/sort?pdir_fid={}&_page={page}&_size=200&_sort=file_name:asc",
                encoded(parent)
            ),
            None,
        )
    }
    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        for page in 1..=10000 {
            let r = self.list_page(parent, page)?;
            let v = response_json(&r)?;
            let files = parse_quark_files(&r.body)?;
            let count = files.len();
            for f in files {
                items.push(CloudFile {
                    id: f.id,
                    parent_id: parent.into(),
                    name: f.name,
                    kind: if f.is_folder { "folder" } else { "file" }.into(),
                    size: f.size,
                    modified_at: f.updated_at,
                })
            }
            let total = v.pointer("/metadata/_total").and_then(Value::as_u64);
            if count < 200 || total.is_some_and(|t| items.len() as u64 >= t) {
                if parent == "0" {
                    items.push(CloudFile {
                        id: QUARK_RECYCLE_BIN_ID.into(),
                        parent_id: "0".into(),
                        name: QUARK_RECYCLE_BIN_NAME.into(),
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
    pub fn mkdir(&self, parent: &str, name: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file",
            Some(json!({"pdir_fid":parent,"file_name":name,"dir_path":"","dir_init_lock":false})),
        )
    }
    pub fn rename(&self, id: &str, name: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/rename",
            Some(json!({"fid":id,"file_name":name})),
        )
    }
    pub fn move_files(
        &self,
        ids: &[String],
        parent: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/move",
            Some(json!({"filelist":ids,"to_pdir_fid":parent,"action_type":1})),
        )
    }
    pub fn copy_files(
        &self,
        ids: &[String],
        parent: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/copy",
            Some(json!({"filelist":ids,"to_pdir_fid":parent,"action_type":1})),
        )
    }
    pub fn trash(&self, ids: &[String]) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/delete",
            Some(json!({"action_type":2,"filelist":ids,"exclude_fids":[]})),
        )
    }
    pub fn download_url(&self, id: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.request("POST", "file/download", Some(json!({"fids":[id]})))
    }

    /// Uploads one regular local file using Quark's authenticated CloudDrive
    /// protocol. Object-store authorizations are generated for each transfer
    /// request and remain local to the call.
    pub fn upload_file<U: CloudUploadTransport>(
        &self,
        parent: &str,
        path: &Path,
        upload_http: &U,
    ) -> Result<CloudFile, QuarkError> {
        if parent == QUARK_RECYCLE_BIN_ID {
            return Err(QuarkError::InvalidInput(
                "回收站不能作为上传目标目录".into(),
            ));
        }
        super::upload::upload_cloud_drive_file(
            upload_http,
            parent,
            path,
            |payload| {
                self.request("POST", "file/upload/pre", Some(payload))
                    .and_then(|response| response_json(&response))
            },
            |payload| {
                self.request("POST", "file/update/hash", Some(payload))
                    .and_then(|response| response_json(&response))
            },
            |payload| {
                self.request("POST", "file/upload/auth", Some(payload))
                    .and_then(|response| response_json(&response))
            },
            |payload| {
                self.request("POST", "file/upload/finish", Some(payload))
                    .and_then(|response| response_json(&response))
            },
        )
    }

    pub fn list_recycle_bin(&self) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        for page in 1..=10_000 {
            let response = self.request(
                "GET",
                &format!("file/recycle/list?_page={page}&_size=200"),
                None,
            )?;
            let value = response_json(&response)?;
            let files = value
                .pointer("/data/list")
                .and_then(Value::as_array)
                .ok_or(QuarkError::InvalidResponse)?;
            let count = files.len();
            items.extend(
                files
                    .iter()
                    .map(quark_recycle_file)
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let total = value.pointer("/metadata/_total").and_then(Value::as_u64);
            if count < 200 || total.is_some_and(|total| items.len() as u64 >= total) {
                return Ok(CloudDirectory {
                    id: QUARK_RECYCLE_BIN_ID.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }

    pub fn restore_recycle_entries(&self, record_ids: &[String]) -> Result<(), QuarkError> {
        validate_recycle_record_ids(record_ids)?;
        self.request(
            "POST",
            "file/recycle/restore",
            Some(json!({"select_mode":2,"record_list":record_ids})),
        )?;
        Ok(())
    }

    pub fn delete_recycle_entries(&self, record_ids: &[String]) -> Result<(), QuarkError> {
        validate_recycle_record_ids(record_ids)?;
        self.request(
            "POST",
            "file/recycle/remove",
            Some(json!({"select_mode":2,"record_list":record_ids})),
        )?;
        Ok(())
    }
}

fn quark_recycle_file(file: &Value) -> Result<CloudFile, QuarkError> {
    Ok(CloudFile {
        id: required(file, "record_id")?,
        parent_id: QUARK_RECYCLE_BIN_ID.into(),
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
        modified_at: file.get("updated_at").and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|value| value.to_string()))
        }),
    })
}

fn validate_recycle_record_ids(record_ids: &[String]) -> Result<(), QuarkError> {
    if record_ids.is_empty()
        || record_ids.iter().any(|record_id| {
            record_id.is_empty()
                || record_id.len() > 512
                || record_id.contains(['?', '#', '\r', '\n'])
                || record_id.chars().any(char::is_control)
        })
    {
        return Err(QuarkError::InvalidInput("回收站记录 ID 无效".into()));
    }
    Ok(())
}

use super::share::{
    CloudShareAdapter, CloudShareEntry, CloudSharePlan, CloudSharePlanStore,
    CloudShareResolvedEntry, RecordedTransferDownload, ShareInspection, ShareOperationResult,
    ShareResolution, TransferRequest,
};

pub struct QuarkShareAdapter<'a> {
    pub credentials: &'a dyn CredentialStore,
    pub http: &'a dyn CloudHttpTransport,
}
impl QuarkShareAdapter<'_> {
    fn client(&self) -> Result<QuarkClient<'_, dyn CloudHttpTransport + '_>, QuarkError> {
        let bytes = self
            .credentials
            .load("quark")?
            .ok_or(QuarkError::NotAuthorized)?;
        let cookie = String::from_utf8(bytes).map_err(|_| QuarkError::Storage)?;
        if cookie.trim().is_empty() {
            return Err(QuarkError::NotAuthorized);
        }
        Ok(QuarkClient::new(cookie, self.http))
    }
}

impl<T: CloudHttpTransport + ?Sized> QuarkClient<'_, T> {
    fn stable_account_id(&self) -> Result<Option<String>, QuarkError> {
        if let Some(id) = self.cookie.split(';').find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == "__uid" && !value.is_empty()).then(|| value.to_owned())
        }) {
            return Ok(Some(id));
        }
        let response = self.request_url(
            "GET",
            "https://pan.quark.cn/account/info?fr=pc&platform=pc".into(),
            None,
        )?;
        let value = response_json(&response)?;
        Ok(value["data"]
            .get("uid")
            .or_else(|| value["data"].get("user_id"))
            .and_then(|id| {
                id.as_str()
                    .map(str::to_owned)
                    .or_else(|| id.as_u64().map(|id| id.to_string()))
            })
            .filter(|id| !id.is_empty()))
    }
    fn share_raw(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.http.send(CloudHttpRequest {
            method,
            url: format!(
                "https://drive-pc.quark.cn/1/clouddrive/{path}{}pr=ucpro&fr=pc",
                if path.contains('?') { "&" } else { "?" }
            ),
            headers: BTreeMap::from([
                ("Cookie".into(), self.cookie.clone()),
                ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
                ("Origin".into(), "https://pan.quark.cn".into()),
                ("Referer".into(), "https://pan.quark.cn/".into()),
            ]),
            body: body.map(|value| value.to_string()),
        })
    }
    fn share_request(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        let response = self.share_raw(method, path, body)?;
        let value: Value =
            serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
        validate_share_codes(&value)?;
        response_json(&response).map_err(|error| match error {
            QuarkError::Provider(_) => {
                QuarkError::Provider("夸克分享请求失败，请检查访问码及分享有效期".into())
            }
            error => error,
        })?;
        Ok(response)
    }
    fn share_download(
        &self,
        response: &CloudHttpResponse,
        name: &str,
    ) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
        let url = parse_quark_download_url(&response.body)?;
        Ok(super::commands::ResolvedCloudDownload {
            url,
            name: name.into(),
            headers: super::share::download_headers(
                &self.cookie,
                &response.set_cookies,
                QUARK_DESKTOP_USER_AGENT,
                Some("https://pan.quark.cn/"),
            ),
        })
    }
}

fn persist_quark_download_cookies(
    credentials: &dyn CredentialStore,
    current_cookie: &str,
    response: &CloudHttpResponse,
) -> Result<(), QuarkError> {
    if response.set_cookies.is_empty() {
        return Ok(());
    }
    let merged_cookie = super::share::download_headers(
        current_cookie,
        &response.set_cookies,
        QUARK_DESKTOP_USER_AGENT,
        None,
    )
    .into_iter()
    .find_map(|(name, value)| (name == "Cookie").then_some(value))
    .ok_or(QuarkError::Storage)?;
    credentials.save("quark", merged_cookie.as_bytes())
}

fn validate_share_codes(value: &Value) -> Result<i64, QuarkError> {
    let code = value["code"].as_i64().ok_or(QuarkError::InvalidResponse)?;
    // The generic decoder gives error_code precedence. Sharing must reject
    // malformed or contradictory codes before that precedence can hide errors.
    if let Some(other) = value.get("error_code") {
        if other.as_i64() != Some(code) {
            return Err(QuarkError::InvalidResponse);
        }
    }
    Ok(code)
}

impl CloudShareAdapter for QuarkShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        let client = self.client()?;
        let id = super::share::share_id(candidate, CloudProvider::Quark)?;
        let response = client.share_raw("POST", "share/sharepage/token", Some(json!({"pwd_id":id,"passcode":candidate.access_code.as_deref().unwrap_or(""),"support_visit_limit_private_share":true})))?;
        let raw: Value =
            serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
        let code = validate_share_codes(&raw)?;
        if code != 0
            && (code == 41008
                || matches!(
                    raw["message"].as_str(),
                    Some("提取码错误" | "提取码不正确" | "请输入提取码" | "提取码不能为空")
                ))
        {
            return if candidate.access_code.is_none() {
                Ok(ShareInspection::AccessCodeRequired)
            } else {
                Err(QuarkError::InvalidInput("夸克分享访问码错误".into()))
            };
        }
        let value = response_json(&response).map_err(|error| match error {
            QuarkError::Provider(_) if code == 41012 => {
                QuarkError::Provider("夸克分享已被取消".into())
            }
            QuarkError::Provider(_) if code == 41031 => {
                QuarkError::Provider("夸克分享因账号封禁而无法访问".into())
            }
            QuarkError::Provider(_) => QuarkError::Provider("夸克分享无效或已过期".into()),
            error => error,
        })?;
        let stoken = required(&value["data"], "stoken")?;
        let mut entries = vec![];
        let mut context = json!({"stoken":stoken,"files":{}});
        let mut queue = std::collections::VecDeque::from([("0".to_owned(), Vec::<String>::new())]);
        let mut visited = std::collections::BTreeSet::new();
        while let Some((parent, directory)) = queue.pop_front() {
            if directory.len() > 128 || !visited.insert(parent.clone()) || visited.len() > 10000 {
                return Err(QuarkError::InvalidResponse);
            }
            let mut count = 0;
            for page in 1..=10000 {
                let response = client.share_request("GET", &format!("share/sharepage/detail?pwd_id={}&stoken={}&pdir_fid={}&_page={page}&_size=200&_fetch_total=1&force=0", encoded(&id), encoded(&stoken), encoded(&parent)), None)?;
                let value = response_json(&response)?;
                let files = parse_quark_files(&response.body)?;
                let size = files.len();
                count += size;
                for (file, raw) in files.into_iter().zip(
                    value["data"]["list"]
                        .as_array()
                        .ok_or(QuarkError::InvalidResponse)?,
                ) {
                    if file.is_folder {
                        let mut relative = directory.clone();
                        relative.push(file.name);
                        queue.push_back((file.id, relative));
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
                    .map(|total| count < total as usize)
                    .unwrap_or(size == 200);
                if !more {
                    break;
                }
                if size == 0 || page == 10000 {
                    return Err(QuarkError::InvalidResponse);
                }
            }
        }
        Ok(ShareInspection::Plan(CloudSharePlan::new(
            CloudProvider::Quark,
            id,
            candidate.access_code.clone(),
            entries,
            context,
        )))
    }
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        resolve_quark_share(self.credentials, self.http, entry)
    }
    fn transfer_owner_fingerprint(
        &self,
        _: &CloudShareResolvedEntry,
    ) -> Result<Option<String>, QuarkError> {
        self.client()?
            .stable_account_id()?
            .map(|id| super::share::stable_owner_fingerprint(CloudProvider::Quark, &id))
            .transpose()
    }
    fn refresh_recorded_transfer(
        &self,
        entry: &CloudShareResolvedEntry,
        created_file_id: &str,
    ) -> Result<ShareOperationResult<RecordedTransferDownload>, QuarkError> {
        if entry.provider != CloudProvider::Quark
            || entry.plan.provider != CloudProvider::Quark
            || created_file_id.is_empty()
        {
            return Err(QuarkError::InvalidResponse);
        }
        let client = self.client()?;
        let response = client.share_raw(
            "POST",
            "file/download",
            Some(json!({"fids":[created_file_id]})),
        )?;
        let value: Value =
            serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
        validate_share_codes(&value)?;
        response_json(&response).map_err(|error| match error {
            QuarkError::Provider(_) => QuarkError::Provider("夸克个人盘文件请求失败".into()),
            error => error,
        })?;
        Ok(ShareOperationResult {
            result: persist_quark_download_cookies(self.credentials, &client.cookie, &response)
                .and_then(|_| client.share_download(&response, &entry.entry.name))
                .map(RecordedTransferDownload::Available),
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
        let file = &context["files"][&entry.entry.entry_id];
        let response = client.share_request("POST", "share/sharepage/save", Some(json!({"fid_list":[entry.entry.entry_id],"fid_token_list":[required(file,"token")?],"to_pdir_fid":"0","pwd_id":entry.plan.canonical_share_id,"stoken":required(context,"stoken")?,"pdir_fid":required(file,"parent")?,"scene":"link","pdir_save_all":false,"exclude_fids":[]})))?;
        let value = response_json(&response)?;
        let task = required(&value["data"], "task_id")?;
        for retry in 0..20 {
            let response = client.share_request(
                "GET",
                &format!("task?task_id={}&retry_index={retry}", encoded(&task)),
                None,
            )?;
            let value = response_json(&response)?;
            match value["data"]["status"].as_i64() {
                Some(2) => {
                    let ids = value
                        .pointer("/data/save_as/save_as_top_fids")
                        .and_then(Value::as_array)
                        .filter(|ids| ids.len() == 1)
                        .ok_or(QuarkError::InvalidResponse)?;
                    let id = ids[0]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or(QuarkError::InvalidResponse)?;
                    if let Some(owner) = &owner {
                        store.record_transfer_with_owner(
                            &entry.plan.source_for(&entry.entry.entry_id),
                            id,
                            owner,
                        )?;
                    } else {
                        store.record_transfer(&entry.plan.source_for(&entry.entry.entry_id), id)?;
                    }
                    let response = client.share_request(
                        "POST",
                        "file/download",
                        Some(json!({"fids":[id]})),
                    )?;
                    persist_quark_download_cookies(self.credentials, &client.cookie, &response)?;
                    return client.share_download(&response, &entry.entry.name);
                }
                Some(0 | 1) => std::thread::sleep(std::time::Duration::from_millis(250)),
                _ => return Err(QuarkError::Provider("夸克转存任务失败".into())),
            }
        }
        Err(QuarkError::Provider("夸克转存尚未完成，请稍后重试".into()))
    }
}

pub fn resolve_quark_share(
    credentials: &dyn CredentialStore,
    http: &dyn CloudHttpTransport,
    entry: &CloudShareResolvedEntry,
) -> Result<ShareResolution, QuarkError> {
    let adapter = QuarkShareAdapter { credentials, http };
    let client = adapter.client()?;
    if entry.provider != CloudProvider::Quark {
        return Err(QuarkError::InvalidResponse);
    }
    let context = &entry.plan.provider_context;
    let response = client.share_request("POST", "file/download", Some(json!({"pwd_id":entry.plan.canonical_share_id,"stoken":required(context,"stoken")?,"fids":[entry.entry.entry_id],"fids_token":[required(&context["files"][&entry.entry.entry_id],"token")?]})))?;
    persist_quark_download_cookies(credentials, &client.cookie, &response)?;
    let value = response_json(&response)?;
    let files = value["data"]
        .as_array()
        .ok_or(QuarkError::InvalidResponse)?;
    // This is the successful download response for one already-selected file,
    // never a directory listing or an authorization/provider error.
    if files.is_empty() {
        return Ok(ShareResolution::TransferRequired(TransferRequest {
            source: entry.plan.source_for(&entry.entry.entry_id),
        }));
    }
    Ok(ShareResolution::Direct(
        client.share_download(&response, &entry.entry.name)?,
    ))
}
