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
            .join("binaries/aria2-next-x86_64-pc-windows-msvc.exe"),
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
        "--pause=true",
        "--pause-metadata=true",
        "--continue=true",
        "--state-dir=fixture\\data\\aria2-next-state",
        "--input-file=fixture\\data\\aria2-next.tasks",
        "--save-session=fixture\\data\\aria2-next.tasks",
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
    assert!(!args.iter().any(|arg| arg.contains("aria2.session")));
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
        debug_logging: false,
        overall_download_limit: 0,
        p2p_upload_limit: 1_048_576,
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

#[test]
fn launch_arguments_enable_detailed_sidecar_logging_only_when_requested() {
    let mut config = config(std::path::Path::new("fixture"));
    config.aria2.debug_logging = true;
    let args = build_aria2_args(&config, &RpcSecret::from_bytes([0xab; 32]));

    assert!(args.contains(&"--log=fixture\\data\\aria2-debug.log".into()));
    assert!(args.contains(&"--log-level=debug".into()));
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
    assert_eq!(client.get_version().await.unwrap().version, "2.8.0");
    let unauthenticated =
        Aria2RpcClient::new(client.endpoint().as_str(), RpcSecret::from_bytes([0; 32])).unwrap();
    assert!(unauthenticated.get_global_stat().await.is_err());
    assert_eq!(client.save_session().await.unwrap(), "OK");
    assert_eq!(client.shutdown().await.unwrap(), "OK");
    assert!(child.wait_for_exit(Duration::from_secs(5)).await.unwrap());
    assert!(!child.is_running().unwrap());
    let session = std::fs::read_to_string(root.path().join("data/aria2-next.tasks")).unwrap();
    assert!(!session.contains("rpc-secret"));
    assert!(!session.contains("token:"));
}

#[tokio::test]
async fn startup_reports_a_busy_rpc_port_without_waiting_for_readiness() {
    let root = tempfile::tempdir().unwrap();
    let port_guard = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = port_guard.local_addr().unwrap().port();
    let mut launch = config(root.path());
    launch.aria2.rpc_port = port;

    let started = std::time::Instant::now();
    assert!(matches!(
        Aria2Process::start(launch).await,
        Err(Aria2Error::RpcPortInUse(actual)) if actual == port
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn bundled_sidecar_writes_native_and_rpc_debug_logs_when_enabled() {
    let root = tempfile::tempdir().unwrap();
    let mut launch = config(root.path());
    launch.aria2.debug_logging = true;
    let rpc_token = launch.aria2.rpc_token.clone();
    let (mut child, client) = Aria2Process::start(launch).await.unwrap();

    assert_eq!(client.get_version().await.unwrap().version, "2.8.0");
    assert_eq!(client.shutdown().await.unwrap(), "OK");
    assert!(child.wait_for_exit(Duration::from_secs(5)).await.unwrap());

    let data = root.path().join("data");
    assert!(data.join("aria2-debug.log").is_file());
    let rpc_log = std::fs::read_to_string(data.join("aria2-rpc-debug.log")).unwrap();
    assert!(rpc_log.contains(r#""operation":"aria2.getVersion""#));
    assert!(!rpc_log.contains(&rpc_token));
}

#[tokio::test]
async fn legacy_cutover_removes_conflicting_state_bundle_before_launch() {
    let root = tempfile::tempdir().unwrap();
    let mut launch = config(root.path());
    launch.binary = root.path().join("missing-aria2-next.exe");
    let stream = launch.data_dir.join("aria2-next-state/stream");
    std::fs::create_dir_all(&stream).unwrap();
    let database = rusqlite::Connection::open(stream.join("state.db")).unwrap();
    database
        .execute_batch(
            "CREATE TABLE downloads(gid TEXT PRIMARY KEY, uri TEXT NOT NULL, path TEXT NOT NULL, etag TEXT NOT NULL, last_modified TEXT NOT NULL, total_length INTEGER NOT NULL, completed_length INTEGER NOT NULL, completed_ranges TEXT NOT NULL, updated_at INTEGER NOT NULL DEFAULT (unixepoch()));",
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO downloads(gid,uri,path,etag,last_modified,total_length,completed_length,completed_ranges) VALUES(?1,?2,?3,'','',10,1,'[]')",
            rusqlite::params![
                "aaaaaaaaaaaaaaaa",
                "https://example.test/duplicate",
                root.path().join("duplicate.bin").to_string_lossy(),
            ],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO downloads(gid,uri,path,etag,last_modified,total_length,completed_length,completed_ranges) VALUES(?1,?2,?3,'','',10,1,'[]')",
            rusqlite::params![
                "cccccccccccccccc",
                "https://example.test/state-only",
                root.path().join("state-only.bin").to_string_lossy(),
            ],
        )
        .unwrap();
    drop(database);
    std::fs::create_dir_all(&launch.data_dir).unwrap();
    std::fs::write(launch.data_dir.join("aria2.session"), b"legacy task\n").unwrap();

    assert!(Aria2Process::start(launch).await.is_err());

    assert!(!root.path().join("data/aria2.session").exists());
    assert!(!stream.exists());
    assert!(root.path().join("data/aria2-next.tasks").is_file());
}

#[tokio::test]
async fn startup_preserves_native_state_when_no_legacy_manifest_exists() {
    let root = tempfile::tempdir().unwrap();
    let mut launch = config(root.path());
    launch.binary = root.path().join("missing-aria2-next.exe");
    let stream = launch.data_dir.join("aria2-next-state/stream");
    std::fs::create_dir_all(&stream).unwrap();
    let state = stream.join("state.db");
    std::fs::write(&state, b"native-state-fixture").unwrap();

    assert!(Aria2Process::start(launch).await.is_err());

    assert_eq!(std::fs::read(state).unwrap(), b"native-state-fixture");
    assert!(root.path().join("data/aria2-next.tasks").is_file());
}
