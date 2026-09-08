use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use thiserror::Error;
use uuid::Uuid;

use crate::{DownloadTask, StoreError, TaskStatus, TaskStore};

use super::sync::{EventSink, NoEvents};
use super::{
    Aria2Api, Aria2Error, Aria2GlobalStat, Aria2Process, Aria2Status, RpcCall, TransferSnapshot,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Error)]
pub enum DownloadServiceError {
    #[error(transparent)]
    Aria2(#[from] Aria2Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("task {0} is not bound to aria2")]
    MissingGid(Uuid),
    #[error("download engine is shutting down")]
    ShuttingDown,
    #[error("select at least one existing torrent file")]
    InvalidFileSelection,
    #[error("task is already submitted")]
    AlreadySubmitted,
    #[error("磁力元数据仍在获取，请稍后重试确认")]
    MetadataTimeout,
    #[error("种子文件未包含可下载文件")]
    EmptyTorrent,
}

pub struct DownloadService {
    api: std::sync::RwLock<Arc<dyn Aria2Api>>,
    store: Arc<TaskStore>,
    accepting: AtomicBool,
    process: std::sync::Mutex<Option<Aria2Process>>,
    events: Arc<dyn EventSink>,
    published: std::sync::Mutex<BTreeMap<Uuid, serde_json::Value>>,
    global: std::sync::Mutex<Option<Aria2GlobalStat>>,
    pub(super) operation: tokio::sync::Mutex<()>,
    restart_budget: super::recovery::RestartBudget,
}

impl DownloadService {
    pub fn new(api: Arc<dyn Aria2Api>, store: Arc<TaskStore>) -> Self {
        Self {
            api: std::sync::RwLock::new(api),
            store,
            accepting: AtomicBool::new(true),
            process: std::sync::Mutex::new(None),
            events: Arc::new(NoEvents),
            published: Default::default(),
            global: Default::default(),
            operation: Default::default(),
            restart_budget: Default::default(),
        }
    }

    pub fn with_process(
        api: Arc<dyn Aria2Api>,
        store: Arc<TaskStore>,
        process: Aria2Process,
    ) -> Self {
        let mut service = Self::new(api, store);
        service.process = std::sync::Mutex::new(Some(process));
        service
    }

    pub fn with_events(mut self, events: Arc<dyn EventSink>) -> Self {
        self.events = events;
        self
    }

    pub fn api(&self) -> Arc<dyn Aria2Api> {
        Arc::clone(&self.api.read().unwrap_or_else(|e| e.into_inner()))
    }
    pub fn store(&self) -> Arc<TaskStore> {
        Arc::clone(&self.store)
    }

    pub async fn submit_uri(
        &self,
        task_id: Uuid,
        uris: Vec<String>,
        options: BTreeMap<String, String>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        if self.store.find(task_id)?.aria_gid.is_some() {
            return Err(DownloadServiceError::AlreadySubmitted);
        }
        let gid = self.api().add_uri(uris, options).await?;
        if let Err(error) = self.store.bind_gid(task_id, &gid) {
            let _ = self.api().remove(&gid).await;
            return Err(error.into());
        }
        self.reconcile_task_unlocked(task_id).await
    }

    pub async fn submit_torrent(
        &self,
        task_id: Uuid,
        torrent_base64: String,
        web_seed_uris: Vec<String>,
        mut options: BTreeMap<String, String>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        if self.store.find(task_id)?.aria_gid.is_some() {
            return Err(DownloadServiceError::AlreadySubmitted);
        }
        // A torrent must remain paused until the user has reviewed aria2's real
        // file list and `select-file` has been applied.
        options.insert("pause".into(), "true".into());
        let gid = self
            .api()
            .add_torrent(torrent_base64, web_seed_uris, options)
            .await?;
        let status = match self.api().tell_status(&gid).await {
            Ok(status) if status.gid == gid && !status.files.is_empty() => status,
            Ok(status) if status.gid == gid && status.files.is_empty() => {
                let _ = self.api().remove(&gid).await;
                return Err(DownloadServiceError::EmptyTorrent);
            }
            Ok(_) => {
                let _ = self.api().remove(&gid).await;
                return Err(Aria2Error::MalformedResponse.into());
            }
            Err(error) => {
                let _ = self.api().remove(&gid).await;
                return Err(error.into());
            }
        };
        if TransferSnapshot::try_from(status.clone()).is_err() {
            let _ = self.api().remove(&gid).await;
            return Err(Aria2Error::MalformedResponse.into());
        }
        let current = match self.store.bind_metadata_gid(task_id, &gid) {
            Ok(task) => task,
            Err(error) => {
                let _ = self.api().remove(&gid).await;
                return Err(error.into());
            }
        };
        self.publish_task(&current)?;
        self.apply_status(current, status, false).await
    }

