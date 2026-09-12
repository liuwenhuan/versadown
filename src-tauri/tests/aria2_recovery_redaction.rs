use alldownloader_lib::{
    aria2::{Aria2GlobalStat, Aria2RpcClient, EventSink, RpcSecret},
    DownloadService, DownloadTask, NewTask, TaskStatus, TaskStore,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const SENSITIVE_DETAIL: &str =
    "signed=https://download.example.invalid/file?authorization=TOP-SECRET";
const SAFE_RECOVERY_ERROR: &str = "网盘分享下载恢复失败，请重试或重新解析分享链接";

#[derive(Default)]
struct RecordedEvents(Mutex<Vec<DownloadTask>>);

impl EventSink for RecordedEvents {
    fn task_updated(&self, task: &DownloadTask) {
        self.0.lock().unwrap().push(task.clone());
    }

    fn global_stat_updated(&self, _: &Aria2GlobalStat) {}
}

async fn server(
    state: &'static str,
    pause_succeeds: bool,
) -> (Aria2RpcClient, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/jsonrpc",
        axum::routing::post(move |axum::Json(request): axum::Json<Value>| async move {
            let method = request["method"].as_str().unwrap();
            let status = json!({
                "gid": "recovery-gid",
                "status": state,
                "totalLength": "100",
                "completedLength": "25",
                "downloadSpeed": "10",
                "uploadSpeed": "0",
                "connections": "1",
                "errorCode": "22",
                "files": []
            });
            let result = match method {
                "aria2.tellActive" if state == "active" => json!([status]),
                "aria2.tellWaiting" if state == "paused" => json!([status]),
                "aria2.tellStopped" if state == "error" => json!([status]),
                "aria2.tellActive" | "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
                "aria2.pause" if pause_succeeds => json!("recovery-gid"),
                "aria2.pause" => json!(false),
                other => panic!("unexpected aria2 method {other}"),
            };
            axum::Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
        }),
    );
    let worker = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        Aria2RpcClient::new(endpoint, RpcSecret::from_bytes([1; 32])).unwrap(),
        worker,
    )
}

async fn recover_with_sensitive_persistence_failure(
    source: &str,
    state: &'static str,
    pause_succeeds: bool,
) -> (DownloadTask, Vec<DownloadTask>) {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    let task = store
        .create(NewTask {
            source: source.into(),
            name: "recovery fixture".into(),
            kind: "file".into(),
            is_private: false,
        })
        .unwrap();
    let mut task = store.bind_gid(task.id, "recovery-gid").unwrap();
    task.status = TaskStatus::Downloading;
    task.aria_error_code = Some("22".into());
    store.apply_transfer_snapshot(task.clone()).unwrap();
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER reject_recovery_snapshot BEFORE UPDATE OF progress ON tasks \
             WHEN NEW.progress=25 BEGIN SELECT RAISE(ABORT,'{SENSITIVE_DETAIL}'); END;"
        ))
        .unwrap();

    let events = Arc::new(RecordedEvents::default());
    let (client, worker) = server(state, pause_succeeds).await;
    let report = DownloadService::new(Arc::new(client), store.clone())
        .with_events(events.clone())
        .recover()
        .await
        .unwrap();
    assert_eq!(report.failed, 1);
    worker.abort();
    let recorded = events.0.lock().unwrap().clone();
    (store.find(task.id).unwrap(), recorded)
}

#[tokio::test]
async fn cloud_share_recovery_never_persists_or_publishes_raw_failure_details() {
    for (state, pause_succeeds, expected_status) in [
        ("paused", true, TaskStatus::Paused),
        ("active", false, TaskStatus::Downloading),
        ("error", true, TaskStatus::Failed),
    ] {
        let (task, events) = recover_with_sensitive_persistence_failure(
            "cloud-share://baidu/plan/entry",
            state,
            pause_succeeds,
        )
        .await;

        assert_eq!(task.status, expected_status);
        assert_eq!(task.error.as_deref(), Some(SAFE_RECOVERY_ERROR));
        assert_eq!(task.aria_error_code.as_deref(), Some("22"));
        assert!(!task.error.as_deref().unwrap().contains(SENSITIVE_DETAIL));
        assert_eq!(
            events.last().and_then(|task| task.error.as_deref()),
            Some(SAFE_RECOVERY_ERROR)
        );
        assert!(events.iter().all(|task| !task
            .error
            .as_deref()
            .unwrap_or_default()
            .contains(SENSITIVE_DETAIL)));
    }
}

#[tokio::test]
async fn ordinary_http_recovery_keeps_the_existing_diagnostic_detail() {
    let (task, events) = recover_with_sensitive_persistence_failure(
        "https://download.example.invalid/file",
        "paused",
        true,
    )
    .await;

    assert!(task.error.as_deref().unwrap().contains(SENSITIVE_DETAIL));
    assert!(events
        .last()
        .and_then(|task| task.error.as_deref())
        .is_some_and(|error| error.contains(SENSITIVE_DETAIL)));
}
