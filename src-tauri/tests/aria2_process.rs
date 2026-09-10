use alldownloader_lib::{
    aria2::{process, rpc, types},
    Aria2Settings,
};
use process::{build_aria2_args, Aria2LaunchConfig, Aria2Process};
use rpc::{Aria2Api, Aria2RpcClient};
use std::{path::PathBuf, time::Duration};
use types::{Aria2Error, RpcSecret};

fn config(root: &std::path::Path) -> Aria2LaunchConfig {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut aria2 = Aria2Settings::default();
    aria2.rpc_port = listener.local_addr().unwrap().port();
    aria2.rpc_token = format!("aria2-process-test-{}", aria2.rpc_port);
    drop(listener);
    Aria2LaunchConfig {
        binary: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
        data_dir: root.join("data"),
        download_dir: root.join("downloads"),
        parent_pid: std::process::id(),
        peer_discovery: false,
        aria2,
    }
}

#[test]
fn production_can_discover_peers_for_trackerless_magnets() {
    let mut config = config(std::path::Path::new("fixture"));
    config.peer_discovery = true;
    let args = build_aria2_args(&config, &RpcSecret::from_bytes([0xab; 32]));
    assert!(args.contains(&"--enable-dht=true".into()));
    assert!(args.contains(&"--bt-enable-lpd=true".into()));
}

#[test]
fn launch_arguments_disable_remote_rpc_and_enable_persistence() {
    let args = build_aria2_args(
        &config(std::path::Path::new("fixture")),
        &RpcSecret::from_bytes([0xab; 32]),
    );
    for expected in [
        "--enable-rpc=true",
        "--rpc-listen-all=false",
        "--rpc-allow-origin-all=false",
        "--save-session-interval=10",
        "--pause-metadata=true",
        "--continue=true",
        "--no-conf=true",
        "--enable-dht=false",
        "--enable-dht6=false",
        "--bt-enable-lpd=false",
    ] {
        assert!(args.iter().any(|arg| arg == expected));
    }
    assert!(args
        .iter()
        .any(|arg| arg == &format!("--stop-with-process={}", std::process::id())));
    assert!(args.iter().any(|arg| arg.starts_with("--save-session=")));
}

#[test]
fn launch_arguments_apply_user_configured_rpc_and_transfer_limits() {
    let mut config = config(std::path::Path::new("fixture"));
    config.aria2 = Aria2Settings {
        rpc_port: 16800,
        rpc_token: "local-token".into(),
        max_concurrent_downloads: 3,
        max_connections_per_server: 4,
        bt_seed_ratio: 1.5,
    };
    let args = build_aria2_args(&config, &RpcSecret::from_token("local-token").unwrap());

    for expected in [
        "--rpc-listen-port=16800",
        "--split=4",
        "--max-connection-per-server=4",
        "--max-concurrent-downloads=3",
        "--seed-ratio=1.5",
    ] {
        assert!(args.iter().any(|arg| arg == expected));
    }
}

#[tokio::test]
async fn readiness_timeout_is_bounded() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Aria2RpcClient::with_timeout(
        format!("http://{}/jsonrpc", listener.local_addr().unwrap()),
        RpcSecret::from_bytes([0xab; 32]),
        Duration::from_secs(2),
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(
        process::wait_rpc_ready(&client, Duration::from_millis(50)).await,
        Err(Aria2Error::ReadinessTimeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn bundled_sidecar_is_authenticated_saves_session_and_exits() {
    let root = tempfile::tempdir().unwrap();
    let (mut child, client) = Aria2Process::start(config(root.path())).await.unwrap();
    assert!(child.is_running().unwrap());
    assert_eq!(client.get_version().await.unwrap().version, "1.37.0");
    let unauthenticated =
        Aria2RpcClient::new(client.endpoint().as_str(), RpcSecret::from_bytes([0; 32])).unwrap();
    assert!(unauthenticated.get_global_stat().await.is_err());
    assert_eq!(client.save_session().await.unwrap(), "OK");
    assert_eq!(client.shutdown().await.unwrap(), "OK");
    assert!(child.wait_for_exit(Duration::from_secs(5)).await.unwrap());
    assert!(!child.is_running().unwrap());
    let session = std::fs::read_to_string(root.path().join("data/aria2.session")).unwrap();
    assert!(!session.contains("rpc-secret"));
    assert!(!session.contains("token:"));
}
