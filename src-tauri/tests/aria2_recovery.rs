#[path = "aria2_sync.rs"]
mod fixture;

use alldownloader_lib::{DownloadService, NewTask, TaskStatus, TaskStore};
use serde_json::json;
use std::{sync::Arc, time::Duration};

async fn slow_server(
    delay: Duration,
) -> (
    alldownloader_lib::aria2::Aria2RpcClient,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/jsonrpc",
        axum::routing::post(
            move |axum::Json(request): axum::Json<serde_json::Value>| async move {
                tokio::time::sleep(delay).await;
                axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":"OK"}))
            },
        ),
    );
    let worker = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        alldownloader_lib::aria2::Aria2RpcClient::new(
            endpoint,
            alldownloader_lib::aria2::RpcSecret::from_bytes([1; 32]),
        )
        .unwrap(),
        worker,
    )
}

#[tokio::test]
async fn shutdown_one_deadline_covers_save_and_shutdown() {
    let (client, server) = slow_server(Duration::from_millis(90)).await;
    let service = DownloadService::new(
        Arc::new(client),
        Arc::new(TaskStore::open_in_memory().unwrap()),
    );
    let began = std::time::Instant::now();
    let report = service.shutdown_report(Duration::from_millis(120)).await;
    assert!(report.session_saved);
    assert!(!report.shutdown_requested);
    assert!(began.elapsed() < Duration::from_millis(170));
    server.abort();
}

#[tokio::test]
async fn native_recovery_reconciles_only_owned_job() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" => json!([fixture::status("owned", "active", 25)]),
        "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
        other => panic!("{other}"),
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("https://example.test/owned"))
        .unwrap();
    store.bind_gid(task.id, "owned").unwrap();

    let report = DownloadService::new(Arc::new(client), Arc::clone(&store))
        .recover_native(root.path())
        .await
        .unwrap();
    assert_eq!(report.reconciled, 1);
    assert_eq!(report.imported, 0);
    assert_eq!(store.find(task.id).unwrap().progress, 25);
    server.abort();
}

#[tokio::test]
async fn reconcile_repairs_a_recovery_placeholder_from_the_reported_file_name() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellStatus" => {
            let mut status = fixture::status("late-name", "active", 30);
            status["files"] = json!([{
                "index": "1",
                "path": "C:\\Downloads\\稍后取得的名称.pdf",
                "length": "100",
                "completedLength": "30",
                "selected": "true"
            }]);
            status
        }
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: "aria2:late-name".into(),
            name: "恢复的下载任务".into(),
            kind: "file".into(),
            is_private: false,
        })
        .unwrap();
    store.bind_gid(task.id, "late-name").unwrap();

    let updated = DownloadService::new(Arc::new(client), Arc::clone(&store))
        .reconcile_task(task.id)
        .await
        .unwrap();

    assert_eq!(updated.name, "稍后取得的名称.pdf");
    assert_eq!(store.find(task.id).unwrap().name, "稍后取得的名称.pdf");
    server.abort();
}

#[tokio::test]
async fn native_recovery_removes_unowned_engine_job_and_confined_payload() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("orphan.bin");
    std::fs::write(&output, b"partial").unwrap();
    std::fs::write(format!("{}.aria2", output.display()), b"legacy").unwrap();
    let wire_output = output.to_string_lossy().into_owned();
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" => {
                let mut status = fixture::status("orphan", "active", 20);
                status["files"] = json!([{
                    "index": "1", "path": wire_output, "length": "100",
                    "completedLength": "7", "selected": "true"
                }]);
                json!([status])
            }
            "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            "aria2.remove" => json!("orphan"),
            other => panic!("{other}"),
        })
        .await;

    let report = DownloadService::new(
        Arc::new(client),
        Arc::new(TaskStore::open_in_memory().unwrap()),
    )
    .recover_native(root.path())
    .await
    .unwrap();
    assert_eq!(report.removed_engine_tasks, 1);
    assert_eq!(report.removed_files, 2);
    assert!(!output.exists());
    assert!(!std::path::PathBuf::from(format!("{}.aria2", output.display())).exists());
    server.abort();
}

#[tokio::test]
async fn native_recovery_preserves_missing_nonterminal_record_and_payload_for_resume() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("missing.bin");
    std::fs::write(&output, b"partial").unwrap();
    std::fs::write(format!("{}.aria2", output.display()), b"legacy").unwrap();
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            name: "missing".into(),
            kind: "magnet".into(),
            is_private: false,
        })
        .unwrap();
    let mut task = store.bind_gid(task.id, "missing-native-gid").unwrap();
    task.status = TaskStatus::Downloading;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    store
        .upsert_files(
            task.id,
            &[(1, output.to_string_lossy().into_owned(), 100, 7, true)],
        )
        .unwrap();
    store.confirm_selection(task.id).unwrap();

    let report = DownloadService::new(Arc::new(client), Arc::clone(&store))
        .recover_native(root.path())
        .await
        .unwrap();
    assert_eq!(report.removed_tasks, 0);
    let preserved = store.find(task.id).unwrap();
    assert_eq!(preserved.status, TaskStatus::Paused);
    assert_eq!(preserved.aria_gid, None);
    assert_eq!(store.files(task.id).unwrap().len(), 1);
    assert!(store.selection_confirmed(task.id).unwrap());
    assert!(output.exists());
    assert!(std::path::PathBuf::from(format!("{}.aria2", output.display())).exists());
    server.abort();
}

#[tokio::test]
async fn native_recovery_preserves_completed_history_when_job_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("complete.bin");
    std::fs::write(&output, b"done").unwrap();
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("https://example.test/complete.bin"))
        .unwrap();
    let mut task = store.bind_gid(task.id, "completed-native-gid").unwrap();
    task.status = TaskStatus::Completed;
    task.completed_length = 4;
    task.total_length = Some(4);
    store.apply_transfer_snapshot(task.clone()).unwrap();
    store
        .upsert_files(
            task.id,
            &[(1, output.to_string_lossy().into_owned(), 4, 4, true)],
        )
        .unwrap();

    let report = DownloadService::new(Arc::new(client), Arc::clone(&store))
        .recover_native(root.path())
        .await
        .unwrap();
    assert_eq!(report.removed_tasks, 0);
    assert_eq!(store.find(task.id).unwrap().status, TaskStatus::Completed);
    assert!(output.exists());
    server.abort();
}
