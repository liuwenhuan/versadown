use std::{
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::models::{may_transition, DownloadTask, NewTask, TaskStatus};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("database mutex poisoned")]
    Poisoned,
    #[error("task {0} was not found")]
    NotFound(Uuid),
    #[error("invalid task status stored in database")]
    InvalidStoredStatus,
    #[error("cannot transition task from {from:?} to {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },
    #[error("value for {field} is outside SQLite's signed 64-bit range")]
    IntegerOutOfRange { field: &'static str },
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

    fn open(mut connection: Connection) -> Result<Self, StoreError> {
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY NOT NULL,
                source TEXT NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                progress INTEGER NOT NULL DEFAULT 0,
                error TEXT
            );",
        )?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn create(&self, new_task: NewTask) -> Result<DownloadTask, StoreError> {
        let task = DownloadTask {
            id: Uuid::new_v4(),
            source: new_task.source,
            name: new_task.name,
            kind: new_task.kind,
            status: TaskStatus::Parsing,
            progress: 0,
            error: None,
            aria_gid: None,
            followed_by_gid: None,
            total_length: None,
            completed_length: 0,
            download_speed: 0,
            upload_speed: 0,
            connections: 0,
            eta_seconds: None,
            aria_error_code: None,
            updated_at: 0,
        };
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("INSERT INTO tasks (id, source, name, kind, status, progress, error, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![task.id.to_string(), task.source, task.name, task.kind, task.status.as_db(), task.progress, task.error, task.updated_at])?;
        Ok(task)
    }

    pub fn transition(&self, id: Uuid, next: TaskStatus) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let current = find_with_connection(&connection, id)?;
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
        let mut statement = connection.prepare(&format!("{TASK_SELECT} ORDER BY rowid DESC"))?;
        let rows = statement.query_map([], task_from_row)?;
        let tasks = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn find(&self, id: Uuid) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        find_with_connection(&connection, id)
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

    pub fn bind_gid(&self, id: Uuid, gid: impl AsRef<str>) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        update_gid(&connection, id, gid.as_ref(), None)?;
        find_with_connection(&connection, id)
    }

    /// Atomically claims an aria2 GID that could not be cleaned up after submission.
    /// This keeps recovery from importing the same live session entry as a second task.
    pub fn bind_gid_and_fail(
        &self,
        id: Uuid,
        gid: impl AsRef<str>,
        message: impl AsRef<str>,
    ) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE tasks SET aria_gid=?1,followed_by_gid=NULL,status='failed',error=?2,
             download_speed=0,upload_speed=0,eta_seconds=NULL,selection_confirmed=0,updated_at=?3
             WHERE id=?4",
            params![
                gid.as_ref(),
                message.as_ref(),
                current_timestamp(),
                id.to_string()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        find_with_connection(&connection, id)
    }

    pub fn bind_metadata_gid(&self, id: Uuid, gid: &str) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed=connection.execute("UPDATE tasks SET aria_gid=?1,status='awaiting_user',selection_confirmed=0,updated_at=?2 WHERE id=?3",params![gid,current_timestamp(),id.to_string()])?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        find_with_connection(&connection, id)
    }

    /// An acknowledged BT confirmation is application intent, independent of the
    /// last successful aria2 transfer snapshot and durable across service restarts.
    pub fn confirm_selection(&self, id: Uuid) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE tasks SET selection_confirmed=1 WHERE id=?1",
            [id.to_string()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        Ok(())
    }

    pub fn selection_confirmed(&self, id: Uuid) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        find_with_connection(&connection, id)?;
        Ok(connection.query_row(
            "SELECT selection_confirmed FROM tasks WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn replace_gid(
        &self,
        id: Uuid,
        expected_gid: impl AsRef<str>,
        replacement_gid: impl AsRef<str>,
    ) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE tasks SET aria_gid = ?1, followed_by_gid = NULL, updated_at = ?2 WHERE id = ?3 AND aria_gid = ?4",
            params![replacement_gid.as_ref(), current_timestamp(), id.to_string(), expected_gid.as_ref()],
        )?;
        if changed == 0 {
            return Err(if find_with_transaction(&transaction, id)?.is_none() {
                StoreError::NotFound(id)
            } else {
                StoreError::Database(rusqlite::Error::QueryReturnedNoRows)
            });
        }
        transaction.commit()?;
        find_with_connection(&connection, id)
    }

    pub fn apply_transfer_snapshot(
        &self,
        snapshot: DownloadTask,
    ) -> Result<DownloadTask, StoreError> {
        self.apply_transfer_snapshot_with_files(snapshot, &[])
    }

    pub fn apply_transfer_snapshot_with_files(
        &self,
        snapshot: DownloadTask,
        files: &[(u32, String, u64, u64, bool)],
    ) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE tasks SET status = ?1, progress = ?2, error = ?3, aria_gid = ?4, followed_by_gid = ?5,
             total_length = ?6, completed_length = ?7, download_speed = ?8, upload_speed = ?9,
             connections = ?10, eta_seconds = ?11, aria_error_code = ?12, updated_at = ?13 WHERE id = ?14",
            params![
                snapshot.status.as_db(), snapshot.progress, snapshot.error, snapshot.aria_gid,
                snapshot.followed_by_gid, snapshot.total_length.map(|value| sqlite_i64("total_length", value)).transpose()?,
                sqlite_i64("completed_length", snapshot.completed_length)?, sqlite_i64("download_speed", snapshot.download_speed)?,
                sqlite_i64("upload_speed", snapshot.upload_speed)?, snapshot.connections,
                snapshot.eta_seconds.map(|value| sqlite_i64("eta_seconds", value)).transpose()?,
                snapshot.aria_error_code, current_timestamp(), snapshot.id.to_string(),
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(snapshot.id));
        }
        for (index, path, length, completed_length, selected) in files {
            transaction.execute(
                "INSERT INTO task_files (task_id,file_index,path,length,completed_length,selected) VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(task_id,file_index) DO UPDATE SET path=excluded.path,length=excluded.length,completed_length=excluded.completed_length,selected=excluded.selected",
                params![snapshot.id.to_string(), index, path, sqlite_i64("length", *length)?, sqlite_i64("completed_length", *completed_length)?, selected],
            )?;
        }
        transaction.commit()?;
        find_with_connection(&connection, snapshot.id)
    }

    pub fn upsert_files(
        &self,
        id: Uuid,
        files: &[(u32, String, u64, u64, bool)],
    ) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        if find_with_transaction(&transaction, id)?.is_none() {
            return Err(StoreError::NotFound(id));
        }
        for (index, path, length, completed_length, selected) in files {
            transaction.execute(
                "INSERT INTO task_files (task_id, file_index, path, length, completed_length, selected)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(task_id, file_index) DO UPDATE SET path = excluded.path, length = excluded.length,
                 completed_length = excluded.completed_length, selected = excluded.selected",
                params![id.to_string(), index, path, sqlite_i64("length", *length)?, sqlite_i64("completed_length", *completed_length)?, selected],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn find_by_gid(&self, gid: impl AsRef<str>) -> Result<Option<DownloadTask>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                &format!("{TASK_SELECT} WHERE aria_gid = ?1"),
                [gid.as_ref()],
                task_from_row,
            )
            .map(Some)
            .or_else(|error| {
                if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(StoreError::Database(error))
                }
            })
    }

    pub fn files(&self, id: Uuid) -> Result<Vec<crate::aria2::TransferFile>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        find_with_connection(&connection, id)?;
        let mut statement = connection.prepare("SELECT file_index,path,length,completed_length,selected FROM task_files WHERE task_id=?1 ORDER BY file_index")?;
        let rows = statement.query_map([id.to_string()], |row| {
            Ok(crate::aria2::TransferFile {
                index: row.get(0)?,
                path: row.get(1)?,
                length: required_u64(row, 2)?,
                completed_length: required_u64(row, 3)?,
                selected: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

const TASK_SELECT: &str = "SELECT id, source, name, kind, status, progress, error, aria_gid, followed_by_gid, total_length, completed_length, download_speed, upload_speed, connections, eta_seconds, aria_error_code, updated_at FROM tasks";

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= 2 {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    for (name, definition) in [
        ("aria_gid", "TEXT"),
        ("followed_by_gid", "TEXT"),
        ("total_length", "INTEGER"),
        ("completed_length", "INTEGER NOT NULL DEFAULT 0"),
        ("download_speed", "INTEGER NOT NULL DEFAULT 0"),
        ("upload_speed", "INTEGER NOT NULL DEFAULT 0"),
        ("connections", "INTEGER NOT NULL DEFAULT 0"),
        ("eta_seconds", "INTEGER"),
        ("aria_error_code", "TEXT"),
        ("updated_at", "INTEGER NOT NULL DEFAULT 0"),
        ("selection_confirmed", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !column_exists(&transaction, name)? {
            transaction.execute_batch(&format!(
                "ALTER TABLE tasks ADD COLUMN {name} {definition};"
            ))?;
        }
    }
    transaction.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS tasks_aria_gid_unique ON tasks(aria_gid) WHERE aria_gid IS NOT NULL;
         CREATE TABLE IF NOT EXISTS task_files (
            task_id TEXT NOT NULL,
            file_index INTEGER NOT NULL,
            path TEXT NOT NULL,
            length INTEGER NOT NULL,
            completed_length INTEGER NOT NULL,
            selected INTEGER NOT NULL,
            PRIMARY KEY(task_id, file_index),
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE
         );
         PRAGMA user_version = 2;",
    )?;
    transaction.commit()?;
    Ok(())
}

fn column_exists(transaction: &Transaction<'_>, name: &str) -> Result<bool, StoreError> {
    let mut statement = transaction.prepare("PRAGMA table_info(tasks)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == name))
}

fn sqlite_i64(field: &'static str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOutOfRange { field })
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn update_gid(
    connection: &Connection,
    id: Uuid,
    gid: &str,
    followed_by_gid: Option<&str>,
) -> Result<(), StoreError> {
    let changed = connection.execute(
        "UPDATE tasks SET aria_gid = ?1, followed_by_gid = ?2, updated_at = ?3 WHERE id = ?4",
        params![gid, followed_by_gid, current_timestamp(), id.to_string()],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound(id));
    }
    Ok(())
}

fn find_with_connection(connection: &Connection, id: Uuid) -> Result<DownloadTask, StoreError> {
    connection
        .query_row(
            &format!("{TASK_SELECT} WHERE id = ?1"),
            [id.to_string()],
            task_from_row,
        )
        .map_err(|error| {
            if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
                StoreError::NotFound(id)
            } else {
                StoreError::Database(error)
            }
        })
}

fn find_with_transaction(
    transaction: &Transaction<'_>,
    id: Uuid,
) -> Result<Option<DownloadTask>, StoreError> {
    transaction
        .query_row(
            &format!("{TASK_SELECT} WHERE id = ?1"),
            [id.to_string()],
            task_from_row,
        )
        .map(Some)
        .or_else(|error| {
            if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(StoreError::Database(error))
            }
        })
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
        aria_gid: row.get(7)?,
        followed_by_gid: row.get(8)?,
        total_length: optional_u64(row, 9)?,
        completed_length: required_u64(row, 10)?,
        download_speed: required_u64(row, 11)?,
        upload_speed: required_u64(row, 12)?,
        connections: row.get(13)?,
        eta_seconds: optional_u64(row, 14)?,
        aria_error_code: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn optional_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| u64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery))
        .transpose()
}

fn required_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(|_| rusqlite::Error::InvalidQuery)
}
