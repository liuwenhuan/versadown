use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

use super::{
    cloud_failure_code, create_cloud_task, password_webview_data_directory_from_root,
    prepare_baidu_account_login, recover_provider_password_session,
    requires_cloud_reauthorization_failure, resolve_download,
    retry_read_with_refreshed_web_session,
};
use crate::cloud::share::{CloudShareDownloadEntry, CloudShareDownloadSelection};
use crate::cloud::{
    baidu::{parse_baidu_web_session, BaiduSession, BAIDU_AUTO_OPEN_LOGIN_JS},
    browser_password_login_preferred, save_password_credential, save_password_login_draft,
    set_baidu_browser_profile_username, set_browser_password_login_preferred,
    tianyi::TianyiSession,
    uc::UcSession,
    xunlei::{XunleiClientProfile, XunleiSession, XunleiWebContext},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudPasswordCredential,
    CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError, SessionWorkKind,
    QUARK_DESKTOP_USER_AGENT,
};
use crate::TaskStore;

#[test]
fn failed_share_preparation_without_an_aria_task_can_be_readded() {
    let store = TaskStore::open_in_memory().unwrap();
    let prepared = store
        .create(crate::NewTask {
            source: "cloud-share://aliyun/share/file".into(),
            name: "file.bin".into(),
            kind: "cloud".into(),
            is_private: false,
        })
        .unwrap();
    let failed = store
        .fail_with_code(
            prepared.id,
            Some("provider_error"),
            "网盘分享下载地址请求失败，请稍后重试",
        )
        .unwrap();

    assert!(super::share_commands::is_retryable_cloud_share_prepare(
        &failed
    ));
}

#[test]
fn failed_share_leaf_is_reused_only_by_its_original_directory_item() {
    use crate::models::{NewCloudDirectoryGroup, NewCloudDirectoryGroupItem};

    let store = TaskStore::open_in_memory().unwrap();
    let source = "cloud-share://aliyun/share/file";
    let first_group = store
        .create_cloud_directory_group(
            NewCloudDirectoryGroup::new(
                CloudProvider::Aliyun,
                "cloud-share://aliyun/share",
                "Share",
                std::path::Path::new("C:/downloads"),
            ),
            &[NewCloudDirectoryGroupItem::new(
                source,
                &["Share"],
                "file.bin",
                Some(7),
            )],
        )
        .unwrap();
    let first_item = store.cloud_directory_group_items(first_group.id).unwrap()[0].id;
    let task = store
        .prepare_cloud_share_directory_leaf_task(
            first_group.id,
            first_item,
            crate::NewTask {
                source: source.into(),
                name: "file.bin".into(),
                kind: "cloud".into(),
                is_private: false,
            },
            None,
        )
        .unwrap();
    store
        .fail_with_code(task.id, Some("provider_error"), "temporary")
        .unwrap();
    let second_group = store
        .create_cloud_directory_group(
            NewCloudDirectoryGroup::new(
                CloudProvider::Aliyun,
                "cloud-share://aliyun/share",
                "Share",
                std::path::Path::new("C:/downloads"),
            ),
            &[NewCloudDirectoryGroupItem::new(
                source,
                &["Share"],
                "file.bin",
                Some(7),
            )],
        )
        .unwrap();
    let second_item = store.cloud_directory_group_items(second_group.id).unwrap()[0].id;
    let second_task = store
        .prepare_cloud_share_directory_leaf_task(
            second_group.id,
            second_item,
            crate::NewTask {
                source: source.into(),
                name: "file.bin".into(),
                kind: "cloud".into(),
                is_private: false,
            },
            None,
        )
        .unwrap();
    store
        .fail_with_code(second_task.id, Some("provider_error"), "temporary")
        .unwrap();

    assert_eq!(
        super::share_commands::retryable_cloud_share_task_for_submission(
            &store,
            source,
            Some(&super::DirectoryLeafContext {
                group_id: first_group.id,
                item_id: first_item,
            }),
        )
        .unwrap()
        .map(|task| task.id),
        Some(task.id)
    );
    assert_eq!(
        super::share_commands::retryable_cloud_share_task_for_submission(
            &store,
            source,
            Some(&super::DirectoryLeafContext {
                group_id: second_group.id,
                item_id: second_item,
            }),
        )
        .unwrap()
        .map(|task| task.id),
        Some(second_task.id)
    );
}

#[test]
fn share_cleanup_only_requires_personal_identity_for_a_recorded_transfer() {
    let mut plan = crate::cloud::share::CloudSharePlan::new(
        CloudProvider::Aliyun,
        "share-cleanup".into(),
        None,
        vec![crate::cloud::share::CloudShareEntry::new(
            "share-cleanup",
            "shared-file".into(),
            "file.bin".into(),
            vec![],
            Some(7),
        )],
        serde_json::json!({}),
    );
    let entry = crate::cloud::share::CloudShareResolvedEntry {
        provider: CloudProvider::Aliyun,
        entry: plan.entries[0].clone(),
        plan: plan.clone(),
    };

    assert!(!super::share_commands::share_entry_needs_cleanup(&entry));

    plan.transfers
        .push(crate::cloud::share::CloudShareTransfer {
            entry_fingerprint: plan.entries[0].entry_fingerprint.clone(),
            created_file_id: "personal-file".into(),
            owner_fingerprint: "a".repeat(64),
            owner_scheme: crate::cloud::share::CloudShareTransferOwnerScheme::AccountV2,
            cleanup_state: crate::cloud::share::CloudShareCleanupState::Pending,
        });
    let entry = crate::cloud::share::CloudShareResolvedEntry {
        provider: CloudProvider::Aliyun,
        entry: plan.entries[0].clone(),
        plan,
    };

    assert!(super::share_commands::share_entry_needs_cleanup(&entry));
}

#[path = "password_login_tests.rs"]
mod password_login_tests;

struct MetadataRequestTransport {
    requests: Mutex<Vec<CloudHttpRequest>>,
    responses: Mutex<VecDeque<&'static str>>,
}

impl CloudHttpTransport for MetadataRequestTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        Ok(CloudHttpResponse {
            status: 200,
            body: self.responses.lock().unwrap().pop_front().unwrap().into(),
            ..Default::default()
        })
    }
}

#[test]
fn provider_metadata_controls_baidu_authorization_root_request() {
    let mut descriptor = crate::cloud::ProviderRegistry::global()
        .get(CloudProvider::Baidu)
        .clone();
    // A valid Baidu directory reference for fsid 123 at /catalog-root.
    descriptor.root_directory_id = "bdr_eyJmc2lkIjoiMTIzIiwicGF0aCI6Ii9jYXRhbG9nLXJvb3QifQ";
    let store = MemoryCredentialStore::default();
    let http = MetadataRequestTransport {
        requests: Mutex::new(vec![]),
        responses: Mutex::new(VecDeque::from([
            r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
            r#"{"errno":0,"list":[]}"#,
        ])),
    };
    let account = super::finalize_baidu_web_authorization(
        &descriptor,
        BaiduSession::cookie("BDUSS=fixture"),
        &store,
        &http,
    )
    .unwrap();
    assert_eq!(account.id, "b1");
    let requests = http.requests.lock().unwrap();
    let root_url = url::Url::parse(&requests[1].url).unwrap();
    assert_eq!(
        root_url
            .query_pairs()
            .find(|(key, _)| key == "dir")
            .unwrap()
            .1,
        "/catalog-root"
    );
    assert!(store.load("baidu").unwrap().is_some());
}

#[test]
fn provider_metadata_controls_tianyi_authorization_root_request() {
    let mut descriptor = crate::cloud::ProviderRegistry::global()
        .get(CloudProvider::Tianyi)
        .clone();
    descriptor.root_directory_id = "98765";
    let store = MemoryCredentialStore::default();
    let http = MetadataRequestTransport {
        requests: Mutex::new(vec![]),
        responses: Mutex::new(VecDeque::from([
            r#"{"res_code":0,"fileListAO":{"fileList":[],"folderList":[],"count":0}}"#,
        ])),
    };
    super::finalize_tianyi_web_authorization(
        &descriptor,
        TianyiSession::cookie("SSON=fixture"),
        &store,
        &http,
    )
    .unwrap();
    let requests = http.requests.lock().unwrap();
    let root_url = url::Url::parse(&requests[0].url).unwrap();
    assert_eq!(
        root_url
            .query_pairs()
            .find(|(key, _)| key == "folderId")
            .unwrap()
            .1,
        "98765"
    );
    assert!(store.load("tianyi").unwrap().is_some());
}

#[test]
fn provider_metadata_controls_upload_picker_title_and_preserves_selection() {
    let mut descriptor = crate::cloud::ProviderRegistry::global()
        .get(CloudProvider::Baidu)
        .clone();
    descriptor.upload_display_name = "动态上传网盘";
    let paths = super::select_cloud_upload_paths(&descriptor, None, |title| {
        assert_eq!(title, "选择上传到 动态上传网盘 的文件");
        Ok(vec![std::path::PathBuf::from("selected.txt")])
    })
    .unwrap();
    assert_eq!(paths, vec![std::path::PathBuf::from("selected.txt")]);
    let paths =
        super::select_cloud_upload_paths(&descriptor, Some(vec!["supplied.txt".into()]), |_| {
            panic!("explicit paths must not open a picker")
        })
        .unwrap();
    assert_eq!(paths, vec![std::path::PathBuf::from("supplied.txt")]);
}

#[test]
fn only_authorization_failures_request_a_new_official_login() {
    assert!(requires_cloud_reauthorization_failure(Some(
        cloud_failure_code(&QuarkError::AuthorizationExpired,)
    )));
    assert!(requires_cloud_reauthorization_failure(Some(
        cloud_failure_code(&QuarkError::CaptchaRequired,)
    )));
    assert!(!requires_cloud_reauthorization_failure(Some(
        cloud_failure_code(&QuarkError::Transport("offline".into()),)
    )));
    assert!(!requires_cloud_reauthorization_failure(Some(
        cloud_failure_code(&QuarkError::Provider("temporary failure".into()),)
    )));
}

