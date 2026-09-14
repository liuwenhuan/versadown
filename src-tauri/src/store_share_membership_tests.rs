use super::*;

#[tokio::test]
async fn aliyun_share_directory_scheduler_links_all_2427_files() {
    use crate::cloud::commands::{
        submit_cloud_directory_plan_with_group_event, CloudDirectoryDownloadPlan,
        DirectoryLeafSubmission, PlannedCloudFile,
    };

    let store = TaskStore::open_in_memory().unwrap();
    let output = tempfile::tempdir().unwrap();
    let plan = CloudDirectoryDownloadPlan {
        files: (0..2_427)
            .map(|index| PlannedCloudFile {
                file_id: format!("cloud-share://aliyun/share-large/file-{index}"),
                relative_directory: vec!["Large Share".into()],
                output_name: format!("file-{index}.bin"),
                size: Some(1_024),
            })
            .collect(),
    };
    let configured = crate::UriSubmissionOptions {
        directory: Some(output.path().to_path_buf()),
        ..Default::default()
    };

    let outcome = submit_cloud_directory_plan_with_group_event(
        &store,
        CloudProvider::Aliyun,
        "cloud-share://aliyun/share-large",
        plan,
        configured,
        false,
        |_| Ok(()),
        |_, source, _, context| {
            let task = store
                .prepare_cloud_share_directory_leaf_task(
                    context.group_id,
                    context.item_id,
                    NewTask {
                        source,
                        name: format!("file-{}.bin", context.item_id),
                        kind: "cloud".into(),
                        is_private: false,
                    },
                    None,
                )
                .unwrap();
            std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
        },
    )
    .await
    .unwrap();

    assert_eq!(outcome.queued_count, 2_427);
    assert!(outcome.failures.is_empty());
    let group_id = uuid::Uuid::parse_str(&outcome.task.payload.task.id.to_string()).unwrap();
    let items = store.cloud_directory_group_items(group_id).unwrap();
    assert_eq!(items.len(), 2_427);
    assert!(items.iter().all(|item| item.task_id.is_some()));
}

#[tokio::test]
async fn all_registered_cloud_directories_keep_one_root_when_resuming_after_reopen() {
    use crate::cloud::commands::{
        recover_interrupted_directory_preparation, retry_cloud_directory_group_with,
        DirectoryLeafSubmission,
    };
    for provider in CloudProvider::ALL {
        for shared in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let database = temp.path().join("tasks.sqlite");
            let store = TaskStore::open_path(&database).unwrap();
            let source = if shared {
                format!("cloud-share://{}/plan/file", provider.key())
            } else {
                format!("cloud://{}/file", provider.key())
            };
            let root = store
                .create_cloud_directory_group(
                    NewCloudDirectoryGroup::new(provider, "root", "Folder", temp.path()),
                    &[NewCloudDirectoryGroupItem::new(
                        &source,
                        &["Folder", "Nested"],
                        "file.txt",
                        Some(100),
                    )],
                )
                .unwrap();
            store.set_cloud_directory_paused(root.id, true).unwrap();
            drop(store);

            let store = TaskStore::open_path(&database).unwrap();
            store.set_cloud_directory_paused(root.id, false).unwrap();
            let outcome = retry_cloud_directory_group_with(
                &store,
                root.id,
                crate::UriSubmissionOptions::default(),
                |actual_provider, file_id, _, context| {
                    assert_eq!(actual_provider, provider);
                    assert_eq!(file_id, source);
                    let input = NewTask {
                        source: source.clone(),
                        name: "file.txt".into(),
                        kind: "cloud".into(),
                        is_private: false,
                    };
                    let task = if shared {
                        store
                            .prepare_cloud_share_directory_leaf_task(
                                context.group_id,
                                context.item_id,
                                input,
                                None,
                            )
                            .unwrap()
                    } else {
                        store
                            .claim_cloud_directory_leaf_task(
                                context.group_id,
                                context.item_id,
                                provider,
                                "test-account",
                                input,
                            )
                            .unwrap()
                            .unwrap()
                    };
                    // Assert before the outer submission loop can perform its
                    // defensive link: polling must never observe a flat leaf.
                    let visible =
                        crate::list_queue_tasks_for_scope(&store, Some("active")).unwrap();
                    assert_eq!(visible.len(), 1, "{} shared={shared}", provider.key());
                    assert_eq!(visible[0].id, root.id.to_string());
                    assert_eq!(
                        store.cloud_directory_group_items(root.id).unwrap()[0].task_id,
                        Some(task.id)
                    );
                    std::future::ready(Ok(DirectoryLeafSubmission::Queued(task.id)))
                },
            )
            .await
            .unwrap();
            assert_eq!(outcome.queue_task.unwrap().id, root.id.to_string());
            let member_id = store.cloud_directory_group_items(root.id).unwrap()[0]
                .task_id
                .unwrap();
            drop(store);

            let store = TaskStore::open_path(&database).unwrap();
            let recovered = recover_interrupted_directory_preparation(&store, member_id).unwrap();
            assert_eq!(recovered.id, member_id);
            assert_eq!(recovered.status, TaskStatus::Paused);
            assert!(retry_cloud_directory_group_with(
                &store,
                root.id,
                Default::default(),
                |_, _, _, _| async { panic!("an already linked member must not be created again") },
            )
            .await
            .is_err());
            let visible = crate::list_queue_tasks_for_scope(&store, Some("active")).unwrap();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].id, root.id.to_string());
            assert_eq!(store.list().unwrap().len(), 1);
            assert_eq!(
                store.cloud_directory_group_items(root.id).unwrap()[0].task_id,
                Some(member_id)
            );
        }
    }
}

