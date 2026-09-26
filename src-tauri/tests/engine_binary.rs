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
