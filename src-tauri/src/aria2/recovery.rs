use super::{Aria2Api, DownloadService, DownloadServiceError};
use crate::{DownloadTask, NewTask, TaskStatus, TaskStore};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Default, Debug, Serialize)]
pub struct ShutdownReport {
    pub session_saved: bool,
    pub shutdown_requested: bool,
    pub force_killed: bool,
    pub child_stopped: bool,
}

#[derive(Default, Debug, Serialize)]
pub struct RecoveryReport {
    pub reconciled: usize,
    pub imported: usize,
    pub completed: usize,
    pub failed: usize,
    pub resumed: usize,
}

#[derive(Debug)]
pub struct RestartReport {
    /// Available after process replacement regardless of task-recovery outcome.
    pub client: super::Aria2RpcClient,
    pub recovery: Result<RecoveryReport, DownloadServiceError>,
}

#[derive(Default)]
pub struct RestartBudget {
    attempted: AtomicBool,
}
impl RestartBudget {
    /// Reserve the sole automatic attempt before invoking the potentially failing restart.
    pub async fn run_once<T, E, F, Fut>(&self, restart: F) -> Option<Result<T, E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        if self.attempted.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(restart().await)
    }
}

impl DownloadService {
    fn record_recovery_failure(
        &self,
        store: &TaskStore,
        mut task: DownloadTask,
        gid: &str,
        error: impl std::fmt::Display,
    ) {
        task.status = TaskStatus::Paused;
        task.download_speed = 0;
        task.upload_speed = 0;
        task.eta_seconds = None;
        task.error = Some(format!("任务恢复失败（{gid}）：{error}"));
        if let Ok(task) = store.apply_transfer_snapshot(task) {
            let _ = self.publish_task(&task);
        }
    }

    async fn record_partial_resume_failure(
        &self,
        store: &TaskStore,
        api: &dyn Aria2Api,
        mut task: DownloadTask,
        gid: &str,
        error: impl std::fmt::Display,
    ) {
        let recovery_error = error.to_string();
        match api.pause(gid).await {
            Ok(_) => self.record_recovery_failure(store, task, gid, recovery_error),
            Err(pause_error) => {
                // The replacement may still be transferring. Do not claim it is paused
                // until aria2 acknowledges the pause; polling can reconcile it later.
                task.status = TaskStatus::Downloading;
                task.download_speed = 0;
                task.upload_speed = 0;
                task.eta_seconds = None;
                task.error = Some(format!(
                    "任务恢复失败（{gid}）：{recovery_error}；状态读取与暂停均失败，可能仍在运行：{pause_error}"
                ));
                if let Ok(task) = store.apply_transfer_snapshot(task) {
                    let _ = self.publish_task(&task);
                }
            }
        }
    }

    fn record_terminal_recovery_failure(
        &self,
        store: &TaskStore,
        mut task: DownloadTask,
        gid: &str,
        error: impl std::fmt::Display,
    ) {
        task.status = TaskStatus::Failed;
        task.download_speed = 0;
        task.upload_speed = 0;
        task.eta_seconds = None;
        task.error = Some(format!("终态任务恢复失败（{gid}）：{error}"));
        if let Ok(task) = store.apply_transfer_snapshot(task) {
            let _ = self.publish_task(&task);
        }
    }