#[test]
fn shared_directory_plan_does_not_repeat_the_promoted_root_in_its_file_tree() {
    let selection = CloudShareDownloadSelection {
        provider: CloudProvider::Quark,
        plan_source: "cloud-share://quark/plan".into(),
        directory_name: Some("YM-JFSMBYHS".into()),
        entries: vec![
            CloudShareDownloadEntry {
                selected_id: "episode".into(),
                source: crate::cloud::share::stable_share_source(
                    CloudProvider::Quark,
                    "share",
                    "episode",
                ),
                name: "episode.mp4".into(),
                relative_directory: vec!["YM-JFSMBYHS".into()],
                size: Some(100),
            },
            CloudShareDownloadEntry {
                selected_id: "bonus".into(),
                source: crate::cloud::share::stable_share_source(
                    CloudProvider::Quark,
                    "share",
                    "bonus",
                ),
                name: "bonus.mp4".into(),
                relative_directory: vec!["YM-JFSMBYHS".into(), "Extras".into()],
                size: Some(200),
            },
        ],
    };

    let plan = super::plan_cloud_share_directory_download(&selection).unwrap();

    assert_eq!(plan.files[0].relative_directory, vec!["YM-JFSMBYHS"]);
    assert_eq!(
        plan.files[1].relative_directory,
        vec!["YM-JFSMBYHS", "Extras"]
    );
}

#[test]
fn newly_failed_share_download_returns_the_reauthorization_action_hint() {
    let store = TaskStore::open_in_memory().unwrap();
    let prepared = store
        .create(crate::NewTask::direct(
            "cloud-share://baidu/share-id/file-id",
        ))
        .unwrap();

    let outcome = super::cloud_share_failure_outcome(
        &store,
        prepared,
        Some("web_auth_required"),
        "网盘要求完成官方操作验证后重试",
    );

    assert_eq!(
        outcome.payload.task.failure_code.as_deref(),
        Some("web_auth_required")
    );
    assert!(outcome.payload.reauthorization_required);
}

#[test]
fn recycle_directory_ids_route_to_provider_recycle_lists() {
    for (provider, recycle_bin_id) in [
        (CloudProvider::Quark, super::QUARK_RECYCLE_BIN_ID),
        (CloudProvider::Pikpak, super::PIKPAK_RECYCLE_BIN_ID),
        (
            CloudProvider::Baidu,
            crate::cloud::baidu::BAIDU_RECYCLE_BIN_ID,
        ),
        (
            CloudProvider::Tianyi,
            crate::cloud::tianyi::TIANYI_RECYCLE_BIN_ID,
        ),
        (
            CloudProvider::Aliyun,
            crate::cloud::aliyun::ALIYUN_RECYCLE_BIN_ID,
        ),
        (
            CloudProvider::Xunlei,
            crate::cloud::xunlei::XUNLEI_RECYCLE_BIN_ID,
        ),
        (CloudProvider::Uc, crate::cloud::uc::UC_RECYCLE_BIN_ID),
    ] {
        assert!(super::is_provider_recycle_bin(provider, recycle_bin_id));
    }
}

#[test]
fn non_recycle_file_ids_are_rejected_by_permanent_delete() {
    let recycle_bin = crate::cloud::CloudDirectory {
        id: super::PIKPAK_RECYCLE_BIN_ID.into(),
        items: vec![crate::cloud::CloudFile {
            id: "recycle-record".into(),
            parent_id: super::PIKPAK_RECYCLE_BIN_ID.into(),
            name: "deleted.txt".into(),
            kind: "file".into(),
            size: None,
            modified_at: None,
        }],
    };

    assert!(super::validate_recycle_entry_ids(&["normal-file".into()], &recycle_bin).is_err());
}

#[test]
fn tianyi_web_session_capture_uses_only_the_cloud_host_cookie_jar() {
    // Break caught: a same-named open.e.189.cn cookie overwrote the
    // cloud.189.cn cookie and made only share-download signing fail.
    assert_eq!(
        super::web_session_cookie_urls(CloudProvider::Tianyi),
        ["https://cloud.189.cn"]
    );
}

#[test]
fn baidu_web_session_capture_uses_the_pan_api_cookie_jar() {
    // Break caught: a host-only Passport cookie with the same name must
    // not replace the Pan cookie that signs file-download requests.
    assert_eq!(
        super::web_session_cookie_urls(CloudProvider::Baidu),
        ["https://pan.baidu.com"]
    );
}

#[test]
fn web_session_refresh_never_retries_a_provider_captcha_challenge() {
    // Break caught: retrying a provider risk challenge can create more
    // challenges. Only a rejected/expired local session is safe to renew
    // from the already-authorized official webview.
    assert!(super::can_retry_read_with_web_session(
        &QuarkError::AuthorizationExpired
    ));
    assert!(super::can_retry_read_with_web_session(
        &QuarkError::WebAuthorizationRequired
    ));
    assert!(!super::can_retry_read_with_web_session(
        &QuarkError::CaptchaRequired
    ));
}

#[test]
fn share_resolution_errors_keep_actionable_types_without_echoing_provider_secrets() {
    let typed = [
        (QuarkError::NotAuthorized, "尚未授权此网盘，请登录后使用"),
        (
            QuarkError::AuthorizationExpired,
            "网盘会话已过期，请重新登录授权",
        ),
        (
            QuarkError::SessionRenewedRetryOperation,
            "登录已自动恢复，请重新执行本次操作",
        ),
        (
            QuarkError::WebAuthorizationRequired,
            "此登录方式需要网页验证，请点击网页授权完成登录",
        ),
        (
            QuarkError::CaptchaRequired,
            "网盘要求完成官方操作验证后重试",
        ),
        (
            QuarkError::AuthorizationPending,
            "正在等待网页登录，请在授权窗口完成登录",
        ),
        (
            QuarkError::PikpakRegionalMembershipRestricted,
            "区域会员在当前 IP 地区下载受限；请切换到有权限 IP 后重新登录重试，或升级全球会员",
        ),
    ];
    for (error, expected) in typed {
        assert_eq!(super::share_resolution_failure_message(&error), expected);
    }
    assert_eq!(
        QuarkError::CaptchaRequired.to_string(),
        "网盘要求完成官方操作验证后重试"
    );

    let secret = "https://secret.example/file?token=do-not-leak";
    for error in [
        QuarkError::Transport(secret.into()),
        QuarkError::Provider(secret.into()),
        QuarkError::InvalidInput(secret.into()),
    ] {
        let message = super::share_resolution_failure_message(&error);
        assert!(!message.contains(secret));
        assert!(!message.contains("token="));
    }
}

fn windows_device_name_variants() -> Vec<String> {
    [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "COM¹",
        "COM²", "COM³", "LPT¹", "LPT²", "LPT³",
    ]
    .into_iter()
    .flat_map(|name| {
        [
            name.to_owned(),
            name.to_ascii_lowercase(),
            format!("{name}.txt"),
            format!("{name}."),
            format!("{name} "),
            format!("{name} .txt. "),
        ]
    })
    .collect()
}

#[test]
fn directory_output_rejects_all_device_names_before_any_filesystem_changes() {
    for name in windows_device_name_variants() {
        let root = tempfile::tempdir().expect("temporary selected download root");
        let result = super::directory_download_output_directory(
            root.path(),
            &["first".into(), "second".into(), name.clone()],
        );
        assert!(
            !root.path().join("first").exists(),
            "device name {name:?} must be rejected before creating ancestor directories"
        );
        assert!(
            matches!(result, Err(QuarkError::InvalidResponse)),
            "device name {name:?} must fail closed, got {result:?}"
        );
    }
}

#[test]
fn directory_output_file_names_avoid_devices_in_confirmation_and_submission() {
    for name in windows_device_name_variants() {
        let expected = format!("_{}", name.trim_end_matches(['.', ' ']));
        let confirmed = super::safe_filename(&name);
        let submitted = super::ResolvedCloudDownload {
            url: "https://download.example/file".into(),
            name: name.clone(),
            headers: vec![],
        }
        .into_uri_submission_options();
        assert_eq!(confirmed, expected, "unsafe confirmation name for {name:?}");
        assert_eq!(submitted.output_name.as_deref(), Some(expected.as_str()));
    }
}

#[test]
fn directory_output_rejects_ads_and_preserves_normal_unicode_names() {
    let root = tempfile::tempdir().expect("temporary selected download root");
    for name in ["notes:stream", "NUL:data", "folder::$INDEX_ALLOCATION"] {
        let result = super::directory_download_output_directory(
            root.path(),
            &["must-not-be-created".into(), name.into()],
        );
        assert!(
            matches!(result, Err(QuarkError::InvalidResponse)),
            "ADS component {name:?} must fail closed, got {result:?}"
        );
        assert!(!root.path().join("must-not-be-created").exists());
    }
    for name in [
        "中文笔记",
        "资料¹",
        "COM10",
        "LPT0",
        "CONSOLE",
        "辅助文件.txt",
    ] {
        let output = super::directory_download_output_directory(root.path(), &[name.into()])
            .expect("ordinary Unicode and device-like names remain valid");
        assert_eq!(output, root.path().join(name));
        assert_eq!(super::safe_filename(name), name);
    }
    assert_eq!(super::safe_filename("notes:stream"), "notes_stream");
}

#[test]
fn directory_output_rejects_provider_path_components_before_creating_directories() {
    let root = tempfile::tempdir().expect("temporary selected download root");
    for component in [
        "C:",
        "C:outside",
        r"C:\outside",
        r"\\server\share",
        r"\outside",
        "/outside",
        ".",
        "..",
        "",
        "nested/file",
        r"nested\file",
        "line\nbreak",
        "control\u{7f}",
    ] {
        let result = super::directory_download_output_directory(
            root.path(),
            &["must-not-be-created".into(), component.into()],
        );
        assert!(
            matches!(result, Err(QuarkError::InvalidResponse)),
            "provider component {component:?} must be rejected, got {result:?}"
        );
        assert!(!root.path().join("must-not-be-created").exists());
    }
}

