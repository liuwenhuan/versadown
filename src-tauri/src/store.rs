use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection, OptionalExtension, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::models::{content_key_for, may_transition, DownloadTask, NewTask, StoredDownloadFile, TaskStatus};

const TASK_COLUMNS: &str = "id, source, name, kind, status, progress, error, aria2_gid, directory, speed, size, content_key, content_files";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("database mutex poisoned")]
    Poisoned,
    #[error("file manifest serialization failed: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("task {0} was not found")]
    NotFound(Uuid),
    #[error("invalid task status stored in database")]
    InvalidStoredStatus,
    #[error("cannot transition task from {from:?} to {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },
    #[error("active task {0} cannot be removed from the list")]
    CannotRemoveActive(Uuid),
}

pub struct TaskStore {
    connection: Mutex<Connection>,
}

impl TaskStore {
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open(Connection::open_in_memory()?)
    }

    pub fn open_path(path: &std::path::Path) -> Result<Self, StoreError> {
        Self::open(Connection::open(path)?)
    }

    fn open(connection: Connection) -> Result<Self, StoreError> {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY NOT NULL,
                source TEXT NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                progress INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                aria2_gid TEXT,
                directory TEXT,
                speed TEXT,
                size TEXT,
                content_key TEXT NOT NULL DEFAULT '',
                content_files TEXT NOT NULL DEFAULT '[]'
            );",
        )?;
        for migration in [
            "ALTER TABLE tasks ADD COLUMN aria2_gid TEXT",
            "ALTER TABLE tasks ADD COLUMN directory TEXT",
            "ALTER TABLE tasks ADD COLUMN speed TEXT",
            "ALTER TABLE tasks ADD COLUMN size TEXT",
            "ALTER TABLE tasks ADD COLUMN content_key TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE tasks ADD COLUMN content_files TEXT NOT NULL DEFAULT '[]'",
        ] {
            let _ = connection.execute(migration, []);
        }
        let missing_keys = {
            let mut statement = connection.prepare(
                "SELECT id, source FROM tasks WHERE content_key IS NULL OR content_key = ''",
            )?;
            let keys = statement
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            keys
        };
        for (id, source) in missing_keys {
            connection.execute(
                "UPDATE tasks SET content_key = ?1 WHERE id = ?2",
                params![content_key_for(&source), id],
            )?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn create(&self, new_task: NewTask) -> Result<DownloadTask, StoreError> {
        let content_key = content_key_for(&new_task.source);
        let task = DownloadTask {
            id: Uuid::new_v4(),
            source: new_task.source,
            name: new_task.name,
            kind: new_task.kind,
            status: TaskStatus::Parsing,
            progress: 0,
            speed: None,
            speed_bytes: 0,
            size: None,
            eta: None,
            connections: None,
            status_message: None,
            children: Vec::new(),
            error: None,
            aria2_gid: None,
            directory: None,
            content_key,
            content_files: Vec::new(),
        };
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("INSERT INTO tasks (id, source, name, kind, status, progress, error, content_key, content_files) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![task.id.to_string(), task.source, task.name, task.kind, task.status.as_db(), task.progress, task.error, task.content_key, "[]"])?;
        Ok(task)
    }

    pub fn transition(&self, id: Uuid, next: TaskStatus) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let current = connection.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id.to_string()], task_from_row)
            .map_err(|error| if matches!(error, rusqlite::Error::QueryReturnedNoRows) { StoreError::NotFound(id) } else { StoreError::Database(error) })?;
        if !may_transition(current.status, next) {
            return Err(StoreError::InvalidTransition {
                from: current.status,
                to: next,
            });
        }
        let progress = if next == TaskStatus::Completed {
            100
        } else {
            current.progress
        };
        connection.execute(
            "UPDATE tasks SET status = ?1, progress = ?2 WHERE id = ?3",
            params![next.as_db(), progress, id.to_string()],
        )?;
        Ok(DownloadTask {
            status: next,
            progress,
            ..current
        })
    }

    pub fn list(&self) -> Result<Vec<DownloadTask>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY rowid DESC"))?;
        let rows = statement.query_map([], task_from_row)?;
        let tasks = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn find(&self, id: Uuid) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id.to_string()], task_from_row)
            .map_err(|error| if matches!(error, rusqlite::Error::QueryReturnedNoRows) { StoreError::NotFound(id) } else { StoreError::Database(error) })
    }

    pub fn update_progress(&self, id: Uuid, progress: u8) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE tasks SET progress = ?1 WHERE id = ?2 AND status = ?3",
            params![
                progress.min(99),
                id.to_string(),
                TaskStatus::Downloading.as_db()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        connection.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id.to_string()], task_from_row)
            .map_err(StoreError::Database)
    }

    pub fn start_download(
        &self,
        id: Uuid,
        gid: &str,
        directory: &Path,
    ) -> Result<DownloadTask, StoreError> {
        let current = self.find(id)?;
        if current.status == TaskStatus::Queued {
            self.transition(id, TaskStatus::Downloading)?;
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "UPDATE tasks SET aria2_gid = ?1, directory = ?2 WHERE id = ?3",
            params![gid, directory.to_string_lossy(), id.to_string()],
        )?;
        drop(connection);
        self.find(id)
    }

    pub fn replace_gid(&self, id: Uuid, gid: &str) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "UPDATE tasks SET aria2_gid = ?1 WHERE id = ?2",
            params![gid, id.to_string()],
        )?;
        drop(connection);
        self.find(id)
    }

    pub fn prepare_retry(&self, id: Uuid) -> Result<DownloadTask, StoreError> {
        self.transition(id, TaskStatus::Queued)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("UPDATE tasks SET progress = 0, error = NULL, aria2_gid = NULL, speed = NULL, size = NULL WHERE id = ?1", [id.to_string()])?;
        drop(connection);
        self.find(id)
    }

    pub fn restart_download(&self, id: Uuid) -> Result<DownloadTask, StoreError> {
        let current = self.find(id)?;
        if current.status != TaskStatus::Completed {
            return Err(StoreError::InvalidTransition {
                from: current.status,
                to: TaskStatus::Queued,
            });
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "UPDATE tasks SET status = ?1, progress = 0, error = NULL, aria2_gid = NULL, speed = NULL, size = NULL, content_files = '[]' WHERE id = ?2",
            params![TaskStatus::Queued.as_db(), id.to_string()],
        )?;
        drop(connection);
        self.find(id)
    }

    pub fn find_by_content_key(&self, content_key: &str) -> Result<Option<DownloadTask>, StoreError> {
        if content_key.trim().is_empty() {
            return Ok(None);
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE content_key = ?1 ORDER BY rowid DESC LIMIT 1"),
                [content_key],
                task_from_row,
            )
            .optional()
            .map_err(StoreError::Database)
    }

    pub fn store_content_files(
        &self,
        id: Uuid,
        files: &[StoredDownloadFile],
    ) -> Result<(), StoreError> {
        let files = serde_json::to_string(files)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE tasks SET content_files = ?1 WHERE id = ?2",
            params![files, id.to_string()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        Ok(())
    }

    pub fn remove_record(&self, id: Uuid) -> Result<(), StoreError> {
        let current = self.find(id)?;
        if !matches!(
            current.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            return Err(StoreError::CannotRemoveActive(id));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("DELETE FROM tasks WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    pub fn sync_transfer(
        &self,
        id: Uuid,
        status: TaskStatus,
        progress: u8,
        speed: Option<String>,
        size: Option<String>,
        error: Option<String>,
    ) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("UPDATE tasks SET status = ?1, progress = ?2, speed = ?3, size = ?4, error = ?5 WHERE id = ?6",
            params![status.as_db(), progress, speed, size, error, id.to_string()])?;
        drop(connection);
        self.find(id)
    }

    pub fn fail(&self, id: Uuid, message: impl Into<String>) -> Result<DownloadTask, StoreError> {
        let mut task = self.transition(id, TaskStatus::Failed)?;
        let message = message.into();
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "UPDATE tasks SET error = ?1 WHERE id = ?2",
            params![message, id.to_string()],
        )?;
        task.error = Some(message);
        Ok(task)
    }
}

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<DownloadTask> {
    let id: String = row.get(0)?;
    let status: String = row.get(4)?;
    Ok(DownloadTask {
        id: Uuid::parse_str(&id).map_err(|_| rusqlite::Error::InvalidQuery)?,
        source: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        status: TaskStatus::from_db(&status).ok_or(rusqlite::Error::InvalidQuery)?,
        progress: row.get(5)?,
        error: row.get(6)?,
        aria2_gid: row.get(7)?,
        directory: row.get(8)?,
        speed: row.get(9)?,
        speed_bytes: 0,
        size: row.get(10)?,
        eta: None,
        connections: None,
        status_message: None,
        children: Vec::new(),
        content_key: row.get(11)?,
        content_files: serde_json::from_str(&row.get::<_, String>(12)?).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::TaskStore;
    use crate::{NewTask, TaskStatus};

    #[test]
    fn finds_the_newest_task_by_stable_content_key_and_retains_its_file_manifest() {
        let store = TaskStore::open_in_memory().unwrap();
        let source = "magnet:?dn=fixture&xt=urn:btih:0123456789012345678901234567890123456789&tr=udp%3A%2F%2Ffirst";
        let first = store.create(NewTask::direct(source)).unwrap();
        let newest = store
            .create(NewTask::direct(
                "magnet:?tr=udp%3A%2F%2Fsecond&xt=urn:btih:0123456789012345678901234567890123456789&dn=renamed",
            ))
            .unwrap();
        let files = vec![crate::StoredDownloadFile {
            path: r"C:\Downloads\fixture.mkv".into(),
            length: 1_572_864,
        }];

        store.store_content_files(newest.id, &files).unwrap();

        let duplicate = store.find_by_content_key(&first.content_key).unwrap().unwrap();
        assert_eq!(duplicate.id, newest.id);
        assert_eq!(duplicate.content_files, files);
    }

    #[test]
    fn stores_live_download_progress() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/game.zip"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();

        let updated = store.update_progress(task.id, 42).unwrap();

        assert_eq!(updated.progress, 42);
        assert_eq!(store.find(task.id).unwrap().progress, 42);
    }

    #[test]
    fn cancelled_task_can_be_reset_for_a_real_retry() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/game.zip"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let task = store.transition(task.id, TaskStatus::Cancelled).unwrap();

        let retried = store.prepare_retry(task.id).unwrap();

        assert_eq!(retried.status, TaskStatus::Queued);
        assert_eq!(retried.progress, 0);
        assert!(retried.error.is_none());
        assert!(retried.aria2_gid.is_none());
    }

    #[test]
    fn completed_task_can_be_reset_only_by_explicit_restart() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/game.zip"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let completed = store.transition(task.id, TaskStatus::Processing).unwrap();
        let completed = store.transition(completed.id, TaskStatus::Completed).unwrap();

        assert!(store.prepare_retry(completed.id).is_err());
        let restarted = store.restart_download(completed.id).unwrap();

        assert_eq!(restarted.status, TaskStatus::Queued);
        assert_eq!(restarted.progress, 0);
        assert!(restarted.aria2_gid.is_none());
    }

    #[test]
    fn terminal_task_record_can_be_removed_without_touching_its_files() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/game.zip"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let task = store.transition(task.id, TaskStatus::Cancelled).unwrap();

        store.remove_record(task.id).unwrap();

        assert!(matches!(
            store.find(task.id),
            Err(super::StoreError::NotFound(_))
        ));
    }
}
