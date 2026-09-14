use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use thiserror::Error;
use uuid::Uuid;

use crate::{
    validate_download_input, validate_task_source, DownloadTask, InputValidationError, StoreError,
    TaskStatus, TaskStore,
};

use super::sync::{EventSink, NoEvents};
use super::{
    Aria2Api, Aria2Error, Aria2GlobalStat, Aria2Options, Aria2Process, Aria2Status, RpcCall,
    TransferSnapshot,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UriSubmissionOptions {
    pub directory: Option<PathBuf>,
    pub output_name: Option<String>,
    pub headers: Vec<(String, String)>,
    pub proxy: Option<String>,
    pub allow_overwrite: bool,
    pub continue_download: bool,
}

impl UriSubmissionOptions {
    pub(crate) fn into_aria2_options(self) -> Result<Aria2Options, DownloadServiceError> {
        let mut options = Aria2Options::new();
        if let Some(directory) = self.directory {
            if directory.as_os_str().is_empty() {
                return Err(DownloadServiceError::InvalidUriOption("directory"));
            }
            options.insert("dir".into(), directory.to_string_lossy().into_owned());
        }
        if let Some(output_name) = self.output_name {
            if !valid_output_name(&output_name) {
                return Err(DownloadServiceError::InvalidUriOption("output name"));
            }
            options.insert("out".into(), output_name);
        }
        if !self.headers.is_empty() {
            let headers = self
                .headers
                .into_iter()
                .map(|(name, value)| {
                    valid_header(&name, &value)
                        .then(|| format!("{name}: {value}"))
                        .ok_or(DownloadServiceError::InvalidUriOption("header"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            options.insert("header".into(), headers.join("\n"));
        }
        if let Some(proxy) = self.proxy {
            if proxy.trim().is_empty() || proxy.contains(['\r', '\n']) {
                return Err(DownloadServiceError::InvalidUriOption("proxy"));
            }
            options.insert("all-proxy".into(), proxy);
        }
        if self.allow_overwrite {
            options.insert("allow-overwrite".into(), "true".into());
        }
        if self.continue_download {
            options.insert("continue".into(), "true".into());
        }
        Ok(options)
    }
}

fn valid_output_name(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.contains(['\r', '\n', '/', '\\', '<', '>', ':', '"', '|', '?', '*'])
        && !value.ends_with(['.', ' '])
}

fn valid_header(name: &str, value: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        && !value.contains(['\r', '\n'])
}

#[derive(Debug, Error)]
pub enum DownloadServiceError {
    #[error(transparent)]
    Input(#[from] InputValidationError),
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
    #[error("invalid aria2 URI option: {0}")]
    InvalidUriOption(&'static str),
    #[error("磁力元数据仍在获取，请稍后重试确认")]
    MetadataTimeout,
    #[error("种子文件未包含可下载文件")]
    EmptyTorrent,
    #[error("种子任务处于不可选择文件的 aria2 状态：{0}")]
    TorrentNotSelectable(String),
    #[error("种子任务处理失败且无法清理 aria2 GID {gid}：{cause}；清理错误：{cleanup}")]
    TorrentCleanupFailed {
        gid: String,
        cause: String,
        cleanup: String,
    },
}

pub struct DownloadService {
    api: std::sync::RwLock<Arc<dyn Aria2Api>>,
    store: Arc<TaskStore>,
    accepting: AtomicBool,
    process: std::sync::Mutex<Option<Aria2Process>>,
    events: Arc<dyn EventSink>,
    published: std::sync::Mutex<BTreeMap<Uuid, serde_json::Value>>,
    global: std::sync::Mutex<Option<Aria2GlobalStat>>,
    resume_versions: std::sync::Mutex<BTreeMap<String, u64>>,
    observed_peers: std::sync::Mutex<HashMap<Uuid, HashSet<String>>>,
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
            resume_versions: Default::default(),
            observed_peers: Default::default(),
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

    fn task_for_uri_submission(&self, task_id: Uuid) -> Result<DownloadTask, DownloadServiceError> {
        let task = self.store.find(task_id)?;
        if let Err(error) = validate_task_source(&task.kind, &task.source) {
            let _ = self.store.fail(task.id, error.to_string());
            return Err(error.into());
        }
        Ok(task)
    }

    pub async fn submit_uri(
        &self,
        task_id: Uuid,
        uris: Vec<String>,
        mut options: BTreeMap<String, String>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        if self.task_for_uri_submission(task_id)?.aria_gid.is_some() {
            return Err(DownloadServiceError::AlreadySubmitted);
        }
        let directory = self.prepare_directory_uri(task_id, &mut options)?;
        let gid = self.api().add_uri(uris, options).await?;
        if let Err(error) = self.store.bind_gid(task_id, &gid) {
            let _ = self.api().remove(&gid).await;
            return Err(error.into());
        }
        self.finish_directory_uri(task_id, &gid, directory).await
    }

    fn prepare_directory_uri(
        &self,
        task_id: Uuid,
        options: &mut Aria2Options,
    ) -> Result<Option<Uuid>, DownloadServiceError> {
        let group = self.store.cloud_directory_group_for_task(task_id)?;
        if group.is_some() {
            // Admit directory members without opening connections. Recheck the
            // durable user intent after the RPC, including pauses during addUri.
            options.insert("pause".into(), "true".into());
        }
        Ok(group.map(|group| group.id))
    }

    async fn finish_directory_uri(
        &self,
        task_id: Uuid,
        gid: &str,
        directory: Option<Uuid>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        if let Some(id) = directory {
            match self
                .store
                .cloud_directory_group(id)?
                .map(|group| group.status)
            {
                Some(TaskStatus::Paused) => {}
                None | Some(TaskStatus::Cancelled | TaskStatus::Recycled) => {
                    self.api().force_remove(gid).await?;
                    return self.persist_removed(self.store.find(task_id)?);
                }
                _ => {
                    self.unpause_gid(gid).await?;
                }
            }
        }
        self.reconcile_task_unlocked(task_id).await
    }

    pub async fn submit_uri_with_options(
        &self,
        task_id: Uuid,
        uris: Vec<String>,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, DownloadServiceError> {
        self.submit_uri(task_id, uris, options.into_aria2_options()?)
            .await
    }

    pub async fn submit_native_media_with_options(
        &self,
        task_id: Uuid,
        uris: Vec<String>,
        options: UriSubmissionOptions,
        media: &str,
    ) -> Result<DownloadTask, DownloadServiceError> {
        if !matches!(media, "hls" | "dash") {
            return Err(DownloadServiceError::InvalidUriOption("media"));
        }
        let mut options = options.into_aria2_options()?;
        options.insert("media".into(), media.into());
        self.submit_uri(task_id, uris, options).await
    }

    /// Re-submits the same durable local task after the caller has completed
    /// duplicate-file preflight and obtained any required consent. The old GID
    /// remains durable until aria2 accepts the replacement URI, avoiding a
    /// false "queued" task when the RPC call itself fails.
    pub async fn resubmit_uri_with_options(
        &self,
        task_id: Uuid,
        uris: Vec<String>,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let mut options = options.into_aria2_options()?;
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        self.task_for_uri_submission(task_id)?;
        let directory = self.prepare_directory_uri(task_id, &mut options)?;
        let gid = self.api().add_uri(uris, options).await?;
        if let Err(error) = self.store.rebind_for_resubmission(task_id, &gid) {
            let _ = self.api().remove(&gid).await;
            return Err(error.into());
        }
        self.clear_observed_peers(task_id);
        self.finish_directory_uri(task_id, &gid, directory).await
    }

    pub async fn resubmit_native_media_with_options(
        &self,
        task_id: Uuid,
        uris: Vec<String>,
        options: UriSubmissionOptions,
        media: &str,
    ) -> Result<DownloadTask, DownloadServiceError> {
        if !matches!(media, "hls" | "dash") {
            return Err(DownloadServiceError::InvalidUriOption("media"));
        }
        let mut options = options.into_aria2_options()?;
        options.insert("media".into(), media.into());
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        self.task_for_uri_submission(task_id)?;
        let directory = self.prepare_directory_uri(task_id, &mut options)?;
        let gid = self.api().add_uri(uris, options).await?;
        if let Err(error) = self.store.rebind_for_resubmission(task_id, &gid) {
            let _ = self.api().remove(&gid).await;
            return Err(error.into());
        }
        self.clear_observed_peers(task_id);
        self.finish_directory_uri(task_id, &gid, directory).await
    }

    /// Compare the caller's pre-resolution snapshot while holding the same
    /// operation lock as cancellation, retries and reconciliation. A stale
    /// request is a no-op: it must neither add a URI nor rebind the task.
    pub async fn compare_and_resubmit_uri_with_options(
        &self,
        expected: &DownloadTask,
        uris: Vec<String>,
        options: UriSubmissionOptions,
    ) -> Result<Option<DownloadTask>, DownloadServiceError> {
        let mut options = options.into_aria2_options()?;
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        let current = self.store.find(expected.id)?;
        if current.status != expected.status
            || current.aria_gid != expected.aria_gid
            || !matches!(
                current.status,
                TaskStatus::Paused
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Completed
            )
        {
            return Ok(None);
        }
        validate_task_source(&current.kind, &current.source)?;
        let directory = self.prepare_directory_uri(expected.id, &mut options)?;
        let gid = self.api().add_uri(uris, options).await?;
        if let Err(error) = self.store.rebind_for_resubmission(expected.id, &gid) {
            let _ = self.api().remove(&gid).await;
            return Err(error.into());
        }
        self.clear_observed_peers(expected.id);
        self.finish_directory_uri(expected.id, &gid, directory)
            .await
            .map(Some)
    }

    pub async fn submit_torrent(
        &self,
        task_id: Uuid,
        torrent_base64: String,
        web_seed_uris: Vec<String>,
        options: BTreeMap<String, String>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        self.submit_torrent_with_mode(task_id, torrent_base64, web_seed_uris, options, false)
            .await
    }

    /// Replaces a previously stopped torrent-backed magnet after the caller
    /// explicitly approved overwriting its files. The prior file selection is
    /// supplied by the caller and applied before the replacement is unpaused.
    pub async fn resubmit_torrent_with_options(
        &self,
        task_id: Uuid,
        torrent_base64: String,
        web_seed_uris: Vec<String>,
        selected_indices: Vec<u32>,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let task = self
            .submit_torrent_with_mode(
                task_id,
                torrent_base64,
                web_seed_uris,
                options.clone().into_aria2_options()?,
                true,
            )
            .await?;
        let selected_indices = if selected_indices.is_empty() {
            self.store
                .files(task_id)?
                .into_iter()
                .filter(|file| file.selected)
                .map(|file| file.index)
                .collect()
        } else {
            selected_indices
        };
        self.select_files_with_options(task.id, selected_indices, options)
            .await
    }

    async fn submit_torrent_with_mode(
        &self,
        task_id: Uuid,
        torrent_base64: String,
        web_seed_uris: Vec<String>,
        mut options: BTreeMap<String, String>,
        rebind_existing_task: bool,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        let task = self.task_for_uri_submission(task_id)?;
        if task.aria_gid.is_some() && !rebind_existing_task {
            return Err(DownloadServiceError::AlreadySubmitted);
        }
        // A torrent must remain paused until the user has reviewed aria2's real
        // file list and `select-file` has been applied.
        options.insert("pause".into(), "true".into());
        // Existing torrent payloads must be verified against the torrent's piece
        // hashes before aria2 decides whether they can be resumed or are complete.
        options.insert("check-integrity".into(), "true".into());
        let gid = self
            .api()
            .add_torrent(torrent_base64, web_seed_uris, options)
            .await?;
        let status = match self.api().tell_status(&gid).await {
            Ok(status) => status,
            Err(error) => {
                return Err(self
                    .cleanup_rejected_torrent(task_id, &gid, error.into())
                    .await)
            }
        };
        if status.gid != gid {
            return Err(self
                .cleanup_rejected_torrent(task_id, &gid, Aria2Error::MalformedResponse.into())
                .await);
        }
        let mut aria_status = status.status.clone();
        let mut snapshot = match TransferSnapshot::try_from(status) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Err(self
                    .cleanup_rejected_torrent(task_id, &gid, error.into())
                    .await)
            }
        };
        if snapshot.status == TaskStatus::Queued {
            if let Err(error) = self.api().pause(&gid).await {
                return Err(self
                    .cleanup_rejected_torrent(task_id, &gid, error.into())
                    .await);
            }
            let paused_status = match self.api().tell_status(&gid).await {
                Ok(status) => status,
                Err(error) => {
                    return Err(self
                        .cleanup_rejected_torrent(task_id, &gid, error.into())
                        .await)
                }
            };
            if paused_status.gid != gid {
                return Err(self
                    .cleanup_rejected_torrent(task_id, &gid, Aria2Error::MalformedResponse.into())
                    .await);
            }
            aria_status = paused_status.status.clone();
            snapshot = match TransferSnapshot::try_from(paused_status) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return Err(self
                        .cleanup_rejected_torrent(task_id, &gid, error.into())
                        .await)
                }
            };
            if snapshot.status != TaskStatus::Paused {
                return Err(self
                    .cleanup_rejected_torrent(
                        task_id,
                        &gid,
                        DownloadServiceError::TorrentNotSelectable(aria_status),
                    )
                    .await);
            }
        }
        match snapshot.status {
            TaskStatus::Paused => {
                if snapshot.files.is_empty() {
                    return Err(self
                        .cleanup_rejected_torrent(task_id, &gid, DownloadServiceError::EmptyTorrent)
                        .await);
                }
                let current = match if rebind_existing_task {
                    self.store
                        .rebind_for_resubmission(task_id, &gid)
                        .and_then(|_| self.store.bind_awaiting_gid(task_id, &gid))
                } else {
                    self.store.bind_awaiting_gid(task_id, &gid)
                } {
                    Ok(task) => task,
                    Err(error) => {
                        return Err(self
                            .cleanup_rejected_torrent(task_id, &gid, error.into())
                            .await)
                    }
                };
                if rebind_existing_task {
                    self.clear_observed_peers(task_id);
                }
                snapshot.status = TaskStatus::AwaitingUser;
                self.persist_snapshot(current, snapshot)
            }
            TaskStatus::Processing
            | TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            // This state is local-only, but treating it as terminal protects
            // against a malformed/internal snapshot entering this code path.
            | TaskStatus::Recycled => {
                if snapshot.status == TaskStatus::Failed && snapshot.error_message.is_none() {
                    snapshot.error_message =
                        Some(format!("aria2 种子任务以 {aria_status} 状态结束"));
                }
                let current = match if rebind_existing_task {
                    self.store.rebind_for_resubmission(task_id, &gid)
                } else {
                    self.store.bind_gid(task_id, &gid)
                } {
                    Ok(task) => task,
                    Err(error) => {
                        return Err(self
                            .cleanup_rejected_torrent(task_id, &gid, error.into())
                            .await)
                    }
                };
                if rebind_existing_task {
                    self.clear_observed_peers(task_id);
                }
                self.persist_snapshot(current, snapshot)?;
                Err(DownloadServiceError::TorrentNotSelectable(aria_status))
            }
            TaskStatus::Downloading
            | TaskStatus::Queued
            | TaskStatus::AwaitingUser
            | TaskStatus::Draft
            | TaskStatus::Parsing => Err(self
                .cleanup_rejected_torrent(
                    task_id,
                    &gid,
                    DownloadServiceError::TorrentNotSelectable(aria_status),
                )
                .await),
        }
    }

    async fn cleanup_rejected_torrent(
        &self,
        task_id: Uuid,
        gid: &str,
        cause: DownloadServiceError,
    ) -> DownloadServiceError {
        let cleanup = match self.api().remove(gid).await {
            Ok(_) => return cause,
            Err(error) => error,
        };
        let cause = cause.to_string();
        let cleanup = cleanup.to_string();
        let message =
            format!("种子任务处理失败且无法清理 aria2 GID {gid}：{cause}；清理错误：{cleanup}");
        match self.store.bind_gid_and_fail(task_id, gid, &message) {
            Ok(task) => match self.publish_task(&task) {
                Ok(()) => DownloadServiceError::TorrentCleanupFailed {
                    gid: gid.into(),
                    cause,
                    cleanup,
                },
                Err(error) => error,
            },
            Err(error) => error.into(),
        }
    }

    pub async fn control(
        &self,
        task_id: Uuid,
        action: DownloadAction,
    ) -> Result<DownloadTask, DownloadServiceError> {
        self.control_with_directory_intent(task_id, action, false)
            .await
    }

    fn resume_version(&self, gid: &str) -> u64 {
        *self
            .resume_versions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(gid)
            .unwrap_or(&0)
    }

    async fn unpause_gid(&self, gid: &str) -> Result<String, Aria2Error> {
        let result = self.api().unpause(gid).await;
        // Even an uncertain RPC completion may have accepted the resume.
        let mut versions = self
            .resume_versions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let version = versions.entry(gid.to_owned()).or_default();
        *version = version.wrapping_add(1);
        result
    }

    pub async fn control_directory_member(
        &self,
        task_id: Uuid,
        action: DownloadAction,
    ) -> Result<DownloadTask, DownloadServiceError> {
        self.control_with_directory_intent(task_id, action, true)
            .await
    }

    async fn control_with_directory_intent(
        &self,
        task_id: Uuid,
        action: DownloadAction,
        directory_intent: bool,
    ) -> Result<DownloadTask, DownloadServiceError> {
        // Interrupt network activity before waiting for a possibly slow status
        // poll. Revalidate the GID under the operation lock before persisting.
        let mut interrupted_gid = None;
        if action == DownloadAction::Pause {
            self.ensure_accepting()?;
            if let Some(gid) = self.store.find(task_id)?.aria_gid.as_deref() {
                let version = self.resume_version(gid);
                if self.api().force_pause(gid).await.is_ok() {
                    interrupted_gid = Some((gid.to_owned(), version));
                }
            }
        }
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        if directory_intent {
            self.reconcile_task_unlocked(task_id).await?;
        }
        let task = self.store.find(task_id)?;
        let action = if directory_intent
            && matches!(action, DownloadAction::Pause | DownloadAction::Resume)
        {
            match self.store.cloud_directory_group_for_task(task_id)? {
                Some(group) if group.status == TaskStatus::Paused => DownloadAction::Pause,
                Some(group)
                    if matches!(group.status, TaskStatus::Cancelled | TaskStatus::Recycled) =>
                {
                    DownloadAction::Cancel
                }
                Some(_) => DownloadAction::Resume,
                None => action,
            }
        } else {
            action
        };
        let gid = task
            .aria_gid
            .as_deref()
            .ok_or(DownloadServiceError::MissingGid(task_id))?;
        match action {
            DownloadAction::Pause => {
                // forcePause is not idempotent: a second call can reject an
                // already accepted pause request. Reissue only for a new GID
                // or a resume that finished after the fast interrupt started.
                let still_interrupted =
                    interrupted_gid
                        .as_ref()
                        .is_some_and(|(interrupted, version)| {
                            interrupted == gid && *version == self.resume_version(gid)
                        });
                if !still_interrupted && task.status != TaskStatus::Paused {
                    if let Err(error) = self.api().force_pause(gid).await {
                        let latest = self.reconcile_task_unlocked(task_id).await?;
                        if matches!(latest.status, TaskStatus::Paused | TaskStatus::Completed) {
                            return Ok(latest);
                        }
                        return Err(error.into());
                    }
                }
            }
            DownloadAction::Resume => {
                if !directory_intent || task.status == TaskStatus::Paused {
                    self.unpause_gid(gid).await?;
                }
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
        self.prepare_magnet_with_options(task_id, magnet, UriSubmissionOptions::default())
            .await
    }

    pub async fn prepare_magnet_with_options(
        &self,
        task_id: Uuid,
        magnet: String,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.ensure_accepting()?;
        let task = self.task_for_uri_submission(task_id)?;
        let requested = match validate_download_input(&magnet) {
            Ok(candidate) => candidate,
            Err(error) => {
                let _ = self.store.fail(task.id, error.to_string());
                return Err(error.into());
            }
        };
        if requested.kind != crate::CandidateKind::Magnet || requested.source != task.source {
            let error = InputValidationError::InvalidMagnet;
            let _ = self.store.fail(task.id, error.to_string());
            return Err(error.into());
        }
        if task.aria_gid.is_some() {
            return self.reconcile_task_unlocked(task_id).await;
        }
        let mut options = options.into_aria2_options()?;
        options.insert("pause-metadata".into(), "true".into());
        // A magnet resolves to a torrent after metadata retrieval. Keep this on
        // the metadata task so its followed download verifies an existing payload
        // before retrying network pieces.
        options.insert("check-integrity".into(), "true".into());
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
        self.select_files_with_options(task_id, indices, Default::default())
            .await
    }

    pub async fn select_files_with_options(
        &self,
        task_id: Uuid,
        indices: Vec<u32>,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let _guard = self.operation.lock().await;
        self.select_files_with_options_unlocked(task_id, indices, options)
            .await
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
        indices: Vec<u32>,
    ) -> Result<DownloadTask, DownloadServiceError> {
        self.select_files_with_options_unlocked(task_id, indices, Default::default())
            .await
    }

    async fn select_files_with_options_unlocked(
        &self,
        task_id: Uuid,
        mut indices: Vec<u32>,
        options: UriSubmissionOptions,
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
        let mut aria2_options = options.into_aria2_options()?;
        aria2_options.insert("select-file".into(), selection);
        self.api().change_option(gid, aria2_options).await?;
        if task.status == TaskStatus::AwaitingUser && !self.store.selection_confirmed(task_id)? {
            self.unpause_gid(gid).await?;
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
            .filter(|task| {
                task.aria_gid.is_some()
                    && !matches!(task.status, TaskStatus::Cancelled | TaskStatus::Recycled)
            })
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
                        Ok(status) => match self.apply_status(task.clone(), status, false).await {
                            Ok(updated) => self.sync_discovered_nodes(updated).await,
                            Err(error) => Err(error),
                        },
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
        mut current: DownloadTask,
        mut status: Aria2Status,
        confirmed: bool,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let mut seen = std::collections::BTreeSet::new();
        if current.aria_gid.as_deref() != Some(status.gid.as_str())
            && current.metadata_gid.as_deref() != Some(status.gid.as_str())
        {
            return Err(Aria2Error::MalformedResponse.into());
        }
        for _ in 0..8 {
            if !seen.insert(status.gid.clone()) {
                return Err(Aria2Error::MalformedResponse.into());
            }
            let Some(next) = status.followed_by.first().cloned() else {
                if super::recovery::needs_recovery_name(&current) {
                    let name = super::recovery::recovery_name(&status).or_else(|| {
                        let files = self.store.files(current.id).ok()?;
                        super::recovery::recovery_file_name(
                            files.iter().map(|file| file.path.as_str()),
                        )
                    });
                    if let Some(name) = name {
                        current.name = name;
                    }
                }
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
                        && current.metadata_gid.as_deref() == Some(&snapshot.gid)
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
        let download_root = config.download_dir.clone();
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
                    self.observed_peers
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clear();
                    client
                };
                let recovery = self.recover_native(&download_root).await;
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
        self.clear_observed_peers(task.id);
        self.publish_task(&task)?;
        Ok(task)
    }

    fn persist_snapshot(
        &self,
        mut task: DownloadTask,
        snapshot: TransferSnapshot,
    ) -> Result<DownloadTask, DownloadServiceError> {
        // aria2 may briefly restore a paused session before it has reloaded its
        // length and file metadata. A paused task must not regress to an older
        // transfer snapshot, including one that only carries placeholder files.
        let preserve_paused_progress = task.status == TaskStatus::Paused
            && snapshot.status == TaskStatus::Paused
            && (snapshot.completed_length < task.completed_length
                || (task.total_length.is_some() && snapshot.total_length.is_none()));
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
        if !preserve_paused_progress {
            task.progress = snapshot.progress_percent.unwrap_or(0);
            task.total_length = snapshot.total_length;
            task.completed_length = snapshot.completed_length;
        }
        task.download_speed = snapshot.download_speed;
        task.upload_speed = snapshot.upload_speed;
        task.connections = snapshot.connections;
        task.eta_seconds = snapshot.eta_seconds;
        if task.status == TaskStatus::Completed {
            task.aria_error_code = None;
            task.error = None;
        } else {
            task.aria_error_code = snapshot.error_code;
            task.error = if task.source.starts_with("cloud-share://") {
                snapshot
                    .error_message
                    .map(|_| "网盘分享下载失败，请重试或重新解析分享链接".into())
            } else {
                snapshot.error_message
            };
        }
        let files = (!preserve_paused_progress)
            .then(|| {
                snapshot
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
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let task = self
            .store
            .apply_transfer_snapshot_with_files(task, &files)?;
        self.publish_task(&task)?;
        Ok(task)
    }

    async fn sync_discovered_nodes(
        &self,
        mut updated: DownloadTask,
    ) -> Result<(), DownloadServiceError> {
        if !matches!(updated.kind.as_str(), "magnet" | "torrent") {
            return Ok(());
        }
        if !matches!(updated.status, TaskStatus::Downloading) {
            if matches!(
                updated.status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Recycled
            ) {
                self.clear_observed_peers(updated.id);
            }
            return Ok(());
        }
        let Some(gid) = updated.aria_gid.as_deref() else {
            return Ok(());
        };
        if let Ok(peers) = self.api().get_peers(gid).await {
            let mut observed = self
                .observed_peers
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let seen = observed.entry(updated.id).or_default();
            seen.extend(
                peers
                    .into_iter()
                    .map(|peer| format!("{}:{}", peer.ip, peer.port)),
            );
            let discovered_nodes = seen.len().try_into().unwrap_or(u32::MAX);
            drop(observed);
            if updated.discovered_nodes != discovered_nodes {
                updated.discovered_nodes = discovered_nodes;
                let updated = self.store.apply_transfer_snapshot(updated)?;
                self.publish_task(&updated)?;
            }
        }
        Ok(())
    }

    fn clear_observed_peers(&self, task_id: Uuid) {
        self.observed_peers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&task_id);
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