#[test]
fn directory_output_uses_the_confirmed_names_inside_the_selected_root() {
    let root = tempfile::tempdir().expect("temporary selected download root");
    let output = super::directory_download_output_directory(
        root.path(),
        &["课程资料".into(), "2026?笔记. ".into()],
    )
    .expect("valid provider names should download below the selected root");
    assert_eq!(output, root.path().join("课程资料").join("2026_笔记"));
    assert!(output.is_dir());
    assert!(output.starts_with(root.path()));
}

#[test]
fn cloud_authorization_window_repositions_each_loaded_page_to_the_top_right() {
    // All providers go through the same external WebView builder.  The
    // post-navigation hook must reset the document to its top-right login
    // area even when a provider performs an intermediate redirect.
    let auth_source = include_str!("auth.rs")
        .split("#[cfg(test)]")
        .next()
        .expect("authorization production source precedes its tests");
    // This module has test-only helpers interleaved with production code, so
    // truncating at its first cfg(test) would omit the viewport script below.
    let browser_source = include_str!("browser.rs");
    let source = format!("{auth_source}\n{browser_source}");
    assert!(source.contains("AUTHORIZATION_TOP_RIGHT_VIEWPORT_JS"));
    assert!(source.contains(".on_page_load(move |window, payload|"));
    assert!(source.contains("PageLoadEvent::Finished"));
    assert!(source.contains("window.eval(AUTHORIZATION_TOP_RIGHT_VIEWPORT_JS)"));
    assert!(source.contains("window.scrollTo({ left: maxLeft, top: 0"));
}

#[test]
fn cloud_authorization_window_allows_safe_oauth_popups() {
    // Social OAuth, including PikPak's Google sign-in, is launched with
    // window.open.  The shared authorization WebView must explicitly let
    // safe popup URLs reach WebView2 instead of silently handling them.
    let source = concat!(include_str!("auth.rs"), include_str!("browser.rs"))
        .split("#[cfg(test)]")
        .next()
        .expect("production source precedes its tests");
    assert!(source.contains(".on_new_window(|url, _features|"));
    assert!(source.contains("allows_cloud_authorization_popup(&url)"));
    assert!(source.contains("NewWindowResponse::Allow"));
    assert!(source.contains("NewWindowResponse::Deny"));

    assert!(super::allows_cloud_authorization_popup(
        &url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap()
    ));
    assert!(super::allows_cloud_authorization_popup(
        &url::Url::parse("about:blank").unwrap()
    ));
    assert!(!super::allows_cloud_authorization_popup(
        &url::Url::parse("file:///C:/private.txt").unwrap()
    ));
    assert!(!super::allows_cloud_authorization_popup(
        &url::Url::parse("javascript:alert(1)").unwrap()
    ));
}

#[test]
fn baidu_authorization_auto_opens_login_without_breaking_changed_pages() {
    // Baidu currently exposes its login dialog behind a landing-page
    // button.  The WebView hook should attempt that exact button only,
    // then quietly give up when the provider changes its page.
    let auth_source = include_str!("auth.rs")
        .split("#[cfg(test)]")
        .next()
        .expect("authorization production source precedes its tests");
    // This module has test-only helpers interleaved with production code, so
    // truncating at its first cfg(test) would omit the viewport script below.
    let browser_source = include_str!("browser.rs");
    let source = format!("{auth_source}\n{browser_source}");
    let script = BAIDU_AUTO_OPEN_LOGIN_JS;
    assert!(source.contains("BAIDU_AUTO_OPEN_LOGIN_JS"));
    assert!(script.contains("\"去登录\""));
    assert!(script.contains("element.click()"));
    assert!(script.contains("attempts >= 20"));
    assert!(source.contains("is_baidu_authorization_page(payload.url())"));
    assert!(source.contains("window.eval(baidu::BAIDU_AUTO_OPEN_LOGIN_JS)"));

    assert!(super::is_baidu_authorization_page(
        &url::Url::parse("https://pan.baidu.com/disk/main").unwrap()
    ));
    assert!(!super::is_baidu_authorization_page(
        &url::Url::parse("https://passport.baidu.com/v2/").unwrap()
    ));
    assert!(!super::is_baidu_authorization_page(
        &url::Url::parse("https://pan.quark.cn/").unwrap()
    ));
}

#[test]
fn stable_cloud_source_decodes_the_file_id_without_accepting_url_parts() {
    assert_eq!(
        super::parse_stable_cloud_source("cloud://quark/file%2Fid%2Btest").unwrap(),
        (CloudProvider::Quark, "file/id+test".to_string())
    );
    for source in [
        "https://example.test/file",
        "cloud://quark/",
        "cloud://unknown/file",
        "cloud://quark/file?token=temporary",
    ] {
        assert!(
            super::parse_stable_cloud_source(source).is_err(),
            "{source}"
        );
    }
}

struct CapturingTransport(Mutex<Option<CloudHttpRequest>>);

impl CloudHttpTransport for CapturingTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        *self.0.lock().unwrap() = Some(request);
        Ok(CloudHttpResponse {
            status: 200,
            body: r#"{"data":[{"download_url":"https://cdn.example.test/fixture","file_name":"fixture.jpg"}]}"#.into(),
            ..Default::default()
        })
    }
}

#[test]
fn quark_download_uses_the_desktop_client_user_agent_for_api_and_aria2() {
    let credentials = MemoryCredentialStore::default();
    credentials.save("quark", b"session=fixture").unwrap();
    let transport = CapturingTransport(Mutex::new(None));

    let download =
        resolve_download(CloudProvider::Quark, "fixture", &credentials, &transport).unwrap();
    let api_request = transport.0.lock().unwrap().clone().unwrap();
    let aria2_user_agent = download
        .headers
        .iter()
        .find(|(name, _)| name == "User-Agent")
        .map(|(_, value)| value);

    assert!(api_request
        .headers
        .get("User-Agent")
        .is_some_and(|value| value.contains("quark-cloud-drive/")));
    assert!(aria2_user_agent.is_some_and(|value| value.contains("quark-cloud-drive/")));
}

struct DownloadCookieTransport;

impl CloudHttpTransport for DownloadCookieTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        Ok(CloudHttpResponse {
            status: 200,
            body: r#"{"data":[{"download_url":"https://cdn.example.test/fixture","file_name":"fixture.jpg"}]}"#.into(),
            set_cookies: vec![
                "__puus=fresh-download-session; Path=/; HttpOnly; Secure".into(),
            ],
        })
    }
}

struct RotatingDownloadCookieTransport(Mutex<usize>);

impl CloudHttpTransport for RotatingDownloadCookieTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let mut requests = self.0.lock().unwrap();
        *requests += 1;
        Ok(CloudHttpResponse {
            status: 200,
            body: r#"{"data":[{"download_url":"https://cdn.example.test/fixture","file_name":"fixture.jpg"}]}"#.into(),
            set_cookies: if *requests == 1 {
                vec!["__puus=fresh-download-session; Path=/; HttpOnly; Secure".into()]
            } else {
                vec![]
            },
        })
    }
}

#[test]
fn quark_download_persists_rotated_cookie_for_the_next_file() {
    let credentials = MemoryCredentialStore::default();
    credentials
        .save("quark", b"sid=web-session; __puus=stale-session")
        .unwrap();
    let transport = RotatingDownloadCookieTransport(Mutex::new(0));

    resolve_download(CloudProvider::Quark, "first", &credentials, &transport).unwrap();
    let second =
        resolve_download(CloudProvider::Quark, "second", &credentials, &transport).unwrap();
    let cookie = second
        .headers
        .iter()
        .find(|(name, _)| name == "Cookie")
        .map(|(_, value)| value.as_str());

    assert_eq!(
        cookie,
        Some("sid=web-session; __puus=fresh-download-session")
    );
    assert_eq!(
        credentials.load("quark").unwrap().as_deref(),
        Some(b"sid=web-session; __puus=fresh-download-session".as_slice())
    );
}

#[test]
fn quark_download_passes_the_response_session_cookie_to_aria2() {
    let credentials = MemoryCredentialStore::default();
    credentials
        .save("quark", b"sid=web-session; __puus=stale-session")
        .unwrap();

    let download = resolve_download(
        CloudProvider::Quark,
        "fixture",
        &credentials,
        &DownloadCookieTransport,
    )
    .unwrap();
    let cookie = download
        .headers
        .iter()
        .find(|(name, _)| name == "Cookie")
        .map(|(_, value)| value);

    assert_eq!(
        cookie.map(String::as_str),
        Some("sid=web-session; __puus=fresh-download-session")
    );
}

#[test]
fn resolved_quark_download_keeps_fresh_cookie_when_building_aria2_options() {
    let credentials = MemoryCredentialStore::default();
    credentials
        .save("quark", b"sid=web-session; __puus=stale-session")
        .unwrap();

    let options = resolve_download(
        CloudProvider::Quark,
        "fixture",
        &credentials,
        &DownloadCookieTransport,
    )
    .unwrap()
    .into_uri_submission_options();

    assert_eq!(options.output_name.as_deref(), Some("fixture.jpg"));
    assert_eq!(
        options.headers,
        vec![
            (
                "Cookie".into(),
                "sid=web-session; __puus=fresh-download-session".into()
            ),
            ("Referer".into(), "https://pan.quark.cn/".into()),
            ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
        ]
    );
}

#[test]
fn aliyun_download_passes_its_official_web_referer_to_aria2() {
    // The live signed-object endpoint rejects a request without this
    // origin context, regardless of whether it uses a Chrome or aria2 UA.
    let credentials = MemoryCredentialStore::default();
    credentials
        .save(
            "aliyun",
            br#"{"refresh_token":"r","access_token":"a","drive_id":"d","browser_access_ready":true}"#,
        )
        .unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"user_id":"u1","nick_name":"alice"}"#,
        r#"{"items":[]}"#,
        r#"{"name":"fixture.txt"}"#,
        r#"{"url":"https://cdn.example.test/fixture?signature=temporary"}"#,
    ])));

    let options = resolve_download(CloudProvider::Aliyun, "fixture", &credentials, &transport)
        .unwrap()
        .into_uri_submission_options();

    assert_eq!(
        options.headers,
        vec![("Referer".into(), "https://www.aliyundrive.com/".into())]
    );
}

