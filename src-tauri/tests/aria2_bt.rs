#[path = "aria2_sync.rs"]
mod fixture;
use alldownloader_lib::{DownloadService, NewTask, TaskStatus, TaskStore};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn torrent_bytes_are_added_paused_then_real_files_are_selected_before_start() {
    let calls = Arc::new(Mutex::new(vec![]));
    let wire = calls.clone();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wire_running = running.clone();
    let (client, server) = fixture::server(move |request| {
        wire.lock().unwrap().push(request.clone());
        match request["method"].as_str().unwrap() {
            "aria2.addTorrent" => {
                assert_eq!(request["params"][1], "dG9ycmVudC1ieXRlcw==");
                assert_eq!(request["params"][3]["pause"], "true");
                json!("torrent-gid")
            }
            "aria2.tellStatus" => {
                let state = if wire_running.load(std::sync::atomic::Ordering::SeqCst) {
                    "active"
                } else {
                    "paused"
                };
                let mut status = fixture::status("torrent-gid", state, 0);
                status["files"] = json!([
                    {"index":"1","path":"game/data.bin","length":"70","completedLength":"0","selected":"true"},
                    {"index":"2","path":"game/readme.txt","length":"30","completedLength":"0","selected":"true"}
                ]);
                status
            }
            "aria2.changeOption" => {
                assert_eq!(request["params"][2]["select-file"], "2");
                json!("OK")
            }
            "aria2.unpause" => {
                wire_running.store(true, std::sync::atomic::Ordering::SeqCst);
                json!("torrent-gid")
            }
            other => panic!("unexpected method {other}"),
        }
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("C:\\Downloads\\game.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let pending = service
        .submit_torrent(
            task.id,
            "dG9ycmVudC1ieXRlcw==".into(),
            vec![],
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(pending.status, TaskStatus::AwaitingUser);
    assert_eq!(pending.aria_gid.as_deref(), Some("torrent-gid"));
    let files = store.files(task.id).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].path, "game/readme.txt");

    let started = service.select_files(task.id, vec![2]).await.unwrap();
    assert_eq!(started.status, TaskStatus::Downloading);
    let calls = calls.lock().unwrap();
    let select = calls
        .iter()
        .position(|call| call["method"] == "aria2.changeOption")
        .unwrap();
    let start = calls
        .iter()
        .position(|call| call["method"] == "aria2.unpause")
        .unwrap();
    assert!(select < start);
    server.abort();
}

#[tokio::test]
async fn torrent_without_real_files_never_enters_empty_confirmation() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("empty-gid"),
        "aria2.tellStatus" => fixture::status("empty-gid", "paused", 0),
        "aria2.remove" => json!("empty-gid"),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("C:\\Downloads\\empty.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let error = service
        .submit_torrent(task.id, "ZW1wdHk=".into(), vec![], Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("未包含可下载文件"), "{error}");
    assert_eq!(store.find(task.id).unwrap().status, TaskStatus::Parsing);
    assert!(store.find(task.id).unwrap().aria_gid.is_none());
    server.abort();
}

#[tokio::test]
async fn malformed_torrent_status_is_removed_without_binding_the_task() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("malformed-gid"),
        "aria2.tellStatus" => json!(false),
        "aria2.remove" => json!("malformed-gid"),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("C:\\Downloads\\malformed.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let error = service
        .submit_torrent(task.id, "bWFsZm9ybWVk".into(), vec![], Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Malformed aria2 RPC response"), "{error}");
    let stored = store.find(task.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Parsing);
    assert!(stored.aria_gid.is_none());
    server.abort();
}

#[tokio::test]
async fn failed_cleanup_binds_the_orphan_gid_and_recovery_does_not_import_a_duplicate() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("tracked-orphan"),
        "aria2.tellStatus" => {
            let mut status = fixture::status("tracked-orphan", "paused", 0);
            status["files"] = json!([{
                "index":"not-a-number",
                "path":"broken.bin",
                "length":"100",
                "completedLength":"0",
                "selected":"true"
            }]);
            status
        }
        "aria2.remove" => json!(false),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("C:\\Downloads\\broken.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let error = service
        .submit_torrent(task.id, "YnJva2Vu".into(), vec![], Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("tracked-orphan"), "{error}");
    let tracked = store.find(task.id).unwrap();
    assert_eq!(tracked.status, TaskStatus::Failed);
    assert_eq!(tracked.aria_gid.as_deref(), Some("tracked-orphan"));
    assert!(
        tracked
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("无法清理"),
        "persisted failure should explain why aria2 still owns the GID"
    );
    server.abort();

    let (recovery_client, recovery_server) =
        fixture::server(|request| match request["method"].as_str().unwrap() {
            "aria2.tellActive" => {
                let mut status = fixture::status("tracked-orphan", "error", 0);
                status["errorCode"] = json!("3");
                status["errorMessage"] = json!("bad torrent");
                json!([status])
            }
            "aria2.tellWaiting" | "aria2.tellStopped" => json!([]),
            other => panic!("unexpected method {other}"),
        })
        .await;
    let recovery = DownloadService::new(Arc::new(recovery_client), store.clone())
        .recover()
        .await
        .unwrap();
    assert_eq!(recovery.imported, 0);
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.find(task.id).unwrap().source, task.source);
    recovery_server.abort();
}

#[tokio::test]
async fn terminal_torrent_states_are_persisted_and_never_offered_for_confirmation() {
    for (aria_status, expected_status) in [
        ("complete", TaskStatus::Completed),
        ("error", TaskStatus::Failed),
        ("removed", TaskStatus::Cancelled),
    ] {
        let gid = format!("terminal-{aria_status}");
        let wire_gid = gid.clone();
        let (client, server) = fixture::server(move |request| match request["method"].as_str().unwrap() {
            "aria2.addTorrent" => json!(wire_gid),
            "aria2.tellStatus" => {
                let mut status = fixture::status(&wire_gid, aria_status, if aria_status == "complete" { 100 } else { 0 });
                status["files"] = json!([{"index":"1","path":"terminal.bin","length":"100","completedLength":status["completedLength"],"selected":"true"}]);
                if aria_status == "error" {
                    status["errorCode"] = json!("3");
                    status["errorMessage"] = json!("bad torrent");
                }
                status
            }
            other => panic!("terminal state must not be cleaned up or confirmed: {other}"),
        }).await;
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask::direct(format!(
                "C:\\Downloads\\{aria_status}.torrent"
            )))
            .unwrap();
        let service = DownloadService::new(Arc::new(client), store.clone());

        let error = service
            .submit_torrent(task.id, "dGVybWluYWw=".into(), vec![], Default::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(aria_status), "{error}");
        let stored = store.find(task.id).unwrap();
        assert_eq!(stored.status, expected_status);
        assert_eq!(stored.aria_gid.as_deref(), Some(gid.as_str()));
        assert_ne!(stored.status, TaskStatus::AwaitingUser);
        assert_eq!(store.files(task.id).unwrap().len(), 1);
        server.abort();
    }
}

#[tokio::test]
async fn waiting_torrent_with_files_is_a_valid_confirmation_candidate() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("waiting-gid"),
        "aria2.tellStatus" => {
            let mut status = fixture::status("waiting-gid", "waiting", 0);
            status["files"] = json!([{"index":"1","path":"queued.bin","length":"100","completedLength":"0","selected":"true"}]);
            status
        }
        other => panic!("unexpected method {other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("C:\\Downloads\\waiting.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let pending = service
        .submit_torrent(task.id, "d2FpdGluZw==".into(), vec![], Default::default())
        .await
        .unwrap();
    assert_eq!(pending.status, TaskStatus::AwaitingUser);
    assert_eq!(pending.aria_gid.as_deref(), Some("waiting-gid"));
    assert_eq!(store.files(task.id).unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn active_torrent_is_rejected_instead_of_becoming_a_confirmation_candidate() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("active-gid"),
        "aria2.tellStatus" => {
            let mut status = fixture::status("active-gid", "active", 1);
            status["files"] = json!([{"index":"1","path":"active.bin","length":"100","completedLength":"1","selected":"true"}]);
            status
        }
        "aria2.remove" => json!("active-gid"),
        other => panic!("unexpected method {other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("C:\\Downloads\\active.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let error = service
        .submit_torrent(task.id, "YWN0aXZl".into(), vec![], Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("active"), "{error}");
    let stored = store.find(task.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Parsing);
    assert!(stored.aria_gid.is_none());
    server.abort();
}

#[tokio::test]
async fn metadata_child_is_persisted_and_selection_precedes_unpause() {
    let calls = Arc::new(Mutex::new(vec![]));
    let wire = calls.clone();
    let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (client, server) = fixture::server(move |request| {
        let method = request["method"].as_str().unwrap();
        wire.lock().unwrap().push(request.clone());
        match method {
            "aria2.addUri" => { assert_eq!(request["params"][2]["pause-metadata"], "true"); json!("meta") },
            "aria2.tellStatus" if request["params"][1] == "meta" => {
                let mut s = fixture::status("meta", "complete", 100); s["followedBy"] = json!(["child"]); s
            },
            "aria2.tellStatus" => {
                let running = active.load(std::sync::atomic::Ordering::SeqCst);
                let mut s = fixture::status("child", if running {"active"} else {"paused"}, 0);
                s["files"] = json!([
                    {"index":"1","path":"a.bin","length":"40","completedLength":"0","selected":"true"},
                    {"index":"2","path":"b.bin","length":"30","completedLength":"0","selected":if running {"false"} else {"true"}},
                    {"index":"3","path":"c.bin","length":"30","completedLength":"0","selected":"true"}
                ]); s
            },
            "aria2.changeOption" => { assert_eq!(request["params"][1], "child"); assert_eq!(request["params"][2]["select-file"], "1,3"); json!("OK") },
            "aria2.unpause" => { active.store(true, std::sync::atomic::Ordering::SeqCst); json!("child") },
            other => panic!("{other}"),
        }
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("magnet:?xt=urn:btih:fixture"))
        .unwrap();
    store.transition(task.id, TaskStatus::AwaitingUser).unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    let metadata = service.prepare_magnet(task.id, task.source).await.unwrap();
    assert_eq!(metadata.status, TaskStatus::AwaitingUser);
    assert_eq!(metadata.aria_gid.as_deref(), Some("child"));
    assert_eq!(store.files(task.id).unwrap().len(), 3);
    let before = calls.lock().unwrap().len();
    assert!(service.select_files(task.id, vec![0, 9]).await.is_err());
    assert_eq!(
        calls.lock().unwrap().len(),
        before,
        "invalid indices must not reach aria2"
    );
    let task = service.select_files(task.id, vec![3, 1, 3]).await.unwrap();
    assert_eq!(task.status, TaskStatus::Downloading);
    assert!(!store.files(task.id).unwrap()[1].selected);
    let calls = calls.lock().unwrap();
    let change = calls
        .iter()
        .position(|c| c["method"] == "aria2.changeOption")
        .unwrap();
    let unpause = calls
        .iter()
        .position(|c| c["method"] == "aria2.unpause")
        .unwrap();
    assert!(change < unpause);
    server.abort();
}

#[tokio::test]
async fn rejected_selection_does_not_unpause_or_change_persisted_files() {
    let (client, server) = fixture::server(|request| {
        assert_eq!(request["method"], "aria2.changeOption");
        json!(false) // invalid response: the control operation did not succeed
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("magnet:?xt=urn:btih:fixture"))
        .unwrap();
    store.bind_gid(task.id, "bt").unwrap();
    store.transition(task.id, TaskStatus::AwaitingUser).unwrap();
    store
        .upsert_files(
            task.id,
            &[
                (1, "a.bin".into(), 40, 0, true),
                (2, "b.bin".into(), 60, 0, true),
            ],
        )
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    assert!(service.set_file_selected(task.id, 2, false).await.is_err());
    assert_eq!(
        store.find(task.id).unwrap().status,
        TaskStatus::AwaitingUser
    );
    assert!(store.files(task.id).unwrap()[1].selected);
    server.abort();
}

#[tokio::test]
async fn metadata_wait_hides_metadata_file_and_returns_real_child_files_before_deadline() {
    let requests = std::sync::atomic::AtomicUsize::new(0);
    let (client, server) = fixture::server(move |r| match r["method"].as_str().unwrap() {
        "aria2.addUri" => json!("meta"),
        "aria2.tellStatus" if r["params"][1] == "meta" => {
            let mut s = fixture::status("meta","active",0);
            s["files"] = json!([{"index":"1","path":"metadata.torrent","length":"42","completedLength":"0","selected":"true"}]);
            if requests.fetch_add(1,std::sync::atomic::Ordering::SeqCst)>0 { s["followedBy"] = json!(["child"]); }
            s
        },
        "aria2.tellStatus" => {
            let mut s = fixture::status("child","paused",0);
            s["files"] = json!([{"index":"1","path":"actual.bin","length":"100","completedLength":"0","selected":"true"}]); s
        },
        other => panic!("{other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("magnet:?xt=urn:btih:fixture"))
        .unwrap();
    store.transition(task.id, TaskStatus::AwaitingUser).unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    service.prepare_magnet(task.id, task.source).await.unwrap();
    assert!(
        store.files(task.id).unwrap().is_empty(),
        "metadata payload must not be offered as a downloadable file"
    );
    let task = service
        .wait_for_metadata(task.id, std::time::Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(task.aria_gid.as_deref(), Some("child"));
    assert_eq!(store.files(task.id).unwrap()[0].path, "actual.bin");
    server.abort();
}

#[tokio::test]
async fn metadata_timeout_leaves_pending_task_available_for_later_sync() {
    let (client, server) = fixture::server(|r| match r["method"].as_str().unwrap() {
        "aria2.addUri" => json!("meta"),
        "aria2.tellStatus" => fixture::status("meta", "active", 0),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("magnet:?xt=urn:btih:fixture"))
        .unwrap();
    store.transition(task.id, TaskStatus::AwaitingUser).unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());
    service.prepare_magnet(task.id, task.source).await.unwrap();
    assert!(service
        .wait_for_metadata(task.id, std::time::Duration::from_millis(20))
        .await
        .is_err());
    assert_eq!(
        store.find(task.id).unwrap().status,
        TaskStatus::AwaitingUser
    );
    assert_eq!(
        store.find(task.id).unwrap().aria_gid.as_deref(),
        Some("meta")
    );
    server.abort();
}

#[tokio::test]
async fn successful_confirmation_survives_status_failure_and_service_reopen() {
    let queries = std::sync::atomic::AtomicUsize::new(0);
    let (client, server) = fixture::server(move |r| match r["method"].as_str().unwrap() {
        "aria2.changeOption" | "aria2.unpause" => json!("OK"),
        "aria2.tellStatus" if queries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
            json!(false)
        }
        "aria2.tellStatus" => fixture::status("child", "active", 30),
        other => panic!("{other}"),
    })
    .await;
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("tasks.sqlite");
    let store = Arc::new(TaskStore::open_path(&database).unwrap());
    let task = store
        .create(NewTask::direct("magnet:?xt=urn:btih:fixture"))
        .unwrap();
    store.bind_gid(task.id, "child").unwrap();
    store.transition(task.id, TaskStatus::AwaitingUser).unwrap();
    store
        .upsert_files(task.id, &[(1, "actual.bin".into(), 100, 0, true)])
        .unwrap();
    let service = DownloadService::new(Arc::new(client.clone()), store.clone());
    assert!(service.select_files(task.id, vec![1]).await.is_err());
    assert_eq!(
        store.find(task.id).unwrap().status,
        TaskStatus::AwaitingUser,
        "failed status read retains last observed state"
    );
    drop(service);
    drop(store);
    let reopened = Arc::new(TaskStore::open_path(&database).unwrap());
    let service = DownloadService::new(Arc::new(client), reopened);
    assert_eq!(
        service.reconcile_task(task.id).await.unwrap().status,
        TaskStatus::Downloading,
        "successful confirmation must not be forgotten after reopening SQLite"
    );
    server.abort();
}

#[tokio::test]
async fn prepare_magnet_persists_pending_state_before_first_status_read_and_retry() {
    let queries = std::sync::atomic::AtomicUsize::new(0);
    let adds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire_adds = adds.clone();
    let (client, server) = fixture::server(move |r| match r["method"].as_str().unwrap() {
        "aria2.addUri" => {
            wire_adds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            json!("meta")
        }
        "aria2.tellStatus" if queries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
            json!(false)
        }
        "aria2.tellStatus" => fixture::status("meta", "active", 0),
        other => panic!("{other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask::direct("magnet:?xt=urn:btih:fixture"))
        .unwrap();
    assert_eq!(task.status, TaskStatus::Parsing);
    let service = DownloadService::new(Arc::new(client), store.clone());
    assert!(service
        .prepare_magnet(task.id, task.source.clone())
        .await
        .is_err());
    assert_eq!(
        store.find(task.id).unwrap().status,
        TaskStatus::AwaitingUser
    );
    assert_eq!(
        store.find(task.id).unwrap().aria_gid.as_deref(),
        Some("meta")
    );
    assert_eq!(
        service
            .prepare_magnet(task.id, task.source)
            .await
            .unwrap()
            .status,
        TaskStatus::AwaitingUser
    );
    assert_eq!(adds.load(std::sync::atomic::Ordering::SeqCst), 1);
    server.abort();
}

#[test]
fn version_one_database_keeps_tasks_and_defaults_confirmation_false() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("tasks.sqlite");
    let store = TaskStore::open_path(&database).unwrap();
    let task = store
        .create(NewTask::direct("http://127.0.0.1/kept"))
        .unwrap();
    store
        .upsert_files(task.id, &[(1, "kept.bin".into(), 100, 0, true)])
        .unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("ALTER TABLE tasks DROP COLUMN selection_confirmed; PRAGMA user_version=1;")
        .unwrap();
    drop(connection);
    let store = TaskStore::open_path(&database).unwrap();
    assert_eq!(store.find(task.id).unwrap().name, "kept");
    assert_eq!(store.files(task.id).unwrap().len(), 1);
    assert!(!store.selection_confirmed(task.id).unwrap());
    store.confirm_selection(task.id).unwrap();
    drop(store);
    assert!(TaskStore::open_path(&database)
        .unwrap()
        .selection_confirmed(task.id)
        .unwrap());
}