fn group(store: &TaskStore) -> (Uuid, Uuid) {
    let group = store
        .create_cloud_directory_group(
            NewCloudDirectoryGroup::new(
                CloudProvider::Baidu,
                "share-root",
                "Share",
                Path::new("C:/downloads"),
            ),
            &[NewCloudDirectoryGroupItem::new(
                "file",
                &["Share"],
                "file.txt",
                Some(7),
            )],
        )
        .unwrap();
    let item = store
        .cloud_directory_group_items(group.id)
        .unwrap()
        .remove(0);
    (group.id, item.id)
}

fn leaf() -> NewTask {
    NewTask {
        source: "cloud-share://baidu/share/file".into(),
        name: "file.txt".into(),
        kind: "cloud".into(),
        is_private: true,
    }
}

#[test]
fn share_membership_new_and_retried_leaf_keep_identity_and_need_no_account() {
    let store = TaskStore::open_in_memory().unwrap();
    let (group, item) = group(&store);
    let first = store
        .prepare_cloud_share_directory_leaf_task(group, item, leaf(), None)
        .unwrap();
    assert_eq!(
        store.cloud_directory_group_items(group).unwrap()[0].task_id,
        Some(first.id)
    );
    assert!(first.is_private);
    assert_eq!(store.cloud_download_account(first.id).unwrap(), None);
    store
        .fail_with_code(first.id, Some("prepare_failed"), "failed")
        .unwrap();
    let retry = store
        .prepare_cloud_share_directory_leaf_task(group, item, leaf(), Some(first.id))
        .unwrap();
    assert_eq!(retry.id, first.id);
    assert_eq!(retry.status, TaskStatus::Parsing);
    assert_eq!(retry.error, None);
    assert_eq!(retry.failure_code, None);
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(
        store.cloud_directory_group_items(group).unwrap()[0].task_id,
        Some(first.id)
    );
}

#[test]
fn share_membership_rejects_inactive_parent_and_cannot_steal_retry_from_another_group() {
    let store = TaskStore::open_in_memory().unwrap();
    let (first_group, first_item) = group(&store);
    let (second_group, second_item) = group(&store);
    let task = store
        .prepare_cloud_share_directory_leaf_task(first_group, first_item, leaf(), None)
        .unwrap();
    assert!(store
        .prepare_cloud_share_directory_leaf_task(second_group, second_item, leaf(), Some(task.id))
        .is_err());
    assert_eq!(
        store.cloud_directory_group_items(first_group).unwrap()[0].task_id,
        Some(task.id)
    );
    assert_eq!(
        store.cloud_directory_group_items(second_group).unwrap()[0].task_id,
        None
    );
    store.recycle_cloud_directory_group(second_group).unwrap();
    assert!(store
        .prepare_cloud_share_directory_leaf_task(second_group, second_item, leaf(), None)
        .is_err());
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn share_membership_retry_cannot_revive_a_task_cancelled_since_lookup() {
    let store = TaskStore::open_in_memory().unwrap();
    let (group, item) = group(&store);
    let mut task = store
        .prepare_cloud_share_directory_leaf_task(group, item, leaf(), None)
        .unwrap();
    task.status = TaskStatus::Cancelled;
    store.apply_transfer_snapshot(task.clone()).unwrap();
    assert!(store
        .prepare_cloud_share_directory_leaf_task(group, item, leaf(), Some(task.id))
        .is_err());
    assert_eq!(store.find(task.id).unwrap().status, TaskStatus::Cancelled);
}

#[test]
fn share_membership_link_write_failure_rolls_back_new_task_and_retry_state() {
    let store = TaskStore::open_in_memory().unwrap();
    let (group, item) = group(&store);
    let old = store.create(leaf()).unwrap();
    store
        .fail_with_code(old.id, Some("prepare_failed"), "keep failure")
        .unwrap();
    store.connection.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_share_link BEFORE UPDATE OF task_id ON cloud_directory_group_items
         BEGIN SELECT RAISE(ABORT, 'injected membership failure'); END;"
    ).unwrap();
    assert!(store
        .prepare_cloud_share_directory_leaf_task(group, item, leaf(), None)
        .is_err());
    assert_eq!(
        store.list().unwrap().len(),
        1,
        "new task INSERT must roll back"
    );
    assert!(store
        .prepare_cloud_share_directory_leaf_task(group, item, leaf(), Some(old.id))
        .is_err());
    let unchanged = store.find(old.id).unwrap();
    assert_eq!(unchanged.status, TaskStatus::Failed);
    assert_eq!(unchanged.error.as_deref(), Some("keep failure"));
    assert_eq!(
        store.cloud_directory_group_items(group).unwrap()[0].task_id,
        None
    );
}

