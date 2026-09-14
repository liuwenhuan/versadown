#[path = "aria2_sync.rs"]
mod fixture;
use alldownloader_lib::{DownloadService, NewTask, TaskStatus, TaskStore, UriSubmissionOptions};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const VALID_MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";

fn magnet_task() -> NewTask {
    NewTask {
        source: VALID_MAGNET.into(),
        name: "fixture".into(),
        kind: "magnet".into(),
        is_private: false,
    }
}

fn torrent_task(source: impl Into<String>) -> NewTask {
    NewTask {
        source: source.into(),
        name: "fixture.torrent".into(),
        kind: "torrent".into(),
        is_private: false,
    }
}

#[tokio::test]
async fn reconcile_tracks_unique_peers_for_an_active_torrent() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "system.multicall" => json!([[fixture::status("torrent-gid", "active", 0)]]),
        "aria2.getPeers" => json!([
            {"ip":"203.0.113.10","port":"51413"},
            {"ip":"203.0.113.10","port":"51413"},
            {"ip":"2001:db8::7","port":"51414"}
        ]),
        "aria2.getGlobalStat" => json!({
            "downloadSpeed":"10", "uploadSpeed":"0", "numActive":"1",
            "numWaiting":"0", "numStopped":"0", "numStoppedTotal":"0"
        }),
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: VALID_MAGNET.into(),
            name: "test".into(),
            kind: "magnet".into(),
            is_private: false,
        })
        .unwrap();
    store.bind_gid(task.id, "torrent-gid").unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    service.reconcile_now().await.unwrap();

    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 2);
    server.abort();
}

#[tokio::test]
async fn reconcile_retains_observed_peers_across_paused_and_queued_same_gid() {
    let stage = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire_stage = stage.clone();
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "system.multicall" => {
                let status = match wire_stage.load(std::sync::atomic::Ordering::SeqCst) {
                    0 => "active",
                    1 => "paused",
                    2 => "waiting",
                    3 => "active",
                    stage => panic!("unexpected reconciliation stage {stage}"),
                };
                json!([[fixture::status("torrent-gid", status, 0)]])
            }
            "aria2.getPeers" => match wire_stage.load(std::sync::atomic::Ordering::SeqCst) {
                0 => json!([
                    {"ip":"203.0.113.10","port":"51413"},
                    {"ip":"203.0.113.11","port":"51413"}
                ]),
                3 => json!([{"ip":"2001:db8::7","port":"51414"}]),
                stage => panic!("getPeers must not run while stage {stage} is not active"),
            },
            "aria2.getGlobalStat" => fixture::global(),
            other => panic!("unexpected method {other}"),
        })
        .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: VALID_MAGNET.into(),
            name: "test".into(),
            kind: "magnet".into(),
            is_private: false,
        })
        .unwrap();
    store.bind_gid(task.id, "torrent-gid").unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    service.reconcile_now().await.unwrap();
    stage.store(1, std::sync::atomic::Ordering::SeqCst);
    service.reconcile_now().await.unwrap();
    stage.store(2, std::sync::atomic::Ordering::SeqCst);
    service.reconcile_now().await.unwrap();
    stage.store(3, std::sync::atomic::Ordering::SeqCst);
    service.reconcile_now().await.unwrap();

    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 3);
    server.abort();
}

