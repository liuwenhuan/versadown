use alldownloader_lib::cloud::{
    commands::{
        cancel_cloud_directory_group_with, cloud_directory_member_retry_options,
        plan_cloud_directory_download, plan_cloud_share_directory_download,
        retry_cloud_directory_group_item_with, retry_cloud_directory_group_with,
        submit_cloud_directory_plan, submit_cloud_directory_plan_with_group_event,
        CloudDownloadSourceGate, DirectoryLeafSubmission,
    },
    share::{CloudShareDownloadEntry, CloudShareDownloadSelection},
    CloudDirectory, CloudFile, CloudProvider, QuarkError,
};
use alldownloader_lib::{
    list_queue_tasks_for_scope, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

fn directory(id: &str, items: Vec<CloudFile>) -> CloudDirectory {
    CloudDirectory {
        id: id.into(),
        items,
    }
}

#[tokio::test]
async fn paused_directory_stops_pending_submission_and_can_resume_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("paused.db");
    let store = TaskStore::open_path(&path).unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Pikpak, "root", "原始目录", |id| {
        Ok(directory(
            id,
            vec![file("one", id, "one.mp4"), file("two", id, "two.mp4")],
        ))
    })
    .unwrap();
    let result = submit_cloud_directory_plan_with_group_event(
        &store,
        CloudProvider::Pikpak,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(temp.path().to_owned()),
            ..Default::default()
        },
        false,
        |root| {
            store
                .set_cloud_directory_paused(uuid::Uuid::parse_str(&root.id).unwrap(), true)
                .unwrap();
            Ok(())
        },
        |_, _, _, _| async { panic!("paused directory must not start another file") },
    )
    .await
    .unwrap();
    let id = result.task.payload.task.id;
    assert_eq!(result.task.queue_task.unwrap().status, TaskStatus::Paused);
    drop(store);
    let store = TaskStore::open_path(&path).unwrap();
    assert_eq!(
        list_queue_tasks_for_scope(&store, Some("active")).unwrap()[0].status,
        TaskStatus::Paused
    );
    store.set_cloud_directory_paused(id, false).unwrap();
    let result = retry_cloud_directory_group_with(
        &store,
        id,
        UriSubmissionOptions {
            directory: Some(temp.path().to_owned()),
            ..Default::default()
        },
        |_, file_id, _, _| {
            let task = store
                .create(NewTask::direct(format!("https://example.test/{file_id}")))
                .unwrap();
            std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
        },
    )
    .await
    .unwrap();
    assert_ne!(result.queue_task.unwrap().status, TaskStatus::Paused);
    assert!(store
        .cloud_directory_group_items(id)
        .unwrap()
        .iter()
        .all(|item| item.task_id.is_some()));
}

#[test]
fn interrupted_preparation_after_restart_is_resumable_without_replaying_a_live_submission() {
    use alldownloader_lib::cloud::commands::recover_interrupted_directory_preparation;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("interrupted.db");
    let store = TaskStore::open_path(&path).unwrap();
    let task = store
        .create(NewTask::direct("https://example.test/unfinished"))
        .unwrap();
    let group_id = cancellation_group(&store, temp.path());
    store
        .link_cloud_directory_group_item_task(
            group_id,
            store.cloud_directory_group_items(group_id).unwrap()[0].id,
            task.id,
        )
        .unwrap();
    drop(store);
    let store = TaskStore::open_path(&path).unwrap();
    let recovered = recover_interrupted_directory_preparation(&store, task.id).unwrap();
    assert_eq!(recovered.status, TaskStatus::Paused);
    assert_eq!(recovered.id, task.id);
    store.bind_gid(task.id, "already-accepted").unwrap();
    let accepted = store.find(task.id).unwrap();
    let unchanged = recover_interrupted_directory_preparation(&store, task.id).unwrap();
    assert_eq!(unchanged.aria_gid.as_deref(), Some("already-accepted"));
    assert_eq!(unchanged.status, accepted.status);
}

