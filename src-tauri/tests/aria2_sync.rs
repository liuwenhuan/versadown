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
async fn late_directory_uri_is_admitted_paused_without_starting_connections() {
    use alldownloader_lib::cloud::{
        commands::{create_cloud_directory_group_for_download, plan_cloud_directory_download},
        CloudProvider,
    };
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Pikpak, "root", "Series", |id| {
        Ok(alldownloader_lib::cloud::CloudDirectory {
            id: id.into(),
            items: vec![alldownloader_lib::cloud::CloudFile {
                id: "file".into(),
                name: "video.mp4".into(),
                kind: "file".into(),
                size: Some(100),
                parent_id: id.into(),
                modified_at: None,
            }],
        })
    })
    .unwrap();
    let group = create_cloud_directory_group_for_download(
        &store,
        CloudProvider::Pikpak,
        "root",
        &plan,
        temp.path(),
        false,
    )
    .unwrap();
    let task = store
        .create(NewTask::direct("https://example.test/file"))
        .unwrap();
    store
        .link_cloud_directory_group_item_task(
            group.id,
            store.cloud_directory_group_items(group.id).unwrap()[0].id,
            task.id,
        )
        .unwrap();
    store.set_cloud_directory_paused(group.id, true).unwrap();
    let options = Arc::new(Mutex::new(Value::Null));
    let observed = options.clone();
    let engine_paused = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let wire_paused = engine_paused.clone();
    let (client, server) = server(move |request| match request["method"].as_str().unwrap() {
        "aria2.addUri" => {
            *observed.lock().unwrap() = request["params"][2].clone();
            json!("late-gid")
        }
        "aria2.tellStatus" => status(
            "late-gid",
            if wire_paused.load(std::sync::atomic::Ordering::SeqCst) {
                "paused"
            } else {
                "active"
            },
            0,
        ),
        "aria2.forcePause" => {
            wire_paused.store(true, std::sync::atomic::Ordering::SeqCst);
            json!("late-gid")
        }
        "aria2.unpause" => {
            wire_paused.store(false, std::sync::atomic::Ordering::SeqCst);
            json!("late-gid")
        }
        other => panic!("unexpected method: {other}"),
    })
    .await;
    let service = DownloadService::new(Arc::new(client), store);
    let result = service
        .submit_uri(
            task.id,
            vec!["https://example.test/file".into()],
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.status, alldownloader_lib::TaskStatus::Paused);
    assert_eq!(options.lock().unwrap()["pause"], "true");
    assert!(engine_paused.load(std::sync::atomic::Ordering::SeqCst));
    service
        .store()
        .set_cloud_directory_paused(group.id, false)
        .unwrap();
    // An older pause completing after a newer resume must obey the new intent.
    let resumed = service
        .control_directory_member(task.id, alldownloader_lib::aria2::DownloadAction::Pause)
        .await
        .unwrap();
    assert_eq!(resumed.status, alldownloader_lib::TaskStatus::Downloading);
    // Explicit leaf pause still works while the rest of the directory runs.
    let paused = service
        .control(task.id, alldownloader_lib::aria2::DownloadAction::Pause)
        .await
        .unwrap();
    assert_eq!(paused.status, alldownloader_lib::TaskStatus::Paused);
    server.abort();
}

#[tokio::test]
async fn pause_wins_when_an_inflight_resume_finishes_after_the_fast_interrupt() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let entering_resume = Arc::new(tokio::sync::Notify::new());
    let release_resume = Arc::new(tokio::sync::Notify::new());
    let interrupted = Arc::new(tokio::sync::Notify::new());
    let paused = Arc::new(AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/jsonrpc",
        axum::routing::post({
            let entering_resume = entering_resume.clone();
            let release_resume = release_resume.clone();
            let interrupted = interrupted.clone();
            let paused = paused.clone();
            move |axum::Json(request): axum::Json<Value>| {
                let entering_resume = entering_resume.clone();
                let release_resume = release_resume.clone();
                let interrupted = interrupted.clone();
                let paused = paused.clone();
                async move {
                    let result = match request["method"].as_str().unwrap() {
                        "aria2.unpause" => {
                            entering_resume.notify_one();
                            release_resume.notified().await;
                            paused.store(false, Ordering::SeqCst);
                            json!("gid")
                        }
                        "aria2.forcePause" => {
                            paused.store(true, Ordering::SeqCst);
                            interrupted.notify_one();
                            json!("gid")
                        }
                        "aria2.tellStatus" => status(
                            "gid",
                            if paused.load(Ordering::SeqCst) {
                                "paused"
                            } else {
                                "active"
                            },
                            10,
                        ),
                        other => panic!("unexpected method {other}"),
                    };
                    axum::Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
                }
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("https://example.test/resume-race"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let service = Arc::new(DownloadService::new(
        Arc::new(Aria2RpcClient::new(endpoint, RpcSecret::from_bytes([1; 32])).unwrap()),
        store,
    ));
    let resume = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .control(task.id, alldownloader_lib::aria2::DownloadAction::Resume)
                .await
        }
    });
    entering_resume.notified().await;
    let pause = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .control(task.id, alldownloader_lib::aria2::DownloadAction::Pause)
                .await
        }
    });
    interrupted.notified().await;
    release_resume.notify_one();
    resume.await.unwrap().unwrap();
    let result = pause.await.unwrap().unwrap();
    server.abort();
    assert_eq!(result.status, alldownloader_lib::TaskStatus::Paused);
    assert!(paused.load(Ordering::SeqCst));
}