#[test]
fn cloud_task_persists_encoded_stable_source_not_signed_url() {
    let store = TaskStore::open_in_memory().unwrap();
    let task = create_cloud_task(
        &store,
        CloudProvider::Quark,
        "folder/file?signature=temporary",
        "fixture.jpg",
    )
    .unwrap();

    assert_eq!(
        task.source,
        "cloud://quark/folder%2Ffile%3Fsignature%3Dtemporary"
    );
    assert_eq!(task.name, "fixture.jpg");
    assert_eq!(task.kind, "cloud");
}

struct SequenceTransport(Mutex<VecDeque<&'static str>>);

fn modern_xunlei_fixture() -> XunleiSession {
    let mut session = XunleiSession::from_web_authorization(
        "fixture-access",
        "fixture-refresh",
        "fixture-user",
        "",
        XunleiClientProfile {
            client_id: "Xqp0kJBXWhwaTpB6".into(),
            client_secret: String::new(),
            device_id: "fixture-device".into(),
            client_version: "1.92.98".into(),
            package_name: "pan.xunlei.com".into(),
            user_agent: "Mozilla/5.0 fixture-browser".into(),
        },
        u64::MAX,
    )
    .unwrap();
    session.web_security.captcha_token = "fixture-captcha".into();
    session.web_security.expires_at = u64::MAX;
    session
}

#[test]
fn xunlei_cdn_download_keeps_browser_identity_without_leaking_api_credentials() {
    let store = MemoryCredentialStore::default();
    super::save_provider_session(CloudProvider::Xunlei, &store, &modern_xunlei_fixture()).unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"id":"fixture-id","name":"fixture.txt","web_content_link":"https://cdn.example.test/fixture"}"#,
    ])));
    let download =
        resolve_download(CloudProvider::Xunlei, "fixture-id", &store, &transport).unwrap();
    assert!(download
        .headers
        .contains(&("User-Agent".into(), "Mozilla/5.0 fixture-browser".into())));
    assert!(download
        .headers
        .contains(&("Referer".into(), "https://pan.xunlei.com/".into())));
    assert!(!download.headers.iter().any(|(key, _)| [
        "authorization",
        "cookie",
        "x-captcha-token"
    ]
    .contains(&key.to_ascii_lowercase().as_str())));
}

#[test]
fn xunlei_operation_failure_does_not_discard_an_already_rotated_refresh_token() {
    let store = MemoryCredentialStore::default();
    super::save_provider_session(CloudProvider::Xunlei, &store, &modern_xunlei_fixture()).unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"error_code":16,"error":"unauthenticated"}"#,
        r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","sub":"fixture-user","expires_in":3600}"#,
        r#"{"error_code":9,"error":"failed_precondition"}"#,
    ])));
    let result = super::with_xunlei_client(&store, &transport, |client| client.directory("0"));
    assert!(matches!(result, Err(QuarkError::Provider(_))));
    let saved: XunleiSession = super::load_provider_session(CloudProvider::Xunlei, &store).unwrap();
    assert_eq!(saved.refresh_token, "rotated-refresh");
}

#[test]
fn xunlei_modern_restore_failure_preserves_an_already_rotated_refresh_token() {
    let store = MemoryCredentialStore::default();
    super::save_provider_session(CloudProvider::Xunlei, &store, &modern_xunlei_fixture()).unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"error_code":16,"error":"unauthenticated"}"#,
        r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","sub":"fixture-user","expires_in":3600}"#,
        r#"{"error_code":9,"error":"failed_precondition"}"#,
    ])));
    let result = super::restore_account(CloudProvider::Xunlei, &store, &transport);
    assert!(matches!(result, Err(QuarkError::Provider(_))));
    let saved: XunleiSession = super::load_provider_session(CloudProvider::Xunlei, &store).unwrap();
    assert_eq!(saved.refresh_token, "rotated-refresh");
    assert_eq!(saved.user_id, "fixture-user");
}

impl CloudHttpTransport for SequenceTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        Ok(CloudHttpResponse {
            status: 200,
            body: self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(QuarkError::InvalidResponse)?
                .into(),
            ..Default::default()
        })
    }
}

struct SessionRecoveryTransport(Mutex<VecDeque<CloudHttpResponse>>);

impl SessionRecoveryTransport {
    fn baidu_password_relogin(account_id: &str) -> Self {
        const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDXN3F7epcT2k2yjuw95Nd3iEDS\nz04mTwPUAOaZIQ+JWtawDWMh6+hpwnT8STs1fa5CZwb1L/nam4zqu/Y4zETCp/uL\nSl8/6jdNsjVtiyq+IlQ0IkmPHiSk022BfBw0RGrSMUGuPPvdjy0eJ5a5pzRYWfK7\nvJ0QdYEdTovg7hm70QIDAQAB\n-----END PUBLIC KEY-----";
        Self(Mutex::new(VecDeque::from([
            CloudHttpResponse {
                status: 200,
                body: String::new(),
                set_cookies: vec!["BAIDUID=seed".into()],
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errInfo":{"no":"0"},"data":{"token":"login-token"}}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "errno": "0",
                    "pubkey": PUBLIC_KEY,
                    "key": "rsa-key-id"
                })
                .to_string(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errInfo":{"no":"0"},"data":{"codeString":""}}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errInfo":{"no":"0"},"data":{}}"#.into(),
                set_cookies: vec!["BDUSS=renewed".into()],
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: format!(
                    r#"{{"errno":0,"user":{{"uk":"{account_id}","netdisk_name":"fixture"}}}}"#
                ),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errno":0,"list":[]}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: format!(
                    r#"{{"errno":0,"user":{{"uk":"{account_id}","netdisk_name":"fixture"}}}}"#
                ),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errno":0,"list":[]}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: format!(
                    r#"{{"errno":0,"user":{{"uk":"{account_id}","netdisk_name":"fixture"}}}}"#
                ),
                ..Default::default()
            },
        ])))
    }

    fn remaining(&self) -> usize {
        self.0
            .lock()
            .map(|responses| responses.len())
            .unwrap_or_default()
    }
}

impl CloudHttpTransport for SessionRecoveryTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.0
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .pop_front()
            .ok_or(QuarkError::InvalidResponse)
    }
}

#[test]
fn baidu_password_webview_uses_one_stable_provider_profile() {
    let root = std::path::Path::new("C:/fixture/app-data");
    assert_eq!(
        password_webview_data_directory_from_root(root, CloudProvider::Baidu).unwrap(),
        root.join("web-auth").join("baidu")
    );
    assert_eq!(
        password_webview_data_directory_from_root(root, CloudProvider::Tianyi).unwrap(),
        root.join("web-auth").join("tianyi")
    );
    assert!(password_webview_data_directory_from_root(root, CloudProvider::Quark).is_err());
}

#[test]
fn baidu_native_risk_rejection_marks_future_recovery_for_the_browser() {
    let store = MemoryCredentialStore::default();
    let credential =
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret");
    let transport = SessionRecoveryTransport(Mutex::new(VecDeque::from([
        CloudHttpResponse {
            status: 200,
            body: String::new(),
            set_cookies: vec!["BAIDUID=seed".into()],
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"errInfo":{"no":"0"},"data":{"token":"login-token"}}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: serde_json::json!({
                "errno": "0",
                "pubkey": "-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDXN3F7epcT2k2yjuw95Nd3iEDS\nz04mTwPUAOaZIQ+JWtawDWMh6+hpwnT8STs1fa5CZwb1L/nam4zqu/Y4zETCp/uL\nSl8/6jdNsjVtiyq+IlQ0IkmPHiSk022BfBw0RGrSMUGuPPvdjy0eJ5a5pzRYWfK7\nvJ0QdYEdTovg7hm70QIDAQAB\n-----END PUBLIC KEY-----",
                "key": "rsa-key-id"
            }).to_string(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"errInfo":{"no":"0"},"data":{"codeString":""}}"#.into(),
            ..Default::default()
        },
        CloudHttpResponse {
            status: 200,
            body: r#"{"errInfo":{"no":"50052","msg":""},"data":{}}"#.into(),
            ..Default::default()
        },
    ])));

    assert!(matches!(
        recover_provider_password_session(CloudProvider::Baidu, &store, &transport, &credential,),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert!(browser_password_login_preferred(&store, CloudProvider::Baidu).unwrap());
}

#[test]
fn baidu_browser_preference_skips_repeating_the_native_password_attempt() {
    let store = MemoryCredentialStore::default();
    set_browser_password_login_preferred(&store, CloudProvider::Baidu, true).unwrap();
    let credential =
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret");
    let transport = SessionRecoveryTransport(Mutex::new(VecDeque::new()));

    assert!(matches!(
        recover_provider_password_session(CloudProvider::Baidu, &store, &transport, &credential,),
        Err(QuarkError::WebAuthorizationRequired)
    ));
    assert_eq!(transport.remaining(), 0);
}

#[test]
fn changing_the_only_baidu_username_requires_profile_replacement() {
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "old-user", "secret"),
    )
    .unwrap();
    set_baidu_browser_profile_username(&store, Some("old-user")).unwrap();
    set_browser_password_login_preferred(&store, CloudProvider::Baidu, true).unwrap();

    assert!(!prepare_baidu_account_login(&store, "old-user").unwrap());
    assert!(browser_password_login_preferred(&store, CloudProvider::Baidu).unwrap());

    assert!(prepare_baidu_account_login(&store, "new-user").unwrap());
    assert!(browser_password_login_preferred(&store, CloudProvider::Baidu).unwrap());
}

#[test]
fn unbound_baidu_web_session_is_reset_before_accepting_a_password_username() {
    let store = MemoryCredentialStore::default();
    store
        .save(CloudProvider::Baidu.key(), b"old-web-session")
        .unwrap();

    assert!(prepare_baidu_account_login(&store, "new-user").unwrap());
}

