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
async fn recovery_isolates_a_malformed_task_and_keeps_reconciling_the_session() {
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" => {
                let mut malformed = fixture::status("malformed", "active", 10);
                malformed["completedLength"] = json!("not-a-number");
                json!([malformed, fixture::status("healthy", "active", 75)])
            }
            "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            other => panic!("{other}"),
        })
        .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let malformed = store
        .create(NewTask::direct("http://127.0.0.1/malformed"))
        .unwrap();
    store.bind_gid(malformed.id, "malformed").unwrap();
    let healthy = store
        .create(NewTask::direct("http://127.0.0.1/healthy"))
        .unwrap();
    store.bind_gid(healthy.id, "healthy").unwrap();

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .expect("one malformed task must not make the engine unavailable");

    assert_eq!(report.failed, 1);
    assert_eq!(report.reconciled, 1);
    assert_eq!(store.find(healthy.id).unwrap().progress, 75);
    let malformed = store.find(malformed.id).unwrap();
    assert_eq!(malformed.status, TaskStatus::Paused);
    assert!(malformed
        .error
        .as_deref()
        .is_some_and(|error| error.contains("恢复失败")));
    server.abort();
}

#[tokio::test]
async fn recovery_isolates_a_missing_tasks_persistence_failure() {
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            other => panic!("{other}"),
        })
        .await;
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/missing"))
        .unwrap();
    let mut task = store.bind_gid(task.id, "missing").unwrap();
    task.status = TaskStatus::Downloading;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_recovery BEFORE UPDATE OF status ON tasks WHEN NEW.status='failed' BEGIN SELECT RAISE(ABORT,'fixture recovery persistence failed'); END;").unwrap();

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .expect("one task's failed-state persistence must not disable the engine");

    assert_eq!(report.failed, 1);
    let task = store.find(task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Paused);
    assert!(task
        .error
        .as_deref()
        .is_some_and(|error| error.contains("恢复失败")));
    server.abort();
}

#[tokio::test]
async fn recovery_isolates_a_completed_tasks_persistence_failure_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let rejected_output = dir.path().join("rejected-complete.bin");
    let healthy_output = dir.path().join("healthy-complete.bin");
    std::fs::write(&rejected_output, b"done").unwrap();
    std::fs::write(&healthy_output, b"safe").unwrap();
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
        other => panic!("{other}"),
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());

    // list() returns newest first, so create the healthy task first and prove recovery
    // continues to it after the rejected task.
    let healthy = store
        .create(NewTask::direct("http://127.0.0.1/healthy-complete.bin"))
        .unwrap();
    let mut healthy = store.bind_gid(healthy.id, "missing-healthy").unwrap();
    healthy.status = TaskStatus::Downloading;
    healthy.total_length = Some(4);
    store.apply_transfer_snapshot(healthy.clone()).unwrap();
    store
        .upsert_files(
            healthy.id,
            &[(1, healthy_output.to_string_lossy().into_owned(), 4, 0, true)],
        )
        .unwrap();

    let rejected = store
        .create(NewTask::direct("http://127.0.0.1/rejected-complete.bin"))
        .unwrap();
    let mut rejected = store.bind_gid(rejected.id, "missing-rejected").unwrap();
    rejected.status = TaskStatus::Downloading;
    rejected.total_length = Some(4);
    store.apply_transfer_snapshot(rejected.clone()).unwrap();
    store
        .upsert_files(
            rejected.id,
            &[(
                1,
                rejected_output.to_string_lossy().into_owned(),
                4,
                0,
                true,
            )],
        )
        .unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER reject_completed_recovery BEFORE UPDATE OF status ON tasks \
             WHEN NEW.status='completed' AND NEW.id='{}' \
             BEGIN SELECT RAISE(ABORT,'fixture completed persistence failed'); END;",
            rejected.id
        ))
        .unwrap();

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .expect("one completed-state persistence failure must not disable recovery");

    assert_eq!(report.failed, 1);
    assert_eq!(report.completed, 1);
    let rejected = store.find(rejected.id).unwrap();
    assert_eq!(rejected.status, TaskStatus::Paused);
    assert!(rejected
        .error
        .as_deref()
        .is_some_and(|error| error.contains("恢复失败")));
    assert_eq!(
        store.find(healthy.id).unwrap().status,
        TaskStatus::Completed
    );
    server.abort();
}

