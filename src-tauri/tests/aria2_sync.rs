use alldownloader_lib::{
    aria2::{Aria2GlobalStat, Aria2RpcClient, EventSink, RpcSecret},
    DownloadService, DownloadTask, NewTask, TaskStore,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub async fn server(
    handler: impl Fn(Value) -> Value + Send + Sync + 'static,
) -> (Aria2RpcClient, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    let app = axum::Router::new().route("/jsonrpc", axum::routing::post(move |axum::Json(request): axum::Json<Value>| {
        let handler = handler.clone();
        async move { axum::Json(json!({"jsonrpc":"2.0", "id": request["id"], "result":handler(request.clone())})) }
    }));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        Aria2RpcClient::new(endpoint, RpcSecret::from_bytes([1; 32])).unwrap(),
        task,
    )
}
pub fn status(gid: &str, state: &str, completed: u64) -> Value {
    json!({"gid":gid,"status":state,"totalLength":"100","completedLength":completed.to_string(),"downloadSpeed":"10","uploadSpeed":"0","connections":"1","files":[]})
}
pub fn global() -> Value {
    json!({"downloadSpeed":"10","uploadSpeed":"0","numActive":"1","numWaiting":"0","numStopped":"0","numStoppedTotal":"0"})
}

struct Events {
    store: Arc<TaskStore>,
    values: Mutex<Vec<u8>>,
    globals: Mutex<usize>,
}
impl EventSink for Events {
    fn task_updated(&self, task: &DownloadTask) {
        assert_eq!(
            self.store.find(task.id).unwrap().progress,
            task.progress,
            "event precedes persistence"
        );
        self.values.lock().unwrap().push(task.progress);
    }
    fn global_stat_updated(&self, _: &Aria2GlobalStat) {
        *self.globals.lock().unwrap() += 1;
    }
}

