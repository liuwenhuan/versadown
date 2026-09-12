use alldownloader_lib::cloud::{
    commands::{
        cloud_directory_member_retry_options, plan_cloud_directory_download,
        plan_cloud_share_directory_download, retry_cloud_directory_group_item_with,
        retry_cloud_directory_group_with, submit_cloud_directory_plan,
        submit_cloud_directory_plan_with_group_event, CloudDownloadSourceGate,
        DirectoryLeafSubmission,
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