#[tokio::test]
async fn recovery_completes_a_full_output_when_the_last_persisted_byte_count_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("completed-before-shutdown.bin");
    std::fs::write(&output, b"done").unwrap();
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct(
            "http://127.0.0.1/completed-before-shutdown.bin",
        ))
        .unwrap();
    let mut task = store
        .bind_gid(task.id, "completed-but-not-persisted")
        .unwrap();
    task.status = TaskStatus::Downloading;
    task.completed_length = 2;
    task.total_length = Some(4);
    store.apply_transfer_snapshot(task.clone()).unwrap();
    store
        .upsert_files(
            task.id,
            &[(1, output.to_string_lossy().into_owned(), 4, 2, true)],
        )
        .unwrap();

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .unwrap();

    assert_eq!(report.completed, 1);
    let recovered = store.find(task.id).unwrap();
    assert_eq!(recovered.status, TaskStatus::Completed);
    assert_eq!(recovered.completed_length, 4);
    assert_eq!(recovered.total_length, Some(4));
    assert_eq!(store.files(task.id).unwrap()[0].completed_length, 4);
    server.abort();
}

#[tokio::test]
async fn recovery_does_not_infer_completion_from_zero_length_or_non_file_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let zero_output = dir.path().join("zero.bin");
    std::fs::write(&zero_output, []).unwrap();
    let directory_output = dir.path().join("not-a-file");
    std::fs::create_dir(&directory_output).unwrap();
    let directory_length = std::fs::metadata(&directory_output).unwrap().len();
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let mut ids = Vec::new();
    for (source, gid, path, length) in [
        ("http://127.0.0.1/zero.bin", "zero-gid", zero_output, 0),
        (
            "http://127.0.0.1/not-a-file",
            "directory-gid",
            directory_output,
            directory_length,
        ),
    ] {
        let task = store.create(NewTask::direct(source)).unwrap();
        let mut task = store.bind_gid(task.id, gid).unwrap();
        task.status = TaskStatus::Downloading;
        task.total_length = Some(length);
        store.apply_transfer_snapshot(task.clone()).unwrap();
        store
            .upsert_files(
                task.id,
                &[(1, path.to_string_lossy().into_owned(), length, 0, true)],
            )
            .unwrap();
        ids.push(task.id);
    }

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .unwrap();

    assert_eq!(report.completed, 0);
    assert_eq!(report.failed, 2);
    for id in ids {
        assert_eq!(store.find(id).unwrap().status, TaskStatus::Failed);
    }
    server.abort();
}

