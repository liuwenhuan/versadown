use alldownloader_lib::cloud::{
    baidu::BaiduSession,
    commands::{logout_provider, restore_account},
    load_password_credential, parse_stable_cloud_source, provider_capabilities,
    save_password_credential,
    tianyi::TianyiSession,
    uc::UcSession,
    xunlei::{XunleiClientProfile, XunleiSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudPasswordCredential,
    CloudProvider, CredentialStore, MemoryCredentialStore, PasswordLoginMode, QuarkError,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

#[test]
fn browser_account_replacement_fails_closed_and_attempts_all_owner_cleanup() {
    use alldownloader_lib::cloud::{
        baidu_browser_profile_username, select_password_credential_account,
        set_baidu_browser_profile_username,
    };
    struct FailingSessionRemoval {
        inner: MemoryCredentialStore,
        removed: Mutex<Vec<String>>,
    }
    impl CredentialStore for FailingSessionRemoval {
        fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
            self.inner.save(key, bytes)
        }
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
            self.inner.load(key)
        }
        fn remove(&self, key: &str) -> Result<(), QuarkError> {
            self.removed.lock().unwrap().push(key.into());
            if key == "baidu" {
                return Err(QuarkError::Storage);
            }
            self.inner.remove(key)
        }
    }
    let store = FailingSessionRemoval {
        inner: MemoryCredentialStore::default(),
        removed: Mutex::new(Vec::new()),
    };
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "a", "alice", "secret-a"),
    )
    .unwrap();
    set_baidu_browser_profile_username(&store, Some("alice")).unwrap();
    store.save("baidu", b"old-session").unwrap();
    assert!(matches!(
        select_password_credential_account(&store, CloudProvider::Baidu, "b"),
        Err(QuarkError::Storage)
    ));
    assert!(load_password_credential(&store, CloudProvider::Baidu)
        .unwrap()
        .is_none());
    assert!(baidu_browser_profile_username(&store).unwrap().is_none());
    assert!(store
        .removed
        .lock()
        .unwrap()
        .iter()
        .any(|key| key == "cloud-password-draft-baidu"));
}

#[test]
fn saved_credential_metadata_only_exists_for_catalog_direct_password_providers() {
    use alldownloader_lib::cloud::{saved_cloud_credential_info, ProviderRegistry};
    let store = MemoryCredentialStore::default();
    for descriptor in ProviderRegistry::global().catalog() {
        if descriptor.authorization.password_login_mode == PasswordLoginMode::RustDirect {
            save_password_credential(
                &store,
                CloudPasswordCredential::new(descriptor.provider, "only", "alice", "secret"),
            )
            .unwrap();
            let json = serde_json::to_value(
                saved_cloud_credential_info(&store, descriptor.provider)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                json,
                serde_json::json!({"username": "alice", "hasSavedPassword": true})
            );
        } else {
            assert!(saved_cloud_credential_info(&store, descriptor.provider)
                .unwrap()
                .is_none());
        }
    }
}

#[test]
fn registry_catalog_is_complete_unique_and_routes_each_share() {
    use alldownloader_lib::cloud::ProviderRegistry;
    use std::collections::HashSet;
    let registry = ProviderRegistry::global();
    let descriptors = registry.all();
    assert_eq!(descriptors.len(), 7);
    assert_eq!(
        descriptors
            .iter()
            .map(|d| d.provider)
            .collect::<HashSet<_>>()
            .len(),
        7
    );
    assert_eq!(
        descriptors
            .iter()
            .map(|d| d.provider.key())
            .collect::<HashSet<_>>()
            .len(),
        7
    );
    for descriptor in descriptors {
        assert!(!descriptor.root_directory_id.is_empty());
        assert!(!descriptor.recycle_bin_directory_id.is_empty());
        assert_ne!(
            descriptor.root_directory_id,
            descriptor.recycle_bin_directory_id
        );
        assert_eq!(
            registry.get(descriptor.provider).provider,
            descriptor.provider
        );
        let auth = url::Url::parse(descriptor.authorization_url).unwrap();
        assert_eq!(auth.scheme(), "https");
        for host in descriptor.share_hosts {
            let url = url::Url::parse(&format!(
                "https://{host}{}example",
                descriptor.share_patterns[0]
            ))
            .unwrap();
            assert_eq!(
                registry.for_share_url(&url).unwrap().provider,
                descriptor.provider
            );
            let spoof = url::Url::parse(&format!(
                "https://{host}.evil.test{}example",
                descriptor.share_patterns[0]
            ))
            .unwrap();
            assert!(registry.for_share_url(&spoof).is_none());
        }
    }
    assert_eq!(
        serde_json::to_value(registry.catalog()).unwrap()[0]["rootDirectoryId"],
        "0"
    );
}