    pub async fn recover(&self) -> Result<RecoveryReport, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        let api = self.api();
        // Do not interpret transport failures as absent session tasks.
        let mut statuses = api.tell_active().await?;
        for stopped in [false, true] {
            let mut offset = 0;
            loop {
                let page = if stopped {
                    api.tell_stopped(offset, 1000).await?
                } else {
                    api.tell_waiting(offset, 1000).await?
                };
                let count = page.len();
                statuses.extend(page);
                if count < 1000 {
                    break;
                }
                offset += 1000;
            }
        }
        let store = self.store();
        let existing = store.list()?;
        let mut report = RecoveryReport::default();
        let session_gids: BTreeSet<_> = statuses.iter().map(|s| s.gid.clone()).collect();
        let followed: BTreeSet<_> = statuses
            .iter()
            .flat_map(|s| s.followed_by.clone())
            .collect();
        let legacy_magnets: Vec<_> = existing
            .iter()
            .filter(|task| {
                task.source.starts_with("magnet:")
                    && task.metadata_gid.is_none()
                    && task.aria_gid.is_some()
                    && task.followed_by_gid.as_deref() == task.aria_gid.as_deref()
                    && task
                        .aria_gid
                        .as_ref()
                        .is_some_and(|gid| !session_gids.contains(gid))
                    && !matches!(
                        task.status,
                        TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
                    )
            })
            .collect();
        let unknown_metadata_parents: Vec<_> = statuses
            .iter()
            .filter(|status| {
                status.status == "complete"
                    && !status.followed_by.is_empty()
                    && !existing.iter().any(|task| {
                        task.aria_gid.as_deref() == Some(status.gid.as_str())
                            || task.metadata_gid.as_deref() == Some(status.gid.as_str())
                    })
                    && status.followed_by.iter().all(|child| {
                        !existing.iter().any(|task| {
                            task.aria_gid.as_deref() == Some(child.as_str())
                                || task.metadata_gid.as_deref() == Some(child.as_str())
                        })
                    })
            })
            .collect();
        let legacy_metadata_claim = match (
            legacy_magnets.as_slice(),
            unknown_metadata_parents.as_slice(),
        ) {
            ([task], [parent]) => Some((task.id, parent.gid.clone())),
            _ => None,
        };
        // Known parents first, so following a metadata GID claims the child before import.
        statuses.sort_by_key(|s| {
            (
                !existing.iter().any(|t| {
                    t.aria_gid.as_deref() == Some(&s.gid)
                        || t.metadata_gid.as_deref() == Some(&s.gid)
                }),
                followed.contains(&s.gid),
            )
        });
        for status in statuses {
            let status_gid = status.gid.clone();
            let raw_status = status.status.clone();
            let followed_by_gid = status.followed_by.first().cloned();
            let task = if let Some(task) = store.find_by_gid(&status.gid)? {
                task
            } else if let Some((task_id, _)) = legacy_metadata_claim
                .as_ref()
                .filter(|(_, parent_gid)| parent_gid.as_str() == status.gid.as_str())
            {
                store.bind_metadata_alias(*task_id, &status.gid)?
            } else {
                // Do not reimport a completed metadata parent whose child is already mapped.
                let mut child_mapped = false;
                for child in &status.followed_by {
                    child_mapped |= store.find_by_gid(child)?.is_some();
                }
                if child_mapped {
                    continue;
                }
                let task = store.create(NewTask {
                    source: format!("aria2:{}", status.gid),
                    name: "恢复的下载任务".into(),
                    kind: "file".into(),
                })?;
                report.imported += 1;
                store.bind_gid(task.id, &status.gid)?
            };
            match self.apply_status(task.clone(), status, false).await {
                Ok(_) => report.reconciled += 1,
                Err(error) => {
                    // A single corrupt status or persistence failure must not make a
                    // healthy aria2 sidecar unavailable to every other task.
                    report.failed += 1;
                    if let Some(child_gid) = followed_by_gid {
                        let mut task = task;
                        task.aria_gid = Some(child_gid.clone());
                        task.followed_by_gid = Some(child_gid.clone());
                        self.record_partial_resume_failure(
                            &store,
                            api.as_ref(),
                            task,
                            &child_gid,
                            error,
                        )
                        .await;
                    } else {
                        match raw_status.as_str() {
                            "active" | "waiting" => {
                                self.record_partial_resume_failure(
                                    &store,
                                    api.as_ref(),
                                    task,
                                    &status_gid,
                                    error,
                                )
                                .await;
                            }
                            "paused" => {
                                self.record_recovery_failure(&store, task, &status_gid, error);
                            }
                            _ => {
                                self.record_terminal_recovery_failure(
                                    &store,
                                    task,
                                    &status_gid,
                                    error,
                                );
                            }
                        }
                    }
                }
            }
        }
        for task in existing {
            let Some(gid) = task.aria_gid.clone() else {
                continue;
            };
            if session_gids.contains(&gid)
                || matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
                )
            {
                continue;
            }
            // A parent may have been replaced above; do not overwrite its new state.
            if store.find(task.id)?.aria_gid != task.aria_gid {
                continue;
            }
            let files = store.files(task.id)?;
            let selected: Vec<_> = files.iter().filter(|f| f.selected).collect();
            let completed = !selected.is_empty()
                && selected.iter().all(|file| {
                    let path = std::path::Path::new(&file.path);
                    file.length > 0
                        && std::fs::metadata(path)
                            .map(|m| m.is_file() && m.len() == file.length)
                            .unwrap_or(false)
                        && !std::path::PathBuf::from(format!("{}.aria2", file.path)).exists()
                });
            let mut task = task;
            if completed {
                let recovery_fallback = task.clone();
                task.status = TaskStatus::Completed;
                task.progress = 100;
                task.completed_length = selected.iter().map(|f| f.length).sum();
                task.total_length = Some(task.completed_length);
                task.error = None;
                task.download_speed = 0;
                task.upload_speed = 0;
                task.eta_seconds = None;
                let files = files
                    .iter()
                    .map(|file| {
                        (
                            file.index,
                            file.path.clone(),
                            file.length,
                            if file.selected {
                                file.length
                            } else {
                                file.completed_length
                            },
                            file.selected,
                        )
                    })
                    .collect::<Vec<_>>();
                match store
                    .apply_transfer_snapshot_with_files(task, &files)
                    .map_err(DownloadServiceError::from)
                    .and_then(|task| {
                        self.publish_task(&task)?;
                        Ok(task)
                    }) {
                    Ok(_) => report.completed += 1,
                    Err(error) => {
                        report.failed += 1;
                        self.record_recovery_failure(&store, recovery_fallback, &gid, error);
                    }
                }
                continue;
            } else {
                // Missing output or session is explicit failure; a retained control file
                // may be continued via the original URI without deleting any partial data.
                let partial = selected
                    .iter()
                    .find(|f| std::path::PathBuf::from(format!("{}.aria2", f.path)).is_file());
                let original_http = url::Url::parse(&task.source)
                    .map(|u| matches!(u.scheme(), "http" | "https"))
                    .unwrap_or(false);
                if original_http && selected.len() == 1 && partial.is_some() {
                    let path = std::path::Path::new(&partial.unwrap().path);
                    let mut options = BTreeMap::from([
                        ("continue".into(), "true".into()),
                        ("allow-overwrite".into(), "false".into()),
                    ]);
                    if let Some(dir) = path.parent() {
                        options.insert("dir".into(), dir.to_string_lossy().into_owned());
                    }
                    if let Some(out) = path.file_name() {
                        options.insert("out".into(), out.to_string_lossy().into_owned());
                    }
                    if task.status == TaskStatus::Paused {
                        options.insert("pause".into(), "true".into());
                    }
                    match api.add_uri(vec![task.source.clone()], options).await {
                        Ok(gid) => {
                            let bound = match store.bind_gid(task.id, &gid) {
                                Ok(bound) => bound,
                                Err(bind_error) => {
                                    let cleanup = match api.remove(&gid).await {
                                        Ok(_) => Ok(()),
                                        Err(remove_error) => api
                                            .force_remove(&gid)
                                            .await
                                            .map(|_| ())
                                            .map_err(|force_error| (remove_error, force_error)),
                                    };
                                    report.failed += 1;
                                    match cleanup {
                                        Ok(()) => self.record_recovery_failure(
                                            &store,
                                            task.clone(),
                                            &gid,
                                            bind_error,
                                        ),
                                        Err((remove_error, force_error)) => {
                                            let message = format!(
                                                "任务恢复失败且无法清理 aria2 GID {gid}：{bind_error}；remove: {remove_error}；forceRemove: {force_error}"
                                            );
                                            match store.bind_gid_and_fail(task.id, &gid, &message) {
                                                Ok(tracked) => {
                                                    let _ = self.publish_task(&tracked);
                                                }
                                                Err(track_error) => self.record_recovery_failure(
                                                    &store,
                                                    task.clone(),
                                                    &gid,
                                                    format!(
                                                        "{message}；无法记录 GID：{track_error}"
                                                    ),
                                                ),
                                            }
                                        }
                                    }
                                    continue;
                                }
                            };
                            match api.tell_status(&gid).await {
                                Ok(status) => {
                                    match self.apply_status(bound.clone(), status, false).await {
                                        Ok(_) => report.resumed += 1,
                                        Err(error) => {
                                            report.failed += 1;
                                            self.record_partial_resume_failure(
                                                &store,
                                                api.as_ref(),
                                                bound,
                                                &gid,
                                                error,
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Err(error) => {
                                    report.failed += 1;
                                    self.record_partial_resume_failure(
                                        &store,
                                        api.as_ref(),
                                        bound,
                                        &gid,
                                        error,
                                    )
                                    .await;
                                }
                            }
                            continue;
                        }
                        Err(_) => {}
                    }
                }
                task.status = TaskStatus::Failed;
                task.error = Some("下载会话不存在，无法从保留的输出恢复".into());
                report.failed += 1;
            }
            let recovery_fallback = task.clone();
            task.download_speed = 0;
            task.upload_speed = 0;
            task.eta_seconds = None;
            match store.apply_transfer_snapshot(task) {
                Ok(task) => {
                    let _ = self.publish_task(&task);
                }
                Err(error) => {
                    let mut task = recovery_fallback;
                    task.status = TaskStatus::Paused;
                    task.download_speed = 0;
                    task.upload_speed = 0;
                    task.eta_seconds = None;
                    task.error = Some(format!("任务恢复失败：{error}"));
                    if let Ok(task) = store.apply_transfer_snapshot(task) {
                        let _ = self.publish_task(&task);
                    }
                }
            }
        }
        Ok(report)
    }

    pub async fn mark_engine_unavailable(&self) -> Result<(), DownloadServiceError> {
        let _guard = self.operation.lock().await;
        let store = self.store();
        for mut task in store.list()? {
            if task.aria_gid.is_some()
                && matches!(
                    task.status,
                    TaskStatus::Downloading | TaskStatus::Queued | TaskStatus::AwaitingUser
                )
            {
                task.error = Some("下载引擎不可用".into());
                task.download_speed = 0;
                task.upload_speed = 0;
                task.eta_seconds = None;
                let task = store.apply_transfer_snapshot(task)?;
                self.publish_task(&task)?;
            }
        }
        Ok(())
    }
}