#[test]
#[cfg(windows)]
fn share_membership_reservation_failure_removes_only_new_tasks_and_preserves_retry_link() {
    use crate::cloud::{
        commands::{submit_new_cloud_share_file_scoped_with, DirectoryLeafContext},
        share::{self, CloudShareEntry, CloudSharePlan},
        DpapiCredentialStore, QuarkError,
    };
    for retry in [false, true] {
        let tasks = Arc::new(TaskStore::open_in_memory().unwrap());
        let (group, item) = group(&tasks);
        let directory = tempfile::tempdir().unwrap();
        let plans = Arc::new(share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().into()).unwrap(),
        ));
        let plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "reservation-race".into(),
            None,
            vec![CloudShareEntry::new(
                "reservation-race",
                "file".into(),
                "file.txt".into(),
                vec![],
                Some(7),
            )],
            serde_json::json!({}),
        );
        let source = plan.source_for("file");
        plans.upsert(&plan).unwrap();
        let existing = retry.then(|| {
            let mut new_task = leaf();
            new_task.source = source.clone();
            tasks
                .prepare_cloud_share_directory_leaf_task(group, item, new_task, None)
                .unwrap()
                .id
        });
        let connection = tasks.connection.lock().unwrap();
        let plan_database =
            Connection::open(directory.path().join("cloud-share-plans.sqlite")).unwrap();
        let sequence = || {
            plan_database
                .query_row(
                    "SELECT value FROM share_metadata WHERE key = 'sequence'",
                    [],
                    |row| row.get::<_, u64>(0),
                )
                .unwrap()
        };
        let initial_sequence = sequence();
        let worker_tasks = tasks.clone();
        let worker_plans = plans.clone();
        let worker_source = source.clone();
        let worker = std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(
                submit_new_cloud_share_file_scoped_with(
                    worker_tasks,
                    &worker_plans,
                    worker_source,
                    Default::default(),
                    Some(DirectoryLeafContext {
                        group_id: group,
                        item_id: item,
                    }),
                    false,
                    true,
                    None,
                    || async {
                        Err::<Arc<crate::DownloadService>, _>(QuarkError::Transport(
                            "engine unavailable".into(),
                        ))
                    },
                    || async { panic!("reservation failure must not resolve provider") },
                ),
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while sequence() == initial_sequence {
            assert!(
                std::time::Instant::now() < deadline,
                "submission never read plan"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // The committed sequence change proves entry resolution finished. The
        // task-store barrier holds submission while this synthetic plan expires.
        plan_database
            .execute("DELETE FROM share_plans", [])
            .unwrap();
        drop(connection);
        let result = worker.join().unwrap();
        let items = tasks.cloud_directory_group_items(group).unwrap();
        if let Some(id) = existing {
            assert_eq!(result.unwrap().payload.task.id, id);
            assert_eq!(tasks.find(id).unwrap().status, TaskStatus::Failed);
            assert_eq!(items[0].task_id, Some(id));
            assert_eq!(tasks.list().unwrap().len(), 1);
        } else {
            assert!(result.is_err());
            assert!(tasks.list().unwrap().is_empty());
            assert_eq!(items[0].task_id, None);
        }
    }
}
