use alldownloader_lib::{NewTask, TaskStatus, TaskStore};

#[test]
fn direct_task_can_move_from_parsing_to_downloading() {
    let store = TaskStore::open_in_memory().expect("memory store");
    let task = store
        .create(NewTask::direct("https://host/file.iso"))
        .expect("task");
    let task = store
        .transition(task.id, TaskStatus::Queued)
        .expect("queue task");
    let task = store
        .transition(task.id, TaskStatus::Downloading)
        .expect("download task");
    assert_eq!(task.status, TaskStatus::Downloading);
}

#[test]
fn rejects_illegal_transition_from_completed_task() {
    let store = TaskStore::open_in_memory().expect("memory store");
    let task = store
        .create(NewTask::direct("https://host/file.iso"))
        .expect("task");
    let task = store.transition(task.id, TaskStatus::Queued).unwrap();
    let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
    let task = store.transition(task.id, TaskStatus::Processing).unwrap();
    let task = store.transition(task.id, TaskStatus::Completed).unwrap();
    assert!(store.transition(task.id, TaskStatus::Downloading).is_err());
}

#[test]
fn resubmission_replaces_a_failed_gid_without_discarding_the_task_identity() {
    let store = TaskStore::open_in_memory().expect("memory store");
    let task = store
        .create(NewTask::direct("https://host/file.iso"))
        .expect("task");
    store.bind_gid(task.id, "old-gid").expect("bind old gid");
    let failed = store.fail(task.id, "previous transfer stopped").unwrap();

    let retried = store
        .rebind_for_resubmission(failed.id, "new-gid")
        .expect("rebind retry gid");

    assert_eq!(retried.id, failed.id);
    assert_eq!(retried.status, TaskStatus::Parsing);
    assert_eq!(retried.aria_gid.as_deref(), Some("new-gid"));
    assert_eq!(retried.error, None);
    assert!(store.find_by_gid("old-gid").unwrap().is_none());
}

#[test]
fn removing_a_task_record_keeps_any_output_file_outside_the_database() {
    let root = std::env::temp_dir().join(format!("remove-task-record-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let output = root.join("fixture.bin");
    std::fs::write(&output, b"payload").unwrap();
    let store = TaskStore::open_in_memory().expect("memory store");
    let task = store
        .create(NewTask::direct("https://host/file.iso"))
        .expect("task");
    store
        .upsert_files(task.id, &[(0, output.to_string_lossy().into_owned(), 7, 7, true)])
        .unwrap();

    store.remove_task_record(task.id).unwrap();

    assert!(store.find(task.id).is_err());
    assert!(output.is_file());
    std::fs::remove_dir_all(root).unwrap();
}
