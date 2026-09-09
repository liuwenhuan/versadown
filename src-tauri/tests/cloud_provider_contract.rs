use alldownloader_lib::cloud::{
    commands::logout_provider, parse_stable_cloud_source, provider_capabilities, CloudProvider,
    CredentialStore, MemoryCredentialStore,
};

#[test]
fn new_providers_have_stable_sources_and_declared_login_methods() {
    for (source, provider, password) in [
        ("cloud://baidu/f1", CloudProvider::Baidu, true),
        ("cloud://tianyi/f1", CloudProvider::Tianyi, true),
        ("cloud://aliyun/f1", CloudProvider::Aliyun, false),
        ("cloud://xunlei/f1", CloudProvider::Xunlei, false),
    ] {
        assert_eq!(parse_stable_cloud_source(source).unwrap().0, provider);
        assert!(provider_capabilities(provider).web);
        assert_eq!(provider_capabilities(provider).password, password);
    }
}

#[test]
fn logout_for_an_unconnected_provider_preserves_its_credential() {
    let store = MemoryCredentialStore::default();
    store.save("baidu", b"still-authorized").unwrap();

    let error = logout_provider(CloudProvider::Baidu, &store).unwrap_err();

    assert_eq!(
        serde_json::to_value(error).unwrap()["code"],
        "provider_unsupported"
    );
    assert_eq!(store.load("baidu").unwrap().unwrap(), b"still-authorized");
}
