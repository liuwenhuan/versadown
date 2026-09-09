use alldownloader_lib::cloud::{
    baidu::BaiduSession,
    commands::{logout_provider, restore_account},
    parse_stable_cloud_source, provider_capabilities,
    tianyi::TianyiSession,
    uc::UcSession,
    xunlei::{XunleiClientProfile, XunleiSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider, CredentialStore,
    MemoryCredentialStore, QuarkError,
};
use std::{collections::VecDeque, sync::Mutex};

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
fn logout_for_a_connected_new_provider_removes_its_credential() {
    let store = MemoryCredentialStore::default();
    store.save("baidu", b"still-authorized").unwrap();

    logout_provider(CloudProvider::Baidu, &store).unwrap();

    assert!(store.load("baidu").unwrap().is_none());
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
