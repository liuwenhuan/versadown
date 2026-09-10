use std::{
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    cloud::CloudProvider,
    models::{
        may_transition, CloudDirectoryGroup, CloudDirectoryGroupItem, DownloadTask,
        NewCloudDirectoryGroup, NewCloudDirectoryGroupItem, NewTask, TaskStatus,
    },
};

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
    #[error("invalid cloud directory group data: {0}")]
    InvalidCloudDirectoryGroupData(&'static str),
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
        let created_at = current_timestamp();
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
            metadata_gid: None,
            total_length: None,
            completed_length: 0,
            download_speed: 0,
            upload_speed: 0,
            connections: 0,
            eta_seconds: None,
            aria_error_code: None,
            created_at,
            updated_at: created_at,
            completed_at: None,
        };
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute("INSERT INTO tasks (id, source, name, kind, status, progress, error, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![task.id.to_string(), task.source, task.name, task.kind, task.status.as_db(), task.progress, task.error, task.created_at, task.updated_at])?;
        Ok(task)
    }

    pub fn create_cloud_directory_group(
        &self,
        new_group: NewCloudDirectoryGroup,
        new_items: &[NewCloudDirectoryGroupItem],
    ) -> Result<CloudDirectoryGroup, StoreError> {
        validate_new_cloud_directory_group(&new_group)?;
        for item in new_items {
            validate_new_cloud_directory_group_item(item)?;
        }

        let timestamp = current_timestamp();
        let group = CloudDirectoryGroup {
            id: Uuid::new_v4(),
            provider: new_group.provider,
            remote_directory_id: new_group.remote_directory_id,
            relative_directory: new_group.relative_directory,
            output_directory: new_group.output_directory,
            created_at: timestamp,
            updated_at: timestamp,
        };
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO cloud_directory_groups (id, provider, remote_directory_id, relative_directory, output_directory, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                group.id.to_string(),
                group.provider.key(),
                group.remote_directory_id,
                group.relative_directory,
                group.output_directory.to_string_lossy(),
                group.created_at,
                group.updated_at,
            ],
        )?;
        for (item_index, new_item) in new_items.iter().enumerate() {
            let item_index = u32::try_from(item_index)
                .map_err(|_| StoreError::InvalidCloudDirectoryGroupData("too many group items"))?;
            transaction.execute(
                "INSERT INTO cloud_directory_group_items (id, group_id, item_index, remote_file_id, relative_directory, output_name, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    group.id.to_string(),
                    item_index,
                    new_item.remote_file_id,
                    serde_json::to_string(&new_item.relative_directory)
                        .map_err(|_| StoreError::InvalidCloudDirectoryGroupData("relative directory"))?,
                    new_item.output_name,
                    new_item.size.map(|value| sqlite_i64("size", value)).transpose()?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(group)
    }

    pub fn cloud_directory_group(
        &self,
        id: Uuid,
    ) -> Result<Option<CloudDirectoryGroup>, StoreError> {
        validate_uuid(id)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT id, provider, remote_directory_id, relative_directory, output_directory, created_at, updated_at
                 FROM cloud_directory_groups WHERE id = ?1",
                [id.to_string()],
                cloud_directory_group_from_row,
            )
            .map(Some)
            .or_else(no_rows_to_none)
    }

    pub fn cloud_directory_groups(&self) -> Result<Vec<CloudDirectoryGroup>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id, provider, remote_directory_id, relative_directory, output_directory, created_at, updated_at
             FROM cloud_directory_groups ORDER BY created_at, id",
        )?;
        let groups = statement
            .query_map([], cloud_directory_group_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(groups)
    }

    pub fn cloud_directory_group_items(
        &self,
        group_id: Uuid,
    ) -> Result<Vec<CloudDirectoryGroupItem>, StoreError> {
        validate_uuid(group_id)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id, group_id, item_index, remote_file_id, relative_directory, output_name, size, task_id, failure_code, failure_message
             FROM cloud_directory_group_items WHERE group_id = ?1 ORDER BY item_index",
        )?;
        let items = statement
            .query_map([group_id.to_string()], cloud_directory_group_item_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn link_cloud_directory_group_item_task(
        &self,
        group_id: Uuid,
        item_id: Uuid,
        task_id: Uuid,
    ) -> Result<(), StoreError> {
        validate_uuid(group_id)?;
        validate_uuid(item_id)?;
        validate_uuid(task_id)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE cloud_directory_group_items
             SET task_id = ?1, failure_code = NULL, failure_message = NULL
             WHERE id = ?2 AND group_id = ?3",
            params![
                task_id.to_string(),
                item_id.to_string(),
                group_id.to_string()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(item_id));
        }
        transaction.execute(
            "UPDATE cloud_directory_groups SET updated_at = ?1 WHERE id = ?2",
            params![current_timestamp(), group_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_cloud_directory_group_item(
        &self,
        group_id: Uuid,
        item_id: Uuid,
        failure_code: impl AsRef<str>,
        failure_message: impl AsRef<str>,
    ) -> Result<(), StoreError> {
        validate_uuid(group_id)?;
        validate_uuid(item_id)?;
        validate_plain_text(failure_code.as_ref(), "failure code")?;
        validate_plain_text(failure_message.as_ref(), "failure message")?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE cloud_directory_group_items
             SET task_id = NULL, failure_code = ?1, failure_message = ?2
             WHERE id = ?3 AND group_id = ?4",
            params![
                failure_code.as_ref(),
                failure_message.as_ref(),
                item_id.to_string(),
                group_id.to_string()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(item_id));
        }
        transaction.execute(
            "UPDATE cloud_directory_groups SET updated_at = ?1 WHERE id = ?2",
            params![current_timestamp(), group_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn cloud_directory_group_for_task(
        &self,
        task_id: Uuid,
    ) -> Result<Option<CloudDirectoryGroup>, StoreError> {
        validate_uuid(task_id)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT groups.id, groups.provider, groups.remote_directory_id, groups.relative_directory,
                        groups.output_directory, groups.created_at, groups.updated_at
                 FROM cloud_directory_groups AS groups
                 INNER JOIN cloud_directory_group_items AS items ON items.group_id = groups.id
                 WHERE items.task_id = ?1",
                [task_id.to_string()],
                cloud_directory_group_from_row,
            )
            .map(Some)
            .or_else(no_rows_to_none)
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
        let completed_at = if next == TaskStatus::Completed {
            current.completed_at.or(Some(current_timestamp()))
        } else {
            current.completed_at
        };
        connection.execute(
            "UPDATE tasks SET status = ?1, progress = ?2, completed_at = ?3 WHERE id = ?4",
            params![next.as_db(), progress, completed_at, id.to_string()],
        )?;
        Ok(DownloadTask {
            status: next,
            progress,
            completed_at,
            ..current
        })
    }

    pub fn list(&self) -> Result<Vec<DownloadTask>, StoreError> {
        self.list_matching("")
    }

    pub fn list_active(&self) -> Result<Vec<DownloadTask>, StoreError> {
        self.list_matching("WHERE status NOT IN ('completed', 'failed', 'cancelled', 'recycled')")
    }

    pub fn list_terminal(&self) -> Result<Vec<DownloadTask>, StoreError> {
        self.list_matching("WHERE status IN ('completed', 'failed', 'cancelled', 'recycled')")
    }

    fn list_matching(&self, filter: &str) -> Result<Vec<DownloadTask>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement =
            connection.prepare(&format!("{TASK_SELECT} {filter} ORDER BY rowid DESC"))?;
        let rows = statement.query_map([], task_from_row)?;
        let tasks = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn find(&self, id: Uuid) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        find_with_connection(&connection, id)
    }

    /// Finds the most recently created task for an identical stable source.
    /// Cloud tasks persist only `cloud://provider/file-id`, so this never
    /// compares an expiring signed download URL or a session credential.
    pub fn find_by_source(&self, source: &str) -> Result<Option<DownloadTask>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                &format!("{TASK_SELECT} WHERE source = ?1 ORDER BY rowid DESC LIMIT 1"),
                [source],
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

    /// Removes only the durable task record. `task_files` rows cascade, while
    /// the actual downloaded payload is never touched by this operation.
    pub fn remove_task_record(&self, id: Uuid) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute("DELETE FROM tasks WHERE id = ?1", [id.to_string()])?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        Ok(())
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

    /// Cleanup may finish after a user action. Update only its safe message,
    /// never restore an old status or transfer snapshot over the current task.
    pub(crate) fn set_completed_cleanup_message(
        &self,
        id: Uuid,
        pending: bool,
    ) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let message =
            pending.then_some("cleanup_pending：下载已完成，网盘转存文件清理未完成，可重试清理");
        connection.execute(
            "UPDATE tasks SET error = ?1 WHERE id = ?2 AND status = 'completed' AND (?1 IS NOT NULL OR substr(error, 1, length('cleanup_pending：')) = 'cleanup_pending：')",
            params![message, id.to_string()],
        )?;
        find_with_connection(&connection, id)
    }

    pub fn bind_gid(&self, id: Uuid, gid: impl AsRef<str>) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        update_gid(&connection, id, gid.as_ref(), None)?;
        find_with_connection(&connection, id)
    }

    /// Records an aria2 GID accepted for an explicit retry without creating a
    /// second task. Output-file rows intentionally remain in place so a later
    /// retry can still decide whether it may resume or needs overwrite consent.
    pub fn rebind_for_resubmission(
        &self,
        id: Uuid,
        gid: impl AsRef<str>,
    ) -> Result<DownloadTask, StoreError> {
        let gid = gid.as_ref();
        if gid.is_empty() {
            return Err(StoreError::Database(rusqlite::Error::InvalidParameterName(
                "empty aria2 gid".into(),
            )));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let current = find_with_connection(&connection, id)?;
        if !matches!(
            current.status,
            TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Completed
        ) {
            return Err(StoreError::InvalidTransition {
                from: current.status,
                to: TaskStatus::Parsing,
            });
        }
        connection.execute(
            "UPDATE tasks SET status='parsing',progress=0,error=NULL,aria_gid=?1,followed_by_gid=NULL,
             metadata_gid=NULL,total_length=NULL,completed_length=0,download_speed=0,upload_speed=0,
             connections=0,eta_seconds=NULL,aria_error_code=NULL,selection_confirmed=0,completed_at=NULL,updated_at=?2
             WHERE id=?3",
            params![gid, current_timestamp(), id.to_string()],
        )?;
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
            "UPDATE tasks SET aria_gid=?1,followed_by_gid=NULL,metadata_gid=NULL,status='failed',error=?2,
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
        let changed=connection.execute("UPDATE tasks SET aria_gid=?1,metadata_gid=?1,status='awaiting_user',selection_confirmed=0,updated_at=?2 WHERE id=?3",params![gid,current_timestamp(),id.to_string()])?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        find_with_connection(&connection, id)
    }

    pub fn bind_metadata_alias(&self, id: Uuid, gid: &str) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE tasks SET metadata_gid=?1,updated_at=?2 WHERE id=?3 AND metadata_gid IS NULL",
            params![gid, current_timestamp(), id.to_string()],
        )?;
        if changed == 0 {
            return match find_with_connection(&connection, id) {
                Ok(_) => Err(StoreError::Database(rusqlite::Error::QueryReturnedNoRows)),
                Err(error) => Err(error),
            };
        }
        find_with_connection(&connection, id)
    }

    pub fn bind_awaiting_gid(&self, id: Uuid, gid: &str) -> Result<DownloadTask, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed=connection.execute("UPDATE tasks SET aria_gid=?1,metadata_gid=NULL,status='awaiting_user',selection_confirmed=0,updated_at=?2 WHERE id=?3",params![gid,current_timestamp(),id.to_string()])?;
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
        let current = find_with_transaction(&transaction, snapshot.id)?
            .ok_or(StoreError::NotFound(snapshot.id))?;
        let completed_at = if snapshot.status == TaskStatus::Completed {
            current.completed_at.or(Some(current_timestamp()))
        } else {
            current.completed_at
        };
        let changed = transaction.execute(
            "UPDATE tasks SET name = ?1, status = ?2, progress = ?3, error = ?4, aria_gid = ?5, followed_by_gid = ?6,
             total_length = ?7, completed_length = ?8, download_speed = ?9, upload_speed = ?10,
             connections = ?11, eta_seconds = ?12, aria_error_code = ?13, updated_at = ?14, completed_at = ?15 WHERE id = ?16",
            params![
                snapshot.name, snapshot.status.as_db(), snapshot.progress, snapshot.error, snapshot.aria_gid,
                snapshot.followed_by_gid, snapshot.total_length.map(|value| sqlite_i64("total_length", value)).transpose()?,
                sqlite_i64("completed_length", snapshot.completed_length)?, sqlite_i64("download_speed", snapshot.download_speed)?,
                sqlite_i64("upload_speed", snapshot.upload_speed)?, snapshot.connections,
                snapshot.eta_seconds.map(|value| sqlite_i64("eta_seconds", value)).transpose()?,
                snapshot.aria_error_code, current_timestamp(), completed_at, snapshot.id.to_string(),
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
                &format!("{TASK_SELECT} WHERE aria_gid = ?1 OR metadata_gid = ?1 LIMIT 1"),
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

const TASK_SELECT: &str = "SELECT id, source, name, kind, status, progress, error, aria_gid, followed_by_gid, total_length, completed_length, download_speed, upload_speed, connections, eta_seconds, aria_error_code, updated_at, completed_at, metadata_gid, created_at FROM tasks";

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= 6 {
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
        ("completed_at", "INTEGER"),
        ("selection_confirmed", "INTEGER NOT NULL DEFAULT 0"),
        ("metadata_gid", "TEXT"),
        ("created_at", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !column_exists(&transaction, name)? {
            transaction.execute_batch(&format!(
                "ALTER TABLE tasks ADD COLUMN {name} {definition};"
            ))?;
        }
    }
    transaction.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS tasks_aria_gid_unique ON tasks(aria_gid) WHERE aria_gid IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS tasks_metadata_gid_unique ON tasks(metadata_gid) WHERE metadata_gid IS NOT NULL;
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
         CREATE TABLE IF NOT EXISTS cloud_directory_groups (
            id TEXT PRIMARY KEY NOT NULL,
            provider TEXT NOT NULL,
            remote_directory_id TEXT NOT NULL,
            relative_directory TEXT NOT NULL,
            output_directory TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS cloud_directory_group_items (
            id TEXT PRIMARY KEY NOT NULL,
            group_id TEXT NOT NULL,
            item_index INTEGER NOT NULL,
            remote_file_id TEXT NOT NULL,
            relative_directory TEXT NOT NULL,
            output_name TEXT NOT NULL,
            size INTEGER,
            task_id TEXT,
            failure_code TEXT,
            failure_message TEXT,
            FOREIGN KEY(group_id) REFERENCES cloud_directory_groups(id) ON DELETE CASCADE,
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE SET NULL,
            UNIQUE(group_id, item_index)
         );
         CREATE INDEX IF NOT EXISTS cloud_directory_group_items_task_id ON cloud_directory_group_items(task_id) WHERE task_id IS NOT NULL;
         UPDATE tasks SET created_at = updated_at WHERE created_at = 0;
         UPDATE tasks SET completed_at = updated_at WHERE status = 'completed' AND completed_at IS NULL AND updated_at > 0;
         PRAGMA user_version = 6;",
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
        "UPDATE tasks SET aria_gid = ?1, followed_by_gid = ?2, metadata_gid = NULL, updated_at = ?3 WHERE id = ?4",
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
        completed_at: row.get(17)?,
        metadata_gid: row.get(18)?,
        created_at: row.get(19)?,
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

fn cloud_directory_group_from_row(row: &Row<'_>) -> rusqlite::Result<CloudDirectoryGroup> {
    let id: String = row.get(0)?;
    let provider: String = row.get(1)?;
    Ok(CloudDirectoryGroup {
        id: parse_cloud_directory_uuid(&id)?,
        provider: cloud_provider_from_db(&provider)?,
        remote_directory_id: row.get(2)?,
        relative_directory: row.get(3)?,
        output_directory: std::path::PathBuf::from(row.get::<_, String>(4)?),
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn cloud_directory_group_item_from_row(row: &Row<'_>) -> rusqlite::Result<CloudDirectoryGroupItem> {
    let id: String = row.get(0)?;
    let group_id: String = row.get(1)?;
    let relative_directory: String = row.get(4)?;
    let task_id: Option<String> = row.get(7)?;
    Ok(CloudDirectoryGroupItem {
        id: parse_cloud_directory_uuid(&id)?,
        group_id: parse_cloud_directory_uuid(&group_id)?,
        item_index: row.get(2)?,
        remote_file_id: row.get(3)?,
        relative_directory: serde_json::from_str(&relative_directory)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        output_name: row.get(5)?,
        size: optional_u64(row, 6)?,
        task_id: task_id
            .as_deref()
            .map(parse_cloud_directory_uuid)
            .transpose()?,
        failure_code: row.get(8)?,
        failure_message: row.get(9)?,
    })
}

fn parse_cloud_directory_uuid(value: &str) -> rusqlite::Result<Uuid> {
    let id = Uuid::parse_str(value).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if id.is_nil() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(id)
}

fn cloud_provider_from_db(value: &str) -> rusqlite::Result<CloudProvider> {
    match value {
        "quark" => Ok(CloudProvider::Quark),
        "pikpak" => Ok(CloudProvider::Pikpak),
        "baidu" => Ok(CloudProvider::Baidu),
        "tianyi" => Ok(CloudProvider::Tianyi),
        "aliyun" => Ok(CloudProvider::Aliyun),
        "xunlei" => Ok(CloudProvider::Xunlei),
        "uc" => Ok(CloudProvider::Uc),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn no_rows_to_none<T>(error: rusqlite::Error) -> Result<Option<T>, StoreError> {
    if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
        Ok(None)
    } else {
        Err(StoreError::Database(error))
    }
}

fn validate_uuid(id: Uuid) -> Result<(), StoreError> {
    if id.is_nil() {
        return Err(StoreError::InvalidCloudDirectoryGroupData("nil UUID"));
    }
    Ok(())
}

fn validate_new_cloud_directory_group(group: &NewCloudDirectoryGroup) -> Result<(), StoreError> {
    validate_plain_text(&group.remote_directory_id, "remote directory ID")?;
    validate_path_component(&group.relative_directory)?;
    if group.output_directory.as_os_str().is_empty() {
        return Err(StoreError::InvalidCloudDirectoryGroupData(
            "output directory",
        ));
    }
    Ok(())
}

fn validate_new_cloud_directory_group_item(
    item: &NewCloudDirectoryGroupItem,
) -> Result<(), StoreError> {
    validate_plain_text(&item.remote_file_id, "remote file ID")?;
    validate_path_component(&item.output_name)?;
    for component in &item.relative_directory {
        validate_path_component(component)?;
    }
    Ok(())
}

fn validate_plain_text(value: &str, field: &'static str) -> Result<(), StoreError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(StoreError::InvalidCloudDirectoryGroupData(field));
    }
    Ok(())
}

fn validate_path_component(component: &str) -> Result<(), StoreError> {
    validate_plain_text(component, "path component")?;
    if matches!(component, "." | "..") || component.contains(['/', '\\']) {
        return Err(StoreError::InvalidCloudDirectoryGroupData("path component"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cloud::CloudProvider,
        models::{NewCloudDirectoryGroup, NewCloudDirectoryGroupItem},
    };

    #[test]
    fn cloud_directory_group_persists_items_across_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tasks.sqlite");
        let base = directory.path().join("downloads");

        let (group_id, item_id) = {
            let store = TaskStore::open_path(&database).unwrap();
            let group = store
                .create_cloud_directory_group(
                    NewCloudDirectoryGroup::new(CloudProvider::Quark, "root-id", "Season 1", &base),
                    &[NewCloudDirectoryGroupItem::new(
                        "remote-1",
                        &["Season 1", "Extras"],
                        "behind.mkv",
                        None,
                    )],
                )
                .unwrap();
            let item = store
                .cloud_directory_group_items(group.id)
                .unwrap()
                .remove(0);
            assert_eq!(item.relative_directory, vec!["Season 1", "Extras"]);
            (group.id, item.id)
        };

        let store = TaskStore::open_path(&database).unwrap();
        let group = store.cloud_directory_group(group_id).unwrap().unwrap();
        let item = store
            .cloud_directory_group_items(group_id)
            .unwrap()
            .remove(0);
        assert_eq!(group.id, group_id);
        assert_eq!(group.provider, CloudProvider::Quark);
        assert_eq!(group.remote_directory_id, "root-id");
        assert_eq!(group.relative_directory, "Season 1");
        assert_eq!(group.output_directory, base);
        assert_eq!(item.id, item_id);
        assert_eq!(item.remote_file_id, "remote-1");
        assert_eq!(item.relative_directory, vec!["Season 1", "Extras"]);
        assert_eq!(item.output_name, "behind.mkv");
    }

    #[test]
    fn cloud_directory_group_returns_only_linked_tasks_and_keeps_failed_item_virtual() {
        let store = TaskStore::open_in_memory().unwrap();
        let base = std::path::PathBuf::from("C:/downloads");
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(CloudProvider::Quark, "root-id", "Season 1", &base),
                &[
                    NewCloudDirectoryGroupItem::new(
                        "remote-episode",
                        &["Season 1"],
                        "episode.mkv",
                        None,
                    ),
                    NewCloudDirectoryGroupItem::new(
                        "remote-failed",
                        &["Season 1", "Extras"],
                        "behind.mkv",
                        None,
                    ),
                ],
            )
            .unwrap();
        let items = store.cloud_directory_group_items(group.id).unwrap();
        let task = store
            .create(NewTask::direct("https://example.test/episode.mkv"))
            .unwrap();

        store
            .link_cloud_directory_group_item_task(group.id, items[0].id, task.id)
            .unwrap();
        store
            .fail_cloud_directory_group_item(
                group.id,
                items[1].id,
                "provider_error",
                "link generation failed",
            )
            .unwrap();

        assert_eq!(
            store.cloud_directory_group_for_task(task.id).unwrap(),
            Some(group.clone())
        );
        assert_eq!(
            store.cloud_directory_group_items(group.id).unwrap()[1].task_id,
            None
        );
        assert_eq!(
            store.cloud_directory_group_items(group.id).unwrap()[1]
                .failure_code
                .as_deref(),
            Some("provider_error")
        );
    }
}