#[test]
fn changed_baidu_login_draft_resets_a_profile_without_a_validated_credential() {
    let store = MemoryCredentialStore::default();
    save_password_login_draft(&store, CloudProvider::Baidu, "old-user", "secret").unwrap();

    assert!(!prepare_baidu_account_login(&store, "old-user").unwrap());
    assert!(prepare_baidu_account_login(&store, "new-user").unwrap());
}

#[test]
fn refreshed_web_session_retries_read_work_and_keeps_the_verified_snapshot() {
    // Break caught: a download-signing request can see an expired stored
    // cookie while the already-authorized official tab has a newer one.
    // The retry must use and retain that newer snapshot.
    let store = MemoryCredentialStore::default();
    store.save("uc", b"previous-session").unwrap();
    let calls = Cell::new(0);

    let result = retry_read_with_refreshed_web_session(
        CloudProvider::Uc,
        &store,
        b"browser-session",
        || {
            calls.set(calls.get() + 1);
            assert_eq!(
                store.load("uc").unwrap().as_deref(),
                Some(b"browser-session".as_slice())
            );
            Ok("signed-download")
        },
    );

    assert_eq!(result.unwrap(), "signed-download");
    assert_eq!(calls.get(), 1);
    assert_eq!(
        store.load("uc").unwrap().as_deref(),
        Some(b"browser-session".as_slice())
    );
}

#[test]
fn rejected_refreshed_web_session_restores_the_previous_snapshot() {
    // Break caught: a stale or incomplete WebView cookie capture must not
    // overwrite a previously usable stored authorization.
    let store = MemoryCredentialStore::default();
    store.save("tianyi", b"previous-session").unwrap();

    let result: Result<(), QuarkError> = retry_read_with_refreshed_web_session(
        CloudProvider::Tianyi,
        &store,
        b"browser-session",
        || Err(QuarkError::AuthorizationExpired),
    );

    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    assert_eq!(
        store.load("tianyi").unwrap().as_deref(),
        Some(b"previous-session".as_slice())
    );
}

#[test]
fn expired_baidu_read_uses_a_matching_saved_password_and_retries_once() {
    let store = MemoryCredentialStore::default();
    super::save_provider_session(
        CloudProvider::Baidu,
        &store,
        &BaiduSession::cookie("BDUSS=expired"),
    )
    .unwrap();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
    )
    .unwrap();
    let transport = SessionRecoveryTransport::baidu_password_relogin("account-1");
    let calls = Cell::new(0);

    let result = super::run_cloud_work(
        CloudProvider::Baidu,
        SessionWorkKind::Read,
        &store,
        &transport,
        || {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(QuarkError::AuthorizationExpired)
            } else {
                Ok("listed after recovery")
            }
        },
    );

    assert_eq!(result.unwrap(), "listed after recovery");
    assert_eq!(calls.get(), 2);
    let saved: BaiduSession = super::load_provider_session(CloudProvider::Baidu, &store).unwrap();
    assert!(saved.cookie.contains("BDUSS=renewed"));
    assert_eq!(transport.remaining(), 0);
}

#[test]
fn tianyi_recoverable_read_errors_always_try_native_password_login_first() {
    for error in [
        QuarkError::AuthorizationExpired,
        QuarkError::NotAuthorized,
        QuarkError::WebAuthorizationRequired,
        QuarkError::CaptchaRequired,
    ] {
        assert!(super::should_attempt_native_password_recovery(
            CloudProvider::Tianyi,
            SessionWorkKind::Read,
            &error,
        ));
    }

    assert!(!super::should_attempt_native_password_recovery(
        CloudProvider::Tianyi,
        SessionWorkKind::Mutation,
        &QuarkError::AuthorizationExpired,
    ));
}

#[test]
fn expired_baidu_mutation_restores_the_session_without_replaying_the_request() {
    let store = MemoryCredentialStore::default();
    super::save_provider_session(
        CloudProvider::Baidu,
        &store,
        &BaiduSession::cookie("BDUSS=expired"),
    )
    .unwrap();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
    )
    .unwrap();
    let transport = SessionRecoveryTransport::baidu_password_relogin("account-1");
    let calls = Cell::new(0);

    let result: Result<(), QuarkError> = super::run_cloud_work(
        CloudProvider::Baidu,
        SessionWorkKind::Mutation,
        &store,
        &transport,
        || {
            calls.set(calls.get() + 1);
            Err(QuarkError::AuthorizationExpired)
        },
    );

    assert!(matches!(
        result,
        Err(QuarkError::SessionRenewedRetryOperation)
    ));
    assert_eq!(calls.get(), 1);
    let saved: BaiduSession = super::load_provider_session(CloudProvider::Baidu, &store).unwrap();
    assert!(saved.cookie.contains("BDUSS=renewed"));
    assert_eq!(transport.remaining(), 0);
}

struct XunleiRootAcceptingTransport(Mutex<Vec<CloudHttpRequest>>);

impl CloudHttpTransport for XunleiRootAcceptingTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.0.lock().unwrap().push(request);
        Ok(CloudHttpResponse {
            status: 200,
            body: r#"{"files":[],"next_page_token":""}"#.into(),
            ..Default::default()
        })
    }
}

struct TianyiRootAcceptingTransport(Mutex<u8>);

impl CloudHttpTransport for TianyiRootAcceptingTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        *self.0.lock().unwrap() += 1;
        Ok(CloudHttpResponse {
            status: 200,
            body: if request.url.contains("getUserInfoForPortal.action") {
                r#"{"res_code":0,"loginName":"alice@189.cn"}"#.into()
            } else {
                r#"{"res_code":0,"fileListAO":{"fileList":[{"id":"root-fixture","name":"fixture","isFolder":false}]}}"#.into()
            },
            ..Default::default()
        })
    }
}

struct TianyiRootRejectingTransport;

impl CloudHttpTransport for TianyiRootRejectingTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        Ok(CloudHttpResponse {
            status: 400,
            body: r#"{"success":false,"errorCode":"session_not_accepted"}"#.into(),
            ..Default::default()
        })
    }
}

struct TianyiMalformedRootTransport;

impl CloudHttpTransport for TianyiMalformedRootTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        Ok(CloudHttpResponse {
            status: 200,
            body: r#"{"res_code":0,"fileListAO":{"fileList":null,"folderList":[]}}"#.into(),
            ..Default::default()
        })
    }
}

#[derive(Clone, Copy)]
enum TianyiRootAuthorizationError {
    Expired,
    WebVerification,
    Captcha,
}

struct TianyiRootAuthorizationErrorTransport(TianyiRootAuthorizationError);

impl CloudHttpTransport for TianyiRootAuthorizationErrorTransport {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        Err(match self.0 {
            TianyiRootAuthorizationError::Expired => QuarkError::AuthorizationExpired,
            TianyiRootAuthorizationError::WebVerification => QuarkError::WebAuthorizationRequired,
            TianyiRootAuthorizationError::Captcha => QuarkError::CaptchaRequired,
        })
    }
}

#[test]
fn baidu_web_authorization_keeps_the_window_open_until_the_root_directory_is_readable() {
    let credentials = MemoryCredentialStore::default();
    let previous = b"previous-verified-session";
    credentials.save("baidu", previous).unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
        r#"{"errno":-6}"#,
    ])));

    let result = super::finalize_baidu_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Baidu),
        BaiduSession::cookie("BDUSS=browser-session"),
        &credentials,
        &transport,
    );

    // The UI loads `/` immediately after accepting this account.  Saving
    // after only the account probe creates a new tab that immediately
    // reports credentials_expired, despite the browser still being able
    // to complete its official verification flow.
    assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
    assert_eq!(credentials.load("baidu").unwrap(), Some(previous.to_vec()));
}

#[test]
fn browser_password_session_never_replaces_a_different_accounts_credentials() {
    let credentials = MemoryCredentialStore::default();
    credentials.save("baidu", b"previous-session").unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"errno":0,"user":{"uk":"other-account","netdisk_name":"baidu"}}"#,
        r#"{"errno":0,"list":[]}"#,
    ])));
    let result = super::validate_browser_password_session(
        CloudProvider::Baidu,
        &credentials,
        &transport,
        &["BDUSS=new-session".into()],
        None,
        Some("original-account"),
        |_| {},
    );
    assert!(matches!(result, Err(QuarkError::InvalidInput(_))));
    assert_eq!(
        credentials.load("baidu").unwrap().unwrap(),
        b"previous-session"
    );
}

#[test]
fn browser_auth_diagnostic_contains_only_safe_status_fields() {
    let script = super::browser_auth_diagnostic_script(
        "账号信息校验",
        "网盘请求失败：天翼云盘拒绝请求（错误码 InvalidDeviceStatus）",
    );

    assert!(script.contains("__alldownloaderAuthDiagnostic"));
    assert!(script.contains("账号信息校验"));
    assert!(script.contains("InvalidDeviceStatus"));
    assert!(!script.contains("username"));
    assert!(!script.contains("password"));
    assert!(!script.contains("cookie"));
}

#[test]
fn tianyi_authorization_window_accepts_only_official_read_and_api_urls() {
    for (method, url) in [
        (
            "GET",
            "https://cloud.189.cn/api/open/user/getUserInfoForPortal.action",
        ),
        (
            "POST",
            "https://cloud.189.cn/api/open/file/createFolder.action",
        ),
        (
            "GET",
            "https://cloud.189.cn/v2/deleteFile.action?fileIdList=1",
        ),
        ("GET", "https://cloud.189.cn/main.action"),
    ] {
        assert!(super::validate_tianyi_browser_request(method, url).is_ok());
    }
    for (method, url) in [
        ("DELETE", "https://cloud.189.cn/api/open/file/item"),
        ("GET", "http://cloud.189.cn/api/open/file/listFiles.action"),
        ("GET", "https://evil.example/api/open/file/listFiles.action"),
        (
            "GET",
            "https://cloud.189.cn.evil.example/api/open/file/listFiles.action",
        ),
        ("GET", "https://cloud.189.cn/private/account"),
    ] {
        assert!(super::validate_tianyi_browser_request(method, url).is_err());
    }
}

