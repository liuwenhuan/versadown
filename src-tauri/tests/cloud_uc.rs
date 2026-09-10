use alldownloader_lib::cloud::{
    uc::{parse_uc_web_session, UcClient, UcSession, UC_DESKTOP_USER_AGENT},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError,
};
use std::{collections::VecDeque, sync::Mutex};

struct RecordingHttp {
    responses: Mutex<VecDeque<CloudHttpResponse>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
}

impl RecordingHttp {
    fn sequence(bodies: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            responses: Mutex::new(
                bodies
                    .into_iter()
                    .map(|body| CloudHttpResponse {
                        status: 200,
                        body: body.into(),
                        ..Default::default()
                    })
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<CloudHttpRequest> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

impl CloudHttpTransport for RecordingHttp {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(QuarkError::InvalidResponse)
    }
}

#[test]
fn uc_restores_only_session_cookie_pairs_from_the_official_web_flow() {
    // Break caught: serializing browser cookie attributes or accepting a missing
    // authenticated UC session makes restored credentials either unsafe or unusable.
    let session = parse_uc_web_session(&[
        "__uid=user-42; Path=/; Secure; HttpOnly".into(),
        "__pus=identity; Path=/; Secure".into(),
        "__puus=session; Path=/; Secure; HttpOnly".into(),
        "tracking=discard; Path=/".into(),
    ])
    .unwrap();

    assert_eq!(
        session.cookie,
        "__uid=user-42; __pus=identity; __puus=session"
    );
    assert!(serde_json::to_string(&session)
        .unwrap()
        .contains("__puus=session"));
    assert!(parse_uc_web_session(&["__uid=user-42; Path=/".into()]).is_err());
}

#[test]
fn uc_web_authorization_waits_until_both_session_cookies_arrive() {
    // Incomplete browser cookies must keep authorization polling alive while
    // the user signs in, rather than terminate it as an expired saved session.
    for (case, cookies) in [
        ("empty", vec![]),
        ("only __uid", vec!["__uid=user-42; Path=/".into()]),
        ("only __pus", vec!["__pus=identity; Path=/".into()]),
        ("only __puus", vec!["__puus=session; Path=/".into()]),
    ] {
        let result = parse_uc_web_session(&cookies);
        assert!(
            matches!(result, Err(QuarkError::AuthorizationPending)),
            "{case}: expected AuthorizationPending, got {result:?}"
        );
    }
}

#[test]
fn uc_web_session_creates_an_account_without_requiring_an_internal_member_request() {
    // A successful official browser login has already issued the session
    // cookies. Requiring an additional undocumented member endpoint before
    // closing that browser window leaves users stuck in the authorization flow
    // when that probe is rejected, even though directory requests can work.
    let http = RecordingHttp::sequence([]);
    let account = UcClient::new(
        UcSession::cookie("__uid=user-42; __pus=identity; __puus=session"),
        &http,
    )
    .web_account()
    .unwrap();

    assert_eq!(account.id, "user-42");
    assert_eq!(account.display_name, "UC网盘");
    assert!(http.requests().is_empty());
}

#[test]
fn uc_lists_root_and_nested_directories_with_the_uc_cookie_contract() {
    // Break caught: changing the UC host/client marker or forgetting pdir_fid
    // silently returns another product's data or an unauthenticated response.
    let http = RecordingHttp::sequence([
        r#"{"code":0,"status":200,"data":{"list":[{"fid":"folder-1","file_name":"movies","dir":true},{"fid":"file-1","file_name":"movie.mkv","dir":false,"size":12}]},"metadata":{"_total":2}}"#,
        r#"{"code":0,"status":200,"data":{"list":[{"fid":"file-2","file_name":"episode.mkv","dir":false,"size":24}]},"metadata":{"_total":1}}"#,
    ]);
    let client = UcClient::new(UcSession::cookie("__pus=identity; __puus=session"), &http);

    let root = client.directory("0").unwrap();
    let nested = client.directory("folder-1").unwrap();

    assert_eq!(root.items[0].name, "movies");
    assert_eq!(root.items[1].size, Some(12));
    assert_eq!(nested.items[0].parent_id, "folder-1");
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(request
            .url
            .starts_with("https://pc-api.uc.cn/1/clouddrive/file/sort?"));
        assert!(request.url.contains("pr=UCBrowser"));
        assert!(request.url.contains("fr=pc"));
        assert_eq!(request.headers["Cookie"], "__pus=identity; __puus=session");
        assert_eq!(request.headers["Origin"], "https://drive.uc.cn");
        assert_eq!(request.headers["Referer"], "https://drive.uc.cn/");
        assert_eq!(request.headers["User-Agent"], UC_DESKTOP_USER_AGENT);
    }
    assert!(requests[0].url.contains("pdir_fid=0"));
    assert!(requests[1].url.contains("pdir_fid=folder-1"));
}

#[test]
fn uc_resolves_fresh_https_download_without_putting_it_in_the_session() {
    // Break caught: accepting a non-HTTPS dlink or retaining the signed URL in
    // durable session state leaks a temporary credential into future task state.
    let http = RecordingHttp::sequence([
        r#"{"code":0,"status":200,"data":[{"fid":"file-1","file_name":"movie.mkv","download_url":"https://download.example/movie.mkv?signature=temporary"}]}"#,
        r#"{"code":0,"status":200,"data":[{"fid":"file-2","download_url":"http://download.example/insecure"}]}"#,
    ]);
    let session = UcSession::cookie("__uid=user-42; __pus=identity; __puus=session");
    let client = UcClient::new(session.clone(), &http);

    let download = client.download("file-1").unwrap();

    assert_eq!(download.name, "movie.mkv");
    assert_eq!(
        download.url,
        "https://download.example/movie.mkv?signature=temporary"
    );
    assert_eq!(download.headers["Cookie"], session.cookie);
    assert!(!serde_json::to_string(&session)
        .unwrap()
        .contains("download.example"));
    assert!(client.download("file-2").is_err());
    let requests = http.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].url,
        "https://pc-api.uc.cn/1/clouddrive/file/download?entry=ft&fr=pc&pr=UCBrowser"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[0].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"fids":["file-1"]})
    );
}