#[tokio::test]
async fn authorization_recovery_reopens_directory_leaves_and_retries_each_once() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("tasks.db");
    let store = TaskStore::open_path(&path).unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| {
        Ok(directory(
            id,
            vec![
                file("not_authorized", id, "一.txt"),
                file("credentials_expired", id, "二.txt"),
                file("web_auth_required", id, "三.txt"),
                file("network_error", id, "四.txt"),
            ],
        ))
    })
    .unwrap();
    let outcome = submit_cloud_directory_plan(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(root.path().to_owned()),
            ..Default::default()
        },
        |_, id, _| {
            std::future::ready(Err(match id.as_str() {
                "not_authorized" => QuarkError::NotAuthorized,
                "credentials_expired" => QuarkError::AuthorizationExpired,
                "web_auth_required" => QuarkError::WebAuthorizationRequired,
                _ => QuarkError::Transport("offline".into()),
            }))
        },
    )
    .await
    .unwrap();
    let group = outcome.task.payload.task.id;
    assert_eq!(
        alldownloader_lib::cloud::commands::reauthorization_provider(&store, group).unwrap(),
        CloudProvider::Quark
    );
    store
        .bind_cloud_download_account(group, CloudProvider::Quark, "alice")
        .unwrap();
    drop(store);
    let store = TaskStore::open_path(&path).unwrap();
    assert!(store
        .reauthorized_cloud_downloads(CloudProvider::Uc, "alice")
        .unwrap()
        .1
        .is_empty());
    assert!(store
        .reauthorized_cloud_downloads(CloudProvider::Quark, "bob")
        .unwrap()
        .1
        .is_empty());
    let (_, leaves) = store
        .reauthorized_cloud_downloads(CloudProvider::Quark, "alice")
        .unwrap();
    assert_eq!(leaves.len(), 3);
    let mut submissions = 0;
    for (group_id, item_id) in leaves {
        retry_cloud_directory_group_item_with(
            &store,
            group_id,
            item_id,
            Default::default(),
            |_, id, _, _| {
                submissions += 1;
                let task = store
                    .create(NewTask::direct(format!("cloud://quark/{id}")))
                    .unwrap();
                std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(submissions, 3);
    assert!(store
        .reauthorized_cloud_downloads(CloudProvider::Quark, "alice")
        .unwrap()
        .1
        .is_empty());
    let remaining = store.cloud_directory_group_items(group).unwrap();
    assert_eq!(remaining.iter().filter(|i| i.task_id.is_none()).count(), 1);
    assert_eq!(
        remaining
            .iter()
            .find(|i| i.task_id.is_none())
            .unwrap()
            .failure_code
            .as_deref(),
        Some("network_error")
    );
    assert!(alldownloader_lib::cloud::commands::reauthorization_provider(&store, group).is_err());
    let linked = remaining.iter().find_map(|item| item.task_id).unwrap();
    store
        .bind_cloud_download_account(linked, CloudProvider::Quark, "alice")
        .unwrap();
    store
        .fail_with_code(linked, Some("credentials_expired"), "expired")
        .unwrap();
    assert_eq!(
        store
            .reauthorized_cloud_downloads(CloudProvider::Quark, "alice")
            .unwrap()
            .0
            .len(),
        1
    );
    // Recycling is only valid once every linked download is terminal.
    for task_id in remaining.iter().filter_map(|item| item.task_id) {
        if task_id != linked {
            store.fail(task_id, "finished with error").unwrap();
        }
    }
    store.recycle_cloud_directory_group(group).unwrap();
    assert!(store
        .reauthorized_cloud_downloads(CloudProvider::Quark, "alice")
        .unwrap()
        .0
        .is_empty());
}

fn file(id: &str, parent_id: &str, name: &str) -> CloudFile {
    CloudFile {
        id: id.into(),
        parent_id: parent_id.into(),
        name: name.into(),
        kind: "file".into(),
        size: None,
        modified_at: None,
    }
}

fn folder(id: &str, parent_id: &str, name: &str) -> CloudFile {
    CloudFile {
        id: id.into(),
        parent_id: parent_id.into(),
        name: name.into(),
        kind: "folder".into(),
        size: None,
        modified_at: None,
    }
}

#[tokio::test]
async fn review_recycling_during_resolution_prevents_atomic_leaf_claim_and_submission() {
    let store = TaskStore::open_in_memory().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| {
        Ok(directory(id, vec![file("file", id, "一.txt")]))
    })
    .unwrap();
    let group = alldownloader_lib::cloud::commands::create_cloud_directory_group_for_download(
        &store,
        CloudProvider::Quark,
        "root",
        &plan,
        destination.path(),
        false,
    )
    .unwrap();
    let item = store
        .cloud_directory_group_items(group.id)
        .unwrap()
        .remove(0);
    store
        .fail_cloud_directory_group_item(group.id, item.id, "web_auth_required", "expired")
        .unwrap();
    let (released, wait) = tokio::sync::oneshot::channel::<()>();
    let submitted = std::cell::Cell::new(0);
    let resolution = async {
        wait.await.unwrap();
        let claimed = store
            .claim_cloud_directory_leaf_task(
                group.id,
                item.id,
                CloudProvider::Quark,
                "alice",
                NewTask::direct("cloud://quark/file"),
            )
            .unwrap();
        if claimed.is_some() {
            submitted.set(submitted.get() + 1);
        }
    };
    tokio::pin!(resolution);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(resolution.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    store.recycle_cloud_directory_group(group.id).unwrap();
    released.send(()).unwrap();
    resolution.await;
    assert!(
        store.list().unwrap().is_empty(),
        "recycled parent must prevent task creation"
    );
    assert!(store.cloud_directory_group_items(group.id).unwrap()[0]
        .task_id
        .is_none());
    assert_eq!(submitted.get(), 0);
}

#[tokio::test]
async fn review_late_duplicate_retry_never_erases_the_successful_leaf_link() {
    let store = TaskStore::open_in_memory().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| {
        Ok(directory(id, vec![file("file", id, "一.txt")]))
    })
    .unwrap();
    let group = alldownloader_lib::cloud::commands::create_cloud_directory_group_for_download(
        &store,
        CloudProvider::Quark,
        "root",
        &plan,
        destination.path(),
        false,
    )
    .unwrap();
    let item = store
        .cloud_directory_group_items(group.id)
        .unwrap()
        .remove(0);
    store
        .fail_cloud_directory_group_item(group.id, item.id, "web_auth_required", "expired")
        .unwrap();
    let gate = CloudDownloadSourceGate::default();
    let (released, wait) = tokio::sync::oneshot::channel::<()>();
    let mut wait = Some(wait);
    let first = retry_cloud_directory_group_item_with(
        &store,
        group.id,
        item.id,
        Default::default(),
        |_, _, _, _| {
            let wait = wait.take().unwrap();
            let gate = &gate;
            let store = &store;
            async move {
                let _source = gate.acquire("cloud://quark/file").await.unwrap();
                wait.await.unwrap();
                let task = store.create(NewTask::direct("cloud://quark/file")).unwrap();
                store
                    .link_cloud_directory_group_item_task(group.id, item.id, task.id)
                    .unwrap();
                Ok(DirectoryLeafSubmission::Queued(task.id))
            }
        },
    );
    tokio::pin!(first);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(first.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let second = retry_cloud_directory_group_item_with(
        &store,
        group.id,
        item.id,
        Default::default(),
        |_, _, _, _| async {
            let _source = gate.acquire("cloud://quark/file").await.unwrap();
            Ok(DirectoryLeafSubmission::Duplicate)
        },
    );
    tokio::pin!(second);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(second.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    released.send(()).unwrap();
    let queued = first.await.unwrap().payload.task.id;
    let _ = second.await;
    store
        .fail_cloud_directory_group_item(group.id, item.id, "duplicate", "late failure")
        .unwrap();
    assert_eq!(
        store.cloud_directory_group_items(group.id).unwrap()[0].task_id,
        Some(queued)
    );
    assert_eq!(store.list().unwrap().len(), 1);
    assert!(
        store.recycle_cloud_directory_group(group.id).is_err(),
        "a claimed Parsing task must prevent recycling before submission completes"
    );
}

fn cancellation_group(store: &TaskStore, output: &std::path::Path) -> uuid::Uuid {
    use alldownloader_lib::cloud::commands::{
        create_cloud_directory_group_for_download, CloudDirectoryDownloadPlan, PlannedCloudFile,
    };
    let plan = CloudDirectoryDownloadPlan {
        files: vec![
            PlannedCloudFile {
                file_id: "first".into(),
                relative_directory: vec!["Series".into(), "Season".into()],
                output_name: "first.mp4".into(),
                size: Some(100),
            },
            PlannedCloudFile {
                file_id: "second".into(),
                relative_directory: vec!["Series".into(), "Season".into()],
                output_name: "second.mp4".into(),
                size: Some(200),
            },
            PlannedCloudFile {
                file_id: "third".into(),
                relative_directory: vec!["Series".into()],
                output_name: "third.mp4".into(),
                size: Some(300),
            },
        ],
    };
    create_cloud_directory_group_for_download(
        store,
        CloudProvider::Quark,
        "cancel-root",
        &plan,
        output,
        false,
    )
    .unwrap()
    .id
}

#[tokio::test]
async fn cancellation_of_unsubmitted_directory_is_durable_and_keeps_files() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("tasks.sqlite");
    let group_id;
    let existing_file = temp.path().join("existing.mp4");
    std::fs::write(&existing_file, b"keep partial download").unwrap();
    {
        let store = TaskStore::open_path(&database).unwrap();
        let group = store
            .cloud_directory_group(cancellation_group(&store, temp.path()))
            .unwrap()
            .unwrap();
        group_id = group.id;
        let result = cancel_cloud_directory_group_with(&store, group.id, |_| async {
            panic!("unsubmitted files must not contact the download engine");
        })
        .await
        .unwrap();
        assert_eq!(result.payload.task.status, TaskStatus::Cancelled);
        let root = result.queue_task.unwrap();
        assert_eq!(root.status, TaskStatus::Cancelled);
        assert_eq!(root.children[0].status, TaskStatus::Cancelled);
        assert_eq!(root.children[0].children[0].status, TaskStatus::Cancelled);
        assert_eq!(root.progress, 0);
        assert!(
            store.list().unwrap().is_empty(),
            "do not fabricate local tasks"
        );
    }
    let reopened = TaskStore::open_path(&database).unwrap();
    assert_eq!(
        reopened
            .cloud_directory_group(group_id)
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );
    assert!(list_queue_tasks_for_scope(&reopened, Some("active"))
        .unwrap()
        .is_empty());
    assert_eq!(
        list_queue_tasks_for_scope(&reopened, Some("terminal")).unwrap()[0].status,
        TaskStatus::Cancelled
    );
    assert_eq!(
        std::fs::read(existing_file).unwrap(),
        b"keep partial download"
    );
    cancel_cloud_directory_group_with(&reopened, group_id, |_| async {
        panic!("idempotent cancellation")
    })
    .await
    .unwrap();
    let retry = retry_cloud_directory_group_item_with(
        &reopened,
        group_id,
        reopened.cloud_directory_group_items(group_id).unwrap()[0].id,
        UriSubmissionOptions::default(),
        |_, _, _, _| async { panic!("cancelled directory must not auto-retry") },
    )
    .await;
    assert!(retry.is_err());
}

#[tokio::test]
async fn cancellation_of_mixed_directory_stops_active_members_only() {
    let store = TaskStore::open_in_memory().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let group = store
        .cloud_directory_group(cancellation_group(&store, temp.path()))
        .unwrap()
        .unwrap();
    let items = store.cloud_directory_group_items(group.id).unwrap();
    let completed = store
        .create(NewTask::direct("https://example.test/completed"))
        .unwrap();
    let completed = store
        .apply_transfer_snapshot(alldownloader_lib::DownloadTask {
            status: TaskStatus::Completed,
            progress: 100,
            completed_length: 100,
            total_length: Some(100),
            ..completed
        })
        .unwrap();
    let active = store
        .create(NewTask::direct("https://example.test/active"))
        .unwrap();
    store
        .link_cloud_directory_group_item_task(group.id, items[0].id, completed.id)
        .unwrap();
    store
        .link_cloud_directory_group_item_task(group.id, items[1].id, active.id)
        .unwrap();
    let result = cancel_cloud_directory_group_with(&store, group.id, |id| {
        assert_eq!(id, active.id);
        std::future::ready(
            store
                .apply_transfer_snapshot(alldownloader_lib::DownloadTask {
                    status: TaskStatus::Cancelled,
                    ..store.find(id).unwrap()
                })
                .map(|_| ())
                .map_err(|e| e.to_string()),
        )
    })
    .await
    .unwrap();
    assert_eq!(result.queue_task.unwrap().status, TaskStatus::Cancelled);
    assert_eq!(
        store.find(completed.id).unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(store.find(active.id).unwrap().status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn cancellation_failure_does_not_skip_siblings_or_hide_active_members() {
    let store = TaskStore::open_in_memory().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let group = store
        .cloud_directory_group(cancellation_group(&store, temp.path()))
        .unwrap()
        .unwrap();
    let items = store.cloud_directory_group_items(group.id).unwrap();
    let first = store
        .create(NewTask::direct("https://example.test/first"))
        .unwrap();
    let second = store
        .create(NewTask::direct("https://example.test/second"))
        .unwrap();
    store
        .link_cloud_directory_group_item_task(group.id, items[0].id, first.id)
        .unwrap();
    store
        .link_cloud_directory_group_item_task(group.id, items[1].id, second.id)
        .unwrap();
    let result = cancel_cloud_directory_group_with(&store, group.id, |id| {
        std::future::ready(if id == first.id {
            Err("engine unavailable".to_string())
        } else {
            store
                .apply_transfer_snapshot(alldownloader_lib::DownloadTask {
                    status: TaskStatus::Cancelled,
                    ..store.find(id).unwrap()
                })
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    })
    .await;
    assert!(result.is_err());
    assert_eq!(store.find(second.id).unwrap().status, TaskStatus::Cancelled);
    let active = list_queue_tasks_for_scope(&store, Some("active")).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].status, TaskStatus::Parsing);
    assert_eq!(active[0].children[1].status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn cancellation_during_submission_stops_the_inflight_leaf_and_skips_remaining_files() {
    let store = TaskStore::open_in_memory().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "Series", |id| {
        Ok(directory(
            id,
            vec![
                file("first", id, "first.mp4"),
                file("second", id, "second.mp4"),
            ],
        ))
    })
    .unwrap();
    let group_id = std::cell::Cell::new(None);
    let submitted = std::cell::Cell::new(0);
    let release = tokio::sync::Notify::new();
    let submission = submit_cloud_directory_plan_with_group_event(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(temp.path().to_path_buf()),
            ..Default::default()
        },
        false,
        |root| {
            group_id.set(Some(uuid::Uuid::parse_str(&root.id).unwrap()));
            Ok(())
        },
        |_, _, _, _| {
            submitted.set(submitted.get() + 1);
            assert_eq!(
                submitted.get(),
                1,
                "no sibling may start after cancellation"
            );
            let store = &store;
            let release = &release;
            async move {
                release.notified().await;
                let task = store
                    .create(NewTask::direct("https://example.test/inflight"))
                    .unwrap();
                Ok(DirectoryLeafSubmission::Queued(task.id))
            }
        },
    );
    tokio::pin!(submission);
    assert!(futures_util::poll!(&mut submission).is_pending());
    let cancellation = cancel_cloud_directory_group_with(&store, group_id.get().unwrap(), |id| {
        std::future::ready(
            store
                .apply_transfer_snapshot(alldownloader_lib::DownloadTask {
                    status: TaskStatus::Cancelled,
                    ..store.find(id).unwrap()
                })
                .map(|_| ())
                .map_err(|e| e.to_string()),
        )
    });
    tokio::pin!(cancellation);
    assert!(
        futures_util::poll!(&mut cancellation).is_pending(),
        "wait for the in-flight submission before acknowledging cancellation"
    );
    release.notify_one();
    let (_, cancelled) = tokio::join!(submission, cancellation);
    assert_eq!(
        cancelled.unwrap().queue_task.unwrap().status,
        TaskStatus::Cancelled
    );
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.list().unwrap()[0].status, TaskStatus::Cancelled);
    assert!(list_queue_tasks_for_scope(&store, Some("active"))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancellation_blocks_linked_failed_member_refresh_and_waits_for_existing_refresh() {
    use alldownloader_lib::cloud::commands::lock_directory_member_submission;
    let store = TaskStore::open_in_memory().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let group_id = cancellation_group(&store, temp.path());
    let failed = store
        .create(NewTask::direct("cloud://quark/refresh"))
        .unwrap();
    let failed = store.fail(failed.id, "authorization expired").unwrap();
    store
        .link_cloud_directory_group_item_task(
            group_id,
            store.cloud_directory_group_items(group_id).unwrap()[0].id,
            failed.id,
        )
        .unwrap();
    // An already-started refresh holds this guard through provider resolution
    // and engine submission, exactly as both production refresh entry points do.
    let refresh = lock_directory_member_submission(&store, failed.id)
        .await
        .unwrap();
    let cancellation = cancel_cloud_directory_group_with(&store, group_id, |id| {
        std::future::ready(
            store
                .apply_transfer_snapshot(alldownloader_lib::DownloadTask {
                    status: TaskStatus::Cancelled,
                    ..store.find(id).unwrap()
                })
                .map(|_| ())
                .map_err(|error| error.to_string()),
        )
    });
    tokio::pin!(cancellation);
    assert!(futures_util::poll!(&mut cancellation).is_pending());
    store
        .rebind_for_resubmission(failed.id, "refreshed-gid")
        .unwrap();
    drop(refresh);
    assert_eq!(
        cancellation.await.unwrap().queue_task.unwrap().status,
        TaskStatus::Cancelled
    );
    assert_eq!(store.find(failed.id).unwrap().status, TaskStatus::Cancelled);
    assert!(lock_directory_member_submission(&store, failed.id)
        .await
        .is_err());

    let unrelated = store
        .create(NewTask::direct("https://example.test/unrelated"))
        .unwrap();
    assert!(lock_directory_member_submission(&store, unrelated.id)
        .await
        .unwrap()
        .is_none());
}

#[test]
fn converts_a_share_directory_selection_into_one_safe_directory_plan() {
    let share_hash = "a".repeat(64);
    let first_hash = "b".repeat(64);
    let second_hash = "c".repeat(64);
    let selection = CloudShareDownloadSelection {
        provider: CloudProvider::Quark,
        plan_source: format!("cloud-share://quark/{share_hash}/{first_hash}"),
        directory_name: Some("夸克分享目录".into()),
        entries: vec![
            CloudShareDownloadEntry {
                selected_id: "first".into(),
                source: format!("cloud-share://quark/{share_hash}/{first_hash}"),
                name: "cover.jpg".into(),
                relative_directory: vec![],
                size: Some(11),
            },
            CloudShareDownloadEntry {
                selected_id: "second".into(),
                source: format!("cloud-share://quark/{share_hash}/{second_hash}"),
                name: "nested.jpg".into(),
                relative_directory: vec!["mobile".into()],
                size: Some(22),
            },
        ],
    };

    let plan = plan_cloud_share_directory_download(&selection).unwrap();

    assert_eq!(plan.files.len(), 2);
    assert_eq!(plan.files[0].relative_directory, vec!["夸克分享目录"]);
    assert_eq!(plan.files[0].output_name, "cover.jpg");
    assert_eq!(plan.files[0].size, Some(11));
    assert_eq!(
        plan.files[1].relative_directory,
        vec!["夸克分享目录", "mobile"]
    );
    assert_eq!(plan.files[1].size, Some(22));
}

#[tokio::test]
async fn retrying_a_linked_share_leaf_reuses_its_original_directory_and_name() {
    let store = TaskStore::open_in_memory().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let share_hash = "a".repeat(64);
    let entry_hash = "b".repeat(64);
    let source = format!("cloud-share://quark/{share_hash}/{entry_hash}");
    let plan =
        plan_cloud_directory_download(CloudProvider::Quark, "root", "夸克分享目录", |id| {
            match id {
                "root" => Ok(directory(id, vec![folder("mobile", id, "mobile")])),
                "mobile" => Ok(directory(id, vec![file("wallpaper", id, "wallpaper.jpg")])),
                _ => Err(QuarkError::InvalidResponse),
            }
        })
        .unwrap();
    let task_id = Arc::new(Mutex::new(None));
    let recorded_task_id = Arc::clone(&task_id);
    submit_cloud_directory_plan(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, _, _| {
            let task = store.create(NewTask::direct(source.clone())).unwrap();
            *recorded_task_id.lock().unwrap() = Some(task.id);
            std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
        },
    )
    .await
    .unwrap();
    let task_id = task_id.lock().unwrap().expect("directory leaf task");

    let configured =
        cloud_directory_member_retry_options(&store, task_id, UriSubmissionOptions::default())
            .unwrap();

    assert_eq!(
        configured.directory,
        Some(destination.path().join("夸克分享目录").join("mobile"))
    );
    assert_eq!(configured.output_name.as_deref(), Some("wallpaper.jpg"));
}

#[test]
fn plans_nested_directory_files_under_a_safe_root_relative_path() {
    let plan =
        plan_cloud_directory_download(CloudProvider::Quark, "root-folder", "课程/资料", |id| {
            match id {
                "root-folder" => Ok(directory(
                    id,
                    vec![
                        file("overview", id, "概览.txt"),
                        folder("chapter-1", id, "第一章"),
                    ],
                )),
                "chapter-1" => Ok(directory(id, vec![file("lesson", id, "讲义.pdf")])),
                _ => Err(QuarkError::InvalidResponse),
            }
        })
        .expect("a non-empty folder tree should produce download entries");

    assert_eq!(plan.files.len(), 2);
    assert_eq!(plan.files[0].file_id, "overview");
    assert_eq!(plan.files[0].relative_directory, vec!["课程_资料"]);
    assert_eq!(plan.files[0].output_name, "概览.txt");
    assert_eq!(plan.files[1].file_id, "lesson");
    assert_eq!(
        plan.files[1].relative_directory,
        vec!["课程_资料", "第一章"]
    );
    assert_eq!(plan.files[1].output_name, "讲义.pdf");
}

#[test]
fn rejects_empty_cycles_duplicate_files_and_pikpak_recycle_bin() {
    let empty = plan_cloud_directory_download(CloudProvider::Quark, "empty", "空目录", |id| {
        Ok(directory(id, vec![]))
    });
    assert!(matches!(empty, Err(QuarkError::InvalidInput(message)) if message.contains("空目录")));

    let cycle = plan_cloud_directory_download(CloudProvider::Quark, "root", "循环", |id| {
        Ok(directory(id, vec![folder("root", id, "又回来了")]))
    });
    assert!(
        matches!(cycle, Err(QuarkError::InvalidInput(message)) if message.contains("循环或重复目录"))
    );

    let duplicate_file =
        plan_cloud_directory_download(CloudProvider::Quark, "root", "重复", |id| {
            Ok(directory(
                id,
                vec![file("same", id, "a.txt"), file("same", id, "b.txt")],
            ))
        });
    assert!(
        matches!(duplicate_file, Err(QuarkError::InvalidInput(message)) if message.contains("重复文件"))
    );

    let recycle_bin = plan_cloud_directory_download(
        CloudProvider::Pikpak,
        "__all_downloader_pikpak_recycle_bin__",
        "回收站",
        |_| panic!("the virtual recycle bin must not be listed"),
    );
    assert!(
        matches!(recycle_bin, Err(QuarkError::InvalidInput(message)) if message.contains("回收站"))
    );
}

#[test]
fn rejects_directory_trees_beyond_the_bounded_traversal_limit() {
    let result = plan_cloud_directory_download(CloudProvider::Quark, "dir-0", "资料", |id| {
        let index = id
            .strip_prefix("dir-")
            .expect("fixture directory id")
            .parse::<usize>()
            .expect("fixture directory index");
        Ok(directory(
            id,
            vec![folder(&format!("dir-{}", index + 1), id, "子目录")],
        ))
    });

    assert!(
        matches!(result, Err(QuarkError::InvalidInput(message)) if message.contains("过多子目录"))
    );
}

#[test]
fn rejects_windows_normalized_output_path_collisions_before_queueing() {
    let normalized_file_name =
        plan_cloud_directory_download(CloudProvider::Quark, "root", "资料", |id| {
            Ok(directory(
                id,
                vec![file("one", id, "a:b.txt"), file("two", id, "a?b.txt")],
            ))
        });
    assert!(
        matches!(normalized_file_name, Err(QuarkError::InvalidInput(message)) if message.contains("输出路径冲突"))
    );

    let case_insensitive_name =
        plan_cloud_directory_download(CloudProvider::Quark, "root", "资料", |id| {
            Ok(directory(
                id,
                vec![file("one", id, "Readme.txt"), file("two", id, "README.TXT")],
            ))
        });
    assert!(
        matches!(case_insensitive_name, Err(QuarkError::InvalidInput(message)) if message.contains("输出路径冲突"))
    );

    let normalized_directory =
        plan_cloud_directory_download(CloudProvider::Quark, "root", "资料", |id| match id {
            "root" => Ok(directory(
                id,
                vec![
                    folder("first", id, "2026:笔记"),
                    folder("second", id, "2026?笔记"),
                ],
            )),
            "first" | "second" => Ok(directory(
                id,
                vec![file(&format!("{id}-file"), id, "说明.txt")],
            )),
            _ => Err(QuarkError::InvalidResponse),
        });
    assert!(
        matches!(normalized_directory, Err(QuarkError::InvalidInput(message)) if message.contains("输出路径冲突"))
    );
}

#[test]
fn rejects_a_file_that_is_a_windows_normalized_ancestor_of_another_target() {
    let result = plan_cloud_directory_download(CloudProvider::Quark, "root", "资料", |id| match id
    {
        "root" => Ok(directory(
            id,
            vec![file("root-file", id, "a"), folder("subtree", id, "A")],
        )),
        "subtree" => Ok(directory(id, vec![file("nested-file", id, "x.txt")])),
        _ => Err(QuarkError::InvalidResponse),
    });

    assert!(
        matches!(result, Err(QuarkError::InvalidInput(message)) if message.contains("输出路径冲突"))
    );
}

#[tokio::test]
async fn publishes_the_directory_root_before_any_leaf_is_created_or_published() {
    let store = TaskStore::open_in_memory().unwrap();
    let plan =
        plan_cloud_directory_download(CloudProvider::Quark, "root", "夸克精选壁纸", |id| {
            Ok(directory(id, vec![file("wallpaper", id, "壁纸_1.jpg")]))
        })
        .unwrap();
    let destination = tempfile::tempdir().unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let root_order = Arc::clone(&order);
    let leaf_order = Arc::clone(&order);

    submit_cloud_directory_plan_with_group_event(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        false,
        move |root| {
            assert_eq!(root.kind, "folder");
            assert_eq!(root.name, "夸克精选壁纸");
            assert_eq!(root.status, TaskStatus::Queued);
            assert_eq!(root.children.len(), 1);
            root_order.lock().unwrap().push("root");
            Ok(())
        },
        |_, _, _, context| {
            assert_eq!(order.lock().unwrap().as_slice(), ["root"]);
            let task = store
                .create(NewTask::direct("cloud://quark/wallpaper"))
                .unwrap();
            store
                .link_cloud_directory_group_item_task(context.group_id, context.item_id, task.id)
                .unwrap();
            assert!(store
                .cloud_directory_group_for_task(task.id)
                .unwrap()
                .is_some());
            leaf_order.lock().unwrap().push("leaf");
            std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
        },
    )
    .await
    .unwrap();

    assert_eq!(order.lock().unwrap().as_slice(), ["root", "leaf"]);
}

#[tokio::test]
async fn directory_submission_persists_parent_links_success_and_keeps_provider_failure_retryable() {
    let store = TaskStore::open_in_memory().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| match id {
        "root" => Ok(directory(
            id,
            vec![
                file("first", id, "第一章.txt"),
                folder("nested", id, "资料"),
            ],
        )),
        "nested" => Ok(directory(id, vec![file("second", id, "第二章.txt")])),
        _ => Err(QuarkError::InvalidResponse),
    })
    .unwrap();

    let destination = tempfile::tempdir().unwrap();
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let submitted_for_callback = Arc::clone(&submitted);
    let outcome = submit_cloud_directory_plan(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, file_id, _| {
            submitted_for_callback.lock().unwrap().push(file_id.clone());
            let result = if file_id == "first" {
                assert_eq!(
                    store.cloud_directory_groups().unwrap().len(),
                    1,
                    "parent must exist before first leaf submission"
                );
                let task = store
                    .create(NewTask::direct("cloud://quark/first"))
                    .unwrap();
                Ok(DirectoryLeafSubmission::Queued(task.id))
            } else {
                Err(QuarkError::Provider("fixture direct-link failure".into()))
            };
            std::future::ready(result)
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome.queued_count, 1);
    assert_eq!(outcome.task.payload.task.kind, "folder");
    let group = store
        .cloud_directory_group(outcome.task.payload.task.id)
        .unwrap()
        .unwrap();
    let result = serde_json::to_value(&outcome.task).unwrap();
    assert_eq!(
        result["queueTask"]["id"],
        serde_json::Value::String(group.id.to_string()),
        "directory submission must return the authoritative recursive queue root"
    );
    assert_eq!(
        result["queueTask"]["children"].as_array().map(Vec::len),
        Some(2)
    );
    let items = store.cloud_directory_group_items(group.id).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].remote_file_id, "first");
    assert!(items[0].task_id.is_some());
    assert_eq!(items[1].remote_file_id, "second");
    assert_eq!(items[1].relative_directory, vec!["课程", "资料"]);
    assert!(items[1].task_id.is_none());
    assert_eq!(items[1].failure_code.as_deref(), Some("provider_error"));
    let mut submitted = submitted.lock().unwrap().clone();
    submitted.sort();
    assert_eq!(submitted, ["first", "second"]);
}

#[tokio::test]
async fn active_list_projects_a_submitted_directory_as_one_nested_folder_root() {
    let store = TaskStore::open_in_memory().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| match id {
        "root" => Ok(directory(
            id,
            vec![
                file("first", id, "第一章.txt"),
                folder("nested", id, "资料"),
            ],
        )),
        "nested" => Ok(directory(id, vec![file("second", id, "第二章.txt")])),
        _ => Err(QuarkError::InvalidResponse),
    })
    .unwrap();
    let destination = tempfile::tempdir().unwrap();
    submit_cloud_directory_plan(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, file_id, _| {
            let mut new_task = NewTask::direct(format!("cloud://quark/{file_id}"));
            new_task.name = match file_id.as_str() {
                "first" => "第一章.txt".into(),
                "second" => "第二章.txt".into(),
                _ => unreachable!("fixture only submits planned leaves"),
            };
            let task = store.create(new_task).unwrap();
            std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
        },
    )
    .await
    .unwrap();

    let visible = list_queue_tasks_for_scope(&store, Some("active")).unwrap();

    assert_eq!(
        visible.len(),
        1,
        "linked directory leaves must not remain top-level"
    );
    assert_eq!(
        visible.iter().filter(|task| task.kind == "folder").count(),
        1
    );
    let root = &visible[0];
    assert_eq!(root.name, "课程");
    assert_eq!(root.children.len(), 2);
    assert_eq!(root.children[0].name, "第一章.txt");
    assert_eq!(root.children[1].kind, "folder");
    assert_eq!(root.children[1].name, "资料");
    assert_eq!(root.children[1].children.len(), 1);
    assert_eq!(root.children[1].children[0].name, "第二章.txt");
}

#[tokio::test]
async fn resubmitting_a_persisted_failed_leaf_does_not_touch_linked_siblings() {
    let store = TaskStore::open_in_memory().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| match id {
        "root" => Ok(directory(
            id,
            vec![
                file("linked", id, "已入队.txt"),
                file("retry-me", id, "失败后重试.txt"),
            ],
        )),
        _ => Err(QuarkError::InvalidResponse),
    })
    .unwrap();
    let destination = tempfile::tempdir().unwrap();
    let first = submit_cloud_directory_plan(
        &store,
        CloudProvider::Quark,
        "root",
        plan.clone(),
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, file_id, _| {
            let result = if file_id == "linked" {
                let task = store
                    .create(NewTask::direct("cloud://quark/linked"))
                    .unwrap();
                Ok(DirectoryLeafSubmission::Queued(task.id))
            } else {
                Err(QuarkError::Provider("fixture failure".into()))
            };
            std::future::ready(result)
        },
    )
    .await
    .unwrap();
    let group = store
        .cloud_directory_group(first.task.payload.task.id)
        .unwrap()
        .unwrap();
    let items = store.cloud_directory_group_items(group.id).unwrap();
    let linked = items
        .iter()
        .find(|item| item.remote_file_id == "linked")
        .unwrap()
        .clone();
    let failed = items
        .iter()
        .find(|item| item.remote_file_id == "retry-me")
        .unwrap()
        .clone();
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let submitted_for_callback = Arc::clone(&submitted);
    let retried = retry_cloud_directory_group_item_with(
        &store,
        group.id,
        failed.id,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, file_id, _, context| {
            submitted_for_callback.lock().unwrap().push(file_id.clone());
            let task = store
                .create(NewTask::direct("cloud://quark/retry-me"))
                .unwrap();
            store
                .link_cloud_directory_group_item_task(context.group_id, context.item_id, task.id)
                .unwrap();
            assert_eq!(
                store.cloud_directory_group_for_task(task.id).unwrap(),
                Some(group.clone())
            );
            async move { Ok(DirectoryLeafSubmission::Queued(task.id)) }
        },
    )
    .await
    .unwrap();
    assert_eq!(submitted.lock().unwrap().as_slice(), ["retry-me"]);
    assert_eq!(retried.payload.task.source, "cloud://quark/retry-me");
    let retried_json = serde_json::to_value(&retried).unwrap();
    assert_eq!(
        retried_json["queueTask"]["id"],
        serde_json::Value::String(group.id.to_string()),
        "a retried virtual leaf must refresh the same directory root"
    );
    let retried_items = store.cloud_directory_group_items(group.id).unwrap();
    assert_eq!(
        retried_items
            .iter()
            .find(|item| item.id == linked.id)
            .unwrap()
            .task_id,
        linked.task_id
    );
    assert!(retried_items
        .iter()
        .find(|item| item.id == failed.id)
        .unwrap()
        .task_id
        .is_some());
    let rejected = retry_cloud_directory_group_item_with(
        &store,
        group.id,
        linked.id,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, _, _, _| async { panic!("linked items must be rejected before submission") },
    )
    .await;
    assert!(
        matches!(rejected, Err(QuarkError::InvalidInput(message)) if message.contains("已经入队"))
    );
}

#[tokio::test]
async fn group_retry_submits_each_unlinked_leaf_once_and_returns_the_group_root() {
    let store = TaskStore::open_in_memory().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| {
        Ok(directory(
            id,
            vec![
                file("linked", id, "已入队.txt"),
                file("first", id, "一.txt"),
                file("second", id, "二.txt"),
            ],
        ))
    })
    .unwrap();
    let destination = tempfile::tempdir().unwrap();
    let first = submit_cloud_directory_plan(
        &store,
        CloudProvider::Quark,
        "root",
        plan,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, file_id, _| {
            let result = if file_id == "linked" {
                Ok(DirectoryLeafSubmission::Queued(
                    store
                        .create(NewTask::direct("cloud://quark/linked"))
                        .unwrap()
                        .id,
                ))
            } else {
                Err(QuarkError::Provider("fixture failure".into()))
            };
            std::future::ready(result)
        },
    )
    .await
    .unwrap();
    let group_id = first.task.payload.task.id;
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let submitted_for_callback = Arc::clone(&submitted);

    let retried = retry_cloud_directory_group_with(
        &store,
        group_id,
        UriSubmissionOptions {
            directory: Some(destination.path().to_path_buf()),
            ..Default::default()
        },
        |_, file_id, _, context| {
            submitted_for_callback.lock().unwrap().push(file_id.clone());
            let task = store
                .create(NewTask::direct(format!("cloud://quark/{file_id}")))
                .unwrap();
            store
                .link_cloud_directory_group_item_task(context.group_id, context.item_id, task.id)
                .unwrap();
            assert_eq!(context.group_id, group_id);
            std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
        },
    )
    .await
    .unwrap();

    let mut submitted = submitted.lock().unwrap().clone();
    submitted.sort();
    assert_eq!(submitted, ["first", "second"]);
    let json = serde_json::to_value(retried).unwrap();
    assert_eq!(json["queueTask"]["id"], group_id.to_string());
    assert_eq!(
        json["queueTask"]["children"].as_array().map(Vec::len),
        Some(3)
    );
}

#[tokio::test]
async fn source_gate_serializes_matching_sources_without_blocking_other_sources() {
    let gate = Arc::new(CloudDownloadSourceGate::default());
    let first = gate
        .acquire("cloud://quark/file-1")
        .await
        .expect("source gate must be available");
    let entered = Arc::new(tokio::sync::Notify::new());
    let waiting_gate = Arc::clone(&gate);
    let waiting_entered = Arc::clone(&entered);
    let waiting = tokio::spawn(async move {
        waiting_entered.notify_one();
        waiting_gate
            .acquire("cloud://quark/file-1")
            .await
            .expect("source gate must be available")
    });

    entered.notified().await;
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "matching source must remain gated");

    let other = tokio::time::timeout(
        Duration::from_millis(100),
        gate.acquire("cloud://quark/file-2"),
    )
    .await
    .expect("a different stable source must not wait")
    .expect("source gate must be available");
    drop(other);
    drop(first);
    tokio::time::timeout(Duration::from_millis(100), waiting)
        .await
        .expect("matching source must proceed after the first submission completes")
        .expect("source gate task must not panic");
}

#[tokio::test]
async fn source_gate_cleanup_reclaims_finished_sources_and_preserves_holders_and_waiters() {
    use std::future::Future;
    let gate = Arc::new(CloudDownloadSourceGate::default());
    for index in 0..1000 {
        drop(
            gate.acquire(&format!("cloud://quark/file-{index}"))
                .await
                .unwrap(),
        );
    }
    assert_eq!(gate.cleanup().unwrap(), 0);
    let held = gate.acquire("cloud://quark/held").await.unwrap();
    assert_eq!(gate.cleanup().unwrap(), 1);
    let mut waiting = Box::pin(gate.acquire("cloud://quark/held"));
    // Poll the actual acquire until Pending, so its strong owner is established.
    std::future::poll_fn(|cx| {
        assert!(waiting.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(held);
    assert_eq!(
        gate.cleanup().unwrap(),
        1,
        "a waiting caller owns the same mutex"
    );
    let next = waiting.await.unwrap();
    assert_eq!(gate.cleanup().unwrap(), 1);
    drop(next);
    assert_eq!(gate.cleanup().unwrap(), 0);
}