#[tokio::test]
async fn changed_snapshots_persist_before_events_and_unchanged_emit_once() {
    let progress = Arc::new(Mutex::new(25));
    let wire_progress = progress.clone();
    let (client, server) = server(move |request| match request["method"].as_str().unwrap() {
        "system.multicall" => json!([[status("gid-1", "active", *wire_progress.lock().unwrap())]]),
        "aria2.getGlobalStat" => global(),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid-1").unwrap();
    let events = Arc::new(Events {
        store: store.clone(),
        values: Mutex::new(vec![]),
        globals: Mutex::new(0),
    });
    let service = DownloadService::new(Arc::new(client), store).with_events(events.clone());
    service.reconcile_now().await.unwrap();
    service.reconcile_now().await.unwrap();
    *progress.lock().unwrap() = 50;
    service.reconcile_now().await.unwrap();
    assert_eq!(*events.values.lock().unwrap(), [25, 50]);
    assert_eq!(*events.globals.lock().unwrap(), 1);
    server.abort();
}

#[tokio::test]
async fn followed_by_uses_child_snapshot_without_completing_metadata_parent() {
    let (client, server) = server(move |request| match request["method"].as_str().unwrap() {
        "aria2.tellStatus" if request["params"][1] == "meta" => {
            let mut s = status("meta", "complete", 100);
            s["followedBy"] = json!(["child"]);
            s
        }
        "aria2.tellStatus" => status("child", "paused", 0),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "meta").unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    let result = service.reconcile_task(task.id).await.unwrap();
    assert_eq!(result.aria_gid.as_deref(), Some("child"));
    assert_eq!(result.status, alldownloader_lib::TaskStatus::Paused);
    assert_eq!(result.progress, 0);
    assert_eq!(store.list().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn synchronizer_polls_active_at_half_second_and_idle_at_two_seconds() {
    use alldownloader_lib::aria2::sync::TransferSynchronizer;
    use std::time::Duration;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = calls.clone();
    let (client, server) = server(move |request| match request["method"].as_str().unwrap() {
        "system.multicall" => {
            wire.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            json!([[status("gid", "paused", 0)]])
        }
        "aria2.getGlobalStat" => global(),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let mut task = store.find(task.id).unwrap();
    task.status = alldownloader_lib::TaskStatus::Downloading;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    let sync = TransferSynchronizer::new(Arc::new(DownloadService::new(
        Arc::new(client),
        store.clone(),
    )));
    assert_eq!(sync.poll_interval().unwrap(), Duration::from_millis(500));
    task.status = alldownloader_lib::TaskStatus::Paused;
    store.apply_transfer_snapshot(task).unwrap();
    assert_eq!(sync.poll_interval().unwrap(), Duration::from_secs(2));
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(async move {
        sync.run(receiver).await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    cancel.send(true).unwrap();
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .unwrap()
        .unwrap();
    server.abort();
}

#[test]
fn notifications_accept_only_documented_events_with_gid() {
    use alldownloader_lib::aria2::sync::notification_gid;
    assert_eq!(
        notification_gid(
            &json!({"jsonrpc":"2.0","method":"aria2.onDownloadComplete","params":[{"gid":"abc"}]})
        ),
        Some("abc")
    );
    assert_eq!(
        notification_gid(
            &json!({"jsonrpc":"2.0","method":"aria2.onDownloadInvented","params":[{"gid":"abc"}]})
        ),
        None
    );
    assert_eq!(
        notification_gid(&json!({"jsonrpc":"2.0","method":"aria2.onDownloadStop","params":[]})),
        None
    );
    assert_eq!(
        notification_gid(
            &json!({"jsonrpc":"2.0","id":"request","method":"aria2.onDownloadStop","params":[{"gid":"abc"}]})
        ),
        None
    );
}

#[tokio::test]
async fn authenticated_websocket_reconnect_wakes_polling_before_idle_interval() {
    use alldownloader_lib::aria2::sync::TransferSynchronizer;
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = requests.clone();
    let (client, http_server) = server(move |r| match r["method"].as_str().unwrap() {
        "system.multicall" => {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            json!([[status("gid", "paused", 0)]])
        }
        "aria2.getGlobalStat" => global(),
        other => panic!("{other}"),
    })
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_client = Aria2RpcClient::new(
        format!("http://{}/jsonrpc", listener.local_addr().unwrap()),
        RpcSecret::from_bytes([1; 32]),
    )
    .unwrap();
    let (connected, mut connection) = tokio::sync::mpsc::channel(2);
    let socket_server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let auth: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(auth["method"], "aria2.getVersion");
            assert_eq!(
                auth["params"][0],
                RpcSecret::from_bytes([1; 32]).authorization_param()
            );
            socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","id":auth["id"],"result":{"version":"1.37.0","enabledFeatures":[]}}).to_string().into())).await.unwrap();
            connected.send(()).await.unwrap();
            if attempt == 0 {
                socket.close(None).await.unwrap();
                continue;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","method":"aria2.onDownloadPause","params":[{"gid":"gid"}]}).to_string().into())).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let sync = TransferSynchronizer::new(Arc::new(DownloadService::new(Arc::new(client), store)))
        .with_websocket(ws_client);
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(sync.run(receiver));
    tokio::time::timeout(Duration::from_secs(1), connection.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), connection.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while requests.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("notification did not wake idle reconciler");
    cancel.send(true).unwrap();
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .unwrap()
        .unwrap();
    socket_server.abort();
    http_server.abort();
}

#[tokio::test]
async fn complete_transfer_finishes_without_nonexistent_postprocessing() {
    let (client, server) = server(|_| status("gid", "complete", 100)).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    let result = service.reconcile_task(task.id).await.unwrap();
    assert_eq!(result.status, alldownloader_lib::TaskStatus::Completed);
    assert_eq!(
        store.find(task.id).unwrap().status,
        alldownloader_lib::TaskStatus::Completed
    );
    server.abort();
}

#[tokio::test]
async fn invalid_file_persistence_rolls_back_task_snapshot_and_emits_nothing() {
    let (client, server) = server(|_| {
        let mut value = status("gid", "active", 25);
        value["files"] = json!([{"index":"1","path":"file.bin","length":u64::MAX.to_string(),"completedLength":"0","selected":"true"}]); value
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let events = Arc::new(Events {
        store: store.clone(),
        values: Mutex::new(vec![]),
        globals: Mutex::new(0),
    });
    let service = DownloadService::new(Arc::new(client), store.clone()).with_events(events.clone());
    assert!(service.reconcile_task(task.id).await.is_err());
    assert_eq!(store.find(task.id).unwrap().progress, 0);
    assert_eq!(
        store.find(task.id).unwrap().status,
        alldownloader_lib::TaskStatus::Parsing
    );
    assert!(events.values.lock().unwrap().is_empty());
    server.abort();
}
