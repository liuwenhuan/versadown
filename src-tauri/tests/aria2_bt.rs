#[path = "aria2_sync.rs"]
mod fixture;
use alldownloader_lib::{DownloadService, NewTask, TaskStatus, TaskStore};
use serde_json::json;
use std::sync::{Arc, Mutex};

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
