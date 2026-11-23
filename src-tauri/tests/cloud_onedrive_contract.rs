//! OneDrive's registry contract, seen from outside the crate: a read-only
//! personal drive plus anonymous public share links.
use versadown_lib::cloud::{
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider, MemoryCredentialStore,
    ProviderRegistry, QuarkError,
};

struct NoIo;
impl CloudHttpTransport for NoIo {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        panic!("this contract must not touch the network")
    }
}

#[test]
fn onedrive_is_a_read_only_account_provider_with_public_shares() {
    let registry = ProviderRegistry::global();
    let onedrive = registry.get(CloudProvider::Onedrive);
    assert!(onedrive.account);
    assert!(!onedrive.public_shares_need_session);
    // Sign-in is the browser OAuth flow behind the web authorization button.
    assert!(onedrive.authorization.web && !onedrive.authorization.password);

    let capabilities = &onedrive.capabilities;
    assert!(capabilities.list && capabilities.download && capabilities.share_inspect);
    assert!(!capabilities.upload && !capabilities.create_directory && !capabilities.rename);
    assert!(!capabilities.r#move && !capabilities.copy && !capabilities.trash);
    assert!(!capabilities.delete && !capabilities.restore);
    assert!(!capabilities.share_transfer && !capabilities.recorded_transfer_resolve);
    assert!(!capabilities.share_create && !capabilities.share_list && !capabilities.share_cancel);
    assert_ne!(
        onedrive.root_directory_id,
        onedrive.recycle_bin_directory_id
    );

    // It is listed as a drive (unlike account-less providers).
    let catalog = serde_json::to_value(registry.catalog()).unwrap();
    let entry = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["provider"] == "onedrive")
        .unwrap();
    assert_eq!(entry["account"], true);
    // The build says whether sign-in is possible; the UI explains a refusal.
    assert!(entry["authorizationConfigured"].is_boolean());
}

#[test]
fn share_links_are_routed_to_onedrive_and_lookalikes_are_not() {
    let registry = ProviderRegistry::global();
    for url in [
        "https://1drv.ms/u/s!AbCdEf",
        "https://onedrive.live.com/redir?resid=A!1&authkey=!B",
    ] {
        let parsed = url::Url::parse(url).unwrap();
        assert_eq!(
            registry.for_share_url(&parsed).unwrap().provider,
            CloudProvider::Onedrive,
            "{url}"
        );
    }
    for url in [
        "https://1drv.ms.evil.test/u/s!AbCdEf",
        "https://evil.test/1drv.ms/u/s!AbCdEf",
    ] {
        assert!(registry
            .for_share_url(&url::Url::parse(url).unwrap())
            .is_none());
    }
}

#[test]
fn share_capabilities_are_read_only_and_build_without_io() {
    let store = MemoryCredentialStore::default();
    let shares =
        ProviderRegistry::global().share_capabilities(CloudProvider::Onedrive, &store, &NoIo);
    assert!(shares.inspect.is_some() && shares.resolve.is_some());
    assert!(shares.transfer.is_none() && shares.recorded.is_none());
}

#[test]
fn drive_operations_without_a_session_ask_for_authorization_before_any_io() {
    let store = MemoryCredentialStore::default();
    let mut driver = ProviderRegistry::global().driver(CloudProvider::Onedrive, &store, &NoIo);
    assert!(matches!(driver.account(), Err(QuarkError::NotAuthorized)));
    assert!(matches!(
        driver.list_directory("root"),
        Err(QuarkError::NotAuthorized)
    ));
    // Read-only: mutations are refused by the capability gate.
    assert!(matches!(
        driver.trash(&["x".into()]),
        Err(QuarkError::ProviderUnsupported)
    ));
}
