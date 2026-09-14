use super::{Aria2Api, Aria2Status, DownloadService, DownloadServiceError};
use crate::{DownloadTask, TaskStatus};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    future::Future,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

fn pending_recovery_name(gid: &str) -> String {
    format!("等待获取文件名（{gid}）")
}

pub(super) fn needs_recovery_name(task: &DownloadTask) -> bool {
    task.source.strip_prefix("aria2:").is_some_and(|gid| {
        task.name == "恢复的下载任务"
            || task.name == pending_recovery_name(gid)
            || task.name.trim().is_empty()
    })
}

pub(super) fn recovery_name(status: &Aria2Status) -> Option<String> {
    if let Some(name) = status
        .bittorrent
        .as_ref()
        .and_then(|torrent| torrent.info.as_ref())
        .and_then(|info| info.name.as_deref())
        .filter(|name| !name.trim().is_empty())
    {
        return Some(name.to_owned());
    }
    recovery_file_name(status.files.iter().map(|file| file.path.as_str()))
}

pub(super) fn recovery_file_name<'a>(paths: impl Iterator<Item = &'a str>) -> Option<String> {
    let paths: Vec<_> = paths
        .filter(|path| !path.trim().is_empty())
        .map(|path| {
            if path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\") {
                path.replace('\\', "/")
            } else {
                path.to_owned()
            }
        })
        .collect();
    let first = paths.first()?;
    let parts: Vec<_> = first.split('/').filter(|part| !part.is_empty()).collect();
    let filename = *parts.last()?;
    if paths.len() > 1 {
        let mut common = parts.len().saturating_sub(1);
        for path in paths.iter().skip(1) {
            let others: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
            common = parts[..common]
                .iter()
                .zip(others.iter().take(others.len().saturating_sub(1)))
                .take_while(|(left, right)| left == right)
                .count();
        }
        if common > 0 && !parts[common - 1].ends_with(':') {
            return Some(parts[common - 1].to_owned());
        }
    }
    (!filename.ends_with(':') && filename != "." && filename != "..").then(|| filename.to_owned())
}

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
    /// Kept in the serialized contract; native recovery never imports jobs.
    pub imported: usize,
    /// Completion is now reported by aria2-next rather than inferred locally.
    pub completed: usize,
    /// Resume is owned by aria2-next and is never synthesized by the app.
    pub resumed: usize,
    pub removed_tasks: usize,
    pub removed_engine_tasks: usize,
    pub removed_files: usize,
    pub failed: usize,
}

#[derive(Debug)]
pub struct RestartReport {
    pub client: super::Aria2RpcClient,
    pub recovery: Result<RecoveryReport, DownloadServiceError>,
}

#[derive(Default)]
pub struct RestartBudget {
    attempted: AtomicBool,
}

impl RestartBudget {
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
    pub async fn recover_native(
        &self,
        download_root: &Path,
    ) -> Result<RecoveryReport, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        let api = self.api();
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
        // A completed metadata parent can bind its followed child while being
        // reconciled. Process already-owned GIDs first so that child is not
        // mistaken for an orphan merely because aria2 returned it earlier.
        statuses.sort_by_key(|status| store.find_by_gid(&status.gid).ok().flatten().is_none());
        let native_gids = statuses
            .iter()
            .flat_map(|status| {
                std::iter::once(status.gid.clone()).chain(status.followed_by.iter().cloned())
            })
            .collect::<BTreeSet<_>>();
        let mut report = RecoveryReport::default();

        for status in statuses {
            let Some(task) = store.find_by_gid(&status.gid)? else {
                self.remove_unowned_native_task(api.as_ref(), &status)
                    .await?;
                report.removed_engine_tasks += 1;
                let paths = status
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>();
                let cleanup = store.purge_incompatible_payload_paths(&paths, download_root)?;
                report.removed_files += cleanup.removed_files;
                continue;
            };

            if self
                .apply_status(task.clone(), status.clone(), false)
                .await
                .is_ok()
            {
                report.reconciled += 1;
                continue;
            }

            self.remove_unowned_native_task(api.as_ref(), &status)
                .await?;
            report.removed_engine_tasks += 1;
            report.failed += 1;
            if !matches!(task.status, TaskStatus::Completed | TaskStatus::Recycled) {
                let cleanup = store.purge_incompatible_tasks(&[task.id], download_root)?;
                report.removed_tasks += cleanup.removed_tasks;
                report.removed_files += cleanup.removed_files;
            }
        }

        let missing = store
            .list()?
            .into_iter()
            .filter(|task| {
                !matches!(task.status, TaskStatus::Completed | TaskStatus::Recycled)
                    && task
                        .aria_gid
                        .as_ref()
                        .is_some_and(|gid| !native_gids.contains(gid))
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let cleanup = store.purge_incompatible_tasks(&missing, download_root)?;
            report.removed_tasks += cleanup.removed_tasks;
            report.removed_files += cleanup.removed_files;
        }
        Ok(report)
    }

    async fn remove_unowned_native_task(
        &self,
        api: &dyn Aria2Api,
        status: &Aria2Status,
    ) -> Result<(), DownloadServiceError> {
        if matches!(status.status.as_str(), "complete" | "error" | "removed") {
            api.remove_download_result(&status.gid).await?;
        } else if api.remove(&status.gid).await.is_err() {
            api.force_remove(&status.gid).await?;
        }
        Ok(())
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
