use versadown_lib::engine_binary::{EngineBinary, EngineBinaryError};

#[test]
fn linux_x86_64_uses_the_pinned_resource_name() {
    assert_eq!(
        EngineBinary::for_target("linux", "x86_64")
            .unwrap()
            .resource_name(),
        "aria2-next-x86_64-unknown-linux-gnu"
    );
}

#[test]
fn unsupported_target_returns_a_typed_error() {
    assert!(matches!(
        EngineBinary::for_target("linux", "aarch64"),
        Err(EngineBinaryError::UnsupportedTarget { .. })
    ));
}

#[test]
fn bundled_linux_engine_matches_the_runtime_checksum_pin() {
    // A sidecar upgrade must update the pin, or the app refuses to start.
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("binaries/aria2-next-x86_64-unknown-linux-gnu");
    EngineBinary::for_target("linux", "x86_64")
        .unwrap()
        .verify(&binary)
        .unwrap();
}
