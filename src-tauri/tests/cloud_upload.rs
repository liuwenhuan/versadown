use alldownloader_lib::cloud::*;
use std::{collections::BTreeMap, sync::Mutex};
struct UploadServer {
    requests: Mutex<Vec<UploadHttpRequest>>,
    fail_complete: bool,
}
impl CloudHttpTransport for UploadServer {
    fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let body: serde_json::Value = serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["hash"], "E039D0592261B67B6966F14814C41CC43012A1C6");
        assert_eq!(body["upload_type"], "UPLOAD_TYPE_RESUMABLE");
        Ok(CloudHttpResponse{status:200,body:r#"{"file":{"id":"real-file","name":"upload.txt","kind":"drive#file","phase":"PHASE_TYPE_PENDING"},"resumable":{"params":{"endpoint":"bucket.oss-ap-southeast-1.aliyuncs.com","bucket":"bucket","key":"objects/item","access_key_id":"temporary-id","access_key_secret":"temporary-key","security_token":"temporary-token"}}}"#.into(), ..Default::default()})
    }
}
impl CloudUploadTransport for UploadServer {
    fn send_upload(&self, r: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        let mut requests = self.requests.lock().unwrap();
        let response=match requests.len(){0=>UploadHttpResponse{status:200,headers:BTreeMap::new(),body:"<InitiateMultipartUploadResult><UploadId>upload-id</UploadId></InitiateMultipartUploadResult>".into()},1=>UploadHttpResponse{status:200,headers:BTreeMap::from([("etag".into(),"\"part-etag\"".into())]),body:String::new()},2=>UploadHttpResponse{status:200,headers:BTreeMap::new(),body:if self.fail_complete{"<Error><Code>InvalidPart</Code></Error>"}else{"<CompleteMultipartUploadResult><ETag>object-etag</ETag></CompleteMultipartUploadResult>"}.into()},_=>panic!("unexpected upload")};
        requests.push(r);
        Ok(response)
    }
}
fn upload(fail: bool) -> (Result<CloudFile, QuarkError>, UploadServer) {
    let root = std::env::temp_dir().join(format!("cloud-upload-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("upload.txt");
    std::fs::write(&path, b"hello").unwrap();
    let store = MemoryCredentialStore::default();
    store.save("pikpak",br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device","captcha_token":"cap"}"#).unwrap();
    let server = UploadServer {
        requests: Mutex::new(Vec::new()),
        fail_complete: fail,
    };
    let result = PikPakClient::new(&server, &store).upload_file("0", &path);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
    (result, server)
}
#[test]
fn pikpak_upload_transfers_real_bytes_and_completes_multipart() {
    let (result, server) = upload(false);
    assert_eq!(result.unwrap().id, "real-file");
    let r = server.requests.lock().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[1].body, b"hello");
    assert!(r[1].headers["Authorization"].starts_with("OSS temporary-id:"));
    assert!(String::from_utf8_lossy(&r[2].body).contains("&quot;part-etag&quot;"));
}
#[test]
fn oss_http_200_error_is_not_upload_success() {
    let (result, _) = upload(true);
    assert!(result.is_err());
}

#[test]
fn pikpak_upload_rejects_the_virtual_recycle_bin_target_before_network() {
    let root = std::env::temp_dir().join(format!("cloud-upload-recycle-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("upload.txt");
    std::fs::write(&path, b"hello").unwrap();
    let store = MemoryCredentialStore::default();
    store.save("pikpak",br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device","captcha_token":"cap"}"#).unwrap();
    let server = UploadServer {
        requests: Mutex::new(Vec::new()),
        fail_complete: false,
    };

    let error = PikPakClient::new(&server, &store)
        .upload_file("__all_downloader_pikpak_recycle_bin__", &path)
        .unwrap_err();

    assert!(matches!(error, QuarkError::InvalidInput(_)));
    assert!(server.requests.lock().unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn upload_transfer_allows_same_account_list_and_logout_without_resurrecting_session() {
    upload_does_not_lock_account(false);
}
#[test]
fn logout_during_upload_init_prevents_late_refresh_from_restoring_credentials() {
    upload_does_not_lock_account(true);
}

#[test]
fn rejected_upload_initialization_renews_session_but_never_replays_the_mutation() {
    use alldownloader_lib::cloud::commands::CloudState;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RejectedInit(AtomicUsize);
    impl CloudHttpTransport for RejectedInit {
        fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            if r.url.contains("/auth/token") {
                return Ok(CloudHttpResponse {
                    status: 200,
                    body: r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#
                        .into(),
                    ..Default::default()
                });
            }
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(CloudHttpResponse {
                status: 401,
                body: "{}".into(),
                ..Default::default()
            })
        }
    }
    impl CloudUploadTransport for RejectedInit {
        fn send_upload(&self, _: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
            panic!("no upload without accepted initialization")
        }
    }
    let root = tempfile::tempdir().unwrap();
    let state = CloudState::new(root.path().join("sessions")).unwrap();
    state.store.save("pikpak", br#"{"access_token":"old","refresh_token":"refresh","expires_at":9999999999,"device_id":"device"}"#).unwrap();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, b"hello").unwrap();
    let http = RejectedInit(AtomicUsize::new(0));
    assert!(matches!(
        state.upload_pikpak_file(&http, "0", file.path()),
        Err(QuarkError::SessionRenewedRetryOperation)
    ));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
}
fn upload_does_not_lock_account(block_init: bool) {
    use alldownloader_lib::cloud::commands::CloudState;
    use std::{
        sync::{mpsc, Arc},
        time::Duration,
    };
    struct BlockingUpload {
        inner: UploadServer,
        started: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        block_init: bool,
    }
    impl CloudHttpTransport for BlockingUpload {
        fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            if self.block_init {
                assert!(
                    r.url.ends_with("/drive/v1/files"),
                    "no stale refresh request is allowed"
                );
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                return Ok(CloudHttpResponse {
                    status: 401,
                    body: r#"{"error_code":16}"#.into(),
                    ..Default::default()
                });
            }
            self.inner.send(r)
        }
    }
    impl CloudUploadTransport for BlockingUpload {
        fn send_upload(&self, r: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
            if self.inner.requests.lock().unwrap().is_empty() {
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            self.inner.send_upload(r)
        }
    }
    struct Directory;
    impl CloudHttpTransport for Directory {
        fn send(&self, r: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            assert!(r.url.contains("/drive/v1/files?"));
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"files":[],"next_page_token":""}"#.into(),
                ..Default::default()
            })
        }
    }
    let root =
        std::env::temp_dir().join(format!("cloud-upload-concurrency-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("upload.txt");
    std::fs::write(&file, b"hello").unwrap();
    let state = Arc::new(CloudState::new(root.join("sessions")).unwrap());
    state.with_provider(CloudProvider::Pikpak,|s|s.save("pikpak",br#"{"access_token":"access","refresh_token":"refresh","expires_at":9999999999,"device_id":"device","captcha_token":"cap","user_id":"same-account"}"#)).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let upload_state = state.clone();
    let upload_path = file.clone();
    let upload = std::thread::spawn(move || {
        let http = BlockingUpload {
            inner: UploadServer {
                requests: Mutex::new(Vec::new()),
                fail_complete: false,
            },
            started: started_tx,
            release: Mutex::new(release_rx),
            block_init,
        };
        upload_state.upload_pikpak_file(&http, "0", &upload_path)
    });
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let manage_state = state.clone();
    let manage = std::thread::spawn(move || {
        let result = manage_state.with_provider(CloudProvider::Pikpak, |s| {
            PikPakClient::new(&Directory, s).list("0")?;
            s.remove("pikpak")
        });
        done_tx.send(result).unwrap();
    });
    let during_transfer = done_rx.recv_timeout(Duration::from_millis(250));
    release_tx.send(()).unwrap();
    let upload_result = upload.join().unwrap();
    manage.join().unwrap();
    assert!(state
        .with_provider(CloudProvider::Pikpak, |s| s.load("pikpak"))
        .unwrap()
        .is_none());
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(root.join("sessions")).unwrap();
    std::fs::remove_dir(root).unwrap();
    assert!(
        during_transfer.is_ok(),
        "same-account list/logout must finish while upload network is blocked"
    );
    during_transfer.unwrap().unwrap();
    if block_init {
        assert!(matches!(
            upload_result,
            Err(QuarkError::AuthorizationExpired)
        ))
    } else {
        assert_eq!(upload_result.unwrap().id, "real-file")
    }
}
