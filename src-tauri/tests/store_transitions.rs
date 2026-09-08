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