#[tokio::test]
async fn pause_interrupt_reaches_engine_while_snapshot_rpc_is_still_blocked() {
    let polling = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let paused = Arc::new(tokio::sync::Notify::new());
    let pause_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jsonrpc", listener.local_addr().unwrap());
    let app = axum::Router::new().route("/jsonrpc", axum::routing::post({
        let polling = polling.clone(); let release = release.clone(); let paused = paused.clone(); let pause_requested = pause_requested.clone();
        move |axum::Json(request): axum::Json<Value>| {
            let polling = polling.clone(); let release = release.clone(); let paused = paused.clone(); let pause_requested = pause_requested.clone();
            async move {
                if request["method"] == "aria2.forcePause" && pause_requested.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return axum::Json(json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":1,"message":"cannot be paused now"}}));
                }
                let result = match request["method"].as_str().unwrap() {
                    "system.multicall" => { polling.notify_one(); release.notified().await; json!([[status("gid", "active", 20)]]) },
                    "aria2.forcePause" => { paused.notify_one(); json!("gid") },
                    "aria2.pause" => json!("gid"),
                    "aria2.tellStatus" => status("gid", "paused", 20),
                    "aria2.getGlobalStat" => global(),
                    other => panic!("unexpected method {other}"),
                };
                axum::Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
            }
        }
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = Aria2RpcClient::new(endpoint, RpcSecret::from_bytes([1; 32])).unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("https://example.test/video"))
        .unwrap();
    store.bind_gid(task.id, "gid").unwrap();
    let service = Arc::new(DownloadService::new(Arc::new(client), store));
    let poll = tokio::spawn({
        let service = service.clone();
        async move { service.reconcile_now().await }
    });
    polling.notified().await;
    let control = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .control(task.id, alldownloader_lib::aria2::DownloadAction::Pause)
                .await
        }
    });
    let interrupted =
        tokio::time::timeout(std::time::Duration::from_millis(500), paused.notified()).await;
    release.notify_one();
    poll.await.unwrap().unwrap();
    assert_eq!(
        control.await.unwrap().unwrap().status,
        alldownloader_lib::TaskStatus::Paused
    );
    server.abort();
    assert!(
        interrupted.is_ok(),
        "pause must not queue behind the stalled poll"
    );
}