#[test]
fn registry_only_advertises_implemented_share_transfer_capabilities() {
    use alldownloader_lib::cloud::ProviderRegistry;
    for (provider, supports_transfer) in [
        (CloudProvider::Quark, true),
        (CloudProvider::Baidu, true),
        (CloudProvider::Uc, true),
        (CloudProvider::Xunlei, true),
        (CloudProvider::Pikpak, false),
        (CloudProvider::Tianyi, false),
        (CloudProvider::Aliyun, false),
    ] {
        let capabilities = &ProviderRegistry::global().get(provider).capabilities;
        assert!(capabilities.share_inspect);
        assert_eq!(
            capabilities.share_transfer, supports_transfer,
            "{provider:?}"
        );
        assert_eq!(
            capabilities.recorded_transfer_resolve, supports_transfer,
            "{provider:?}"
        );
    }
}

#[test]
fn registry_keeps_password_browser_hosts_https_and_provider_scoped() {
    use alldownloader_lib::cloud::ProviderRegistry;
    let baidu = ProviderRegistry::global().get(CloudProvider::Baidu);
    assert!(baidu
        .accepts_password_login_url(&url::Url::parse("https://passport.baidu.com/login").unwrap()));
    for value in [
        "http://passport.baidu.com/login",
        "https://passport.baidu.com:444/login",
        "https://passport.baidu.com.evil.test/login",
        "https://open.e.189.cn/login",
    ] {
        assert!(!baidu.accepts_password_login_url(&url::Url::parse(value).unwrap()));
    }
    let tianyi = ProviderRegistry::global().get(CloudProvider::Tianyi);
    assert!(
        tianyi.accepts_password_login_url(&url::Url::parse("https://open.e.189.cn/login").unwrap())
    );
    assert!(!ProviderRegistry::global()
        .get(CloudProvider::Quark)
        .accepts_password_login_url(&url::Url::parse("https://pan.quark.cn").unwrap()));
}

struct RecordingHttp(Mutex<VecDeque<&'static str>>);

impl RecordingHttp {
    fn sequence(responses: impl IntoIterator<Item = &'static str>) -> Self {
        Self(Mutex::new(responses.into_iter().collect()))
    }
}

impl CloudHttpTransport for RecordingHttp {
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

struct FailingSessionRemovalStore {
    values: Mutex<BTreeMap<String, Vec<u8>>>,
    removals: Mutex<Vec<String>>,
}

impl FailingSessionRemovalStore {
    fn new() -> Self {
        Self {
            values: Mutex::new(BTreeMap::new()),
            removals: Mutex::new(Vec::new()),
        }
    }

    fn removals(&self) -> Vec<String> {
        self.removals.lock().unwrap().clone()
    }
}

impl CredentialStore for FailingSessionRemovalStore {
    fn save(&self, key: &str, value: &[u8]) -> Result<(), QuarkError> {
        self.values
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .insert(key.into(), value.into());
        Ok(())
    }

    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .get(key)
            .cloned())
    }

    fn remove(&self, key: &str) -> Result<(), QuarkError> {
        self.removals
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .push(key.into());
        if key == "baidu" {
            return Err(QuarkError::Storage);
        }
        self.values
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .remove(key);
        Ok(())
    }
}

#[test]
fn new_providers_have_stable_sources_and_declared_login_methods() {
    for (source, provider, password) in [
        ("cloud://baidu/f1", CloudProvider::Baidu, true),
        ("cloud://tianyi/f1", CloudProvider::Tianyi, true),
        ("cloud://aliyun/f1", CloudProvider::Aliyun, false),
        ("cloud://xunlei/f1", CloudProvider::Xunlei, false),
        ("cloud://uc/f1", CloudProvider::Uc, false),
    ] {
        assert_eq!(parse_stable_cloud_source(source).unwrap().0, provider);
        assert!(provider_capabilities(provider).web);
        assert_eq!(provider_capabilities(provider).password, password);
    }
}

#[test]
fn capabilities_include_provider_and_distinguish_direct_from_official_web_password_login() {
    let baidu = provider_capabilities(CloudProvider::Baidu);
    assert_eq!(serde_json::to_value(baidu).unwrap()["provider"], "baidu");
    assert_eq!(baidu.password_login_mode, PasswordLoginMode::RustDirect);
    assert!(baidu.password);

    let aliyun = provider_capabilities(CloudProvider::Aliyun);
    assert_eq!(
        aliyun.password_login_mode,
        PasswordLoginMode::OfficialWebView
    );
    assert!(!aliyun.password);
}

#[test]
fn logout_for_a_connected_new_provider_removes_its_credential() {
    let store = MemoryCredentialStore::default();
    store.save("baidu", b"still-authorized").unwrap();

    logout_provider(CloudProvider::Baidu, &store).unwrap();

    assert!(store.load("baidu").unwrap().is_none());
}

