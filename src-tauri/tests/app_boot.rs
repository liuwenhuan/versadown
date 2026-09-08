#[test]
fn application_name_is_stable() {
    assert_eq!(alldownloader_lib::app_version(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn background_ui_test_mode_keeps_native_windows_hidden() {
    assert!(!alldownloader_lib::should_show_windows(Some("1")));
    assert!(alldownloader_lib::should_show_windows(None));
    assert!(alldownloader_lib::should_show_windows(Some("0")));
}
