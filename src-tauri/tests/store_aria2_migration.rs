use std::path::PathBuf;

use alldownloader_lib::{DownloadTask, TaskStatus, TaskStore};
use rusqlite::Connection;
use uuid::Uuid;

fn temporary_database_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "alldownloader-store-migration-{}.sqlite",
        Uuid::new_v4()
    ))
}

fn legacy_database_with_one_task() -> (PathBuf, Uuid) {
    let path = temporary_database_path();
    let id = Uuid::new_v4();
    let connection = Connection::open(&path).expect("open legacy database");
    connection
        .execute_batch(
            "CREATE TABLE tasks (
                id TEXT PRIMARY KEY NOT NULL,
                source TEXT NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                progress INTEGER NOT NULL DEFAULT 0,
                error TEXT
            );",
        )
        .expect("create legacy tasks table");
    connection
        .execute(
            "INSERT INTO tasks (id, source, name, kind, status, progress, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                id.to_string(),
                "https://example.test/legacy.zip",
                "legacy.zip",
                "file",
                "queued",
                0_u8,
                Option::<String>::None,
            ),
        )
        .expect("insert legacy task");
    (path, id)
}

#[test]
fn legacy_database_is_preserved_and_accepts_a_gid() {
    let (path, id) = legacy_database_with_one_task();
    let store = TaskStore::open_path(&path).expect("migrate legacy database");

    let task = store.find(id).expect("preserved legacy task");
    assert_eq!(task.name, "legacy.zip");
    assert_eq!(task.aria_gid, None);

    store.bind_gid(id, "gid-1").expect("bind gid");
    assert_eq!(store.find_by_gid("gid-1").unwrap().unwrap().id, id);

    drop(store);
    std::fs::remove_file(path).expect("remove test database");
}

#[test]
fn replacing_a_gid_keeps_the_same_local_task() {
    let store = TaskStore::open_in_memory().expect("memory store");
    let task = store
        .create(alldownloader_lib::NewTask::direct(
            "https://example.test/file.iso",
        ))
        .expect("create task");
    store.bind_gid(task.id, "metadata-gid").unwrap();

    let replaced = store
        .replace_gid(task.id, "metadata-gid", "download-gid")
        .unwrap();

    assert_eq!(replaced.id, task.id);
    assert_eq!(replaced.aria_gid.as_deref(), Some("download-gid"));
    assert!(store.find_by_gid("metadata-gid").unwrap().is_none());
    assert_eq!(
        store.find_by_gid("download-gid").unwrap().unwrap().id,
        task.id
    );
}

#[test]
fn transfer_snapshots_and_file_upserts_persist_real_aria2_values() {
    let path = temporary_database_path();
    let store = TaskStore::open_path(&path).expect("open store");
    let task = store
        .create(alldownloader_lib::NewTask::direct(
            "https://example.test/archive.zip",
        ))
        .expect("create task");
    store.bind_gid(task.id, "gid-42").unwrap();

    store
        .apply_transfer_snapshot(DownloadTask {
            id: task.id,
            source: task.source,
            name: task.name,
            kind: task.kind,
            status: TaskStatus::Downloading,
            progress: 25,
            error: None,
            aria_gid: Some("gid-42".to_string()),
            followed_by_gid: Some("next-gid".to_string()),
            total_length: Some(1_000),
            completed_length: 250,
            download_speed: 50,
            upload_speed: 2,
            connections: 3,
            eta_seconds: Some(15),
            aria_error_code: None,
            updated_at: 1_725_000_000,
        })
        .expect("persist snapshot");
    store
        .upsert_files(
            task.id,
            &[
                (1, "archive/first.bin".to_string(), 700, 250, true),
                (2, "archive/second.bin".to_string(), 300, 0, false),
            ],
        )
        .expect("persist files");
    store
        .upsert_files(
            task.id,
            &[(1, "archive/first.bin".to_string(), 700, 500, true)],
        )
        .expect("update one file");

    let stored = store.find(task.id).expect("read snapshot");
    assert_eq!(stored.status, TaskStatus::Downloading);
    assert_eq!(stored.total_length, Some(1_000));
    assert_eq!(stored.completed_length, 250);
    assert_eq!(stored.download_speed, 50);
    assert_eq!(stored.eta_seconds, Some(15));
    assert_eq!(stored.followed_by_gid.as_deref(), Some("next-gid"));

    let connection = Connection::open(&path).expect("inspect file rows");
    let mut statement = connection
        .prepare("SELECT file_index, path, length, completed_length, selected FROM task_files WHERE task_id = ?1 ORDER BY file_index")
        .expect("prepare file inspection");
    let files = statement
        .query_map([task.id.to_string()], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .expect("query files")
        .collect::<Result<Vec<_>, _>>()
        .expect("read files");
    assert_eq!(
        files,
        vec![
            (1, "archive/first.bin".to_string(), 700, 500, true),
            (2, "archive/second.bin".to_string(), 300, 0, false),
        ]
    );

    drop(statement);
    drop(connection);
    drop(store);
    std::fs::remove_file(path).expect("remove test database");
}