#[test]
fn tianyi_authorization_window_strips_native_identity_and_cookie_headers() {
    let filtered = super::tianyi_browser_request_headers(BTreeMap::from([
        ("Accept".into(), "application/json".into()),
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ),
        (
            "Browser-Id".into(),
            "1234567890abcdef1234567890abcdef".into(),
        ),
        ("Cookie".into(), "secret=session".into()),
        ("User-Agent".into(), "Native".into()),
        ("Origin".into(), "https://cloud.189.cn".into()),
        ("Referer".into(), "https://cloud.189.cn/".into()),
    ]));

    assert_eq!(
        filtered.get("Accept").map(String::as_str),
        Some("application/json")
    );
    assert!(filtered.contains_key("Content-Type"));
    assert!(filtered.contains_key("Browser-Id"));
    assert!(!filtered.contains_key("Cookie"));
    assert!(!filtered.contains_key("User-Agent"));
    assert!(!filtered.contains_key("Origin"));
    assert!(!filtered.contains_key("Referer"));
    assert!(!filtered.values().any(|value| value.contains("secret")));
}

struct TianyiAppAcceptingWebRejectingTransport;

impl CloudHttpTransport for TianyiAppAcceptingWebRejectingTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        if request.headers.contains_key("SessionKey") {
            return Ok(CloudHttpResponse {
                status: 200,
                body: r#"<listFiles><fileList></fileList></listFiles>"#.into(),
                ..Default::default()
            });
        }
        Err(QuarkError::AuthorizationExpired)
    }
}

#[test]
fn tianyi_browser_context_capture_observes_fetch_as_well_as_xhr() {
    let script = include_str!("../tianyi-browser-context.js");

    assert!(script.contains("XMLHttpRequest.prototype"));
    assert!(script.contains("window.fetch"));
    assert!(script.contains("headers.get('browser-id')"));
    assert!(script.contains("window.__alldownloaderTianyiContext"));
}

#[test]
fn tianyi_session_diagnostic_exposes_names_but_never_cookie_values() {
    let mut session = TianyiSession::cookie(
        "COOKIE_LOGIN_USER=private-login; JSESSIONID=private-session; apm_sid=private-device",
    );
    session.auth_method = super::CloudAuthMethod::Password;
    session.apply_browser_context(&serde_json::json!({
        "browser_id":"1234567890abcdef1234567890abcdef",
        "user_agent":"OfficialBrowser/140"
    }));

    let shape = super::tianyi_session_safe_shape(&session);

    assert_eq!(shape["authMethod"], "password");
    assert_eq!(
        shape["cookieNames"],
        serde_json::json!(["COOKIE_LOGIN_USER", "JSESSIONID", "apm_sid"])
    );
    assert_eq!(shape["browserContext"], true);
    assert_eq!(shape["transport"], "web");
    let serialized = shape.to_string();
    assert!(!serialized.contains("private-login"));
    assert!(!serialized.contains("private-session"));
    assert!(!serialized.contains("private-device"));
}

#[test]
fn tianyi_session_diagnostic_identifies_native_app_without_exposing_tokens() {
    let session: TianyiSession = serde_json::from_value(serde_json::json!({
        "cookie": "",
        "authMethod": "password",
        "app": {
            "sessionKey": "private-session-key",
            "sessionSecret": "private-session-secret",
            "accessToken": "private-access-token",
            "loginName": "private-account"
        }
    }))
    .unwrap();

    let shape = super::tianyi_session_safe_shape(&session);

    assert_eq!(shape["transport"], "nativeApp");
    assert_eq!(shape["cookieNames"], serde_json::json!([]));
    let serialized = shape.to_string();
    assert!(!serialized.contains("private-session"));
    assert!(!serialized.contains("private-access"));
    assert!(!serialized.contains("private-account"));
}

#[test]
fn browser_password_session_preserves_previous_credentials_when_directory_rejects_login() {
    let credentials = MemoryCredentialStore::default();
    credentials.save("baidu", b"previous-session").unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
        r#"{"errno":-9}"#,
    ])));
    assert!(super::validate_browser_password_session(
        CloudProvider::Baidu,
        &credentials,
        &transport,
        &["BDUSS=new-session".into()],
        None,
        Some("b1"),
        |_| {},
    )
    .is_err());
    assert_eq!(
        credentials.load("baidu").unwrap().unwrap(),
        b"previous-session"
    );
}

#[test]
fn browser_password_session_persists_real_tianyi_account_after_validation() {
    let credentials = MemoryCredentialStore::default();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"res_code":0,"userInfo":{"userId":"t1","nickName":"tianyi"}}"#,
        r#"{"res_code":0,"fileListAO":{"fileList":[]}}"#,
    ])));
    let account = super::validate_browser_password_session(
        CloudProvider::Tianyi,
        &credentials,
        &transport,
        &["SSON=new-session".into()],
        Some(&serde_json::json!({"browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"})),
        Some("t1"),
        |_| {},
    )
    .unwrap();
    assert_eq!(account.id, "t1");
    let session: TianyiSession =
        super::load_provider_session(CloudProvider::Tianyi, &credentials).unwrap();
    assert_eq!(session.auth_method, super::CloudAuthMethod::Password);
    assert!(session.cookie.contains("SSON=new-session"));
    assert_eq!(session.browser_id, "1234567890abcdef1234567890abcdef");
    assert_eq!(session.user_agent, "OfficialBrowser/140");
}

#[test]
fn browser_password_session_waits_for_tianyi_browser_identity_before_native_validation() {
    let credentials = MemoryCredentialStore::default();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"res_code":0,"loginName":"alice@example.org"}"#,
    ])));
    let mut stage = "";
    let result = super::validate_browser_password_session(
        CloudProvider::Tianyi,
        &credentials,
        &transport,
        &["SSON=new-session".into()],
        None,
        None,
        |current| stage = current,
    );

    assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
    assert_eq!(stage, "等待官网浏览器标识");
}

#[test]
fn browser_password_session_accepts_tianyi_files_and_profile_containing_challenge_words() {
    for (profile, directory) in [
        (
            r#"{"res_code":0,"loginName":"sms@example.org","nickName":"risk analyst"}"#,
            r#"{"res_code":0,"fileListAO":{"fileList":[]}}"#,
        ),
        (
            r#"{"res_code":0,"loginName":"alice@example.org"}"#,
            r#"{"res_code":0,"fileListAO":{"fileList":[{"id":"f1","name":"短信备份-verify.txt","size":10}]}}"#,
        ),
    ] {
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([profile, directory])));
        let account = super::validate_browser_password_session(
            CloudProvider::Tianyi,
            &credentials,
            &transport,
            &["SSON=new-session".into()],
            Some(&serde_json::json!({"browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"})),
            None,
            |_| {},
        )
        .expect("valid profile and filenames must finish authorization");
        assert!(!account.id.is_empty());
        assert!(credentials.load("tianyi").unwrap().is_some());
    }
}

#[test]
fn browser_password_session_reports_tianyi_rejection_without_replacing_credentials() {
    for (responses, expected_stage) in [
        (vec![r#"{"res_code":0}"#], "账号信息校验"),
        (
            vec![
                r#"{"res_code":0,"loginName":"alice@example.org"}"#,
                r#"{"res_code":-1,"msg":"需要短信验证码"}"#,
            ],
            "目录访问校验",
        ),
    ] {
        let credentials = MemoryCredentialStore::default();
        credentials.save("tianyi", b"previous-session").unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from(responses)));
        let mut stage = "";
        assert!(super::validate_browser_password_session(
            CloudProvider::Tianyi,
            &credentials,
            &transport,
            &["SSON=new-session".into()],
            Some(&serde_json::json!({"browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"})),
            None,
            |current| stage = current,
        )
        .is_err());
        assert_eq!(stage, expected_stage);
        assert_eq!(
            credentials.load("tianyi").unwrap().unwrap(),
            b"previous-session"
        );
    }
}

#[test]
fn baidu_web_authorization_persists_a_browser_session_that_restores_the_root_directory() {
    let credentials = MemoryCredentialStore::default();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
        r#"{"errno":0,"list":[]}"#,
        r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
        r#"{"errno":0,"list":[]}"#,
    ])));
    let session = parse_baidu_web_session(&[
        "BDUSS_BFESS=browser-session".into(),
        "PANPSC=browser-context".into(),
    ])
    .expect("the official WebView should provide its authenticated cookie pairs");

    let account = super::finalize_baidu_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Baidu),
        session,
        &credentials,
        &transport,
    )
    .expect("a browser session with a readable root should authorize");
    let restored = super::restore_account(CloudProvider::Baidu, &credentials, &transport)
        .expect("the saved browser session should restore after reopening the tab");
    let root = super::list_cloud_directory(CloudProvider::Baidu, &credentials, &transport, "/")
        .expect("the restored tab should load its root directory");

    assert_eq!(account.id, "b1");
    assert_eq!(restored.id, "b1");
    assert!(root.items.iter().any(|item| {
        item.id == crate::cloud::baidu::BAIDU_RECYCLE_BIN_ID
            && item.parent_id == "/"
            && item.name == "回收站"
            && item.kind == "folder"
    }));
}

#[test]
fn tianyi_web_authorization_persists_a_session_after_root_directory_validates() {
    let credentials = MemoryCredentialStore::default();
    let transport = TianyiRootAcceptingTransport(Mutex::new(0));

    let account = super::finalize_tianyi_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Tianyi),
        TianyiSession::cookie("SSON=official-browser-session"),
        &credentials,
        &transport,
    )
    .expect("a browser session with a readable root directory should authorize");

    assert_eq!(account.id, "tianyi-web");
    assert_eq!(account.provider, CloudProvider::Tianyi);
    assert_eq!(*transport.0.lock().unwrap(), 1);
    assert!(credentials.load("tianyi").unwrap().is_some());
}

