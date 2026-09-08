#[path = "aria2_sync.rs"]
mod fixture;
use alldownloader_lib::{DownloadService, NewTask, TaskStatus, TaskStore};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

async fn slow_server(
    delay: Duration,
) -> (
    alldownloader_lib::aria2::Aria2RpcClient,
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Receiver<String>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let (sender, receiver) = tokio::sync::mpsc::channel(20);
    let app = axum::Router::new().route(
        "/jsonrpc",
        axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
            let sender = sender.clone();
            async move {
                sender
                    .send(request["method"].as_str().unwrap().to_string())
                    .await
                    .unwrap();
                tokio::time::sleep(delay).await;
                axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":"OK"}))
            }
        }),
    );
    let worker = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        alldownloader_lib::aria2::Aria2RpcClient::new(
            endpoint,
            alldownloader_lib::aria2::RpcSecret::from_bytes([1; 32]),
        )
        .unwrap(),
        worker,
        receiver,
    )
}

#[tokio::test]
async fn shutdown_one_deadline_covers_both_rpc_phases() {
    let (client, server, _requests) = slow_server(Duration::from_millis(90)).await;
    let service = DownloadService::new(
        Arc::new(client),
        Arc::new(TaskStore::open_in_memory().unwrap()),
    );
    let began = std::time::Instant::now();
    let report = service.shutdown_report(Duration::from_millis(120)).await;
    assert!(report.session_saved);
    assert!(
        !report.shutdown_requested,
        "shutdown must not get a fresh timeout after saveSession"
    );
    assert!(began.elapsed() < Duration::from_millis(170));
    server.abort();
}

#[tokio::test]
async fn shutdown_deadline_includes_busy_operation_lock_and_stops_owned_child() {
    use alldownloader_lib::aria2::{Aria2LaunchConfig, Aria2Process};
    let root = tempfile::tempdir().unwrap();
    let config = Aria2LaunchConfig {
        binary: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
        data_dir: root.path().join("data"),
        download_dir: root.path().join("downloads"),
        parent_pid: std::process::id(),
        peer_discovery: false,
    };
    let (child, _) = Aria2Process::start(config).await.unwrap();
    let (client, server, mut requests) = slow_server(Duration::from_secs(1)).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let service = Arc::new(DownloadService::with_process(
        Arc::new(client),
        store,
        child,
    ));
    let operation_service = service.clone();
    let operation = tokio::spawn(async move {
        operation_service
            .control(task.id, alldownloader_lib::DownloadAction::Pause)
            .await
    });
    assert_eq!(requests.recv().await.as_deref(), Some("aria2.pause"));
    let began = std::time::Instant::now();
    let report = service.shutdown_report(Duration::from_millis(100)).await;
    assert!(
        began.elapsed() < Duration::from_millis(200),
        "operation-lock wait exceeded shutdown deadline"
    );
    assert!(report.force_killed && report.child_stopped);
    assert!(!report.session_saved && !report.shutdown_requested);
    assert_eq!(service.engine_running().await.unwrap(), Some(false));
    operation.abort();
    server.abort();
}

#[tokio::test]
async fn shutdown_attempts_shutdown_after_save_failure_and_rejects_later_submissions() {
    let calls = Arc::new(Mutex::new(vec![]));
    let wire = calls.clone();
    let (client, server) = fixture::server(move |request| {
        let method = request["method"].as_str().unwrap().to_string();
        wire.lock().unwrap().push(method.clone());
        match method.as_str() {
            "aria2.saveSession" => json!(false),
            "aria2.shutdown" => json!("OK"),
            other => panic!("{other}"),
        }
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store);
    let report = service.shutdown_report(Duration::from_millis(50)).await;
    assert!(!report.session_saved);
    assert!(report.shutdown_requested);
    assert!(!report.force_killed);
    assert!(service
        .submit_uri(task.id, vec![task.source], Default::default())
        .await
        .is_err());
    assert_eq!(
        *calls.lock().unwrap(),
        ["aria2.saveSession", "aria2.shutdown"]
    );
    server.abort();
}

#[tokio::test]
async fn recovery_matches_known_imports_unknown_and_reconciles_missing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("finished.bin");
    std::fs::write(&output, b"done").unwrap();
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" => json!([fixture::status("known", "active", 25)]),
            "aria2.tellWaiting" => json!([fixture::status("unknown", "paused", 0)]),
            "aria2.tellStopped" => json!([]),
            other => panic!("{other}"),
        })
        .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let known = store
        .create(NewTask::direct("http://127.0.0.1/known"))
        .unwrap();
    store.bind_gid(known.id, "known").unwrap();
    let missing = store
        .create(NewTask::direct("http://127.0.0.1/missing"))
        .unwrap();
    let mut missing = store.bind_gid(missing.id, "missing").unwrap();
    missing.status = TaskStatus::Downloading;
    store.apply_transfer_snapshot(missing.clone()).unwrap();
    let finished = store
        .create(NewTask::direct("http://127.0.0.1/finished"))
        .unwrap();
    let mut finished = store.bind_gid(finished.id, "finished").unwrap();
    finished.status = TaskStatus::Downloading;
    store.apply_transfer_snapshot(finished.clone()).unwrap();
    store
        .upsert_files(
            finished.id,
            &[(1, output.to_string_lossy().into_owned(), 4, 4, true)],
        )
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    let report = service.recover().await.unwrap();
    assert_eq!(report.imported, 1);
    assert_eq!(store.find(known.id).unwrap().progress, 25);
    assert_eq!(store.find(missing.id).unwrap().status, TaskStatus::Failed);
    assert_eq!(
        store.find(finished.id).unwrap().status,
        TaskStatus::Completed
    );
    let imported = store.find_by_gid("unknown").unwrap().unwrap();
    assert_eq!(imported.name, "恢复的下载任务");
    assert_eq!(imported.status, TaskStatus::Paused);
    assert_eq!(store.list().unwrap().len(), 4);
    service.recover().await.unwrap();
    assert_eq!(store.list().unwrap().len(), 4);
    server.abort();
}