#[tokio::test]
async fn recovery_claims_regenerated_magnet_child_for_the_original_local_task() {
    let (initial_client, initial_server) =
        fixture::server(|request| match request["method"].as_str().unwrap() {
            "aria2.tellStatus" if request["params"][1] == "metadata-parent" => {
                let mut parent = fixture::status("metadata-parent", "complete", 100);
                parent["followedBy"] = json!(["old-child"]);
                parent
            }
            "aria2.tellStatus" => {
                let mut child = fixture::status("old-child", "paused", 0);
                child["files"] = json!([{
                    "index": "1",
                    "path": "payload.bin",
                    "length": "100",
                    "completedLength": "0",
                    "selected": "true"
                }]);
                child
            }
            other => panic!("{other}"),
        })
        .await;
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    let original = store
        .create(NewTask {
            source: "magnet:?xt=urn:btih:fixture".into(),
            name: "fixture".into(),
            kind: "torrent".into(),
        })
        .unwrap();
    store
        .bind_metadata_gid(original.id, "metadata-parent")
        .unwrap();
    let initial = DownloadService::new(Arc::new(initial_client), store.clone());
    let switched = initial.reconcile_task(original.id).await.unwrap();
    assert_eq!(switched.aria_gid.as_deref(), Some("old-child"));
    initial_server.abort();
    drop(initial);
    drop(store);

    let store = Arc::new(TaskStore::open_path(&database).unwrap());

    // aria2 serializes the metadata parent. On the next process start that parent
    // may invent a different child GID, while the old child is absent.
    let (recovery_client, recovery_server) =
        fixture::server(|request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" => json!([]),
            "aria2.tellWaiting" => {
                let mut child = fixture::status("new-child", "paused", 0);
                child["files"] = json!([{
                    "index": "1",
                    "path": "payload.bin",
                    "length": "100",
                    "completedLength": "0",
                    "selected": "true"
                }]);
                json!([child])
            }
            "aria2.tellStopped" => {
                let mut parent = fixture::status("metadata-parent", "complete", 100);
                parent["followedBy"] = json!(["new-child"]);
                json!([parent])
            }
            "aria2.tellStatus" if request["params"][1] == "new-child" => {
                let mut child = fixture::status("new-child", "paused", 0);
                child["files"] = json!([{
                    "index": "1",
                    "path": "payload.bin",
                    "length": "100",
                    "completedLength": "0",
                    "selected": "true"
                }]);
                child
            }
            other => panic!("{other}"),
        })
        .await;
    let recovery = DownloadService::new(Arc::new(recovery_client), store.clone())
        .recover()
        .await
        .unwrap();

    let recovered = store.find(original.id).unwrap();
    assert_eq!(recovery.imported, 0);
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(recovered.id, original.id);
    assert_eq!(recovered.aria_gid.as_deref(), Some("new-child"));
    assert_eq!(recovered.metadata_gid.as_deref(), Some("metadata-parent"));
    assert_eq!(recovered.status, TaskStatus::AwaitingUser);
    recovery_server.abort();
}

#[tokio::test]
async fn migrated_v2_magnet_claims_its_single_regenerated_metadata_parent() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("tasks.sqlite");
    let original_id = uuid::Uuid::new_v4();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE tasks (
                id TEXT PRIMARY KEY NOT NULL, source TEXT NOT NULL, name TEXT NOT NULL,
                kind TEXT NOT NULL, status TEXT NOT NULL, progress INTEGER NOT NULL DEFAULT 0,
                error TEXT, aria_gid TEXT, followed_by_gid TEXT, total_length INTEGER,
                completed_length INTEGER NOT NULL DEFAULT 0,
                download_speed INTEGER NOT NULL DEFAULT 0,
                upload_speed INTEGER NOT NULL DEFAULT 0, connections INTEGER NOT NULL DEFAULT 0,
                eta_seconds INTEGER, aria_error_code TEXT, updated_at INTEGER NOT NULL DEFAULT 0,
                selection_confirmed INTEGER NOT NULL DEFAULT 0
             );
             PRAGMA user_version = 2;",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO tasks
             (id,source,name,kind,status,aria_gid,followed_by_gid)
             VALUES (?1,'magnet:?xt=urn:btih:legacy','legacy','torrent','awaiting_user','old-child','old-child')",
            [original_id.to_string()],
        )
        .unwrap();
    drop(connection);

    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    assert_eq!(store.find(original_id).unwrap().metadata_gid, None);
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" => json!([]),
        "aria2.tellWaiting" => {
            let mut child = fixture::status("new-child", "paused", 0);
            child["files"] = json!([{
                "index": "1", "path": "payload.bin", "length": "100",
                "completedLength": "0", "selected": "true"
            }]);
            json!([child])
        }
        "aria2.tellStopped" => {
            let mut parent = fixture::status("metadata-parent", "complete", 100);
            parent["followedBy"] = json!(["new-child"]);
            json!([parent])
        }
        "aria2.tellStatus" if request["params"][1] == "new-child" => {
            let mut child = fixture::status("new-child", "paused", 0);
            child["files"] = json!([{
                "index": "1", "path": "payload.bin", "length": "100",
                "completedLength": "0", "selected": "true"
            }]);
            child
        }
        other => panic!("{other}"),
    })
    .await;

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .unwrap();

    assert_eq!(report.imported, 0);
    assert_eq!(store.list().unwrap().len(), 1);
    let recovered = store.find(original_id).unwrap();
    assert_eq!(recovered.aria_gid.as_deref(), Some("new-child"));
    assert_eq!(recovered.metadata_gid.as_deref(), Some("metadata-parent"));
    assert_eq!(recovered.status, TaskStatus::AwaitingUser);
    server.abort();
}

