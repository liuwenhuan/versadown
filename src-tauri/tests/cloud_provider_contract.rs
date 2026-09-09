use alldownloader_lib::cloud::{
    parse_stable_cloud_source, provider_capabilities, CloudProvider,
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