#[test]
fn uc_account_uses_the_member_endpoint_and_stable_uid_cookie() {
    // Break caught: treating capacity metadata as an account id breaks restored
    // session identity when UC's member response does not include a nickname.
    let http = RecordingHttp::sequence([
        r#"{"code":0,"status":200,"data":{"total_capacity":100,"use_capacity":1,"member_type":"NORMAL"}}"#,
    ]);
    let account = UcClient::new(
        UcSession::cookie("__uid=user-42; __pus=identity; __puus=session"),
        &http,
    )
    .account()
    .unwrap();

    assert_eq!(account.id, "user-42");
    assert_eq!(account.display_name, "UC网盘");
    let request = http.requests().pop().unwrap();
    assert_eq!(request.method, "GET");
    assert!(request
        .url
        .starts_with("https://pc-api.uc.cn/1/clouddrive/member?"));
    assert!(request.url.contains("pr=UCBrowser"));
    assert!(request.url.contains("fr=pc"));
}

#[test]
fn uc_file_mutations_keep_the_uc_endpoint_and_payload_contract() {
    // Break caught: translating a UC mutation to another drive's endpoint or
    // action shape risks moving/deleting the wrong files after authentication.
    let http = RecordingHttp::sequence([
        r#"{"code":0,"status":200,"data":{"fid":"folder-2"}}"#,
        r#"{"code":0,"status":200,"data":{}}"#,
        r#"{"code":0,"status":200,"data":{"task_id":"move-task"}}"#,
        r#"{"code":0,"status":200,"data":{"task_id":"copy-task"}}"#,
        r#"{"code":0,"status":200,"data":{"task_id":"trash-task"}}"#,
    ]);
    let client = UcClient::new(UcSession::cookie("__pus=identity; __puus=session"), &http);
    let ids = vec!["file-1".to_owned(), "folder-1".to_owned()];

    client.mkdir("0", "new-folder").unwrap();
    client.rename("file-1", "renamed.mkv").unwrap();
    client.move_files(&ids, "target").unwrap();
    client.copy_files(&ids, "target").unwrap();
    client.trash(&ids).unwrap();

    let requests = http.requests();
    assert_eq!(requests.len(), 5);
    assert!(requests[0].url.contains("/file?pr=UCBrowser&fr=pc"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[0].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"pdir_fid":"0","file_name":"new-folder","dir_path":"","dir_init_lock":false})
    );
    assert!(requests[1].url.contains("/file/rename?pr=UCBrowser&fr=pc"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[1].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"fid":"file-1","file_name":"renamed.mkv"})
    );
    assert!(requests[2].url.contains("/file/move?pr=UCBrowser&fr=pc"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[2].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"filelist":["file-1","folder-1"],"to_pdir_fid":"target","action_type":1})
    );
    assert!(requests[3].url.contains("/file/copy?pr=UCBrowser&fr=pc"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[3].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"filelist":["file-1","folder-1"],"to_pdir_fid":"target","action_type":1})
    );
    assert!(requests[4].url.contains("/file/delete?pr=UCBrowser&fr=pc"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(requests[4].body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"action_type":2,"filelist":["file-1","folder-1"],"exclude_fids":[]})
    );
}
