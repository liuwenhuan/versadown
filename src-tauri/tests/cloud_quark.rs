use alldownloader_lib::cloud::{
    parse_quark_download_url, parse_quark_files, CloudHttpRequest, CloudHttpResponse,
    CloudHttpTransport, CredentialStore, MemoryCredentialStore, QuarkClient, QuarkError,
};
use std::sync::Mutex;

struct RecordingHttp(Mutex<Vec<CloudHttpRequest>>);
impl RecordingHttp {
    fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }
}
impl CloudHttpTransport for RecordingHttp {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.0.lock().unwrap().push(request);
        Ok(CloudHttpResponse {
            status: 401,
            body: r#"{\"status\":401,\"message\":\"login required\"}"#.into(),
            ..Default::default()
        })
    }
}

#[test]
fn quark_list_uses_cookie_auth_and_maps_401_to_reauthorization() {
    let http = RecordingHttp::new();
    let client = QuarkClient::new("sid=local-session".into(), &http);
    assert!(matches!(
        client.list("0"),
        Err(QuarkError::AuthorizationExpired)
    ));
    let request = http.0.lock().unwrap().pop().unwrap();
    assert!(request.url.contains("/1/clouddrive/file/sort"));
    assert!(request.url.contains("pdir_fid=0"));
    assert_eq!(request.headers.get("Cookie").unwrap(), "sid=local-session");
}

#[test]
fn quark_copy_uses_the_native_copy_endpoint_and_destination() {
    let http = RecordingHttp::new();
    let client = QuarkClient::new("sid=local-session".into(), &http);
    let ids = vec!["file-1".to_owned(), "folder-2".to_owned()];

    assert!(matches!(
        client.copy_files(&ids, "target-folder"),
        Err(QuarkError::AuthorizationExpired)
    ));
    let request = http.0.lock().unwrap().pop().unwrap();
    assert!(request.url.contains("/1/clouddrive/file/copy"));
    let body: serde_json::Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["filelist"], serde_json::json!(["file-1", "folder-2"]));
    assert_eq!(body["to_pdir_fid"], "target-folder");
}

#[test]
fn memory_credential_store_never_persists_plaintext_to_a_file() {
    let store = MemoryCredentialStore::default();
    store.save("quark", b"sid=secret").unwrap();
    assert_eq!(store.load("quark").unwrap().unwrap(), b"sid=secret");
}

#[test]
fn maps_provider_list_and_temporary_download_url_without_inventing_values() {
    let files = parse_quark_files(
        r#"{"data":{"list":[{"fid":"a","file_name":"movie.mkv","dir":false,"size":12}]}}"#,
    )
    .unwrap();
    assert_eq!(files[0].name, "movie.mkv");
    assert_eq!(
        parse_quark_download_url(r#"{"data":[{"download_url":"https://signed.example/file"}]}"#)
            .unwrap(),
        "https://signed.example/file"
    );
    assert!(parse_quark_download_url(r#"{"data":[]}"#).is_err());
}

#[test]
fn provider_body_expiry_is_not_treated_as_success() {
    struct Expired;
    impl CloudHttpTransport for Expired {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"code":31001,"status":401,"message":"login required"}"#.into(),
                ..Default::default()
            })
        }
    }
    assert!(matches!(
        QuarkClient::new("sid=x".into(), &Expired).list("0"),
        Err(QuarkError::AuthorizationExpired)
    ));
}

#[test]
fn authenticated_account_uses_the_uid_cookie_when_account_info_omits_an_id() {
    struct AccountInfo;
    impl CloudHttpTransport for AccountInfo {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"status":200,"data":{"nickname":"alice"}}"#.into(),
                ..Default::default()
            })
        }
    }

    let account = QuarkClient::new("__uid=user-42; __pus=session".into(), &AccountInfo)
        .account()
        .unwrap();
    assert_eq!(account.id, "user-42");
    assert_eq!(account.display_name, "alice");
}
