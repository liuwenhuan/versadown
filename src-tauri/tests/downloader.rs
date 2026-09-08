use alldownloader_lib::{confirm_download, prepare_download, TaskStatus, TaskStore};

#[test]
fn single_http_result_queues_and_starts_without_confirmation() {
    let store = TaskStore::open_in_memory().unwrap();
    let outcome = prepare_download("https://example.com/file.bin", &store).unwrap();
    assert!(!outcome.requires_confirmation);
    assert_eq!(outcome.task.status, TaskStatus::Downloading);
}

#[test]
fn magnet_waits_for_file_confirmation_before_it_starts() {
    let store = TaskStore::open_in_memory().unwrap();
    let outcome = prepare_download("magnet:?xt=urn:btih:abc", &store).unwrap();
    assert!(outcome.requires_confirmation);
    assert_eq!(outcome.task.status, TaskStatus::AwaitingUser);
    let task = confirm_download(outcome.task.id, &store).unwrap();
    assert_eq!(task.status, TaskStatus::Downloading);
}
