use versadown_lib::{CloudConnector, MockConnector, Provider};

#[test]
fn mock_quark_account_lists_root_after_connection() {
    let connector = MockConnector::connected(Provider::Quark);
    let directory = connector.list_directory("/").expect("mock root");
    assert!(!directory.items.is_empty());
}

#[test]
fn disconnected_connector_requires_user_authorization() {
    let connector = MockConnector::disconnected(Provider::PikPak);
    assert!(connector.list_directory("/").is_err());
}