#[test]
fn tianyi_driver_rejects_a_web_snapshot_without_the_native_browser_identity() {
    let credentials = MemoryCredentialStore::default();
    let transport = TianyiRootAcceptingTransport(Mutex::new(0));
    let input = crate::cloud::driver::WebAuthorizationInput {
        cookies: vec!["SSON=official-browser-session".into()],
        storage: serde_json::Value::Null,
        browser_context: Some(serde_json::json!({})),
    };

    let result = crate::cloud::ProviderRegistry::global()
        .driver(CloudProvider::Tianyi, &credentials, &transport)
        .auth()
        .complete_web(&input);

    assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
    assert_eq!(*transport.0.lock().unwrap(), 0);
    assert!(credentials.load("tianyi").unwrap().is_none());
}

#[test]
fn disabled_tianyi_driver_does_not_reactivate_legacy_password_credentials() {
    let credentials = MemoryCredentialStore::default();
    credentials
        .save(
            "tianyi",
            br#"{"cookie":"","authMethod":"password","app":{"sessionKey":"app-key","sessionSecret":"app-secret","accessToken":"access-token","loginName":"alice@189.cn"}}"#,
        )
        .unwrap();
    let transport = TianyiRootAcceptingTransport(Mutex::new(0));
    let input = crate::cloud::driver::WebAuthorizationInput {
        cookies: vec!["SSON=official-browser-session".into()],
        storage: serde_json::Value::Null,
        browser_context: Some(serde_json::json!({
            "browser_id": "1234567890abcdef1234567890abcdef",
            "user_agent": "OfficialBrowser/140"
        })),
    };

    let result = crate::cloud::ProviderRegistry::global()
        .driver(CloudProvider::Tianyi, &credentials, &transport)
        .auth()
        .complete_web(&input);

    assert!(matches!(result, Err(QuarkError::InvalidInput(_))));
    let saved: serde_json::Value =
        serde_json::from_slice(&credentials.load("tianyi").unwrap().unwrap()).unwrap();
    assert_eq!(saved["cookie"], "");
    assert_eq!(saved["app"]["sessionKey"], "app-key");
}

#[test]
fn tianyi_driver_does_not_let_a_valid_app_session_mask_an_invalid_web_candidate() {
    let credentials = MemoryCredentialStore::default();
    let existing = br#"{"cookie":"","auth_method":"password","app":{"sessionKey":"app-key","sessionSecret":"app-secret","accessToken":"access-token","loginName":"alice@189.cn"}}"#;
    credentials.save("tianyi", existing).unwrap();
    let input = crate::cloud::driver::WebAuthorizationInput {
        cookies: vec!["SSON=rejected-browser-session".into()],
        storage: serde_json::Value::Null,
        browser_context: Some(serde_json::json!({
            "browser_id": "1234567890abcdef1234567890abcdef",
            "user_agent": "OfficialBrowser/140"
        })),
    };

    let result = crate::cloud::ProviderRegistry::global()
        .driver(
            CloudProvider::Tianyi,
            &credentials,
            &TianyiAppAcceptingWebRejectingTransport,
        )
        .auth()
        .complete_web(&input);

    assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
    assert_eq!(
        credentials.load("tianyi").unwrap().as_deref(),
        Some(existing.as_slice())
    );
}

#[test]
fn tianyi_web_authorization_does_not_persist_a_session_when_root_is_unreadable() {
    let credentials = MemoryCredentialStore::default();

    let result = super::finalize_tianyi_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Tianyi),
        TianyiSession::cookie("SSON=browser-session"),
        &credentials,
        &TianyiRootRejectingTransport,
    );

    assert!(matches!(result, Err(QuarkError::Provider(_))));
    assert!(credentials.load("tianyi").unwrap().is_none());
}

#[test]
fn xunlei_web_authorization_uses_bearer_when_the_cookie_context_has_no_session_id() {
    // Break caught: rejecting a completed official browser login before
    // the documented `sessionid || access_token` fallback can run leaves
    // the authorization window polling forever.
    let credentials = MemoryCredentialStore::default();
    let session = XunleiSession::from_web_authorization(
        "browser-access",
        "browser-refresh",
        "browser-user",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    let context = XunleiWebContext {
        session_id: String::new(),
        peer_id: "official-peer".into(),
        device_id: String::new(),
        credit_key: "official-credit".into(),
    };

    let root = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
    let account = super::finalize_xunlei_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
        session,
        context,
        &credentials,
        &root,
    )
    .expect("a complete browser context can use its current bearer as session id");

    assert_eq!(account.id, "browser-user");
    let saved: XunleiSession = serde_json::from_slice(
        &credentials
            .load("xunlei")
            .unwrap()
            .expect("session is saved"),
    )
    .unwrap();
    assert_eq!(saved.web_context.session_id, "browser-access");
}

#[test]
fn xunlei_web_authorization_accepts_a_current_bearer_without_a_refresh_token() {
    // Break caught: treating an optional native refresh credential as a
    // prerequisite for the already-current official browser bearer keeps
    // a completed login stuck in its authorization window.
    let credentials = MemoryCredentialStore::default();
    let session = super::parse_xunlei_web_session(serde_json::json!({
        "access_token": "browser-access",
        "user_id": "browser-user",
    }))
    .expect("the current browser bearer needs no native refresh token");
    let context = XunleiWebContext {
        session_id: String::new(),
        peer_id: "official-peer".into(),
        device_id: String::new(),
        credit_key: "official-credit".into(),
    };

    let root = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
    let account = super::finalize_xunlei_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
        session,
        context,
        &credentials,
        &root,
    )
    .expect("the current bearer completes authorization until it expires");

    assert_eq!(account.id, "browser-user");
    let saved: XunleiSession = serde_json::from_slice(
        &credentials
            .load("xunlei")
            .unwrap()
            .expect("session is saved"),
    )
    .unwrap();
    assert!(saved.refresh_token.is_empty());
    assert_eq!(saved.web_context.session_id, "browser-access");
}

#[test]
fn xunlei_web_authorization_persists_only_after_the_root_directory_is_readable() {
    // Break caught: accepting extracted browser state without probing the
    // exact root request the workspace needs closes the official window
    // and creates a tab backed by an unusable session.
    let credentials = MemoryCredentialStore::default();
    let transport = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
    let session = XunleiSession::from_web_authorization(
        "browser-access",
        "",
        "browser-user",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    let context = XunleiWebContext {
        session_id: String::new(),
        peer_id: "official-peer".into(),
        device_id: String::new(),
        credit_key: "official-credit".into(),
    };

    let account = super::finalize_xunlei_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
        session,
        context,
        &credentials,
        &transport,
    )
    .expect("a browser session with a readable root directory should authorize");

    assert_eq!(account.id, "browser-user");
    let requests = transport.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers["Authorization"],
        "Bearer browser-access"
    );
    assert_eq!(requests[0].headers["Peer-Id"], "official-peer");
    assert!(requests[0].url.contains("_sessid=browser-access"));
    assert!(credentials.load("xunlei").unwrap().is_some());
}

#[test]
fn xunlei_web_authorization_keeps_the_window_open_when_root_is_unreadable() {
    // Break caught: a displayable account is not an authorized workspace.
    // The candidate must not replace stored credentials until the browser
    // bearer can read the root directory.
    let credentials = MemoryCredentialStore::default();
    let previous = b"previous-verified-xunlei-session".to_vec();
    credentials.save("xunlei", &previous).unwrap();
    let session = XunleiSession::from_web_authorization(
        "browser-access",
        "",
        "browser-user",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    let context = XunleiWebContext {
        session_id: String::new(),
        peer_id: "official-peer".into(),
        device_id: String::new(),
        credit_key: "official-credit".into(),
    };
    let rejecting = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"error_code":4121,"error":"expired"}"#,
    ])));

    let error = super::finalize_xunlei_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
        session,
        context,
        &credentials,
        &rejecting,
    )
    .unwrap_err();

    assert!(matches!(error, QuarkError::AuthorizationPending));
    assert_eq!(credentials.load("xunlei").unwrap(), Some(previous));
}

#[test]
fn xunlei_modern_authorization_reports_final_root_rejection_instead_of_polling_forever() {
    // Once the SDK has returned an account, exhausted authentication or
    // captcha retries are a visible failure, not another "not logged in" poll.
    for rejection in [
        r#"{"error_code":16,"error":"unauthenticated","error_description":"private-response"}"#,
        r#"{"error_code":4001,"error":"captcha_required","error_description":"private-response"}"#,
        r#"{"error_code":4002,"error":"captcha_invalid","error_description":"private-response"}"#,
    ] {
        let credentials = MemoryCredentialStore::default();
        let previous = b"previous-verified-xunlei-session".to_vec();
        credentials.save("xunlei", &previous).unwrap();
        let mut session = modern_xunlei_fixture();
        session.refresh_token.clear();
        let rejecting = SequenceTransport(Mutex::new(VecDeque::from([rejection, rejection])));
        let error = super::finalize_xunlei_web_authorization(
            crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
            session,
            XunleiWebContext::default(),
            &credentials,
            &rejecting,
        )
        .unwrap_err();
        assert!(matches!(&error, QuarkError::Provider(message) if !message.is_empty()));
        assert!(!error.to_string().contains("private-response"));
        assert_eq!(credentials.load("xunlei").unwrap(), Some(previous));
    }
}

#[test]
fn xunlei_modern_authorization_saves_the_account_after_a_successful_root_probe() {
    let credentials = MemoryCredentialStore::default();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"files":[],"next_page_token":""}"#,
    ])));
    let account = super::finalize_xunlei_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
        modern_xunlei_fixture(),
        XunleiWebContext::default(),
        &credentials,
        &transport,
    )
    .unwrap();
    assert_eq!(account.id, "fixture-user");
    let saved: XunleiSession =
        super::load_provider_session(CloudProvider::Xunlei, &credentials).unwrap();
    assert_eq!(saved.user_id, account.id);
}

