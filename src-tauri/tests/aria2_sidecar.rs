use alldownloader_lib::aria2_binary_name;
use sha2::{Digest, Sha256};

#[test]
fn windows_sidecar_has_tauri_target_suffix() {
    assert_eq!(aria2_binary_name(), "aria2-next-x86_64-pc-windows-msvc.exe");
}

#[test]
fn bundled_aria2_next_matches_the_pinned_official_release() {
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(aria2_binary_name());
    let digest = format!("{:x}", Sha256::digest(std::fs::read(binary).unwrap()));
    assert_eq!(
        digest,
        "ad5159a68b3132d83444d4bb8285d750f5c3579fe581fc14211426b30bd39d35"
    );
}