    pub async fn control(
        &self,
        task_id: Uuid,
        action: DownloadAction,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        let task = self.store.find(task_id)?;
        let gid = task
            .aria_gid
            .as_deref()
            .ok_or(DownloadServiceError::MissingGid(task_id))?;
        match action {
            DownloadAction::Pause => {
                self.api().pause(gid).await?;
            }
            DownloadAction::Resume => {
                self.api().unpause(gid).await?;
            }
            DownloadAction::Cancel => {
                if self.api().remove(gid).await.is_err() {
                    self.api().force_remove(gid).await?;
                }
                return self.persist_removed(task);
            }
        }
        self.reconcile_task_unlocked(task_id).await
    }

    pub async fn reconcile_task(
        &self,
        task_id: Uuid,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.reconcile_task_unlocked(task_id).await
    }

    async fn reconcile_task_unlocked(
        &self,
        task_id: Uuid,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let current = self.store.find(task_id)?;
        let gid = current
            .aria_gid
            .clone()
            .ok_or(DownloadServiceError::MissingGid(task_id))?;
        self.apply_status(current, self.api().tell_status(&gid).await?, false)
            .await
    }

    pub async fn prepare_magnet(
        &self,
        task_id: Uuid,
        magnet: String,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        let task = self.store.find(task_id)?;
        if task.aria_gid.is_some() {
            return self.reconcile_task_unlocked(task_id).await;
        }
        let options = BTreeMap::from([("pause-metadata".into(), "true".into())]);
        let gid = self.api().add_uri(vec![magnet], options).await?;
        let current = match self.store.bind_metadata_gid(task_id, &gid) {
            Ok(task) => task,
            Err(error) => {
                let _ = self.api().remove(&gid).await;
                return Err(error.into());
            }
        };
        self.publish_task(&current)?;
        self.apply_status(current, self.api().tell_status(&gid).await?, false)
            .await
    }

    pub async fn select_files(
        &self,
        task_id: Uuid,
        indices: Vec<u32>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.select_files_unlocked(task_id, indices).await
    }