#[tokio::test]
async fn recovery_does_not_guess_a_metadata_parent_when_multiple_legacy_magnets_are_orphaned() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" => json!([]),
        "aria2.tellWaiting" => {
            let mut child = fixture::status("new-child", "paused", 0);
            child["files"] = json!([{
                "index": "1", "path": "payload.bin", "length": "100",
                "completedLength": "0", "selected": "true"
            }]);
            json!([child])
        }
        "aria2.tellStopped" => {
            let mut parent = fixture::status("metadata-parent", "complete", 100);
            parent["followedBy"] = json!(["new-child"]);
            json!([parent])
        }
        "aria2.tellStatus" if request["params"][1] == "new-child" => {
            let mut child = fixture::status("new-child", "paused", 0);
            child["files"] = json!([{
                "index": "1", "path": "payload.bin", "length": "100",
                "completedLength": "0", "selected": "true"
            }]);
            child
        }
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    for (source, gid) in [
        ("magnet:?xt=urn:btih:legacy-one", "old-child-one"),
        ("magnet:?xt=urn:btih:legacy-two", "old-child-two"),
    ] {
        let task = store
            .create(NewTask {
                source: source.into(),
                name: source.into(),
                kind: "torrent".into(),
            })
            .unwrap();
        let mut task = store.bind_gid(task.id, gid).unwrap();
        task.followed_by_gid = Some(gid.into());
        task.status = TaskStatus::AwaitingUser;
        store.apply_transfer_snapshot(task).unwrap();
    }

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .unwrap();

    assert_eq!(report.imported, 1);
    assert_eq!(store.list().unwrap().len(), 3);
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .filter(|task| task.metadata_gid.as_deref() == Some("metadata-parent"))
            .count(),
        0
    );
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
async fn recovery_isolates_partial_tell_status_failure_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let completed_file = dir.path().join("later-complete.bin");
    let partial_file = dir.path().join("partial-status-failure.bin");
    std::fs::write(&completed_file, b"done").unwrap();
    std::fs::write(&partial_file, b"part").unwrap();
    std::fs::write(
        dir.path().join("partial-status-failure.bin.aria2"),
        b"fixture",
    )
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let wire_calls = calls.clone();
    let (client, server) = fixture::server(move |request| {
        let method = request["method"].as_str().unwrap().to_string();
        wire_calls.lock().unwrap().push(method.clone());
        match method.as_str() {
            "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            "aria2.addUri" => json!("resumed-with-bad-status"),
            "aria2.tellStatus" => json!(false),
            "aria2.pause" => json!("resumed-with-bad-status"),
            other => panic!("{other}"),
        }
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());

    // The newer partial task is visited first; this completed task proves the loop
    // proceeds after tellStatus fails for the replacement GID.
    let completed = store
        .create(NewTask::direct("http://127.0.0.1/later-complete.bin"))
        .unwrap();
    let mut completed = store.bind_gid(completed.id, "missing-complete").unwrap();
    completed.status = TaskStatus::Downloading;
    completed.total_length = Some(4);
    store.apply_transfer_snapshot(completed.clone()).unwrap();
    store
        .upsert_files(
            completed.id,
            &[(1, completed_file.to_string_lossy().into_owned(), 4, 0, true)],
        )
        .unwrap();

    let partial = store
        .create(NewTask::direct(
            "http://127.0.0.1/partial-status-failure.bin",
        ))
        .unwrap();
    let mut partial = store.bind_gid(partial.id, "missing-partial").unwrap();
    partial.status = TaskStatus::Downloading;
    partial.total_length = Some(100);
    partial.completed_length = 4;
    store.apply_transfer_snapshot(partial.clone()).unwrap();
    store
        .upsert_files(
            partial.id,
            &[(1, partial_file.to_string_lossy().into_owned(), 100, 4, true)],
        )
        .unwrap();

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .expect("one replacement tellStatus failure must not disable recovery");

    assert_eq!(report.failed, 1);
    assert_eq!(report.completed, 1);
    assert_eq!(report.resumed, 0);
    let partial = store.find(partial.id).unwrap();
    assert_eq!(partial.aria_gid.as_deref(), Some("resumed-with-bad-status"));
    assert_eq!(partial.status, TaskStatus::Paused);
    assert!(partial
        .error
        .as_deref()
        .is_some_and(|error| error.contains("恢复失败")));
    assert_eq!(
        store.find(completed.id).unwrap().status,
        TaskStatus::Completed
    );
    let recovery_calls = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|method| {
            matches!(
                method.as_str(),
                "aria2.addUri" | "aria2.tellStatus" | "aria2.pause"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        recovery_calls,
        ["aria2.addUri", "aria2.tellStatus", "aria2.pause"]
    );
    server.abort();
}

#[tokio::test]
async fn recovery_does_not_claim_partial_is_paused_when_status_and_pause_both_fail() {
    let dir = tempfile::tempdir().unwrap();
    let partial_file = dir.path().join("possibly-running.bin");
    std::fs::write(&partial_file, b"part").unwrap();
    std::fs::write(dir.path().join("possibly-running.bin.aria2"), b"fixture").unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let wire_calls = calls.clone();
    let (client, server) = fixture::server(move |request| {
        let method = request["method"].as_str().unwrap().to_string();
        wire_calls.lock().unwrap().push(method.clone());
        match method.as_str() {
            "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            "aria2.addUri" => json!("possibly-running"),
            "aria2.tellStatus" | "aria2.pause" => json!(false),
            other => panic!("{other}"),
        }
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let partial = store
        .create(NewTask::direct("http://127.0.0.1/possibly-running.bin"))
        .unwrap();
    let mut partial = store.bind_gid(partial.id, "missing-partial").unwrap();
    partial.status = TaskStatus::Downloading;
    partial.total_length = Some(100);
    partial.completed_length = 4;
    store.apply_transfer_snapshot(partial.clone()).unwrap();
    store
        .upsert_files(
            partial.id,
            &[(1, partial_file.to_string_lossy().into_owned(), 100, 4, true)],
        )
        .unwrap();

    let report = DownloadService::new(Arc::new(client), store.clone())
        .recover()
        .await
        .expect("a partial task's status and pause failures must remain isolated");

    assert_eq!(report.failed, 1);
    assert_eq!(report.resumed, 0);
    let partial = store.find(partial.id).unwrap();
    assert_eq!(partial.aria_gid.as_deref(), Some("possibly-running"));
    assert_eq!(partial.status, TaskStatus::Downloading);
    assert!(partial
        .error
        .as_deref()
        .is_some_and(|error| error.contains("状态读取与暂停均失败，可能仍在运行")));
    let recovery_calls = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|method| {
            matches!(
                method.as_str(),
                "aria2.addUri" | "aria2.tellStatus" | "aria2.pause"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        recovery_calls,
        ["aria2.addUri", "aria2.tellStatus", "aria2.pause"]
    );
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
    // The new client remains usable and a single persistence failure is reported
    // without turning the entire replacement engine unavailable.
    assert_ne!(replacement.client.endpoint().as_str(), old_endpoint);
    assert_eq!(replacement.recovery.unwrap().failed, 1);
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
