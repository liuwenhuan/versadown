//! Runtime-only cloud admission and cancellation coordination. No request data
//! or credentials are retained here; jobs own their ephemeral options.
use super::{
    download_scheduler::{AdmissionOutcome, CloudDownloadScheduler},
    CloudProvider, QuarkError,
};
use crate::{DownloadAction, DownloadTask, TaskStatus, TaskStore, UriSubmissionOptions};
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
    retry_intent: Mutex<RetryIntent>,
    directory: Mutex<Option<DirectoryAdmission>>,
}

#[derive(Clone, Copy, Debug)]
struct DirectoryAdmission {
    id: Uuid,
    max_active_members: usize,
}

#[derive(Clone, Debug, Default)]
struct RetryIntent {
    original_gid: Option<String>,
    native_resume_gid: Option<String>,
    allow_overwrite: bool,
    continue_download: bool,
}

#[derive(Clone, Default)]
pub struct CloudDownloadDispatch {
    pub scheduler: CloudDownloadScheduler,
    tasks: Arc<Mutex<HashMap<Uuid, Arc<TaskDispatch>>>>,
}

struct Slot {
    scheduler: CloudDownloadScheduler,
    id: Uuid,
    directory: Option<DirectoryAdmission>,
    accepted: bool,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if !self.accepted {
            if let Some(directory) = self.directory {
                self.scheduler
                    .release_directory_member(directory.id, self.id, false);
            } else {
                self.scheduler.release(self.id);
            }
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

    /// A directory root is one logical cloud task; this only controls how many
    /// of its independently-addressed files may be admitted concurrently.
    pub fn configure_directory(&self, id: Uuid, directory: Uuid, max_active_members: u8) {
        self.task(id)
            .directory
            .lock()
            .unwrap()
            .replace(DirectoryAdmission {
                id: directory,
                max_active_members: usize::from(max_active_members.max(1)),
            });
    }

    /// Terminal children free their file admission. The directory's logical
    /// task admission survives until the projected directory root is terminal.
    pub fn release_terminal(&self, id: Uuid, directory_finished: bool) {
        let directory = *self.task(id).directory.lock().unwrap();
        if let Some(directory) = directory {
            self.scheduler
                .release_directory_member(directory.id, id, directory_finished);
        } else {
            self.scheduler.release(id);
        }
        self.scheduler.cancel(id);
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
        self.prepare_expected_inner(store, expected, None).await
    }

    pub async fn prepare_expected_with_retry_intent(
        &self,
        store: &TaskStore,
        expected: &DownloadTask,
        allow_overwrite: bool,
        continue_download: bool,
    ) -> Result<(DownloadTask, u64), QuarkError> {
        self.prepare_expected_inner(store, expected, Some((allow_overwrite, continue_download)))
            .await
    }

    async fn prepare_expected_inner(
        &self,
        store: &TaskStore,
        expected: &DownloadTask,
        retry_options: Option<(bool, bool)>,
    ) -> Result<(DownloadTask, u64), QuarkError> {
        let id = expected.id;
        let state = self.task(id);
        let _guard = state.submission.clone().lock_owned().await;
        let mut task = store.find(id).map_err(|_| QuarkError::Storage)?;
        if task.status != expected.status
            || task.aria_gid != expected.aria_gid
            || task.updated_at != expected.updated_at
        {
            return Err(QuarkError::InvalidInput(
                "任务状态已变化，本次提交已跳过".into(),
            ));
        }
        let native_resume = retry_options.is_some_and(|(_, continue_download)| continue_download)
            && task.status == TaskStatus::Paused
            && crate::xunlei_native::is_native_gid(task.aria_gid.as_deref());
        if task.status == TaskStatus::Recycled
            || (!native_resume
                && task.aria_gid.is_some()
                && matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Paused
                ))
        {
            return Err(QuarkError::InvalidInput("任务正在下载或已回收".into()));
        }
        {
            let mut intent = state.retry_intent.lock().unwrap();
            if task.aria_gid.is_some() {
                intent.original_gid.clone_from(&task.aria_gid);
            }
            if let Some((allow_overwrite, continue_download)) = retry_options {
                intent.allow_overwrite = allow_overwrite;
                intent.continue_download = continue_download && !allow_overwrite;
                if !intent.continue_download {
                    intent.native_resume_gid = None;
                } else if let Some(gid) = task.aria_gid.as_deref() {
                    // Retain identity even if the native database is momentarily
                    // unavailable; submission must fail closed, never guess length.
                    intent.native_resume_gid =
                        (!crate::aria2::native_resume::has_legacy_control(store, &task))
                            .then(|| gid.to_owned());
                }
            }
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

    pub fn retry_submission_options(
        &self,
        id: Uuid,
        mut options: UriSubmissionOptions,
    ) -> UriSubmissionOptions {
        let intent = self.task(id).retry_intent.lock().unwrap().clone();
        if intent.allow_overwrite {
            options.allow_overwrite = true;
            options.continue_download = false;
        } else if intent.continue_download {
            options.continue_download = true;
            options.native_resume_gid = intent.native_resume_gid;
        }
        options
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
        if action == DownloadAction::Cancel {
            task.aria_gid = self
                .task(id)
                .retry_intent
                .lock()
                .unwrap()
                .original_gid
                .clone();
        }
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
        let directory = *state.directory.lock().unwrap();
        let admission = tokio::select! {
            admission = async {
                if let Some(directory) = directory {
                    self.scheduler
                        .enqueue_directory(provider, directory.id, id, directory.max_active_members)
                        .await
                } else {
                    self.scheduler.enqueue(provider, id).await
                }
            } => admission,
            _ = &mut changed => return None,
        };
        if admission != AdmissionOutcome::Granted {
            return None;
        }
        let mut slot = Slot {
            scheduler: self.scheduler.clone(),
            id,
            directory,
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
                *state.retry_intent.lock().unwrap() = RetryIntent::default();
                return Some(task);
            }
        }
        match result {
            Ok(task) => Some(task),
            Err(error) => {
                let original_gid = state.retry_intent.lock().unwrap().original_gid.clone();
                if let (Some(original_gid), Ok(mut current)) = (original_gid, store.find(id)) {
                    current.aria_gid = Some(original_gid);
                    let _ = store.apply_transfer_snapshot(current);
                }
                store
                    .fail_with_code(
                        id,
                        Some(super::commands::cloud_failure_code(&error)),
                        super::commands::share_resolution_failure_message(&error),
                    )
                    .ok()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NewTask;

    #[tokio::test]
    async fn paused_xunlei_native_task_can_reenter_dispatch_for_fresh_authorization() {
        let store = TaskStore::open_in_memory().unwrap();
        let mut task = store
            .create(NewTask::direct("cloud://xunlei/file-id"))
            .unwrap();
        task.status = TaskStatus::Paused;
        task.aria_gid = Some("xunlei-native:42".into());
        let task = store.apply_transfer_snapshot(task).unwrap();
        let dispatch = CloudDownloadDispatch::default();

        let (queued, _) = dispatch
            .prepare_expected_with_retry_intent(&store, &task, false, true)
            .await
            .unwrap();

        assert_eq!(queued.status, TaskStatus::Queued);
        assert!(queued.aria_gid.is_none());
        let options = dispatch.retry_submission_options(task.id, UriSubmissionOptions::default());
        assert!(options.continue_download);
        assert_eq!(
            options.native_resume_gid.as_deref(),
            Some("xunlei-native:42")
        );
    }

    #[tokio::test]
    async fn paused_aria_task_cannot_reenter_dispatch_while_it_owns_a_gid() {
        let store = TaskStore::open_in_memory().unwrap();
        let mut task = store
            .create(NewTask::direct("cloud://xunlei/file-id"))
            .unwrap();
        task.status = TaskStatus::Paused;
        task.aria_gid = Some("0123456789abcdef".into());
        let task = store.apply_transfer_snapshot(task).unwrap();

        assert!(CloudDownloadDispatch::default()
            .prepare_expected_with_retry_intent(&store, &task, false, true)
            .await
            .is_err());
    }
}