    /// Wait only up to the caller's UI deadline. Background synchronization continues
    /// after timeout; no metadata task is cancelled or falsely offered as a BT file.
    pub async fn wait_for_metadata(
        &self,
        task_id: Uuid,
        duration: std::time::Duration,
    ) -> Result<DownloadTask, DownloadServiceError> {
        tokio::time::timeout(duration, async {
            loop {
                self.ensure_accepting()?;
                let task = self.reconcile_task(task_id).await?;
                if matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
                    return Err(Aria2Error::ProcessExited.into());
                }
                let ready_gid =
                    !task.source.starts_with("magnet:") || task.followed_by_gid == task.aria_gid;
                if ready_gid && !self.store.files(task_id)?.is_empty() {
                    return Ok(task);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| DownloadServiceError::MetadataTimeout)?
    }

    pub async fn set_file_selected(
        &self,
        task_id: Uuid,
        index: u32,
        selected: bool,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        let files = self.store.files(task_id)?;
        if !files.iter().any(|f| f.index == index) {
            return Err(DownloadServiceError::InvalidFileSelection);
        }
        let indices = files
            .into_iter()
            .filter(|f| {
                if f.index == index {
                    selected
                } else {
                    f.selected
                }
            })
            .map(|f| f.index)
            .collect();
        self.select_files_unlocked(task_id, indices).await
    }

    async fn select_files_unlocked(
        &self,
        task_id: Uuid,
        mut indices: Vec<u32>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        self.ensure_accepting()?;
        let task = self.store.find(task_id)?;
        let gid = task
            .aria_gid
            .as_deref()
            .ok_or(DownloadServiceError::MissingGid(task_id))?;
        let files = self.store.files(task_id)?;
        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty()
            || indices
                .iter()
                .any(|index| !files.iter().any(|file| file.index == *index))
        {
            return Err(DownloadServiceError::InvalidFileSelection);
        }
        let selection = indices
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.api()
            .change_option(gid, BTreeMap::from([("select-file".into(), selection)]))
            .await?;
        if task.status == TaskStatus::AwaitingUser && !self.store.selection_confirmed(task_id)? {
            self.api().unpause(gid).await?;
        }
        self.store.confirm_selection(task_id)?;
        self.apply_status(task.clone(), self.api().tell_status(gid).await?, true)
            .await
    }

    pub async fn reconcile_now(&self) -> Result<(), DownloadServiceError> {
        let _guard = self.operation.lock().await;
        let tasks: Vec<_> = self
            .store
            .list()?
            .into_iter()
            .filter(|task| task.aria_gid.is_some() && task.status != TaskStatus::Cancelled)
            .collect();
        let mut first_error = None;
        for chunk in tasks.chunks(100) {
            let calls = chunk
                .iter()
                .map(|task| {
                    RpcCall::new("aria2.tellStatus", vec![serde_json::json!(task.aria_gid)])
                })
                .collect();
            let responses = self.api().multicall(calls).await?;
            if responses.len() != chunk.len() {
                return Err(Aria2Error::MalformedResponse.into());
            }
            for (task, response) in chunk.iter().zip(responses) {
                let result = match response {
                    Ok(value) => match serde_json::from_value::<Aria2Status>(value) {
                        Ok(status) => self
                            .apply_status(task.clone(), status, false)
                            .await
                            .map(|_| ()),
                        Err(_) => Err(Aria2Error::MalformedResponse.into()),
                    },
                    Err(fault) => Err(Aria2Error::Rpc(fault).into()),
                };
                if let Err(error) = result {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        let stat = self.api().get_global_stat().await?;
        let mut previous = self.global.lock().unwrap_or_else(|e| e.into_inner());
        if previous.as_ref() != Some(&stat) {
            self.events.global_stat_updated(&stat);
            *previous = Some(stat);
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn apply_status(
        &self,
        current: DownloadTask,
        mut status: Aria2Status,
        confirmed: bool,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let mut seen = std::collections::BTreeSet::new();
        if current.aria_gid.as_deref() != Some(status.gid.as_str()) {
            return Err(Aria2Error::MalformedResponse.into());
        }
        for _ in 0..8 {
            if !seen.insert(status.gid.clone()) {
                return Err(Aria2Error::MalformedResponse.into());
            }
            let Some(next) = status.followed_by.first().cloned() else {
                let mut snapshot = TransferSnapshot::try_from(status)?;
                if !confirmed
                    && !self.store.selection_confirmed(current.id)?
                    && current.status == TaskStatus::AwaitingUser
                    && matches!(
                        snapshot.status,
                        TaskStatus::Downloading
                            | TaskStatus::Paused
                            | TaskStatus::Queued
                            | TaskStatus::Processing
                    )
                {
                    snapshot.status = TaskStatus::AwaitingUser;
                    if current.source.starts_with("magnet:")
                        && current.aria_gid.as_deref() == Some(&snapshot.gid)
                        && current.followed_by_gid != current.aria_gid
                    {
                        snapshot.files.clear();
                    }
                }
                return self.persist_snapshot(current, snapshot);
            };
            status = self.api().tell_status(&next).await?;
            if status.gid != next {
                return Err(Aria2Error::MalformedResponse.into());
            }
        }
        Err(Aria2Error::MalformedResponse.into())
    }

    pub fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
    }
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }

    pub async fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> Result<bool, DownloadServiceError> {
        let report = self.shutdown_report(timeout).await;
        if !report.session_saved || !report.shutdown_requested || !report.child_stopped {
            return Err(Aria2Error::Transport.into());
        }
        Ok(!report.force_killed)
    }

    pub async fn shutdown_report(
        &self,
        timeout: std::time::Duration,
    ) -> super::recovery::ShutdownReport {
        let deadline = tokio::time::Instant::now() + timeout;
        self.stop_accepting();
        let mut report = super::recovery::ShutdownReport {
            session_saved: false,
            shutdown_requested: false,
            child_stopped: true,
            force_killed: false,
        };
        let _guard = tokio::time::timeout_at(deadline, self.operation.lock())
            .await
            .ok();
        if _guard.is_some() {
            // Every stage spends the same entry deadline, including operation-lock wait.
            report.session_saved = matches!(
                tokio::time::timeout_at(deadline, self.api().save_session()).await,
                Ok(Ok(_))
            );
            report.shutdown_requested = matches!(
                tokio::time::timeout_at(deadline, self.api().shutdown()).await,
                Ok(Ok(_))
            );
        }
        loop {
            {
                // Never hold the owned-child mutex across an await. A blocked control
                // cannot prevent the deadline path from terminating this child.
                let mut process = self.process.lock().unwrap_or_else(|e| e.into_inner());
                let Some(process) = process.as_mut() else {
                    return report;
                };
                if matches!(process.is_running(), Ok(false)) {
                    return report;
                }
                if tokio::time::Instant::now() >= deadline {
                    report.force_killed = true;
                    report.child_stopped = process.terminate_fallback().is_ok();
                    return report;
                }
            }
            tokio::time::sleep_until(
                (tokio::time::Instant::now() + std::time::Duration::from_millis(25)).min(deadline),
            )
            .await;
        }
    }

    pub async fn engine_running(&self) -> Result<Option<bool>, DownloadServiceError> {
        self.process
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .map(|child| child.is_running().map_err(Into::into))
            .transpose()
    }

    pub async fn restart_if_exited(
        &self,
        config: super::Aria2LaunchConfig,
    ) -> Result<Option<super::recovery::RestartReport>, DownloadServiceError> {
        self.ensure_accepting()?;
        if self.engine_running().await? != Some(false) {
            return Ok(None);
        }
        self.mark_engine_unavailable().await?;
        self.restart_budget
            .run_once(|| async {
                let client = {
                    let _guard = self.operation.lock().await;
                    self.ensure_accepting()?;
                    let (mut process, client) = Aria2Process::start(config).await?;
                    let mut owned = self.process.lock().unwrap_or_else(|e| e.into_inner());
                    if !self.is_accepting() {
                        process.terminate_fallback()?;
                        return Err(DownloadServiceError::ShuttingDown);
                    }
                    *owned = Some(process);
                    *self.api.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(client.clone());
                    client
                };
                let recovery = self.recover().await;
                Ok::<_, DownloadServiceError>(super::recovery::RestartReport { client, recovery })
            })
            .await
            .transpose()
    }

    fn ensure_accepting(&self) -> Result<(), DownloadServiceError> {
        if self.is_accepting() {
            Ok(())
        } else {
            Err(DownloadServiceError::ShuttingDown)
        }
    }

    fn persist_removed(
        &self,
        mut task: DownloadTask,
    ) -> Result<DownloadTask, DownloadServiceError> {
        task.status = TaskStatus::Cancelled;
        task.download_speed = 0;
        task.upload_speed = 0;
        let task = self.store.apply_transfer_snapshot(task)?;
        self.publish_task(&task)?;
        Ok(task)
    }

    fn persist_snapshot(
        &self,
        mut task: DownloadTask,
        snapshot: TransferSnapshot,
    ) -> Result<DownloadTask, DownloadServiceError> {
        if task.aria_gid.as_deref() != Some(&snapshot.gid) {
            task.followed_by_gid = Some(snapshot.gid.clone());
        }
        task.aria_gid = Some(snapshot.gid.clone());
        // No post-processing worker is configured: an aria2-complete transfer is final.
        task.status = if snapshot.status == TaskStatus::Processing {
            TaskStatus::Completed
        } else {
            snapshot.status
        };
        task.progress = snapshot.progress_percent.unwrap_or(0);
        task.total_length = snapshot.total_length;
        task.completed_length = snapshot.completed_length;
        task.download_speed = snapshot.download_speed;
        task.upload_speed = snapshot.upload_speed;
        task.connections = snapshot.connections;
        task.eta_seconds = snapshot.eta_seconds;
        task.aria_error_code = snapshot.error_code;
        task.error = snapshot.error_message;
        let files = snapshot
            .files
            .into_iter()
            .map(|file| {
                (
                    file.index,
                    file.path,
                    file.length,
                    file.completed_length,
                    file.selected,
                )
            })
            .collect::<Vec<_>>();
        let task = self
            .store
            .apply_transfer_snapshot_with_files(task, &files)?;
        self.publish_task(&task)?;
        Ok(task)
    }

    pub(super) fn publish_task(&self, task: &DownloadTask) -> Result<(), DownloadServiceError> {
        let mut value = serde_json::to_value(task).map_err(|_| Aria2Error::MalformedResponse)?;
        value.as_object_mut().unwrap().remove("updated_at");
        value["files"] = serde_json::to_value(self.store.files(task.id)?)
            .map_err(|_| Aria2Error::MalformedResponse)?;
        let mut published = self.published.lock().unwrap_or_else(|e| e.into_inner());
        if published.get(&task.id) != Some(&value) {
            published.insert(task.id, value);
            self.events.task_updated(task);
        }
        Ok(())
    }
}
