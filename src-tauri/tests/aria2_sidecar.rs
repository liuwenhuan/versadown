use alldownloader_lib::aria2_binary_name;

#[test]
fn windows_sidecar_has_tauri_target_suffix() {
    assert_eq!(aria2_binary_name(), "aria2c-x86_64-pc-windows-msvc.exe");
}
