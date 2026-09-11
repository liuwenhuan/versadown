use super::{
    types::{encoded, required},
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudHttpRequest, CloudHttpResponse,
    CloudHttpTransport, CloudProvider, CloudUploadTransport, QuarkError, UploadHttpRequest,
    UploadHttpResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use zeroize::Zeroizing;

const CLOUD: &str = "https://cloud.189.cn";
const LOGIN: &str = "https://open.e.189.cn";
pub const TIANYI_RECYCLE_BIN_ID: &str = "__all_downloader_tianyi_recycle_bin__";
const TIANYI_RECYCLE_BIN_NAME: &str = "回收站";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{
        share::{test_support::*, *},
        CredentialStore, MemoryCredentialStore,
    };
    fn credentials() -> MemoryCredentialStore {
        let store = MemoryCredentialStore::default();
        store
            .save(
                "tianyi",
                &serde_json::to_vec(&TianyiSession::cookie(
                    "COOKIE_LOGIN_USER=user; SESSION=old",
                ))
                .unwrap(),
            )
            .unwrap();
        store
    }
    fn info() -> Value {
        json!({"res_code":0,"shareId":22,"fileId":"root","fileName":"root","isFolder":true,"needAccessCode":1,"shareMode":1})
    }
    fn checked() -> Value {
        json!({"res_code":0,"shareId":22})
    }

    #[test]
    fn tianyi_web_session_keeps_cloud_scoped_cookie_needed_by_share_downloads() {
        // Break caught: list operations can authenticate with the SSO cookie
        // alone while the share-download endpoint also requires a cloud-host
        // session cookie issued by the official page.
        let session = parse_tianyi_web_session(&[
            "COOKIE_LOGIN_USER=authorized-user".into(),
            "JSESSIONID=cloud-download-session".into(),
        ])
        .expect("an official Tianyi web session");

        assert!(session.cookie.contains("COOKIE_LOGIN_USER=authorized-user"));
        assert!(session.cookie.contains("JSESSIONID=cloud-download-session"));
    }

    #[test]
    fn tianyi_share_prompts_for_access_code_before_requiring_hidden_share_id() {
        let store = credentials();
        // The live endpoint omits shareId until an access code has been
        // supplied. The user should still receive the access-code prompt.
        let protected = json!({
            "res_code": 0,
            "fileId": "protected-file",
            "fileName": "protected.zip",
            "isFolder": false,
            "needAccessCode": 1,
            "shareMode": 1
        });
        let http = Http::new(vec![protected]);
        let mut input = candidate(CloudProvider::Tianyi);
        input.access_code = None;

        let result = provider_share_adapter(CloudProvider::Tianyi, &store, &http)
            .unwrap()
            .inspect(&input);

        assert!(matches!(result, Ok(ShareInspection::AccessCodeRequired)));
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn tianyi_share_accepts_zero_count_directory_without_list_fields() {
        let store = credentials();
        let directory = json!({
            "res_code": 0,
            "shareId": 22,
            "fileId": "empty-root",
            "fileName": "empty",
            "isFolder": true,
            "needAccessCode": 0,
            "shareMode": 1
        });
        // The live API returns exactly this compact empty-directory shape.
        let empty_page = json!({"res_code": 0, "fileListAO": {"count": 0}});
        let http = Http::new(vec![directory, empty_page]);
        let mut input = candidate(CloudProvider::Tianyi);
        input.access_code = None;

        let result = provider_share_adapter(CloudProvider::Tianyi, &store, &http)
            .unwrap()
            .inspect(&input);

        let ShareInspection::Plan(plan) = result.unwrap() else {
            panic!("empty directory should still produce a share plan")
        };
        assert!(matches!(plan.root_kind, CloudShareRootKind::Directory));
        assert!(plan.entries.is_empty());
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }
    #[test]
    fn tianyi_share_validates_code_recurses_pages_and_carries_session_headers() {
        let store = credentials();
        let first = json!({"res_code":0,"fileListAO":{"count":2,"folderList":[{"id":"dir","name":"folder"}],"fileList":[]}});
        let last = json!({"res_code":0,"fileListAO":{"count":2,"folderList":[],"fileList":[{"id":"root-file","name":"root.txt","size":2}]}});
        let child = json!({"res_code":0,"fileListAO":{"count":1,"folderList":[],"fileList":[{"id":"child","name":"verify.mp4","size":7}]}});
        let http = Http::new(vec![
            info(),
            checked(),
            first,
            last,
            child,
            checked(),
            json!({"res_code":0,"fileDownloadUrl":"http://cdn.example/file"}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Tianyi, &store, &http).unwrap();
        let ShareInspection::Plan(plan) =
            adapter.inspect(&candidate(CloudProvider::Tianyi)).unwrap()
        else {
            panic!()
        };
        assert!(matches!(plan.root_kind, CloudShareRootKind::Directory));
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[1].relative_directory, vec!["folder"]);
        let ShareResolution::Direct(direct) = adapter.resolve(&entry(plan)).unwrap() else {
            panic!()
        };
        assert_eq!(direct.url, "http://cdn.example/file");
        assert!(direct.headers.contains(&(
            "Cookie".into(),
            "COOKIE_LOGIN_USER=user; SESSION=issued".into()
        )));
        assert!(direct
            .headers
            .contains(&("User-Agent".into(), TIANYI_WEB_USER_AGENT.into())));
        let requests = http.requests.lock().unwrap();
        assert!(requests[1].url.contains("checkAccessCode.action?"));
        assert!(requests[1].url.contains("accessCode=a123"));
        assert!(requests[3].url.contains("pageNum=2"));
        assert!(requests[4].url.contains("fileId=dir"));
        assert!(requests[6].url.contains("shareId=22"));
        assert!(requests[6].url.contains("getFileDownloadUrl.action?"));
    }
    #[test]
    fn tianyi_share_code_expiry_risk_and_malformed_success_fail_closed() {
        let store = credentials();
        let mut input = candidate(CloudProvider::Tianyi);
        input.access_code = None;
        let http = Http::new(vec![info()]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Tianyi, &store, &http)
                .unwrap()
                .inspect(&input),
            Ok(ShareInspection::AccessCodeRequired)
        ));
        for response in [
            json!({"res_code":"ShareNotFound"}),
            json!({"res_code":0,"result":401}),
            json!({"res_code":"AccessCodeError"}),
            json!({"res_code":0,"errorCode":"Forbidden"}),
            json!({"res_code":-1}),
            json!({"res_code":0,"errorMsg":"captcha required"}),
        ] {
            let http = Http::new(vec![response]);
            assert!(provider_share_adapter(CloudProvider::Tianyi, &store, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Tianyi))
                .is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
        let http = Http::new(vec![
            json!({"res_code":0,"shareId":22,"fileId":"single","fileName":"verify.mp4","isFolder":false,"needAccessCode":0,"shareMode":1}),
            json!({"res_code":0,"fileDownloadUrl":""}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Tianyi, &store, &http).unwrap();
        let ShareInspection::Plan(plan) = adapter.inspect(&input).unwrap() else {
            panic!()
        };
        assert!(adapter.resolve(&entry(plan)).is_err());
    }

    #[test]
    fn tianyi_share_rejects_forbidden_error_text_even_with_a_url() {
        let store = credentials();
        for error in [
            json!({"errorMsg":"forbidden"}),
            json!({"error":"forbidden"}),
            json!({"errorMsg":{"code":"Forbidden"}}),
        ] {
            let mut response = json!({"res_code":0,"fileDownloadUrl":"https://cdn.example/f"});
            response
                .as_object_mut()
                .unwrap()
                .extend(error.as_object().unwrap().clone());
            let http = Http::new(vec![
                json!({"res_code":0,"shareId":22,"fileId":"single","fileName":"verify.mp4","isFolder":false,"needAccessCode":0,"shareMode":1}),
                response,
            ]);
            let adapter = provider_share_adapter(CloudProvider::Tianyi, &store, &http).unwrap();
            let ShareInspection::Plan(plan) =
                adapter.inspect(&candidate(CloudProvider::Tianyi)).unwrap()
            else {
                panic!()
            };
            assert!(adapter.resolve(&entry(plan)).is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn tianyi_share_public_file_download_does_not_require_an_access_code() {
        let store = credentials();
        let http = Http::new(vec![
            json!({"res_code":0,"shareId":22,"fileId":"single","fileName":"verify.mp4","isFolder":false,"needAccessCode":0,"shareMode":1}),
            json!({"res_code":0,"fileDownloadUrl":"https://cdn.example/f"}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Tianyi, &store, &http).unwrap();
        let mut input = candidate(CloudProvider::Tianyi);
        input.access_code = None;
        let ShareInspection::Plan(plan) = adapter.inspect(&input).unwrap() else {
            panic!()
        };
        assert!(matches!(
            plan.root_kind,
            CloudShareRootKind::File { ref entry_id } if entry_id == "single"
        ));
        assert!(matches!(
            adapter.resolve(&entry(plan)),
            Ok(ShareResolution::Direct(_))
        ));
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn tianyi_share_invalid_session_key_requires_reauthorization() {
        let store = credentials();
        let http = Http::new(vec![
            json!({"res_code":0,"shareId":22,"fileId":"single","fileName":"verify.mp4","isFolder":false,"needAccessCode":0,"shareMode":1}),
            json!({"errorCode":"InvalidSessionKey","errorMsg":"session invalid","success":false}),
        ]);
        let adapter = provider_share_adapter(CloudProvider::Tianyi, &store, &http).unwrap();
        let mut input = candidate(CloudProvider::Tianyi);
        input.access_code = None;
        let ShareInspection::Plan(plan) = adapter.inspect(&input).unwrap() else {
            panic!()
        };

        let result = adapter.resolve(&entry(plan));

        assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    }

    #[test]
    fn tianyi_share_rejects_wrong_checked_code_and_repeated_directory_entries() {
        let store = credentials();
        for checked in [
            json!({"res_code":"AccessCodeError"}),
            json!({"res_code":0,"shareId":999}),
        ] {
            let http = Http::new(vec![info(), checked]);
            assert!(provider_share_adapter(CloudProvider::Tianyi, &store, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Tianyi))
                .is_err());
            assert_eq!(http.requests.lock().unwrap().len(), 2);
        }
        let page = json!({"res_code":0,"fileListAO":{"count":2,"folderList":[],"fileList":[{"id":"f1","name":"file.txt","size":7}]}});
        let http = Http::new(vec![info(), checked(), page.clone(), page]);
        assert!(provider_share_adapter(CloudProvider::Tianyi, &store, &http)
            .unwrap()
            .inspect(&candidate(CloudProvider::Tianyi))
            .is_err());
        assert_eq!(http.requests.lock().unwrap().len(), 4);
        let empty = MemoryCredentialStore::default();
        let http = Http::new(vec![]);
        assert!(matches!(
            provider_share_adapter(CloudProvider::Tianyi, &empty, &http)
                .unwrap()
                .inspect(&candidate(CloudProvider::Tianyi)),
            Err(QuarkError::NotAuthorized)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn tianyi_share_preserves_metadata_mode_and_directory_ids_at_every_depth() {
        let store = credentials();
        for mode in [json!(0), json!("2")] {
            let mut metadata = info();
            metadata["shareMode"] = mode.clone();
            let http = Http::new(vec![
                metadata,
                checked(),
                json!({"res_code":0,"fileListAO":{"count":1,"folderList":[{"id":"level-one","name":"one"}],"fileList":[]}}),
                json!({"res_code":0,"fileListAO":{"count":1,"folderList":[{"id":"level-two","name":"two"}],"fileList":[]}}),
                json!({"res_code":0,"fileListAO":{"count":1,"folderList":[],"fileList":[{"id":"leaf","name":"file.txt","size":7}]}}),
            ]);
            let adapter = provider_share_adapter(CloudProvider::Tianyi, &store, &http).unwrap();
            let ShareInspection::Plan(plan) =
                adapter.inspect(&candidate(CloudProvider::Tianyi)).unwrap()
            else {
                panic!()
            };
            assert_eq!(plan.entries[0].relative_directory, vec!["one", "two"]);
            let expected_mode = if mode.is_number() { "0" } else { "2" };
            let requests = http.requests.lock().unwrap();
            for (request, directory) in requests[2..].iter().zip(["root", "level-one", "level-two"])
            {
                let query: BTreeMap<String, String> = url::Url::parse(&request.url)
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
                assert_eq!(
                    query.get("shareMode").map(String::as_str),
                    Some(expected_mode)
                );
                assert_eq!(query.get("fileId").map(String::as_str), Some(directory));
                assert_eq!(
                    query.get("shareDirFileId").map(String::as_str),
                    Some(directory)
                );
                assert_eq!(query.get("shareId").map(String::as_str), Some("22"));
            }
            assert_eq!(requests.len(), 5);
            assert_eq!(
                plan.provider_context["share_mode"],
                if mode.is_number() { json!(0) } else { json!(2) }
            );
            assert_eq!(plan.provider_context["share_dir_file_id"], "root");
        }
    }
}
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
    // The WebView has already scoped these cookies to cloud.189.cn.  Keep the
    // complete valid cookie jar: Tianyi's directory endpoint accepts only SSO
    // cookies in some sessions, but its share-download endpoint can require a
    // cloud-host session cookie such as JSESSIONID as well.
    let mut pairs = BTreeMap::new();
    for raw in cookies {
        let pair = raw.split(';').next().unwrap_or("").trim();
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if valid_cookie_name(name) && !value.is_empty() && !value.chars().any(char::is_control) {
            pairs.insert(name, value);
        }
    }
    if !pairs.contains_key("COOKIE_LOGIN_USER") && !pairs.contains_key("SSON") {
        return Err(QuarkError::AuthorizationExpired);
    }
    Ok(TianyiSession::cookie(
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
        let mut items = files
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
        if parent == "-11" {
            items.push(CloudFile {
                id: TIANYI_RECYCLE_BIN_ID.into(),
                parent_id: parent.into(),
                name: TIANYI_RECYCLE_BIN_NAME.into(),
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

    pub fn list_recycle_bin(&self) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        let mut seen = BTreeSet::new();
        for page in 1..=10_000 {
            let value = self.request(
                "GET",
                "/api/open/file/listRecycleBinFiles.action",
                &format!("pageNum={page}&pageSize=100&iconOption=1&family=false"),
                None,
            )?;
            let files = value
                .get("fileList")
                .or_else(|| value.pointer("/data/fileList"))
                .and_then(Value::as_array)
                .ok_or(QuarkError::InvalidResponse)?;
            let length = files.len();
            for file in files {
                let id = file
                    .get("id")
                    .and_then(string_or_number)
                    .filter(|id| valid_tianyi_recycle_record_id(id))
                    .ok_or(QuarkError::InvalidResponse)?;
                if !seen.insert(id.clone()) {
                    return Err(QuarkError::InvalidResponse);
                }
                items.push(CloudFile {
                    id,
                    parent_id: TIANYI_RECYCLE_BIN_ID.into(),
                    name: required(file, "name").or_else(|_| required(file, "fileName"))?,
                    kind: if bool_value(file.get("isFolder").or_else(|| file.get("folder"))) {
                        "folder"
                    } else {
                        "file"
                    }
                    .into(),
                    size: u64_value(file.get("size").or_else(|| file.get("fileSize"))),
                    modified_at: file
                        .get("lastOpTime")
                        .or_else(|| file.get("lastOpDate"))
                        .and_then(string_or_number),
                });
            }
            if length < 100 {
                return Ok(CloudDirectory {
                    id: TIANYI_RECYCLE_BIN_ID.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }

    pub fn restore_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        let ids = validated_tianyi_recycle_record_ids(ids)?;
        let task_infos = serde_json::to_string(
            &ids.iter()
                .map(|id| json!({ "fileId": id }))
                .collect::<Vec<_>>(),
        )
        .map_err(|_| QuarkError::InvalidResponse)?;
        self.form_request(
            "POST",
            "/api/open/batch/createBatchTask.action",
            &[("type", "RESTORE"), ("taskInfos", &task_infos)],
        )
        .map(|_| ())
    }

    pub fn delete_recycle_entries(&self, ids: &[String]) -> Result<(), QuarkError> {
        let ids = validated_tianyi_recycle_record_ids(ids)?;
        self.request(
            "GET",
            "/v2/deleteFile.action",
            &format!("fileIdList={}", encoded(&ids.join(","))),
            None,
        )
        .map(|_| ())
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
            ("/api/open/file/renameFile.action", "fileId", "destFileName")
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
        let task_infos =
            serde_json::to_string(&task_infos).map_err(|_| QuarkError::InvalidResponse)?;
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

use super::{
    share::{
        CloudShareAdapter, CloudShareEntry, CloudSharePlan, CloudShareResolvedEntry,
        CloudShareRootKind, ShareInspection, ShareResolution,
    },
    CredentialStore,
};

pub struct TianyiShareAdapter<'a> {
    pub credentials: &'a dyn CredentialStore,
    pub http: &'a dyn CloudHttpTransport,
}
struct TianyiShareSession<'a> {
    cookie: String,
    http: &'a dyn CloudHttpTransport,
}
impl TianyiShareAdapter<'_> {
    fn session(&self) -> Result<TianyiShareSession<'_>, QuarkError> {
        let bytes = self
            .credentials
            .load("tianyi")?
            .ok_or(QuarkError::NotAuthorized)?;
        let session: TianyiSession =
            serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if session.cookie.trim().is_empty() {
            return Err(QuarkError::NotAuthorized);
        }
        Ok(TianyiShareSession {
            cookie: session.cookie,
            http: self.http,
        })
    }
}
fn tianyi_share_json(response: &CloudHttpResponse) -> Result<Value, QuarkError> {
    if response.status == 401 {
        return Err(QuarkError::AuthorizationExpired);
    }
    let value: Value =
        serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse)?;
    if !value.is_object() {
        return Err(QuarkError::InvalidResponse);
    }
    // Only protocol error fields can signal a challenge, never filenames or URLs.
    for field in ["errorMsg", "res_message", "message", "error"] {
        if let Some(text) = value[field].as_str() {
            let lower = text.to_ascii_lowercase();
            if ["captcha", "risk", "安全验证", "验证码"]
                .iter()
                .any(|needle| lower.contains(needle))
            {
                return Err(QuarkError::WebAuthorizationRequired);
            }
        }
    }
    if ["errorCode", "error_code"].iter().any(|field| {
        value[*field]
            .as_str()
            .is_some_and(|code| code.eq_ignore_ascii_case("InvalidSessionKey"))
    }) {
        return Err(QuarkError::AuthorizationExpired);
    }
    for key in ["res_code", "result", "errorCode", "error_code"] {
        if let Some(code) = value.get(key) {
            match i64_value(code) {
                Some(0) => {}
                Some(-1 | 401) => return Err(QuarkError::AuthorizationExpired),
                _ => {
                    return Err(QuarkError::Provider(
                        "天翼云盘分享不可用，请检查访问码、有效期及权限".into(),
                    ))
                }
            }
        }
    }
    for key in ["errorMsg", "error"] {
        if let Some(error) = value.get(key) {
            let error = error.as_str().ok_or(QuarkError::InvalidResponse)?;
            if !matches!(
                error.to_ascii_lowercase().as_str(),
                "" | "success" | "ok" | "成功" | "操作成功"
            ) {
                return Err(QuarkError::Provider("天翼云盘分享请求失败".into()));
            }
        }
    }
    if value.get("res_code").is_none() || !(200..300).contains(&response.status) {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(value)
}
impl TianyiShareSession<'_> {
    fn headers(&self) -> BTreeMap<String, String> {
        let mut headers: BTreeMap<_, _> = super::share::download_headers(
            &self.cookie,
            &[],
            TIANYI_WEB_USER_AGENT,
            Some("https://cloud.189.cn/"),
        )
        .into_iter()
        .collect();
        headers.insert("Accept".into(), "application/json;charset=UTF-8".into());
        headers.insert("Origin".into(), CLOUD.into());
        headers
    }
    fn request(&mut self, path: &str, query: &str) -> Result<Value, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method: "GET",
            url: format!("{CLOUD}{path}?{query}"),
            headers: self.headers(),
            body: None,
        })?;
        let value = tianyi_share_json(&response)?;
        self.cookie = super::share::download_headers(
            &self.cookie,
            &response.set_cookies,
            TIANYI_WEB_USER_AGENT,
            None,
        )
        .into_iter()
        .find(|(name, _)| name == "Cookie")
        .ok_or(QuarkError::InvalidResponse)?
        .1;
        Ok(value)
    }
    fn check(&mut self, code: &str, access: &str, expected: &str) -> Result<(), QuarkError> {
        let value = self.request(
            "/api/open/share/checkAccessCode.action",
            &format!("shareCode={}&accessCode={}", encoded(code), encoded(access)),
        )?;
        if value.get("shareId").and_then(string_or_number).as_deref() != Some(expected) {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(())
    }
}
fn tianyi_share_flag(value: &Value) -> Result<bool, QuarkError> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        _ => match i64_value(value) {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(QuarkError::InvalidResponse),
        },
    }
}
impl CloudShareAdapter for TianyiShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        let code = super::share::share_id(candidate, CloudProvider::Tianyi)?;
        let mut session = self.session()?;
        let info = session.request(
            "/api/open/share/getShareInfoByCode.action",
            &format!("shareCode={}", encoded(&code)),
        )?;
        let needs_code = match info.get("needAccessCode") {
            Some(value) => tianyi_share_flag(value)?,
            None => false,
        };
        if needs_code {
            let Some(access) = candidate.access_code.as_deref() else {
                return Ok(ShareInspection::AccessCodeRequired);
            };
            let id = info
                .get("shareId")
                .and_then(string_or_number)
                .filter(|id| !id.is_empty())
                .ok_or(QuarkError::InvalidResponse)?;
            session.check(&code, access, &id)?;
        }
        let id = info
            .get("shareId")
            .and_then(string_or_number)
            .filter(|id| !id.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        let root = info
            .get("fileId")
            .and_then(string_or_number)
            .filter(|id| !id.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        let share_mode = info
            .get("shareMode")
            .and_then(i64_value)
            .filter(|mode| *mode >= 0)
            .ok_or(QuarkError::InvalidResponse)?;
        let context = json!({"share_id":id,"needs_code":needs_code,"share_mode":share_mode,"share_dir_file_id":root});
        let mut entries = vec![];
        let folder = tianyi_share_flag(&info["isFolder"])?;
        let root_kind = if folder {
            CloudShareRootKind::Directory
        } else {
            CloudShareRootKind::File {
                entry_id: root.clone(),
            }
        };
        if !folder {
            entries.push(CloudShareEntry::new(
                &code,
                root,
                required(&info, "fileName")?,
                vec![],
                u64_value(info.get("fileSize")),
            ));
        } else {
            let mut queue = std::collections::VecDeque::from([(root.clone(), vec![])]);
            let mut seen = std::collections::BTreeSet::from([root]);
            let mut requests = 0usize;
            while let Some((parent, directory)) = queue.pop_front() {
                let mut listed = 0u64;
                for page in 1..=10_000 {
                    requests += 1;
                    if requests > 10_000 || directory.len() > 128 {
                        return Err(QuarkError::InvalidResponse);
                    }
                    // Tianyi requires both directory parameters to identify the current
                    // traversal node; neither stays pinned to the original share root.
                    let value = session.request("/api/open/share/listShareDir.action",&format!("shareId={}&fileId={}&shareDirFileId={}&isFolder=true&shareMode={share_mode}&accessCode={}&pageNum={page}&pageSize=100",encoded(&id),encoded(&parent),encoded(&parent),encoded(candidate.access_code.as_deref().unwrap_or(""))))?;
                    let files = tianyi_file_list(&value)?;
                    let length = files.len();
                    listed += length as u64;
                    for (file, as_folder) in files {
                        let fid = file
                            .get("id")
                            .or_else(|| file.get("fileId"))
                            .and_then(string_or_number)
                            .filter(|id| !id.is_empty())
                            .ok_or(QuarkError::InvalidResponse)?;
                        let name =
                            required(file, "name").or_else(|_| required(file, "fileName"))?;
                        if !seen.insert(fid.clone()) {
                            return Err(QuarkError::InvalidResponse);
                        }
                        let folder = as_folder
                            || file
                                .get("isFolder")
                                .map(tianyi_share_flag)
                                .transpose()?
                                .unwrap_or(false);
                        if folder {
                            let mut path = directory.clone();
                            path.push(name);
                            queue.push_back((fid, path));
                        } else {
                            entries.push(CloudShareEntry::new(
                                &code,
                                fid,
                                name,
                                directory.clone(),
                                u64_value(file.get("size").or_else(|| file.get("fileSize"))),
                            ));
                        }
                    }
                    let count = u64_value(value.pointer("/fileListAO/count"));
                    if count.is_some_and(|count| listed >= count)
                        || (count.is_none() && length < 100)
                    {
                        break;
                    }
                    if length == 0 || page == 10_000 {
                        return Err(QuarkError::InvalidResponse);
                    }
                }
            }
        }
        let mut plan = CloudSharePlan::new(
            CloudProvider::Tianyi,
            code,
            candidate.access_code.clone(),
            entries,
            context,
        );
        plan.root_kind = root_kind;
        Ok(ShareInspection::Plan(plan))
    }
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        resolve_tianyi_share(self.credentials, self.http, entry)
    }
}
pub fn resolve_tianyi_share(
    credentials: &dyn CredentialStore,
    http: &dyn CloudHttpTransport,
    entry: &CloudShareResolvedEntry,
) -> Result<ShareResolution, QuarkError> {
    if entry.provider != CloudProvider::Tianyi || entry.plan.provider != CloudProvider::Tianyi {
        return Err(QuarkError::InvalidResponse);
    }
    let adapter = TianyiShareAdapter { credentials, http };
    let mut session = adapter.session()?;
    let id = required(&entry.plan.provider_context, "share_id")?;
    // Protected shares must revalidate their access code; public shares rely on
    // the download API's shareId check without calling a password-only endpoint.
    if entry.plan.provider_context["needs_code"]
        .as_bool()
        .ok_or(QuarkError::InvalidResponse)?
    {
        session.check(
            &entry.plan.canonical_share_id,
            entry.access_code().ok_or(QuarkError::InvalidResponse)?,
            &id,
        )?;
    }
    let value = session.request(
        "/api/open/file/getFileDownloadUrl.action",
        &format!(
            "fileId={}&shareId={}&dt=1",
            encoded(&entry.entry.entry_id),
            encoded(&id)
        ),
    )?;
    let url = required(&value, "fileDownloadUrl")?;
    super::share::validate_direct_url(&url)?;
    Ok(ShareResolution::Direct(
        super::commands::ResolvedCloudDownload {
            url,
            name: entry.entry.name.clone(),
            headers: session.headers().into_iter().collect(),
        },
    ))
}
fn string_or_number(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn valid_tianyi_recycle_record_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn validated_tianyi_recycle_record_ids(ids: &[String]) -> Result<Vec<String>, QuarkError> {
    if ids.is_empty()
        || ids.iter().any(|id| !valid_tianyi_recycle_record_id(id))
        || ids
            .iter()
            .enumerate()
            .any(|(index, id)| ids[..index].contains(id))
    {
        return Err(QuarkError::InvalidInput("回收站记录 ID 无效".into()));
    }
    Ok(ids.to_vec())
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
                value
                    .as_str()
                    .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
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
        let listings = [
            (container.get("fileList"), "file", false),
            (container.get("folderList"), "folder", true),
        ];
        for (listing, nested_name, folder) in listings {
            append_tianyi_entries(&mut items, listing, nested_name, folder);
        }
        // An authenticated but brand-new drive can validly contain neither
        // files nor folders.  Only an explicit empty array is trusted as an
        // empty list; null or scalar values can be malformed API responses.
        let known_empty_listing = listings
            .iter()
            .filter_map(|(listing, _, _)| *listing)
            .all(|listing| listing.as_array().is_some_and(Vec::is_empty));
        let has_listing = listings.iter().any(|(listing, _, _)| listing.is_some());
        let server_confirmed_empty =
            !has_listing && u64_value(container.get("count")).is_some_and(|count| count == 0);
        if !items.is_empty() || (has_listing && known_empty_listing) || server_confirmed_empty {
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
            // XML represents an empty collection as `<fileList/>` rather
            // than `[]`. Normalize only these known collection nodes before
            // directory validation so an arbitrary empty JSON scalar cannot
            // be mistaken for an empty drive.
            if text.is_empty() && matches!(self.name.as_str(), "fileList" | "folderList") {
                return Value::Array(Vec::new());
            }
            return Value::String(text.into());
        }
        if !text.is_empty() {
            self.children
                .insert("#text".into(), Value::String(text.into()));
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
        match reader
            .read_event()
            .map_err(|_| QuarkError::InvalidResponse)?
        {
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
