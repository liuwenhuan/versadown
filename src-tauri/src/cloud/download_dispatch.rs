//! Runtime-only cloud admission and cancellation coordination. No request data
//! or credentials are retained here; jobs own their ephemeral options.
use super::{
    download_scheduler::{AdmissionOutcome, CloudDownloadScheduler},
    CloudProvider, QuarkError,
};
use crate::{DownloadAction, DownloadTask, TaskStatus, TaskStore};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};
use uuid::Uuid;

#[derive(Default)]
struct TaskDispatch {
    version: AtomicU64,
    submission: Arc<AsyncMutex<()>>,
    job: AsyncMutex<()>,
    changed: Notify,
}

#[derive(Clone, Default)]
pub struct CloudDownloadDispatch {
    pub scheduler: CloudDownloadScheduler,
    tasks: Arc<Mutex<HashMap<Uuid, Arc<TaskDispatch>>>>,
}

struct Slot {
    scheduler: CloudDownloadScheduler,
    id: Uuid,
    accepted: bool,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if !self.accepted {
            self.scheduler.release(self.id);
        }
    }
}

impl CloudDownloadDispatch {
    fn task(&self, id: Uuid) -> Arc<TaskDispatch> {
        self.tasks.lock().unwrap().entry(id).or_default().clone()
    }

    pub async fn submission_lock(&self, id: Uuid) -> OwnedMutexGuard<()> {
        self.task(id).submission.clone().lock_owned().await
    }

    /// Invalidate before changing local intent. The worker drops a pending
    /// resolver and its slot; an accepted aria2 submission retains ownership.
    pub fn invalidate(&self, id: Uuid) {
        self.task(id).version.fetch_add(1, Ordering::SeqCst);
        self.scheduler.cancel(id);
        self.task(id).changed.notify_waiters();
    }

    pub async fn prepare(
        &self,
        store: &TaskStore,
        id: Uuid,
    ) -> Result<(DownloadTask, u64), QuarkError> {
        let expected = store.find(id).map_err(|_| QuarkError::Storage)?;
        self.prepare_expected(store, &expected).await
    }

    pub async fn prepare_expected(
        &self,
        store: &TaskStore,
        expected: &DownloadTask,
    ) -> Result<(DownloadTask, u64), QuarkError> {
        let id = expected.id;
        let _guard = self.submission_lock(id).await;
        let mut task = store.find(id).map_err(|_| QuarkError::Storage)?;
        if task.status != expected.status
            || task.aria_gid != expected.aria_gid
            || task.updated_at != expected.updated_at
        {
            return Err(QuarkError::InvalidInput(
                "任务状态已变化，本次提交已跳过".into(),
            ));
        }
        if task.status == TaskStatus::Recycled
            || (task.aria_gid.is_some()
                && matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Paused
                ))
        {
            return Err(QuarkError::InvalidInput("任务正在下载或已回收".into()));
        }
        self.invalidate(id);
        task.status = TaskStatus::Queued;
        task.aria_gid = None;
        task.error = None;
        task.failure_code = None;
        task.aria_error_code = None;
        task.followed_by_gid = None;
        task.download_speed = 0;
        task.upload_speed = 0;
        let task = store
            .apply_transfer_snapshot(task)
            .map_err(|_| QuarkError::Storage)?;
        Ok((task, self.task(id).version.load(Ordering::SeqCst)))
    }

    /// Returns None once aria2 owns the task; callers then use normal engine control.
    pub async fn control_unsubmitted(
        &self,
        store: &TaskStore,
        id: Uuid,
        action: DownloadAction,
    ) -> Result<Option<DownloadTask>, QuarkError> {
        let _guard = self.submission_lock(id).await;
        let mut task = store.find(id).map_err(|_| QuarkError::Storage)?;
        if task.aria_gid.is_some() {
            return Ok(None);
        }
        let status = match (action, task.status) {
            (DownloadAction::Pause, TaskStatus::Queued | TaskStatus::Parsing) => TaskStatus::Paused,
            (
                DownloadAction::Cancel,
                TaskStatus::Queued | TaskStatus::Parsing | TaskStatus::Paused,
            ) => TaskStatus::Cancelled,
            _ => return Err(QuarkError::InvalidInput("当前任务不能执行此操作".into())),
        };
        self.invalidate(id);
        task.status = status;
        task.download_speed = 0;
        task.upload_speed = 0;
        store
            .apply_transfer_snapshot(task)
            .map(Some)
            .map_err(|_| QuarkError::Storage)
    }

    fn runnable(&self, store: &TaskStore, id: Uuid, version: u64) -> bool {
        self.task(id).version.load(Ordering::SeqCst) == version
            && store
                .find(id)
                .is_ok_and(|task| task.status == TaskStatus::Queued && task.aria_gid.is_none())
            && store.cloud_directory_group_for_task(id).is_ok_and(|group| {
                group.is_none_or(|group| {
                    !matches!(
                        group.status,
                        TaskStatus::Paused | TaskStatus::Cancelled | TaskStatus::Recycled
                    )
                })
            })
    }

    /// Resolve only after admission. A per-task job lock prevents an old
    /// resolver's slot guard from releasing a newer retry's slot.
    pub async fn run<R, RF, S, SF, T>(
        &self,
        store: &TaskStore,
        provider: CloudProvider,
        id: Uuid,
        version: u64,
        resolve: R,
        submit: S,
    ) -> Option<DownloadTask>
    where
        R: FnOnce() -> RF,
        RF: Future<Output = Result<T, QuarkError>>,
        S: FnOnce(T) -> SF,
        SF: Future<Output = Result<DownloadTask, QuarkError>>,
    {
        let state = self.task(id);
        let _job = state.job.lock().await;
        let changed = state.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if !self.runnable(store, id, version) {
            return None;
        }
        let admission = tokio::select! {
            admission = self.scheduler.enqueue(provider, id) => admission,
            _ = &mut changed => return None,
        };
        if admission != AdmissionOutcome::Granted {
            return None;
        }
        let mut slot = Slot {
            scheduler: self.scheduler.clone(),
            id,
            accepted: false,
        };
        if !self.runnable(store, id, version) {
            return None;
        }
        let resolved = tokio::select! {
            result = resolve() => result,
            _ = &mut changed => return None,
        };
        let _submission = state.submission.lock().await;
        if !self.runnable(store, id, version) {
            return None;
        }
        let result = match resolved {
            Ok(resolved) => submit(resolved).await,
            Err(error) => Err(error),
        };
        // Submission may bind a GID and then fail status reconciliation. In that
        // case aria2 still owns the slot; never oversubscribe by releasing it.
        if let Ok(task) = store.find(id) {
            if task.aria_gid.is_some() {
                slot.accepted = !matches!(
                    task.status,
                    TaskStatus::Completed
                        | TaskStatus::Failed
                        | TaskStatus::Cancelled
                        | TaskStatus::Recycled
                );
                return Some(task);
            }
        }
        match result {
            Ok(task) => Some(task),
            Err(error) => store
                .fail_with_code(
                    id,
                    Some(super::commands::cloud_failure_code(&error)),
                    super::commands::share_resolution_failure_message(&error),
                )
                .ok(),
        }
    }
}
