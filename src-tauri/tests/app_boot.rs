#[test]
fn application_name_is_stable() {
    assert_eq!(alldownloader_lib::app_version(), env!("CARGO_PKG_VERSION"));
}