#[tokio::test]
async fn restart_budget_allows_exactly_one_attempt_even_when_restart_fails() {
    use alldownloader_lib::aria2::recovery::RestartBudget;
    let budget = RestartBudget::default();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let first = budget
        .run_once(|| async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<(), _>("failed")
        })
        .await;
    assert_eq!(first, Some(Err("failed")));
    let second = budget
        .run_once(|| async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<(), &str>(())
        })
        .await;
    assert_eq!(second, None);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn owned_engine_restarts_once_and_shutdown_kills_only_after_deadline() {
    use alldownloader_lib::aria2::{Aria2Api, Aria2LaunchConfig, Aria2Process};
    let root = tempfile::tempdir().unwrap();
    let config = Aria2LaunchConfig {
        binary: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
        data_dir: root.path().join("data"),
        download_dir: root.path().join("downloads"),
        parent_pid: std::process::id(),
        peer_discovery: false,
    };
    let (child, client) = Aria2Process::start(config.clone()).await.unwrap();
    let service = Arc::new(DownloadService::with_process(
        Arc::new(client.clone()),
        Arc::new(TaskStore::open_in_memory().unwrap()),
        child,
    ));
    client.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.engine_running().await.unwrap() == Some(true) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let sync = alldownloader_lib::aria2::sync::TransferSynchronizer::new(service.clone())
        .with_websocket(client)
        .with_restart_config(config.clone());
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(sync.run(receiver));
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.engine_running().await.unwrap() != Some(true) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    cancel.send(true).unwrap();
    worker.await.unwrap();
    assert_eq!(service.engine_running().await.unwrap(), Some(true));
    service.api().shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.engine_running().await.unwrap() == Some(true) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert!(service
        .restart_if_exited(config.clone())
        .await
        .unwrap()
        .is_none());
    assert_eq!(service.engine_running().await.unwrap(), Some(false));

    // A fake RPC shutdown acknowledges without stopping the actual owned sidecar.
    let (child, _) = Aria2Process::start(config).await.unwrap();
    let (fake, fake_server) = fixture::server(|_| json!("OK")).await;
    let service = DownloadService::with_process(
        Arc::new(fake),
        Arc::new(TaskStore::open_in_memory().unwrap()),
        child,
    );
    let began = std::time::Instant::now();
    let report = service.shutdown_report(Duration::from_millis(120)).await;
    assert!(report.force_killed && report.child_stopped);
    assert!(began.elapsed() >= Duration::from_millis(120));
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(service.engine_running().await.unwrap(), Some(false));
    fake_server.abort();
}

#[tokio::test]
async fn recovery_resumes_control_file_in_its_original_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("partial.bin");
    std::fs::write(&file, b"part").unwrap();
    std::fs::write(dir.path().join("partial.bin.aria2"), b"fixture").unwrap();
    let expected_dir = dir.path().to_string_lossy().to_string();
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            "aria2.addUri" => {
                assert_eq!(request["params"][2]["continue"], "true");
                assert_eq!(request["params"][2]["dir"], expected_dir);
                assert_eq!(request["params"][2]["out"], "partial.bin");
                json!("resumed")
            }
            "aria2.tellStatus" => fixture::status("resumed", "active", 4),
            other => panic!("{other}"),
        })
        .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/partial.bin"))
        .unwrap();
    let mut task = store.bind_gid(task.id, "missing").unwrap();
    task.status = TaskStatus::Downloading;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    store
        .upsert_files(
            task.id,
            &[(1, file.to_string_lossy().into_owned(), 100, 4, true)],
        )
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    assert_eq!(service.recover().await.unwrap().resumed, 1);
    let task = store.find(task.id).unwrap();
    assert_eq!(task.aria_gid.as_deref(), Some("resumed"));
    assert_eq!(task.status, TaskStatus::Downloading);
    assert_eq!(std::fs::read(file).unwrap(), b"part");
    assert!(dir.path().join("partial.bin.aria2").is_file());
    server.abort();
}