#[tokio::test]
async fn retry_does_not_merge_peers_from_a_cancelled_torrent_session() {
    let retried = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let retry_state = retried.clone();
    let (client, server) =
        fixture::server(move |request| match request["method"].as_str().unwrap() {
            "system.multicall" => json!([[fixture::status(
                if retry_state.load(std::sync::atomic::Ordering::SeqCst) {
                    "new-gid"
                } else {
                    "old-gid"
                },
                "active",
                0,
            )]]),
            "aria2.getPeers" => {
                if retry_state.load(std::sync::atomic::Ordering::SeqCst) {
                    json!([{"ip":"2001:db8::7","port":"51414"}])
                } else {
                    json!([
                        {"ip":"203.0.113.10","port":"51413"},
                        {"ip":"203.0.113.11","port":"51413"}
                    ])
                }
            }
            "aria2.remove" => json!("old-gid"),
            "aria2.addUri" => {
                retry_state.store(true, std::sync::atomic::Ordering::SeqCst);
                json!("new-gid")
            }
            "aria2.tellStatus" => fixture::status("new-gid", "active", 0),
            "aria2.getGlobalStat" => json!({
                "downloadSpeed":"10", "uploadSpeed":"0", "numActive":"1",
                "numWaiting":"0", "numStopped":"0", "numStoppedTotal":"0"
            }),
            other => panic!("unexpected method {other}"),
        })
        .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: VALID_MAGNET.into(),
            name: "test".into(),
            kind: "magnet".into(),
            is_private: false,
        })
        .unwrap();
    store.bind_gid(task.id, "old-gid").unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    service.reconcile_now().await.unwrap();
    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 2);
    service
        .control(task.id, alldownloader_lib::DownloadAction::Cancel)
        .await
        .unwrap();
    service
        .resubmit_uri_with_options(
            task.id,
            vec![VALID_MAGNET.into()],
            UriSubmissionOptions::default(),
        )
        .await
        .unwrap();

    service.reconcile_now().await.unwrap();

    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 1);
    server.abort();
}

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
                assert_eq!(request["params"][3]["check-integrity"], "true");
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
        .create(torrent_task("C:\\Downloads\\game.torrent"))
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
async fn confirmed_magnet_overwrite_restarts_selection_and_clears_observed_peers() {
    let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wire_running = running.clone();
    let selection_applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wire_selection_applied = selection_applied.clone();
    let resubmitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wire_resubmitted = resubmitted.clone();
    let (client, server) = fixture::server(move |request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => {
            assert_eq!(request["params"][3]["pause"], "true");
            assert_eq!(request["params"][3]["check-integrity"], "true");
            assert_eq!(request["params"][3]["allow-overwrite"], "true");
            wire_resubmitted.store(true, std::sync::atomic::Ordering::SeqCst);
            json!("replacement-gid")
        }
        "system.multicall" => json!([[
            fixture::status(
                if wire_resubmitted.load(std::sync::atomic::Ordering::SeqCst) {
                    "replacement-gid"
                } else {
                    "previous-gid"
                },
                "active",
                0,
            )
        ]]),
        "aria2.getPeers" => {
            if wire_resubmitted.load(std::sync::atomic::Ordering::SeqCst) {
                json!([{"ip":"2001:db8::7","port":"51414"}])
            } else {
                json!([
                    {"ip":"203.0.113.10","port":"51413"},
                    {"ip":"203.0.113.11","port":"51413"}
                ])
            }
        }
        "aria2.getGlobalStat" => fixture::global(),
        "aria2.tellStatus" => {
            let status = if wire_running.load(std::sync::atomic::Ordering::SeqCst) {
                "active"
            } else {
                "paused"
            };
            let mut transfer = fixture::status("replacement-gid", status, 0);
            transfer["files"] = json!([
                {"index":"1","path":"bundle/keep.bin","length":"10","completedLength":"0","selected":if wire_selection_applied.load(std::sync::atomic::Ordering::SeqCst) {"false"} else {"true"}},
                {"index":"2","path":"bundle/download.bin","length":"20","completedLength":"0","selected":"true"}
            ]);
            transfer
        }
        "aria2.changeOption" => {
            assert_eq!(request["params"][1], "replacement-gid");
            assert_eq!(request["params"][2]["select-file"], "2");
            assert_eq!(request["params"][2]["allow-overwrite"], "true");
            wire_selection_applied.store(true, std::sync::atomic::Ordering::SeqCst);
            json!("OK")
        }
        "aria2.unpause" => {
            wire_running.store(true, std::sync::atomic::Ordering::SeqCst);
            json!("replacement-gid")
        }
        other => panic!("unexpected method {other}"),
    })
    .await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: VALID_MAGNET.into(),
            name: "fixture".into(),
            kind: "magnet".into(),
            is_private: false,
        })
        .unwrap();
    store.bind_gid(task.id, "previous-gid").unwrap();
    store
        .upsert_files(
            task.id,
            &[
                (1, "bundle/keep.bin".into(), 10, 4, false),
                (2, "bundle/download.bin".into(), 20, 6, true),
            ],
        )
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    service.reconcile_now().await.unwrap();
    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 2);
    store.fail(task.id, "interrupted").unwrap();

    let restarted = service
        .resubmit_torrent_with_options(
            task.id,
            "dG9ycmVudC1ieXRlcw==".into(),
            vec![],
            vec![2],
            UriSubmissionOptions {
                allow_overwrite: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(restarted.status, TaskStatus::Downloading);
    assert_eq!(restarted.aria_gid.as_deref(), Some("replacement-gid"));
    assert!(!store.files(task.id).unwrap()[0].selected);
    service.reconcile_now().await.unwrap();
    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 1);
    server.abort();
}

