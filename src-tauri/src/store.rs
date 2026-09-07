use std::sync::Mutex;

use rusqlite::{params, Connection, Row};
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
                error TEXT
            );",
        )?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    pub fn create(&self, new_task: NewTask) -> Result<DownloadTask, StoreError> {
        let task = DownloadTask { id: Uuid::new_v4(), source: new_task.source, name: new_task.name, kind: new_task.kind, status: TaskStatus::Parsing, progress: 0, error: None };
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("INSERT INTO tasks (id, source, name, kind, status, progress, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![task.id.to_string(), task.source, task.name, task.kind, task.status.as_db(), task.progress, task.error])?;
        Ok(task)
    }

    pub fn transition(&self, id: Uuid, next: TaskStatus) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let current = connection.query_row("SELECT id, source, name, kind, status, progress, error FROM tasks WHERE id = ?1", [id.to_string()], task_from_row)
            .map_err(|error| if matches!(error, rusqlite::Error::QueryReturnedNoRows) { StoreError::NotFound(id) } else { StoreError::Database(error) })?;
        if !may_transition(current.status, next) { return Err(StoreError::InvalidTransition { from: current.status, to: next }); }
        let progress = if next == TaskStatus::Completed { 100 } else { current.progress };
        connection.execute("UPDATE tasks SET status = ?1, progress = ?2 WHERE id = ?3", params![next.as_db(), progress, id.to_string()])?;
        Ok(DownloadTask { status: next, progress, ..current })
    }

    pub fn list(&self) -> Result<Vec<DownloadTask>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare("SELECT id, source, name, kind, status, progress, error FROM tasks ORDER BY rowid DESC")?;
        let rows = statement.query_map([], task_from_row)?;
        let tasks = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn find(&self, id: Uuid) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.query_row("SELECT id, source, name, kind, status, progress, error FROM tasks WHERE id = ?1", [id.to_string()], task_from_row)
            .map_err(|error| if matches!(error, rusqlite::Error::QueryReturnedNoRows) { StoreError::NotFound(id) } else { StoreError::Database(error) })
    }

    pub fn fail(&self, id: Uuid, message: impl Into<String>) -> Result<DownloadTask, StoreError> {
        let mut task = self.transition(id, TaskStatus::Failed)?;
        let message = message.into();
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("UPDATE tasks SET error = ?1 WHERE id = ?2", params![message, id.to_string()])?;
        task.error = Some(message);
        Ok(task)
    }
}

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<DownloadTask> {
    let id: String = row.get(0)?;
    let status: String = row.get(4)?;
    Ok(DownloadTask {
        id: Uuid::parse_str(&id).map_err(|_| rusqlite::Error::InvalidQuery)?, source: row.get(1)?, name: row.get(2)?, kind: row.get(3)?,
        status: TaskStatus::from_db(&status).ok_or(rusqlite::Error::InvalidQuery)?, progress: row.get(5)?, error: row.get(6)?,
    })
}