#[test]
fn logout_removes_the_password_recovery_credential_too() {
    let store = MemoryCredentialStore::default();
    store.save("baidu", b"still-authorized").unwrap();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
    )
    .unwrap();

    logout_provider(CloudProvider::Baidu, &store).unwrap();

    assert!(store.load("baidu").unwrap().is_none());
    assert!(load_password_credential(&store, CloudProvider::Baidu)
        .unwrap()
        .is_none());
}

#[test]
fn logout_attempts_password_recovery_cleanup_when_normal_session_removal_fails() {
    let store = FailingSessionRemovalStore::new();
    store.save("baidu", b"still-authorized").unwrap();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
    )
    .unwrap();

    assert!(matches!(
        logout_provider(CloudProvider::Baidu, &store),
        Err(QuarkError::Storage)
    ));
    assert!(store.removals().contains(&"baidu".to_owned()));
    assert!(store
        .removals()
        .contains(&"cloud-password-baidu".to_owned()));
    assert!(load_password_credential(&store, CloudProvider::Baidu)
        .unwrap()
        .is_none());
}

#[test]
fn each_new_stable_source_resolves_to_its_new_provider() {
    for source in [
        "cloud://baidu/f1",
        "cloud://tianyi/f1",
        "cloud://aliyun/f1",
        "cloud://xunlei/f1",
        "cloud://uc/f1",
    ] {
        let (provider, id) = parse_stable_cloud_source(source).unwrap();
        assert!(!id.is_empty());
        assert!(matches!(
            provider,
            CloudProvider::Baidu
                | CloudProvider::Tianyi
                | CloudProvider::Aliyun
                | CloudProvider::Xunlei
                | CloudProvider::Uc
        ));
    }
}

#[test]
fn restore_account_dispatches_each_new_provider_without_falling_back_to_unsupported() {
    let baidu = MemoryCredentialStore::default();
    baidu
        .save(
            "baidu",
            &serde_json::to_vec(&BaiduSession::cookie("BDUSS=session")).unwrap(),
        )
        .unwrap();
    assert_eq!(
        restore_account(
            CloudProvider::Baidu,
            &baidu,
            &RecordingHttp::sequence([r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#]),
        )
        .unwrap()
        .provider,
        CloudProvider::Baidu
    );

    let tianyi = MemoryCredentialStore::default();
    tianyi
        .save(
            "tianyi",
            &serde_json::to_vec(&TianyiSession::cookie("COOKIE_LOGIN_USER=session")).unwrap(),
        )
        .unwrap();
    assert_eq!(
        restore_account(
            CloudProvider::Tianyi,
            &tianyi,
            &RecordingHttp::sequence([
                r#"{"res_code":0,"userInfo":{"userId":"t1","nickName":"tianyi"}}"#
            ]),
        )
        .unwrap()
        .provider,
        CloudProvider::Tianyi
    );

    let aliyun = MemoryCredentialStore::default();
    aliyun
        .save(
            "aliyun",
            br#"{"refresh_token":"refresh","access_token":"old","drive_id":"d1"}"#,
        )
        .unwrap();
    assert_eq!(
        restore_account(
            CloudProvider::Aliyun,
            &aliyun,
            &RecordingHttp::sequence([
                r#"{"access_token":"access","refresh_token":"next","default_drive_id":"d1"}"#,
                r#"{"user_id":"a1","nick_name":"aliyun"}"#,
                r#"{"items":[]}"#,
            ]),
        )
        .unwrap()
        .provider,
        CloudProvider::Aliyun
    );

    let xunlei = MemoryCredentialStore::default();
    let xunlei_session = XunleiSession::with_access(
        "access",
        "refresh",
        XunleiClientProfile {
            client_id: "client".into(),
            client_secret: "secret".into(),
            device_id: "device".into(),
            client_version: "1.0".into(),
            package_name: "package".into(),
            user_agent: "agent".into(),
        },
    );
    xunlei
        .save("xunlei", &serde_json::to_vec(&xunlei_session).unwrap())
        .unwrap();
    assert_eq!(
        restore_account(
            CloudProvider::Xunlei,
            &xunlei,
            &RecordingHttp::sequence([r#"{"id":"x1","name":"xunlei"}"#]),
        )
        .unwrap()
        .provider,
        CloudProvider::Xunlei
    );

    let uc = MemoryCredentialStore::default();
    uc.save(
        "uc",
        &serde_json::to_vec(&UcSession::cookie(
            "__uid=u1; __pus=identity; __puus=session",
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        restore_account(
            CloudProvider::Uc,
            &uc,
            &RecordingHttp::sequence([r#"{"code":0,"status":200,"data":{"nickname":"uc"}}"#]),
        )
        .unwrap()
        .provider,
        CloudProvider::Uc
    );
}