#[tokio::test]
async fn selected_torrent_uses_the_chosen_destination_before_unpausing() {
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wire_started = started.clone();
    let (client, server) = fixture::server(move |request| match request["method"].as_str().unwrap() {
        "aria2.changeOption" => {
            assert_eq!(request["params"][2]["select-file"], "1");
            assert_eq!(request["params"][2]["dir"], "E:\\Media");
            json!("OK")
        }
        "aria2.unpause" => {
            wire_started.store(true, std::sync::atomic::Ordering::SeqCst);
            json!("torrent-gid")
        }
        "aria2.tellStatus" => {
            let mut status = fixture::status("torrent-gid", if wire_started.load(std::sync::atomic::Ordering::SeqCst) { "active" } else { "paused" }, 0);
            status["files"] = json!([{"index":"1","path":"D:/合集/episode.mp4","length":"168996012","completedLength":"0","selected":"true"}]);
            status
        }
        other => panic!("unexpected method {other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store.create(magnet_task()).unwrap();
    store.bind_gid(task.id, "torrent-gid").unwrap();
    store.transition(task.id, TaskStatus::AwaitingUser).unwrap();
    store
        .upsert_files(
            task.id,
            &[(1, "D:/合集/episode.mp4".into(), 168996012, 0, true)],
        )
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let started_task = service
        .select_files_with_options(
            task.id,
            vec![1],
            UriSubmissionOptions {
                directory: Some(PathBuf::from("E:\\Media")),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(started_task.status, TaskStatus::Downloading);
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
        .create(torrent_task("C:\\Downloads\\empty.torrent"))
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
        .create(torrent_task("C:\\Downloads\\malformed.torrent"))
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
        .create(torrent_task("C:\\Downloads\\broken.torrent"))
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
    let recovery_root = tempfile::tempdir().unwrap();
    let recovery = DownloadService::new(Arc::new(recovery_client), store.clone())
        .recover_native(recovery_root.path())
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
            .create(torrent_task(format!(
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
    let calls = Arc::new(Mutex::new(Vec::new()));
    let wire_calls = calls.clone();
    let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wire_paused = paused.clone();
    let (client, server) = fixture::server(move |request| {
        wire_calls
            .lock()
            .unwrap()
            .push(request["method"].as_str().unwrap().to_owned());
        match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("waiting-gid"),
        "aria2.tellStatus" => {
            let state = if wire_paused.load(std::sync::atomic::Ordering::SeqCst) {
                "paused"
            } else {
                "waiting"
            };
            let mut status = fixture::status("waiting-gid", state, 0);
            status["files"] = json!([{"index":"1","path":"queued.bin","length":"100","completedLength":"0","selected":"true"}]);
            status
        }
        "aria2.pause" => {
            wire_paused.store(true, std::sync::atomic::Ordering::SeqCst);
            json!("waiting-gid")
        }
        other => panic!("unexpected method {other}"),
    }}).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(torrent_task("C:\\Downloads\\waiting.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let pending = service
        .submit_torrent(task.id, "d2FpdGluZw==".into(), vec![], Default::default())
        .await
        .unwrap();
    assert_eq!(pending.status, TaskStatus::AwaitingUser);
    assert_eq!(pending.aria_gid.as_deref(), Some("waiting-gid"));
    assert_eq!(store.files(task.id).unwrap().len(), 1);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "aria2.addTorrent",
            "aria2.tellStatus",
            "aria2.pause",
            "aria2.tellStatus"
        ],
        "waiting must be paused and verified before confirmation is exposed"
    );
    server.abort();
}

#[tokio::test]
async fn failed_waiting_pause_and_cleanup_persists_the_gid_as_failed() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("unpaused-orphan"),
        "aria2.tellStatus" => {
            let mut status = fixture::status("unpaused-orphan", "waiting", 0);
            status["files"] = json!([{"index":"1","path":"queued.bin","length":"100","completedLength":"0","selected":"true"}]);
            status
        }
        "aria2.pause" | "aria2.remove" => json!(false),
        other => panic!("unexpected method {other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(torrent_task("C:\\Downloads\\unpaused.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let error = service
        .submit_torrent(task.id, "dW5wYXVzZWQ=".into(), vec![], Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("unpaused-orphan"), "{error}");
    let stored = store.find(task.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Failed);
    assert_eq!(stored.aria_gid.as_deref(), Some("unpaused-orphan"));
    assert_ne!(stored.status, TaskStatus::AwaitingUser);
    server.abort();
}

#[tokio::test]
async fn waiting_torrent_must_recheck_as_paused_even_if_it_reaches_a_terminal_state() {
    let status_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire_reads = status_reads.clone();
    let (client, server) = fixture::server(move |request| match request["method"].as_str().unwrap() {
        "aria2.addTorrent" => json!("raced-gid"),
        "aria2.tellStatus" => {
            let completed = wire_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
            let mut status = fixture::status(
                "raced-gid",
                if completed { "complete" } else { "waiting" },
                if completed { 100 } else { 0 },
            );
            status["files"] = json!([{"index":"1","path":"raced.bin","length":"100","completedLength":if completed {"100"} else {"0"},"selected":"true"}]);
            status
        }
        "aria2.pause" | "aria2.remove" => json!("raced-gid"),
        other => panic!("unexpected method {other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(torrent_task("C:\\Downloads\\raced.torrent"))
        .unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    let error = service
        .submit_torrent(task.id, "cmFjZWQ=".into(), vec![], Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("complete"), "{error}");
    let stored = store.find(task.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Parsing);
    assert!(stored.aria_gid.is_none());
    assert_eq!(status_reads.load(std::sync::atomic::Ordering::SeqCst), 2);
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
        .create(torrent_task("C:\\Downloads\\active.torrent"))
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
            "aria2.addUri" => {
                assert_eq!(request["params"][2]["pause-metadata"], "true");
                assert_eq!(request["params"][2]["check-integrity"], "true");
                json!("meta")
            }
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
    let task = store.create(magnet_task()).unwrap();
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
    let task = store.create(magnet_task()).unwrap();
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
    let task = store.create(magnet_task()).unwrap();
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
    let task = store.create(magnet_task()).unwrap();
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
    let task = store.create(magnet_task()).unwrap();
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
    let task = store.create(magnet_task()).unwrap();
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
