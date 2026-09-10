use alldownloader_lib::cloud::{
    commands::{
        create_cloud_directory_group_for_download, plan_cloud_directory_download,
        CloudDownloadSourceGate,
    },
    CloudDirectory, CloudFile, CloudProvider, QuarkError,
};
use alldownloader_lib::TaskStore;
use std::{sync::Arc, time::Duration};

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

#[test]
fn persists_one_directory_group_before_leaf_submission_and_keeps_failed_leaf_retryable() {
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

    let group = create_cloud_directory_group_for_download(
        &store,
        CloudProvider::Quark,
        "root",
        &plan,
        std::path::Path::new("C:/downloads"),
    )
    .unwrap();
    let items = store.cloud_directory_group_items(group.id).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].remote_file_id, "first");
    assert_eq!(items[0].size, None);
    assert_eq!(items[1].remote_file_id, "second");
    assert_eq!(items[1].relative_directory, vec!["课程", "资料"]);

    store
        .fail_cloud_directory_group_item(
            group.id,
            items[1].id,
            "provider_error",
            "网盘拒绝下载请求",
        )
        .unwrap();
    let failed = store
        .cloud_directory_group_items(group.id)
        .unwrap()
        .remove(1);
    assert!(failed.task_id.is_none());
    assert_eq!(failed.failure_code.as_deref(), Some("provider_error"));
}

#[test]
fn retry_candidate_is_a_single_unlinked_persisted_leaf() {
    let store = TaskStore::open_in_memory().unwrap();
    let plan = plan_cloud_directory_download(CloudProvider::Quark, "root", "课程", |id| {
        Ok(directory(id, vec![file("retry-me", id, "失败后重试.txt")]))
    })
    .unwrap();
    let group = create_cloud_directory_group_for_download(
        &store,
        CloudProvider::Quark,
        "root",
        &plan,
        std::path::Path::new("C:/downloads"),
    )
    .unwrap();
    let item = store
        .cloud_directory_group_items(group.id)
        .unwrap()
        .remove(0);
    store
        .fail_cloud_directory_group_item(group.id, item.id, "provider_error", "网盘拒绝下载请求")
        .unwrap();

    let retry = store
        .cloud_directory_group_items(group.id)
        .unwrap()
        .remove(0);
    assert_eq!(retry.remote_file_id, "retry-me");
    assert!(
        retry.task_id.is_none(),
        "retry must submit only this persisted leaf"
    );
    assert_eq!(retry.relative_directory, vec!["课程"]);
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
