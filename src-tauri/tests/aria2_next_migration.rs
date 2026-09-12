use alldownloader_lib::{run_aria2_next_migration, DownloadTask, NewTask, TaskStatus, TaskStore};

fn complete(mut task: DownloadTask) -> DownloadTask {
    task.status = TaskStatus::Completed;
    task.progress = 100;
    task.total_length = Some(8);
    task.completed_length = 8;
    task
}

#[test]
fn versioned_migration_truncates_the_legacy_session_exactly_once() {
    let temporary = tempfile::tempdir().unwrap();
    let data_dir = temporary.path().join("data");
    let download_root = temporary.path().join("downloads");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&download_root).unwrap();
    std::fs::write(data_dir.join("aria2.session"), b"legacy task\n").unwrap();
    let store = TaskStore::open_path(&data_dir.join("tasks.sqlite")).unwrap();
    let old = store
        .create(NewTask::direct("https://example.test/old.bin"))
        .unwrap();

    let report = run_aria2_next_migration(&store, &data_dir, &download_root)
        .unwrap()
        .expect("first run must migrate");
    assert_eq!(report.removed_tasks, 1);
    assert_eq!(std::fs::read(data_dir.join("aria2.session")).unwrap(), b"");
    assert!(store.find(old.id).is_err());

    let after = store
        .create(NewTask::direct("https://example.test/new.bin"))
        .unwrap();
    assert!(run_aria2_next_migration(&store, &data_dir, &download_root)
        .unwrap()
        .is_none());
    assert!(store.find(after.id).is_ok());
}

#[test]
fn removes_incomplete_legacy_tasks_and_only_payloads_inside_download_root() {
    let temporary = tempfile::tempdir().unwrap();
    let download_root = temporary.path().join("downloads");
    let outside_root = temporary.path().join("outside");
    std::fs::create_dir_all(&download_root).unwrap();
    std::fs::create_dir_all(&outside_root).unwrap();
    let store = TaskStore::open_path(&temporary.path().join("tasks.sqlite")).unwrap();

    let inside_path = download_root.join("partial.bin");
    let inside_control = download_root.join("partial.bin.aria2");
    std::fs::write(&inside_path, b"partial").unwrap();
    std::fs::write(&inside_control, b"control").unwrap();
    let inside = store
        .create(NewTask::direct("https://example.test/partial.bin"))
        .unwrap();
    store
        .upsert_files(
            inside.id,
            &[(1, inside_path.to_string_lossy().into_owned(), 100, 7, true)],
        )
        .unwrap();

    let outside_path = outside_root.join("keep.bin");
    std::fs::write(&outside_path, b"outside").unwrap();
    let outside = store
        .create(NewTask::direct("https://example.test/outside.bin"))
        .unwrap();
    store
        .upsert_files(
            outside.id,
            &[(1, outside_path.to_string_lossy().into_owned(), 7, 3, true)],
        )
        .unwrap();

    let completed_path = download_root.join("complete.bin");
    std::fs::write(&completed_path, b"complete").unwrap();
    let completed = store
        .create(NewTask::direct("https://example.test/complete.bin"))
        .unwrap();
    store
        .apply_transfer_snapshot(complete(completed.clone()))
        .unwrap();
    store
        .upsert_files(
            completed.id,
            &[(1, completed_path.to_string_lossy().into_owned(), 8, 8, true)],
        )
        .unwrap();

    let report = store
        .purge_incompatible_engine_tasks(&download_root)
        .unwrap();

    assert_eq!(report.removed_tasks, 2);
    assert_eq!(report.removed_files, 2);
    assert_eq!(report.skipped_outside_files, 1);
    assert!(!inside_path.exists());
    assert!(!inside_control.exists());
    assert!(outside_path.exists());
    assert!(completed_path.exists());
    assert!(store.find(inside.id).is_err());
    assert!(store.find(outside.id).is_err());
    assert_eq!(
        store.find(completed.id).unwrap().status,
        TaskStatus::Completed
    );
}