#[test]
fn xunlei_web_authorization_uses_the_selected_context_from_official_web_storage() {
    // Break caught: Xunlei's current browser login keeps the credentials
    // and drive challenge context in separate Web Storage records. If the
    // native bridge reads only cookies, a completed login remains pending
    // and the authorization window never closes.
    let (session, context) = super::parse_xunlei_web_authorization(serde_json::json!({
        "access_token": "browser-access",
        "user_id": "browser-user",
        "peer_id": "official-peer",
        "device_id": "official-device",
        "credit_key": "official-credit"
    }))
    .expect("the selected fields emitted from official Web Storage form a usable browser session");

    assert_eq!(session.user_id, "browser-user");
    assert_eq!(context.peer_id, "official-peer");
    assert_eq!(context.device_id, "official-device");
    assert_eq!(context.credit_key, "official-credit");
}

#[test]
fn xunlei_web_authorization_discards_a_native_client_secret_from_web_storage() {
    // Break caught: the official web authorization bridge must never
    // transfer a native client secret out of Web Storage or persist it.
    let session = super::parse_xunlei_web_session(serde_json::json!({
        "access_token": "browser-access",
        "user_id": "browser-user",
        "client_secret": "must-not-cross-webview-boundary"
    }))
    .expect("the browser bearer itself remains a valid candidate");

    assert!(session.profile.client_secret.is_empty());
}

#[test]
fn xunlei_restore_rejects_a_saved_browser_session_when_its_root_is_expired() {
    // Break caught: restoring the display account without the root probe
    // recreates an empty Xunlei tab after the saved browser bearer has
    // expired.
    let credentials = MemoryCredentialStore::default();
    let mut session = XunleiSession::from_web_authorization(
        "browser-access",
        "",
        "browser-user",
        "",
        XunleiClientProfile::default(),
        0,
    )
    .unwrap();
    session.web_context = XunleiWebContext {
        session_id: String::new(),
        peer_id: "official-peer".into(),
        device_id: String::new(),
        credit_key: "official-credit".into(),
    };
    credentials
        .save("xunlei", &serde_json::to_vec(&session).unwrap())
        .unwrap();
    let expired_root = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"error_code":4121,"error":"expired"}"#,
    ])));

    let result = super::restore_account(CloudProvider::Xunlei, &credentials, &expired_root);

    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
}

#[test]
fn uc_web_authorization_keeps_unverified_browser_cookies_out_of_storage() {
    // Break caught: saving immediately after merely seeing __pus/__puus
    // creates an unusable tab when UC requires web verification for its
    // first real directory request.
    let credentials = MemoryCredentialStore::default();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"code":0,"status":200,"message":"verify in official browser"}"#,
    ])));

    let error = super::finalize_uc_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Uc),
        UcSession::cookie("__uid=browser-user; __pus=identity; __puus=session"),
        &credentials,
        &transport,
    )
    .unwrap_err();

    assert!(matches!(error, QuarkError::AuthorizationPending));
    assert!(credentials.load("uc").unwrap().is_none());
}

#[test]
fn uc_web_authorization_does_not_replace_a_previous_session_when_root_verification_fails() {
    // Break caught: persisting a candidate browser cookie before the first
    // root read succeeds destroys a previously working UC authorization.
    let credentials = MemoryCredentialStore::default();
    credentials
        .save("uc", b"previous-verified-session")
        .unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"code":0,"status":200,"message":"risk verification required"}"#,
    ])));

    let error = super::finalize_uc_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Uc),
        UcSession::cookie("__uid=new-user; __pus=new-identity; __puus=new-session"),
        &credentials,
        &transport,
    )
    .unwrap_err();

    assert!(matches!(error, QuarkError::AuthorizationPending));
    assert_eq!(
        credentials.load("uc").unwrap().as_deref(),
        Some(b"previous-verified-session".as_slice())
    );
}

#[test]
fn uc_web_authorization_saves_a_candidate_only_after_root_verification_succeeds() {
    // Break caught: never persisting a verified candidate leaves a
    // successful official login unable to restore its UC workspace.
    let credentials = MemoryCredentialStore::default();
    credentials
        .save("uc", b"previous-verified-session")
        .unwrap();
    let transport = SequenceTransport(Mutex::new(VecDeque::from([
        r#"{"code":0,"status":200,"data":{"list":[]},"metadata":{"_total":0}}"#,
    ])));

    let account = super::finalize_uc_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Uc),
        UcSession::cookie("__uid=new-user; __pus=new-identity; __puus=new-session"),
        &credentials,
        &transport,
    )
    .expect("a verified root response completes authorization");

    assert_eq!(account.id, "new-user");
    let saved: UcSession =
        serde_json::from_slice(&credentials.load("uc").unwrap().unwrap()).unwrap();
    assert_eq!(
        saved.cookie,
        "__uid=new-user; __pus=new-identity; __puus=new-session"
    );
}

#[test]
fn tianyi_web_authorization_maps_root_authorization_errors_to_pending() {
    for error in [
        TianyiRootAuthorizationError::Expired,
        TianyiRootAuthorizationError::WebVerification,
        TianyiRootAuthorizationError::Captcha,
    ] {
        let credentials = MemoryCredentialStore::default();
        let result = super::finalize_tianyi_web_authorization(
            crate::cloud::ProviderRegistry::global().get(CloudProvider::Tianyi),
            TianyiSession::cookie("SSON=candidate-session"),
            &credentials,
            &TianyiRootAuthorizationErrorTransport(error),
        );

        assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
        assert!(credentials.load("tianyi").unwrap().is_none());
    }
}

#[test]
fn tianyi_web_authorization_keeps_the_old_session_when_candidate_root_is_unreadable() {
    let credentials = MemoryCredentialStore::default();
    let existing = serde_json::to_vec(&TianyiSession::cookie("SSON=existing-session"))
        .expect("the test session serializes");
    credentials.save("tianyi", &existing).unwrap();

    let result = super::finalize_tianyi_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Tianyi),
        TianyiSession::cookie("SSON=candidate-session"),
        &credentials,
        &TianyiRootRejectingTransport,
    );

    assert!(matches!(result, Err(QuarkError::Provider(_))));
    assert_eq!(credentials.load("tianyi").unwrap(), Some(existing));
}

#[test]
fn tianyi_web_authorization_keeps_the_old_session_when_candidate_root_shape_is_malformed() {
    let credentials = MemoryCredentialStore::default();
    let existing = serde_json::to_vec(&TianyiSession::cookie("SSON=existing-session"))
        .expect("the test session serializes");
    credentials.save("tianyi", &existing).unwrap();

    let result = super::finalize_tianyi_web_authorization(
        crate::cloud::ProviderRegistry::global().get(CloudProvider::Tianyi),
        TianyiSession::cookie("SSON=candidate-session"),
        &credentials,
        &TianyiMalformedRootTransport,
    );

    assert!(matches!(result, Err(QuarkError::InvalidResponse)));
    assert_eq!(credentials.load("tianyi").unwrap(), Some(existing));
}

#[test]
fn xunlei_browser_session_does_not_require_a_native_client_secret() {
    let session = super::parse_xunlei_web_session(serde_json::json!({
        "access_token": "browser-access",
        "refresh_token": "browser-refresh",
        "user_id": "browser-user",
        "client_id": "official-web-client",
        "device_id": "web-device",
        "client_version": "1.0.0",
        "package_name": "pan.xunlei.com",
        "user_agent": "Mozilla/5.0",
    }))
    .unwrap();

    assert!(session.profile.client_secret.is_empty());
    assert_eq!(session.user_id, "browser-user");
}

#[test]
fn xunlei_web_authorization_accepts_the_official_browser_user_state_without_native_identity() {
    // The current official web client keeps the freshly issued tokens in
    // Nuxt's users.curUser state.  It does not publish a native client
    // profile, so requiring that profile traps a completed web login in
    // its authorization window forever.
    let session = super::parse_xunlei_web_session(serde_json::json!({
        "access_token": "browser-access",
        "refresh_token": "browser-refresh",
        "user_id": "browser-user"
    }))
    .expect("official browser state needs no fabricated native profile");

    assert!(session.profile.client_id.is_empty());
    assert_eq!(session.user_id, "browser-user");
}

#[test]
fn xunlei_web_capture_preserves_modern_security_context_for_root_validation() {
    // Dropping the bridge's official signature makes the first Rust root
    // request unable to initialize its operation captcha.
    let session = super::parse_xunlei_web_session(serde_json::json!({
        "access_token": "browser-access", "user_id": "browser-user",
        "client_id": "Xqp0kJBXWhwaTpB6", "package_name": "pan.xunlei.com",
        "web_security": {
            "captcha_sign": "official-signature", "timestamp": "1789036800000",
            "action": "GET:/drive/v1/files", "client_version": "1.92.98",
            "package_name": "pan.xunlei.com"
        }
    }))
    .unwrap();
    assert!(session.profile.is_modern_web());
    assert_eq!(session.web_security.captcha_sign, "official-signature");
    assert_eq!(session.web_security.timestamp, "1789036800000");
    assert_eq!(session.web_security.action, "GET:/drive/v1/files");
    assert!(session.web_security.captcha_token.is_empty());
    assert!(session.web_context.credit_key.is_empty());
}

#[test]
fn xunlei_web_capture_rejects_malformed_security_before_saving() {
    let result = super::parse_xunlei_web_session(serde_json::json!({
        "access_token": "browser-access", "user_id": "browser-user",
        "web_security": {"captcha_sign": {"unexpected": "object"}}
    }));
    assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
}

#[test]
fn aliyun_browser_token_keeps_the_current_bearer_and_default_drive() {
    let session = super::parse_aliyun_web_session(serde_json::json!({
        "access_token": "browser-access",
        "refresh_token": "browser-refresh",
        "default_drive_id": "browser-drive",
    }))
    .unwrap();

    assert_eq!(session.access_token, "browser-access");
    assert_eq!(session.refresh_token, "browser-refresh");
    assert_eq!(session.drive_id, "browser-drive");
}