#[tokio::test]
async fn paused_task_keeps_persisted_progress_when_aria2_restores_without_lengths() {
    let (client, server) = server(move |request| match request["method"].as_str().unwrap() {
        "system.multicall" => {
            json!([[json!({
                "gid": "gid-1",
                "status": "paused",
                "totalLength": "0",
                "completedLength": "0",
                "downloadSpeed": "0",
                "uploadSpeed": "0",
                "connections": "0",
                "files": []
            })]])
        }
        "aria2.getGlobalStat" => global(),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let mut task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid-1").unwrap();
    task = store.find(task.id).unwrap();
    task.status = alldownloader_lib::TaskStatus::Paused;
    task.progress = 60;
    task.total_length = Some(100);
    task.completed_length = 60;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    store
        .upsert_files(task.id, &[(1, "restoring.bin".into(), 100, 60, true)])
        .unwrap();

    DownloadService::new(Arc::new(client), store.clone())
        .reconcile_now()
        .await
        .unwrap();

    let restored = store.find(task.id).unwrap();
    assert_eq!(restored.status, alldownloader_lib::TaskStatus::Paused);
    assert_eq!(restored.progress, 60);
    assert_eq!(restored.total_length, Some(100));
    assert_eq!(restored.completed_length, 60);
    let files = store.files(task.id).unwrap();
    assert_eq!(files[0].length, 100);
    assert_eq!(files[0].completed_length, 60);
    server.abort();
}

#[tokio::test]
async fn paused_task_keeps_persisted_progress_when_restore_has_placeholder_files() {
    let (client, server) = server(move |request| match request["method"].as_str().unwrap() {
        "system.multicall" => {
            json!([[json!({
                "gid": "gid-1",
                "status": "paused",
                "totalLength": "0",
                "completedLength": "0",
                "downloadSpeed": "0",
                "uploadSpeed": "0",
                "connections": "0",
                "files": [{
                    "index": "1",
                    "path": "restoring.bin",
                    "length": "0",
                    "completedLength": "0",
                    "selected": "true"
                }]
            })]])
        }
        "aria2.getGlobalStat" => global(),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let mut task = store
        .create(NewTask::direct("http://127.0.0.1/file"))
        .unwrap();
    store.bind_gid(task.id, "gid-1").unwrap();
    task = store.find(task.id).unwrap();
    task.status = alldownloader_lib::TaskStatus::Paused;
    task.progress = 60;
    task.total_length = Some(100);
    task.completed_length = 60;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    store
        .upsert_files(task.id, &[(1, "restoring.bin".into(), 100, 60, true)])
        .unwrap();

    DownloadService::new(Arc::new(client), store.clone())
        .reconcile_now()
        .await
        .unwrap();

    let restored = store.find(task.id).unwrap();
    assert_eq!(restored.status, alldownloader_lib::TaskStatus::Paused);
    assert_eq!(restored.progress, 60);
    assert_eq!(restored.total_length, Some(100));
    assert_eq!(restored.completed_length, 60);
    let files = store.files(task.id).unwrap();
    assert_eq!(files[0].length, 100);
    assert_eq!(files[0].completed_length, 60);
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
    store.apply_transfer_snapshot(task.clone()).unwrap();
    assert_eq!(sync.poll_interval().unwrap(), Duration::from_secs(2));
    let mut sharing = store.find(task.id).unwrap();
    sharing.status = alldownloader_lib::TaskStatus::Completed;
    sharing.p2p_sharing = true;
    store.apply_transfer_snapshot(sharing).unwrap();
    assert_eq!(sync.poll_interval().unwrap(), Duration::from_millis(500));
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

#[tokio::test]
async fn cancel_acknowledgement_persists_event_and_missing_gid_does_not_break_later_polling() {
    let cancelled_queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = cancelled_queries.clone();
    let (client, server) = server(move |r| match r["method"].as_str().unwrap() {
        "aria2.remove" => json!("removed-gid"),
        "system.multicall" => serde_json::Value::Array(
            r["params"][0]
                .as_array()
                .unwrap()
                .iter()
                .map(|call| {
                    if call["params"][1] == "removed-gid" {
                        wire.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        json!({"code":1,"message":"GID not found"})
                    } else {
                        json!([status("active-gid", "active", 50)])
                    }
                })
                .collect(),
        ),
        "aria2.getGlobalStat" => global(),
        other => panic!("unexpected request after cancel: {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let cancelled = store
        .create(NewTask::direct("http://127.0.0.1/cancelled"))
        .unwrap();
    store.bind_gid(cancelled.id, "removed-gid").unwrap();
    let active = store
        .create(NewTask::direct("http://127.0.0.1/active"))
        .unwrap();
    store.bind_gid(active.id, "active-gid").unwrap();
    let events = Arc::new(Events {
        store: store.clone(),
        values: Mutex::new(vec![]),
        globals: Mutex::new(0),
    });
    let service = DownloadService::new(Arc::new(client), store.clone()).with_events(events.clone());
    assert_eq!(
        service
            .control(cancelled.id, alldownloader_lib::DownloadAction::Cancel)
            .await
            .unwrap()
            .status,
        alldownloader_lib::TaskStatus::Cancelled
    );
    assert_eq!(
        events.values.lock().unwrap().len(),
        1,
        "cancel acknowledgement must publish persisted task"
    );
    service
        .reconcile_now()
        .await
        .expect("missing cancelled GID must not fail synchronization");
    assert_eq!(
        cancelled_queries.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(store.find(active.id).unwrap().progress, 50);
    assert_eq!(
        store.find(cancelled.id).unwrap().status,
        alldownloader_lib::TaskStatus::Cancelled
    );
    server.abort();
}