#[tokio::test]
async fn replacement_client_remains_available_when_recovery_persistence_fails() {
    use alldownloader_lib::aria2::{Aria2Api, Aria2LaunchConfig, Aria2Process};
    let root = tempfile::tempdir().unwrap();
    let config = Aria2LaunchConfig {
        binary: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
        data_dir: root.path().join("data"),
        download_dir: root.path().join("downloads"),
        parent_pid: std::process::id(),
        peer_discovery: false,
    };
    let database = root.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/missing"))
        .unwrap();
    let mut task = store.bind_gid(task.id, "missing").unwrap();
    task.status = TaskStatus::Downloading;
    store.apply_transfer_snapshot(task).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_recovery BEFORE UPDATE OF status ON tasks WHEN NEW.status='failed' BEGIN SELECT RAISE(ABORT,'fixture recovery persistence failed'); END;").unwrap();
    let (child, client) = Aria2Process::start(config.clone()).await.unwrap();
    let old_endpoint = client.endpoint().to_string();
    let service = DownloadService::with_process(Arc::new(client.clone()), store, child);
    client.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.engine_running().await.unwrap() == Some(true) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let result = service.restart_if_exited(config).await;
    assert!(
        result.is_ok(),
        "recovery failure discarded an already-running replacement client: {result:?}"
    );
    let replacement = result.unwrap().unwrap();
    // The new client must be usable by the notification coordinator even when
    // importing/reconciling the persisted tasks failed.
    assert_ne!(replacement.client.endpoint().as_str(), old_endpoint);
    assert!(replacement.recovery.is_err());
    assert_eq!(
        replacement.client.get_version().await.unwrap().version,
        "1.37.0"
    );
    assert!(
        service
            .shutdown_report(Duration::from_secs(5))
            .await
            .child_stopped
    );
}

#[tokio::test]
async fn recovery_failure_still_switches_websocket_and_wakes_idle_sync() {
    use alldownloader_lib::aria2::{
        sync::TransferSynchronizer, Aria2Api, Aria2GlobalStat, Aria2LaunchConfig, Aria2Process,
        EventSink,
    };
    struct Events {
        target: uuid::Uuid,
        ready: tokio::sync::Notify,
        downloading: tokio::sync::Notify,
    }
    impl EventSink for Events {
        fn task_updated(&self, t: &alldownloader_lib::DownloadTask) {
            if t.id == self.target && t.status == TaskStatus::Downloading {
                self.downloading.notify_one();
            }
        }
        fn global_stat_updated(&self, _: &Aria2GlobalStat) {
            self.ready.notify_one();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let config = Aria2LaunchConfig {
        binary: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
        data_dir: root.path().join("data"),
        download_dir: root.path().join("downloads"),
        parent_pid: std::process::id(),
        peer_discovery: false,
    };
    let database = root.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    let missing = store
        .create(NewTask::direct("http://127.0.0.1/missing"))
        .unwrap();
    let mut missing = store.bind_gid(missing.id, "missing").unwrap();
    missing.status = TaskStatus::Paused;
    store.apply_transfer_snapshot(missing).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_recovery BEFORE UPDATE OF status ON tasks WHEN NEW.status='failed' BEGIN SELECT RAISE(ABORT,'fixture recovery failure'); END;").unwrap();
    let target = store
        .create(NewTask::direct("http://127.0.0.1/target"))
        .unwrap();
    let events = Arc::new(Events {
        target: target.id,
        ready: Default::default(),
        downloading: Default::default(),
    });
    let (child, client) = Aria2Process::start(config.clone()).await.unwrap();
    let service = Arc::new(
        DownloadService::with_process(Arc::new(client.clone()), store.clone(), child)
            .with_events(events.clone()),
    );
    client.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.engine_running().await.unwrap() == Some(true) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let sync = TransferSynchronizer::new(service.clone())
        .with_websocket(client)
        .with_restart_config(config);
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(sync.run(receiver));
    tokio::time::timeout(Duration::from_secs(5), events.ready.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/file", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/file",
        axum::routing::get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "fixture"
        }),
    );
    let file_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let gid = service
        .api()
        .add_uri(
            vec![uri],
            std::collections::BTreeMap::from([("pause".into(), "true".into())]),
        )
        .await
        .unwrap();
    store.bind_gid(target.id, &gid).unwrap();
    service.api().unpause(&gid).await.unwrap();
    let woke = tokio::time::timeout(Duration::from_secs(1), events.downloading.notified()).await;
    cancel.send(true).unwrap();
    worker.await.unwrap();
    service.shutdown_report(Duration::from_secs(5)).await;
    file_server.abort();
    assert!(
        woke.is_ok(),
        "notification from replacement engine did not wake idle polling after recovery failed"
    );
}
