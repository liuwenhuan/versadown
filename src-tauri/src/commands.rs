use std::{
    collections::{HashMap, HashSet},
    fs,
    io::ErrorKind,
    path::Path,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::downloader::load_torrent_base64;
use crate::models::{CloudDirectoryGroup, CloudDirectoryGroupItem, QueueTaskPayload};
use crate::{
    cloud::{share::CloudShareConfirmation, CloudProvider, CredentialStore, DpapiCredentialStore},
    confirm_download, duplicate_state, prepare_download, resolve_magnet_torrent,
    retry_duplicate_state, AiSettings, AiSettingsInput, AppSettings, DownloadAction,
    DownloadServiceError, DownloadTask, DuplicateDownloadState, LauncherVisibility, ParseOutcome,
    ParseResource, ProxySettings, TaskStatus, TaskStore, TransferFile, UriSubmissionOptions,
};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParseDownloadOutcome {
    Task(ParseOutcome),
    ShareConfirmation(CloudShareConfirmation),
    AccessCodeRequired {
        provider: CloudProvider,
        message: &'static str,
    },
}

pub struct AppState {
    pub store: Arc<TaskStore>,
    pub runtime: Arc<crate::EngineRuntime>,
    pub debug_reports: Mutex<bool>,
    pub settings: Mutex<AppSettings>,
    pub settings_path: PathBuf,
    pub ai_secrets: DpapiCredentialStore,
    pub cloud_download_sources: crate::cloud::commands::CloudDownloadSourceGate,
}

#[derive(Clone, Serialize)]
pub struct DownloadTaskPayload {
    #[serde(flatten)]
    pub task: DownloadTask,
    pub files: Vec<TransferFile>,
    pub cleanup_pending: bool,
}

#[derive(Clone, Serialize)]
pub struct TaskActionOutcome {
    #[serde(flatten)]
    pub payload: DownloadTaskPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<DuplicateDownloadState>,
    /// Present only when an action affects a durable cloud-directory group.
    /// Ordinary task and torrent action JSON remains unchanged.
    #[serde(rename = "queueTask", skip_serializing_if = "Option::is_none")]
    pub queue_task: Option<QueueTaskPayload>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudShareBatchFailure {
    pub selected_id: String,
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudShareBatchOutcome {
    pub outcomes: Vec<TaskActionOutcome>,
    pub failures: Vec<CloudShareBatchFailure>,
}

pub(crate) fn task_payload(
    store: &TaskStore,
    task: DownloadTask,
) -> Result<DownloadTaskPayload, String> {
    task_payload_with_cleanup(store, None, task)
}

pub(crate) fn task_payload_with_cleanup(
    store: &TaskStore,
    plans: Option<&crate::cloud::share::CloudSharePlanStore>,
    task: DownloadTask,
) -> Result<DownloadTaskPayload, String> {
    let cleanup_pending =
        if task.status == TaskStatus::Completed && task.source.starts_with("cloud-share://") {
            match plans {
                Some(plans) => plans
                    .cleanup_pending_for_task(&task.source, task.id)
                    .map_err(|_| "无法读取网盘转存清理状态".to_string())?,
                None => false,
            }
        } else {
            false
        };
    let files = store.files(task.id).map_err(|error| error.to_string())?;
    Ok(DownloadTaskPayload {
        task,
        files,
        cleanup_pending,
    })
}

pub(crate) fn task_action_outcome(
    store: &TaskStore,
    task: DownloadTask,
    duplicate: Option<DuplicateDownloadState>,
) -> Result<TaskActionOutcome, String> {
    let queue_task = store
        .cloud_directory_group_for_task(task.id)
        .map_err(|error| error.to_string())?
        .map(|group| cloud_directory_group_queue_task(store, group.id))
        .transpose()?;
    Ok(TaskActionOutcome {
        payload: task_payload(store, task)?,
        duplicate,
        queue_task,
    })
}

fn share_action_parse_outcome(action: TaskActionOutcome) -> ParseDownloadOutcome {
    ParseDownloadOutcome::Task(ParseOutcome {
        task: action.payload.task,
        requires_confirmation: false,
        resources: Vec::new(),
        duplicate: action.duplicate,
    })
}

#[derive(Serialize)]
pub struct Settings {
    pub debug_reports: bool,
}

fn proxy_url(proxy: &ProxySettings) -> Result<Option<String>, String> {
    if !proxy.enabled {
        return Ok(None);
    }
    if !matches!(proxy.protocol.as_str(), "http" | "https") {
        return Err("aria2 仅支持 HTTP/HTTPS 代理".into());
    }
    if proxy.host.trim().is_empty() || proxy.host.contains("://") || proxy.port == 0 {
        return Err("代理服务器地址或端口无效".into());
    }
    let host = proxy.host.trim();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let mut value = url::Url::parse(&format!("{}://{}:{}", proxy.protocol, host, proxy.port))
        .map_err(|_| "代理服务器地址无效".to_string())?;
    if !proxy.username.is_empty() {
        value
            .set_username(&proxy.username)
            .map_err(|_| "代理用户名无效".to_string())?;
    }
    if !proxy.password.is_empty() {
        value
            .set_password(Some(&proxy.password))
            .map_err(|_| "代理密码无效".to_string())?;
    }
    Ok(Some(value.to_string()))
}

fn validated_download_directory(value: &str) -> Result<PathBuf, String> {
    let directory = PathBuf::from(value.trim());
    if directory.as_os_str().is_empty() || !directory.is_absolute() {
        return Err("下载目录必须是绝对路径".into());
    }
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建下载目录：{error}"))?;
    Ok(directory)
}

pub(crate) fn download_submission_options_for_directory(
    state: &AppState,
    destination_directory: Option<&str>,
) -> Result<UriSubmissionOptions, String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    let directory = destination_directory
        .map(str::to_owned)
        .unwrap_or_else(|| settings.download_directory.clone());
    let proxy = proxy_url(&settings.proxy)?;
    drop(settings);
    Ok(UriSubmissionOptions {
        directory: Some(validated_download_directory(&directory)?),
        proxy,
        ..Default::default()
    })
}

pub(crate) fn download_submission_options(
    state: &AppState,
) -> Result<UriSubmissionOptions, String> {
    download_submission_options_for_directory(state, None)
}

#[tauri::command]
pub async fn parse_download(
    app: AppHandle,
    input: String,
    access_code: Option<String>,
    state: State<'_, AppState>,
) -> Result<ParseDownloadOutcome, String> {
    if let Some(candidate) =
        crate::parser::extract_cloud_share_candidate(&input, access_code.as_deref())
            .map_err(|error| error.to_string())?
    {
        let provider = candidate.provider;
        return match crate::cloud::commands::inspect_cloud_share(app.clone(), candidate)
            .await
            .map_err(|error| error.to_string())?
        {
            crate::cloud::share::ShareConfirmationOutcome::Confirmation(plan) => {
                Ok(ParseDownloadOutcome::ShareConfirmation(plan))
            }
            crate::cloud::share::ShareConfirmationOutcome::AutoSubmit { source } => {
                let configured = download_submission_options(&state)?;
                let action = crate::cloud::commands::submit_cloud_share_file(
                    app, &state, source, configured,
                )
                .await
                .map_err(|error| error.to_string())?;
                Ok(share_action_parse_outcome(action))
            }
            crate::cloud::share::ShareConfirmationOutcome::AccessCodeRequired => {
                Ok(ParseDownloadOutcome::AccessCodeRequired {
                    provider,
                    message: "此分享需要访问码，请输入后重新解析",
                })
            }
        };
    }
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let mut outcome = prepare_download(&input, &state.store).map_err(|error| error.to_string())?;
    if outcome.duplicate.is_some() {
        return Ok(ParseDownloadOutcome::Task(outcome));
    }
    if outcome.task.kind == "torrent" {
        let options = download_submission_options(&state)?
            .into_aria2_options()
            .map_err(|error| error.to_string())?;
        let encoded = match load_torrent_base64(&outcome.task.source).await {
            Ok(encoded) => encoded,
            Err(error) => {
                let message = error.to_string();
                let _ = state.store.fail(outcome.task.id, message.clone());
                return Err(message);
            }
        };
        outcome.task = match service
            .submit_torrent(outcome.task.id, encoded, vec![], options)
            .await
        {
            Ok(task) => task,
            Err(error) => {
                let message = error.to_string();
                if state
                    .store
                    .find(outcome.task.id)
                    .is_ok_and(|task| task.status == TaskStatus::Parsing)
                {
                    let _ = state.store.fail(outcome.task.id, message.clone());
                }
                return Err(message);
            }
        };
        outcome.resources = parse_resources(&state.store, outcome.task.id)?;
    } else if !outcome.requires_confirmation {
        let options = download_submission_options(&state)?;
        outcome.task = service
            .submit_uri_with_options(outcome.task.id, vec![outcome.task.source.clone()], options)
            .await
            .map_err(|error| error.to_string())?;
    } else if outcome.task.kind == "magnet" {
        let options = download_submission_options(&state)?;
        let metadata_settings = state
            .settings
            .lock()
            .map_err(|_| "设置锁不可用".to_string())?
            .magnet_metadata
            .clone();
        if let Ok(Some(resolved)) =
            resolve_magnet_torrent(&metadata_settings, &outcome.task.source).await
        {
            if let Some(title) = resolved.title {
                outcome.task.name = title;
                outcome.task = state
                    .store
                    .apply_transfer_snapshot(outcome.task)
                    .map_err(|error| error.to_string())?;
            }
            outcome.task = service
                .submit_torrent(
                    outcome.task.id,
                    resolved.torrent,
                    vec![],
                    options
                        .into_aria2_options()
                        .map_err(|error| error.to_string())?,
                )
                .await
                .map_err(|error| error.to_string())?;
        } else {
            outcome.task = service
                .prepare_magnet_with_options(outcome.task.id, outcome.task.source.clone(), options)
                .await
                .map_err(|error| error.to_string())?;
            match service
                .wait_for_metadata(outcome.task.id, std::time::Duration::from_secs(12))
                .await
            {
                Ok(task) => outcome.task = task,
                Err(DownloadServiceError::MetadataTimeout) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        outcome.resources = parse_resources(&state.store, outcome.task.id)?;
    }
    Ok(ParseDownloadOutcome::Task(outcome))
}

#[tauri::command]
pub async fn confirm_cloud_share_download(
    app: AppHandle,
    plan_source: String,
    selected_ids: Vec<String>,
    destination_directory: Option<String>,
    state: State<'_, AppState>,
) -> Result<CloudShareBatchOutcome, crate::cloud::QuarkError> {
    let cloud = app
        .try_state::<Arc<crate::cloud::commands::CloudState>>()
        .ok_or(crate::cloud::QuarkError::Storage)?;
    let sources = crate::cloud::share::selected_share_sources(
        &cloud.share_plans,
        &plan_source,
        &selected_ids,
    )?;
    let configured =
        download_submission_options_for_directory(&state, destination_directory.as_deref())
            .map_err(crate::cloud::QuarkError::InvalidInput)?;
    let selections = selected_ids.into_iter().zip(sources).collect();
    let state = &*state;
    Ok(submit_cloud_share_batch_with(selections, |source| {
        let app = app.clone();
        let configured = configured.clone();
        async move {
            crate::cloud::commands::submit_cloud_share_file(app, state, source, configured).await
        }
    })
    .await)
}

async fn submit_cloud_share_batch_with<F, Fut>(
    selections: Vec<(String, String)>,
    mut submit: F,
) -> CloudShareBatchOutcome
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<TaskActionOutcome, crate::cloud::QuarkError>>,
{
    let mut result = CloudShareBatchOutcome {
        outcomes: Vec::new(),
        failures: Vec::new(),
    };
    for (selected_id, source) in selections {
        match submit(source).await {
            Ok(outcome) => result.outcomes.push(outcome),
            Err(error) => result.failures.push(CloudShareBatchFailure {
                selected_id,
                code: cloud_share_failure_code(&error),
                message: crate::cloud::commands::share_resolution_failure_message(&error),
            }),
        }
    }
    result
}

fn cloud_share_failure_code(error: &crate::cloud::QuarkError) -> &'static str {
    use crate::cloud::QuarkError;
    match error {
        QuarkError::NotAuthorized => "not_authorized",
        QuarkError::AuthorizationExpired => "credentials_expired",
        QuarkError::SessionRenewedRetryOperation => "session_renewed_retry_operation",
        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired => "web_auth_required",
        QuarkError::PikpakRegionalMembershipRestricted => "pikpak_regional_membership_restricted",
        QuarkError::AuthorizationPending => "authorization_pending",
        QuarkError::InvalidResponse => "invalid_response",
        QuarkError::Storage => "credential_storage_error",
        QuarkError::Transport(_) => "network_error",
        QuarkError::Provider(_) => "provider_error",
        QuarkError::InvalidInput(_) => "invalid_input",
        QuarkError::UploadUnavailable | QuarkError::ProviderUnsupported => "provider_unsupported",
    }
}

fn parse_resources(store: &TaskStore, task_id: Uuid) -> Result<Vec<ParseResource>, String> {
    Ok(store
        .files(task_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|file| ParseResource {
            id: file.index.to_string(),
            index: file.index,
            name: file.path,
            size: format!("{} B", file.length),
            selected: file.selected,
        })
        .collect())
}

#[tauri::command]
pub async fn confirm_download_command(
    task_id: String,
    selected_ids: Vec<String>,
    destination_directory: Option<String>,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    let options =
        download_submission_options_for_directory(&state, destination_directory.as_deref())?;
    if current.aria_gid.is_some() {
        let indices = selected_ids
            .into_iter()
            .map(|value| value.parse::<u32>().map_err(|_| "文件索引无效".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let task = service
            .select_files_with_options(id, indices, options)
            .await
            .map_err(|error| error.to_string())?;
        return task_payload(&state.store, task);
    }
    if let Some(error) = unbound_torrent_confirmation_error(&current) {
        return Err(error.to_string());
    }
    let task = confirm_download(id, &state.store).map_err(|error| error.to_string())?;
    let task = service
        .submit_uri_with_options(task.id, vec![task.source], options)
        .await
        .map_err(|error| error.to_string())?;
    task_payload(&state.store, task)
}

fn unbound_torrent_confirmation_error(task: &DownloadTask) -> Option<&'static str> {
    (task.kind == "torrent" && task.aria_gid.is_none())
        .then_some("种子文件尚未完成解析，请重新添加任务")
}

#[tauri::command]
pub fn list_tasks(
    state: State<'_, AppState>,
    app: AppHandle,
    scope: Option<String>,
) -> Result<Vec<QueueTaskPayload>, String> {
    let cloud = app.try_state::<Arc<crate::cloud::commands::CloudState>>();
    // Restore share-cleanup messages before reading the same projected list that
    // the command returns. The reusable helper below deliberately has no
    // application-state dependency, so the durable directory grouping boundary
    // can be exercised outside a running Tauri webview.
    if let Some(cloud) = cloud.as_ref() {
        let _ = cloud
            .share_plans
            .restore_cleanup_pending_messages(&state.store);
    }
    let mut tasks = list_queue_tasks_for_scope(&state.store, scope.as_deref())?;
    if let Some(plans) = cloud.as_ref().map(|cloud| &cloud.share_plans) {
        apply_queue_cleanup_projection(&mut tasks, plans)?;
    }
    Ok(tasks)
}

/// Returns the durable queue projection used by [`list_tasks`] for a scope.
/// Cloud-directory children are removed from the top level and nested beneath
/// their one synthetic folder root.
pub fn list_queue_tasks_for_scope(
    store: &TaskStore,
    scope: Option<&str>,
) -> Result<Vec<QueueTaskPayload>, String> {
    let leaves = list_tasks_with_cleanup_projection_for_scope(store, None, None)?;
    project_cloud_directory_groups(store, leaves, scope)
}

/// Produces the single durable queue root for one cloud-directory group.
/// This is intentionally derived through the same projection used by
/// `list_tasks`, so actions and task-update events cannot publish a stale or
/// flat representation of a directory member.
pub fn cloud_directory_group_queue_task(
    store: &TaskStore,
    group_id: Uuid,
) -> Result<QueueTaskPayload, String> {
    list_queue_tasks_for_scope(store, None)?
        .into_iter()
        .find(|task| task.id == group_id.to_string())
        .ok_or_else(|| "网盘目录下载任务不存在".to_string())
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum TaskUpdatePayload {
    Queue(QueueTaskPayload),
    Task(DownloadTaskPayload),
}

/// Selects the externally visible event representation for one task update.
/// A member of a cloud-directory group always updates its synthetic root.
pub(crate) fn task_update_payload(
    store: &TaskStore,
    plans: Option<&crate::cloud::share::CloudSharePlanStore>,
    task: DownloadTask,
) -> Result<TaskUpdatePayload, String> {
    if let Some(group) = store
        .cloud_directory_group_for_task(task.id)
        .map_err(|error| error.to_string())?
    {
        return cloud_directory_group_queue_task(store, group.id).map(TaskUpdatePayload::Queue);
    }
    task_payload_with_cleanup(store, plans, task).map(TaskUpdatePayload::Task)
}

fn apply_queue_cleanup_projection(
    tasks: &mut [QueueTaskPayload],
    plans: &crate::cloud::share::CloudSharePlanStore,
) -> Result<(), String> {
    for task in tasks {
        if task.status == TaskStatus::Completed && task.source.starts_with("cloud-share://") {
            task.cleanup_pending = plans
                .cleanup_pending_for_task(
                    &task.source,
                    Uuid::parse_str(&task.id).map_err(|_| "任务 ID 无效".to_string())?,
                )
                .map_err(|_| "无法读取网盘转存清理状态".to_string())?;
        }
        apply_queue_cleanup_projection(&mut task.children, plans)?;
    }
    Ok(())
}

fn queue_task_payload_from_task(
    store: &TaskStore,
    task: DownloadTask,
) -> Result<QueueTaskPayload, String> {
    let files = store.files(task.id).map_err(|error| error.to_string())?;
    Ok(QueueTaskPayload {
        id: task.id.to_string(),
        source: task.source,
        name: task.name,
        kind: task.kind,
        status: task.status,
        progress: task.progress,
        error: task.error,
        aria_gid: task.aria_gid,
        followed_by_gid: task.followed_by_gid,
        metadata_gid: task.metadata_gid,
        total_length: task.total_length,
        completed_length: task.completed_length,
        download_speed: task.download_speed,
        upload_speed: task.upload_speed,
        connections: task.connections,
        eta_seconds: task.eta_seconds,
        aria_error_code: task.aria_error_code,
        created_at: task.created_at,
        updated_at: task.updated_at,
        completed_at: task.completed_at,
        files,
        cleanup_pending: false,
        children: Vec::new(),
    })
}

fn task_is_active(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Recycled
    )
}

fn status_in_scope(status: TaskStatus, scope: Option<&str>) -> Result<bool, String> {
    match scope {
        None | Some("all") => Ok(true),
        Some("active") => Ok(task_is_active(status)),
        Some("terminal") => Ok(!task_is_active(status)),
        Some(_) => Err("任务列表范围无效".to_string()),
    }
}

fn virtual_cloud_directory_item_payload(
    group: &CloudDirectoryGroup,
    item: &CloudDirectoryGroupItem,
    error: Option<String>,
) -> QueueTaskPayload {
    QueueTaskPayload {
        id: item.id.to_string(),
        source: format!(
            "cloud-directory-item://{}/{}",
            group.provider.key(),
            item.id
        ),
        name: item.output_name.clone(),
        kind: "file".to_string(),
        status: TaskStatus::Failed,
        progress: 0,
        error,
        aria_gid: None,
        followed_by_gid: None,
        metadata_gid: None,
        total_length: item.size,
        completed_length: 0,
        download_speed: 0,
        upload_speed: 0,
        connections: 0,
        eta_seconds: None,
        aria_error_code: None,
        created_at: group.created_at,
        updated_at: group.updated_at,
        completed_at: None,
        files: Vec::new(),
        cleanup_pending: false,
        children: Vec::new(),
    }
}

fn cloud_directory_folder_payload(
    group: &CloudDirectoryGroup,
    components: &[String],
) -> QueueTaskPayload {
    let encoded_path: String =
        url::form_urlencoded::byte_serialize(components.join("/").as_bytes()).collect();
    QueueTaskPayload {
        id: format!("group:{}:dir:{encoded_path}", group.id),
        source: format!("cloud-directory://{}/{}", group.provider.key(), group.id),
        name: components
            .last()
            .cloned()
            .unwrap_or_else(|| group.relative_directory.clone()),
        kind: "folder".to_string(),
        status: TaskStatus::Completed,
        progress: 100,
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
        created_at: group.created_at,
        updated_at: group.updated_at,
        completed_at: None,
        files: Vec::new(),
        cleanup_pending: false,
        children: Vec::new(),
    }
}

fn insert_cloud_directory_leaf(
    group: &CloudDirectoryGroup,
    parent: &mut QueueTaskPayload,
    components: &[String],
    prefix: &mut Vec<String>,
    leaf: QueueTaskPayload,
) {
    let Some((component, rest)) = components.split_first() else {
        parent.children.push(leaf);
        return;
    };
    prefix.push(component.clone());
    let encoded: String =
        url::form_urlencoded::byte_serialize(prefix.join("/").as_bytes()).collect();
    let folder_id = format!("group:{}:dir:{encoded}", group.id);
    let index = parent
        .children
        .iter()
        .position(|child| child.id == folder_id)
        .unwrap_or_else(|| {
            parent
                .children
                .push(cloud_directory_folder_payload(group, prefix));
            parent.children.len() - 1
        });
    insert_cloud_directory_leaf(group, &mut parent.children[index], rest, prefix, leaf);
    prefix.pop();
}

fn aggregate_cloud_directory_root(
    group: &CloudDirectoryGroup,
    children: Vec<QueueTaskPayload>,
) -> QueueTaskPayload {
    fn collect_leaves<'a>(node: &'a QueueTaskPayload, leaves: &mut Vec<&'a QueueTaskPayload>) {
        if node.children.is_empty() {
            leaves.push(node);
        } else {
            for child in &node.children {
                collect_leaves(child, leaves);
            }
        }
    }
    let mut leaves = Vec::new();
    for child in &children {
        collect_leaves(child, &mut leaves);
    }
    // A directory group is a virtual queue root.  Once every real download
    // member has been recycled, it must move with those members even though a
    // retained virtual failure can still be shown below it.
    let linked_leaves = leaves
        .iter()
        .filter(|item| !item.source.starts_with("cloud-directory-item://"))
        .collect::<Vec<_>>();
    let all_linked_recycled = !linked_leaves.is_empty()
        && linked_leaves
            .iter()
            .all(|item| item.status == TaskStatus::Recycled);
    let has_active = leaves.iter().any(|item| task_is_active(item.status));
    let has_failure = leaves.iter().any(|item| {
        matches!(
            item.status,
            TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Recycled
        )
    });
    let all_completed = !leaves.is_empty()
        && leaves
            .iter()
            .all(|item| item.status == TaskStatus::Completed);
    let status = if all_linked_recycled {
        TaskStatus::Recycled
    } else if has_active {
        leaves
            .iter()
            .find(|item| task_is_active(item.status))
            .map(|item| item.status)
            .unwrap_or(TaskStatus::Queued)
    } else if all_completed {
        TaskStatus::Completed
    } else if has_failure {
        TaskStatus::Failed
    } else {
        TaskStatus::Failed
    };
    let has_known_total = leaves.iter().any(|item| item.total_length.is_some());
    let total_length = leaves
        .iter()
        .filter_map(|item| item.total_length)
        .sum::<u64>();
    let completed_length = leaves
        .iter()
        .filter(|item| item.total_length.is_some())
        .map(|item| item.completed_length)
        .sum::<u64>();
    let progress = if total_length == 0 {
        u8::from(status == TaskStatus::Completed) * 100
    } else {
        ((completed_length.saturating_mul(100) / total_length).min(100)) as u8
    };
    let updated_at = leaves
        .iter()
        .map(|item| item.updated_at)
        .max()
        .unwrap_or(group.updated_at)
        .max(group.updated_at);
    QueueTaskPayload {
        id: group.id.to_string(),
        source: format!("cloud-directory://{}/{}", group.provider.key(), group.id),
        name: group.relative_directory.clone(),
        kind: "folder".to_string(),
        status,
        progress,
        error: (status == TaskStatus::Failed)
            .then(|| leaves.iter().find_map(|item| item.error.clone()))
            .flatten(),
        aria_gid: None,
        followed_by_gid: None,
        metadata_gid: None,
        total_length: has_known_total.then_some(total_length),
        completed_length,
        download_speed: leaves.iter().map(|item| item.download_speed).sum(),
        upload_speed: leaves.iter().map(|item| item.upload_speed).sum(),
        connections: leaves.iter().map(|item| item.connections).sum(),
        eta_seconds: None,
        aria_error_code: None,
        created_at: group.created_at,
        updated_at,
        completed_at: (status == TaskStatus::Completed).then(|| {
            leaves
                .iter()
                .filter_map(|leaf| leaf.completed_at)
                .max()
                .unwrap_or(updated_at)
        }),
        files: Vec::new(),
        cleanup_pending: false,
        children,
    }
}

pub(crate) fn project_cloud_directory_groups(
    store: &TaskStore,
    leaves: Vec<DownloadTask>,
    scope: Option<&str>,
) -> Result<Vec<QueueTaskPayload>, String> {
    let groups = store
        .cloud_directory_groups()
        .map_err(|error| error.to_string())?;
    let mut group_items = Vec::with_capacity(groups.len());
    let mut grouped_task_ids = HashSet::new();
    for group in &groups {
        let items = store
            .cloud_directory_group_items(group.id)
            .map_err(|error| error.to_string())?;
        grouped_task_ids.extend(items.iter().filter_map(|item| item.task_id));
        group_items.push(items);
    }
    let mut task_by_id = leaves
        .iter()
        .cloned()
        .map(|task| (task.id, task))
        .collect::<HashMap<_, _>>();
    let mut projected = leaves
        .into_iter()
        .filter(|task| !grouped_task_ids.contains(&task.id))
        .filter(|task| status_in_scope(task.status, scope).unwrap_or(false))
        .map(|task| queue_task_payload_from_task(store, task))
        .collect::<Result<Vec<_>, _>>()?;
    for (group, items) in groups.into_iter().zip(group_items) {
        let mut root = cloud_directory_folder_payload(&group, &[]);
        root.id = group.id.to_string();
        root.name = group.relative_directory.clone();
        for item in items {
            let leaf = match item.task_id.and_then(|id| task_by_id.remove(&id)) {
                Some(task) => queue_task_payload_from_task(store, task)?,
                None => virtual_cloud_directory_item_payload(
                    &group,
                    &item,
                    item.failure_message
                        .clone()
                        .or_else(|| Some("关联的下载任务不存在，需重新提交网盘文件".to_string())),
                ),
            };
            // Planned paths include the selected directory itself because that
            // component is needed for the local output path. The synthetic
            // group root already represents it in the queue, so omit exactly
            // that leading component from its visible child tree.
            let visible_components = item
                .relative_directory
                .strip_prefix(&[group.relative_directory.clone()])
                .unwrap_or(&item.relative_directory);
            insert_cloud_directory_leaf(
                &group,
                &mut root,
                visible_components,
                &mut Vec::new(),
                leaf,
            );
        }
        let root = aggregate_cloud_directory_root(&group, root.children);
        if status_in_scope(root.status, scope)? {
            projected.push(root);
        }
    }
    Ok(projected)
}

fn list_tasks_with_cleanup_projection_for_scope(
    store: &TaskStore,
    plans: Option<&crate::cloud::share::CloudSharePlanStore>,
    scope: Option<&str>,
) -> Result<Vec<DownloadTask>, String> {
    // Listing remains available if credentials are unavailable. The projection
    // only reads the journal; even an Attempted record never triggers deletion.
    if let Some(plans) = plans {
        let _ = plans.restore_cleanup_pending_messages(store);
    }
    match scope {
        None | Some("all") => store.list(),
        Some("active") => store.list_active(),
        Some("terminal") => store.list_terminal(),
        Some(_) => return Err("任务列表范围无效".to_string()),
    }
    .map_err(|error| error.to_string())
}

pub(crate) fn list_tasks_with_cleanup_projection(
    store: &TaskStore,
    plans: Option<&crate::cloud::share::CloudSharePlanStore>,
) -> Result<Vec<DownloadTask>, String> {
    list_tasks_with_cleanup_projection_for_scope(store, plans, None)
}

fn force_cancel_unsubmitted_task(
    store: &TaskStore,
    mut task: DownloadTask,
) -> Result<DownloadTask, String> {
    if task.aria_gid.is_some()
        || !matches!(
            task.status,
            TaskStatus::Draft
                | TaskStatus::Parsing
                | TaskStatus::AwaitingUser
                | TaskStatus::Queued
                | TaskStatus::Downloading
                | TaskStatus::Processing
                | TaskStatus::Paused
        )
    {
        return Err("当前任务不能强制取消".to_string());
    }
    task.status = TaskStatus::Cancelled;
    task.download_speed = 0;
    task.upload_speed = 0;
    task.eta_seconds = None;
    store
        .apply_transfer_snapshot(task)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn task_action(
    task_id: String,
    action: String,
    app: AppHandle,
) -> Result<TaskActionOutcome, String> {
    if task_id.starts_with("group:") {
        return Err("目录节点仅用于展开，不能执行下载操作".to_string());
    }
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let state = app.state::<AppState>();
    if let Some(group) = state
        .store
        .cloud_directory_group(id)
        .map_err(|error| error.to_string())?
    {
        return run_cloud_directory_group_action(app.clone(), &state, &group, &action).await;
    }
    for group in state
        .store
        .cloud_directory_groups()
        .map_err(|error| error.to_string())?
    {
        if let Some(item) = state
            .store
            .cloud_directory_group_items(group.id)
            .map_err(|error| error.to_string())?
            .iter()
            .find(|item| item.id == id && item.task_id.is_none())
        {
            return match action.as_str() {
                "retry" => crate::cloud::commands::retry_cloud_directory_group_item(
                    app.clone(),
                    group.id,
                    item.id,
                )
                .await
                .map_err(|error| error.to_string()),
                _ => Err("未入队的网盘目录文件不能执行此操作".to_string()),
            };
        }
    }
    run_local_task_action(task_id, action, app).await
}

async fn run_cloud_directory_group_action(
    app: AppHandle,
    state: &AppState,
    group: &CloudDirectoryGroup,
    action: &str,
) -> Result<TaskActionOutcome, String> {
    if !matches!(action, "pause" | "resume" | "cancel" | "retry") {
        return Err("当前任务不能执行此操作".to_string());
    }
    let items = state
        .store
        .cloud_directory_group_items(group.id)
        .map_err(|error| error.to_string())?;
    let mut directory_retry =
        if action == "retry" && items.iter().any(|item| item.task_id.is_none()) {
            Some(
                crate::cloud::commands::retry_cloud_directory_group(app.clone(), group.id)
                    .await
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
    let mut first_outcome = None;
    for item in items {
        let Some(task_id) = item.task_id else {
            continue;
        };
        let current = state
            .store
            .find(task_id)
            .map_err(|error| error.to_string())?;
        let applicable = match action {
            "pause" => matches!(
                current.status,
                TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Processing
            ),
            "resume" => current.status == TaskStatus::Paused,
            "cancel" => task_is_active(current.status),
            "retry" => matches!(current.status, TaskStatus::Failed | TaskStatus::Cancelled),
            _ => false,
        };
        if applicable {
            let outcome =
                run_local_task_action(task_id.to_string(), action.to_string(), app.clone()).await?;
            first_outcome.get_or_insert(outcome);
        }
    }
    if let Some(outcome) = first_outcome {
        let mut outcome = directory_retry.take().unwrap_or(outcome);
        outcome.queue_task = Some(cloud_directory_group_queue_task(&state.store, group.id)?);
        return Ok(outcome);
    }
    if let Some(outcome) = directory_retry {
        return Ok(outcome);
    }
    Err("目录内没有可执行此操作的下载任务".to_string())
}

async fn run_local_task_action(
    task_id: String,
    action: String,
    app: AppHandle,
) -> Result<TaskActionOutcome, String> {
    let state = app.state::<AppState>();
    if action == "retry_cleanup" {
        let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
        let task = crate::cloud::commands::cleanup_cloud_share_task(app.clone(), id, true)
            .await
            .map_err(|_| "网盘转存文件清理未完成，可重试清理".to_string())?;
        let cloud = app.try_state::<Arc<crate::cloud::commands::CloudState>>();
        return Ok(TaskActionOutcome {
            payload: task_payload_with_cleanup(
                &state.store,
                cloud.as_ref().map(|cloud| &cloud.share_plans),
                task,
            )?,
            duplicate: None,
            queue_task: None,
        });
    }
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    if action == "cancel" && current.aria_gid.is_none() {
        let task = force_cancel_unsubmitted_task(&state.store, current)?;
        return task_action_outcome(&state.store, task, None);
    }
    if action == "retry" {
        if !matches!(current.status, TaskStatus::Failed | TaskStatus::Cancelled) {
            return Err("当前任务不能重新下载".to_string());
        }
        if let Some(duplicate) =
            retry_duplicate_state(&state.store, &current).map_err(|error| error.to_string())?
        {
            return task_action_outcome(&state.store, current, Some(duplicate));
        }
        let task = resubmit_task(app.clone(), &state, current, Default::default()).await?;
        return task_action_outcome(&state.store, task, None);
    }
    let action = match action.as_str() {
        "pause" => DownloadAction::Pause,
        "resume" => DownloadAction::Resume,
        "cancel" => DownloadAction::Cancel,
        _ => return Err("当前任务不能执行此操作".to_string()),
    };
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let task = service
        .control(id, action)
        .await
        .map_err(|error| error.to_string())?;
    task_action_outcome(&state.store, task, None)
}

#[tauri::command]
pub async fn resolve_duplicate_download(
    task_id: String,
    action: String,
    app: AppHandle,
) -> Result<DownloadTaskPayload, String> {
    let state = app.state::<AppState>();
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    let resolution = duplicate_state(&state.store, &current).map_err(|error| error.to_string())?;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let task = match (action.as_str(), resolution) {
        ("resume", DuplicateDownloadState::ResumeAvailable)
            if current.status == TaskStatus::Paused && current.aria_gid.is_some() =>
        {
            service
                .control(current.id, DownloadAction::Resume)
                .await
                .map_err(|error| error.to_string())?
        }
        ("resume", DuplicateDownloadState::ResumeAvailable) => {
            resubmit_task(
                app.clone(),
                &state,
                current,
                UriSubmissionOptions {
                    continue_download: true,
                    ..Default::default()
                },
            )
            .await?
        }
        ("restart_missing", DuplicateDownloadState::CompletedFilesMissing) => {
            resubmit_task(app.clone(), &state, current, Default::default()).await?
        }
        ("overwrite", DuplicateDownloadState::OverwriteRequired) => {
            resubmit_task(
                app.clone(),
                &state,
                current,
                UriSubmissionOptions {
                    allow_overwrite: true,
                    ..Default::default()
                },
            )
            .await?
        }
        _ => return Err("重复下载状态已变化，请重新提交下载链接".to_string()),
    };
    task_payload(&state.store, task)
}

async fn resubmit_task(
    app: AppHandle,
    state: &AppState,
    task: DownloadTask,
    mut options: UriSubmissionOptions,
) -> Result<DownloadTask, String> {
    // Match the cleanup worker's source gate for both retry and overwrite.
    let _share_submission = if task.source.starts_with("cloud-share://") {
        Some(
            state
                .cloud_download_sources
                .acquire(&task.source)
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if task.source.starts_with("cloud-share://") {
        let cloud = app
            .try_state::<Arc<crate::cloud::commands::CloudState>>()
            .ok_or_else(|| "网盘分享下载状态不可用，请重新解析分享链接".to_string())?;
        reserve_cloud_share_resubmission(&cloud.share_plans, &task)?;
    }
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let configured = download_submission_options(state)?;
    if options.directory.is_none() {
        options.directory = configured.directory;
    }
    if options.proxy.is_none() {
        options.proxy = configured.proxy;
    }
    if task.source.starts_with("cloud://") || task.source.starts_with("cloud-share://") {
        let share = task.source.starts_with("cloud-share://");
        let resolved = if share {
            crate::cloud::commands::resolve_stable_cloud_share_download(app, &task.source).await
        } else {
            crate::cloud::commands::resolve_stable_cloud_download(app, &task.source).await
        }
        .map_err(|error| {
            crate::cloud::commands::share_resolution_failure_message(&error).to_string()
        })?;
        if share {
            return crate::cloud::share::resubmit_current_share_download(
                &service, &task, resolved, options,
            )
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "任务状态已变化，本次重试已跳过".into());
        }
        let signed_url = resolved.url.clone();
        let mut cloud_options = resolved.into_uri_submission_options();
        cloud_options.directory = options.directory.clone();
        cloud_options.proxy = options.proxy.clone();
        cloud_options.allow_overwrite = options.allow_overwrite;
        cloud_options.continue_download = options.continue_download;
        return service
            .resubmit_uri_with_options(task.id, vec![signed_url], cloud_options)
            .await
            .map_err(|error| error.to_string());
    }
    if task.kind == "torrent" {
        return Err("种子任务请重新添加后再选择文件下载".to_string());
    }
    if task.kind == "magnet" {
        let selected_indices = state
            .store
            .files(task.id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|file| file.selected)
            .map(|file| file.index)
            .collect();
        let metadata_settings = state
            .settings
            .lock()
            .map_err(|_| "设置锁不可用".to_string())?
            .magnet_metadata
            .clone();
        let resolved = resolve_magnet_torrent(&metadata_settings, &task.source)
            .await
            .map_err(|error| format!("无法重新解析磁链：{error}"))?
            .ok_or_else(|| "磁链解析服务未返回种子文件，无法安全覆盖下载".to_string())?;
        if let Some(title) = resolved.title {
            let mut titled_task = task.clone();
            titled_task.name = title;
            state
                .store
                .apply_transfer_snapshot(titled_task)
                .map_err(|error| error.to_string())?;
        }
        return service
            .resubmit_torrent_with_options(
                task.id,
                resolved.torrent,
                vec![],
                selected_indices,
                options,
            )
            .await
            .map_err(|error| error.to_string());
    }
    options.output_name = None;
    service
        .resubmit_uri_with_options(task.id, vec![task.source], options)
        .await
        .map_err(|error| error.to_string())
}

fn reserve_cloud_share_resubmission(
    plans: &crate::cloud::share::CloudSharePlanStore,
    task: &DownloadTask,
) -> Result<(), String> {
    plans
        .reserve_local_task(&task.source, task.id)
        .map_err(|_| "无法准备网盘分享重试，请重新解析分享链接".to_string())
}

fn task_output_directory(store: &TaskStore, task_id: Uuid) -> Result<std::path::PathBuf, String> {
    store
        .files(task_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|file| {
            std::path::PathBuf::from(file.path)
                .parent()
                .map(|path| path.to_path_buf())
        })
        .find(|directory| directory.is_dir())
        .ok_or_else(|| "下载文件已不存在，无法打开所在目录".to_string())
}

/// Called after event publication; the task worker never blocks the aria2
/// synchronization callback. The source gate is released on every return path.
pub(crate) fn schedule_cloud_share_refresh(app: AppHandle, task: &DownloadTask) {
    if task.status != TaskStatus::Failed || !task.source.starts_with("cloud-share://") {
        return;
    }
    let Some(cloud) = app.try_state::<Arc<crate::cloud::commands::CloudState>>() else {
        return;
    };
    if !cloud
        .share_refresh_budget
        .claim(&task.source, task.aria_error_code.as_deref())
        .unwrap_or(false)
    {
        return;
    }
    let failed = task.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let Ok(_guard) = state.cloud_download_sources.acquire(&failed.source).await else {
            return;
        };
        let Ok(current) = state.store.find(failed.id) else {
            return;
        };
        // A user retry, cancellation, or recycle while waiting wins.
        if current.status != TaskStatus::Failed || current.aria_gid != failed.aria_gid {
            return;
        }
        if !matches!(retry_duplicate_state(&state.store, &current), Ok(None)) {
            return;
        }
        // One attempt only. Challenge/unsupported errors preserve the failed task.
        let Ok(resolved) = crate::cloud::commands::resolve_stable_cloud_share_download(
            app.clone(),
            &current.source,
        )
        .await
        else {
            return;
        };
        // Network resolution may outlive a user cancellation or manual retry.
        let Ok(latest) = state.store.find(failed.id) else {
            return;
        };
        if latest.status != TaskStatus::Failed || latest.aria_gid != failed.aria_gid {
            return;
        }
        if !matches!(retry_duplicate_state(&state.store, &latest), Ok(None)) {
            return;
        }
        let Ok(options) = download_submission_options(&state) else {
            return;
        };
        let Ok(service) = state.runtime.service().await else {
            return;
        };
        let _ = crate::cloud::share::resubmit_current_share_download(
            &service, &failed, resolved, options,
        )
        .await;
    });
}

const VIDEO_EXTENSIONS: &[&str] = &[
    "3gp", "asf", "avi", "flv", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "mts", "ts",
    "webm", "wmv",
];

fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            VIDEO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

fn task_video_paths(store: &TaskStore, task_id: Uuid) -> Result<Vec<PathBuf>, String> {
    Ok(store
        .files(task_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|file| file.selected)
        .map(|file| PathBuf::from(file.path))
        .filter(|path| path.is_file() && is_video_path(path))
        .collect())
}

fn video_playlist_path(task_id: Uuid, videos: &[PathBuf]) -> Result<PathBuf, String> {
    let directory = std::env::temp_dir().join("AllDownloader").join("playlists");
    fs::create_dir_all(&directory).map_err(|error| format!("无法准备播放列表目录：{error}"))?;
    let playlist = directory.join(format!("{task_id}-{}.m3u", Uuid::new_v4()));
    let entries = videos
        .iter()
        .map(|path| path.to_string_lossy().replace(['\r', '\n'], ""))
        .collect::<Vec<_>>()
        .join("\r\n");
    fs::write(&playlist, format!("#EXTM3U\r\n{entries}\r\n"))
        .map_err(|error| format!("无法创建视频播放列表：{error}"))?;
    Ok(playlist)
}

#[tauri::command]
pub fn open_task_directory(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let directory = match state
        .store
        .cloud_directory_group(id)
        .map_err(|error| error.to_string())?
    {
        Some(group) => cloud_directory_group_output_root(&group)?,
        None => task_output_directory(&state.store, id)?,
    };
    tauri_plugin_opener::open_path(&directory, None::<&str>)
        .map_err(|error| format!("无法打开下载目录：{error}"))
}

fn cloud_directory_group_output_root(group: &CloudDirectoryGroup) -> Result<PathBuf, String> {
    let relative = Path::new(&group.relative_directory);
    if relative.is_absolute() || relative.components().count() != 1 {
        return Err("目录下载任务的输出目录不安全，无法打开".to_string());
    }
    let Some(name) = relative.file_name().filter(|name| !name.is_empty()) else {
        return Err("目录下载任务缺少输出目录名称".to_string());
    };
    let root = group.output_directory.join(name);
    if !root.starts_with(&group.output_directory) {
        return Err("目录下载任务的输出目录不安全，无法打开".to_string());
    }
    Ok(root)
}

#[tauri::command]
pub fn play_task_videos(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = state.store.find(id).map_err(|error| error.to_string())?;
    if task.status != TaskStatus::Completed {
        return Err("仅已完成任务可以播放视频".to_string());
    }
    let videos = task_video_paths(&state.store, id)?;
    if videos.is_empty() {
        return Err("此任务中没有可播放的视频文件".to_string());
    }
    let target = if videos.len() == 1 {
        videos.into_iter().next().expect("checked non-empty")
    } else {
        video_playlist_path(id, &videos)?
    };
    tauri_plugin_opener::open_path(&target, None::<&str>)
        .map_err(|error| format!("无法调用系统默认播放器：{error}"))
}

#[tauri::command]
pub fn remove_task_record(
    task_id: String,
    state: State<'_, AppState>,
    cloud: State<'_, Arc<crate::cloud::commands::CloudState>>,
) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = state.store.find(id).map_err(|error| error.to_string())?;
    if matches!(
        task.status,
        TaskStatus::Draft
            | TaskStatus::Parsing
            | TaskStatus::AwaitingUser
            | TaskStatus::Queued
            | TaskStatus::Downloading
            | TaskStatus::Processing
            | TaskStatus::Paused
    ) {
        return Err("请先取消活动下载任务，再移除任务记录".to_string());
    }
    remove_task_record_and_unlink_share(&state.store, &cloud.share_plans, &task)
}

fn remove_task_record_and_unlink_share(
    store: &TaskStore,
    plans: &crate::cloud::share::CloudSharePlanStore,
    task: &DownloadTask,
) -> Result<(), String> {
    store
        .remove_task_record(task.id)
        .map_err(|error| error.to_string())?;
    if task.source.starts_with("cloud-share://") {
        plans
            .unlink_local_task(&task.source, task.id)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn recycle_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<QueueTaskPayload, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    if state
        .store
        .cloud_directory_group(id)
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return recycle_cloud_directory_group(&state.store, id);
    }
    let task = state.store.find(id).map_err(|error| error.to_string())?;
    if !matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        return Err("请先取消活动下载任务，再移入回收站".to_string());
    }
    let recycled = state
        .store
        .transition(id, TaskStatus::Recycled)
        .map_err(|error| error.to_string())?;
    queue_task_payload_from_task(&state.store, recycled)
}

fn recycle_cloud_directory_group(
    store: &TaskStore,
    group_id: Uuid,
) -> Result<QueueTaskPayload, String> {
    let group = store
        .cloud_directory_group(group_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "目录下载任务不存在".to_string())?;
    let members = store
        .cloud_directory_group_items(group.id)
        .map_err(|error| error.to_string())?;
    let mut tasks = Vec::new();
    for member in members {
        let Some(task_id) = member.task_id else {
            continue;
        };
        let task = store.find(task_id).map_err(|error| error.to_string())?;
        if !matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Recycled
        ) {
            return Err("请先取消目录中的活动下载任务，再移入回收站".to_string());
        }
        tasks.push(task);
    }
    if tasks.is_empty() {
        return Err("目录下载任务中没有可移入回收站的文件".to_string());
    }
    for task in tasks {
        if task.status != TaskStatus::Recycled {
            store
                .transition(task.id, TaskStatus::Recycled)
                .map_err(|error| error.to_string())?;
        }
    }
    cloud_directory_group_queue_task(store, group.id)
}

fn remove_recorded_download_file(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("拒绝删除非绝对路径的下载文件：{}", path.display()));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            fs::remove_file(path)
                .map_err(|error| format!("无法删除下载文件 {}：{error}", path.display()))
        }
        Ok(_) => Err(format!("拒绝删除非文件下载项：{}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法读取下载文件 {}：{error}", path.display())),
    }
}

fn remove_task_payload_files(store: &TaskStore, task_id: Uuid) -> Result<(), String> {
    for file in store.files(task_id).map_err(|error| error.to_string())? {
        let path = PathBuf::from(file.path);
        remove_recorded_download_file(&path)?;
        let mut sidecar = path.into_os_string();
        sidecar.push(".aria2");
        remove_recorded_download_file(Path::new(&sidecar))?;
    }
    Ok(())
}

#[tauri::command]
pub fn purge_recycled_tasks(
    task_ids: Vec<String>,
    delete_files: bool,
    state: State<'_, AppState>,
    cloud: State<'_, Arc<crate::cloud::commands::CloudState>>,
) -> Result<(), String> {
    let mut ids = Vec::with_capacity(task_ids.len());
    let mut group_ids = Vec::new();
    for task_id in task_ids {
        let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
        if state
            .store
            .cloud_directory_group(id)
            .map_err(|error| error.to_string())?
            .is_some()
        {
            if !group_ids.contains(&id) {
                group_ids.push(id);
            }
        } else if !ids.contains(&id) {
            ids.push(id);
        }
    }
    purge_recycled_task_records(&state.store, &cloud.share_plans, &ids, delete_files)?;
    for group_id in group_ids {
        purge_cloud_directory_group(&state.store, &cloud.share_plans, group_id, delete_files)?;
    }
    Ok(())
}

fn cloud_directory_group_tasks_for_purge(
    store: &TaskStore,
    group_id: Uuid,
) -> Result<(CloudDirectoryGroup, Vec<DownloadTask>), String> {
    let group = store
        .cloud_directory_group(group_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "目录下载任务不存在".to_string())?;
    let mut tasks = Vec::new();
    for item in store
        .cloud_directory_group_items(group.id)
        .map_err(|error| error.to_string())?
    {
        let Some(task_id) = item.task_id else {
            continue;
        };
        let task = store.find(task_id).map_err(|error| error.to_string())?;
        if !matches!(task.status, TaskStatus::Recycled | TaskStatus::Cancelled) {
            return Err("只有回收站中的目录下载任务可以彻底删除".to_string());
        }
        tasks.push(task);
    }
    if tasks.is_empty() {
        return Err("目录下载任务中没有可彻底删除的文件".to_string());
    }
    Ok((group, tasks))
}

fn purge_cloud_directory_group(
    store: &TaskStore,
    plans: &crate::cloud::share::CloudSharePlanStore,
    group_id: Uuid,
    delete_files: bool,
) -> Result<(), String> {
    let (group, tasks) = cloud_directory_group_tasks_for_purge(store, group_id)?;
    let task_ids = tasks.iter().map(|task| task.id).collect::<Vec<_>>();
    purge_recycled_task_records(store, plans, &task_ids, delete_files)?;
    store
        .remove_cloud_directory_group(group.id)
        .map_err(|error| error.to_string())
}

fn purge_recycled_task_records(
    store: &TaskStore,
    plans: &crate::cloud::share::CloudSharePlanStore,
    ids: &[Uuid],
    delete_files: bool,
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut tasks = Vec::with_capacity(ids.len());
    for id in ids {
        let task = store.find(*id).map_err(|error| error.to_string())?;
        if !matches!(task.status, TaskStatus::Recycled | TaskStatus::Cancelled) {
            return Err("只有回收站中的任务可以彻底删除".to_string());
        }
        tasks.push(task);
    }
    if delete_files {
        for id in ids {
            remove_task_payload_files(store, *id)?;
        }
    }
    for task in tasks {
        remove_task_record_and_unlink_share(store, plans, &task)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn select_files(
    task_id: String,
    indices: Vec<u32>,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = service
        .select_files(id, indices)
        .await
        .map_err(|error| error.to_string())?;
    task_payload(&state.store, task)
}

#[tauri::command]
pub async fn cancel_download_file(
    task_id: String,
    file_index: u32,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    if !state
        .store
        .files(id)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|file| file.index == file_index)
    {
        return Err("文件索引无效".to_string());
    }
    let task = service
        .set_file_selected(id, file_index, false)
        .await
        .map_err(|error| error.to_string())?;
    task_payload(&state.store, task)
}

#[tauri::command]
pub fn get_download_engine_state(state: State<'_, AppState>) -> crate::EngineAvailability {
    state.runtime.status()
}

#[tauri::command]
pub async fn retry_download_engine(
    state: State<'_, AppState>,
) -> Result<crate::EngineAvailability, String> {
    let result = state.runtime.retry().await;
    if result.available {
        Ok(result)
    } else {
        Err(result
            .error
            .unwrap_or_else(|| "下载引擎不可用，请稍后重试".into()))
    }
}

#[tauri::command]
pub async fn get_download_global_stat(
    state: State<'_, AppState>,
) -> Result<crate::Aria2GlobalStat, String> {
    state
        .runtime
        .global_stat()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())
        .map(|settings| settings.clone())
}

fn persist_settings(path: &Path, settings: &AppSettings) -> Result<(), String> {
    let json =
        serde_json::to_vec_pretty(settings).map_err(|error| format!("无法保存设置：{error}"))?;
    fs::write(path, json).map_err(|error| format!("无法保存设置：{error}"))
}

pub fn update_launcher_visibility_from_native_menu(
    app: &AppHandle,
    visibility: LauncherVisibility,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "应用设置尚未初始化".to_string())?;
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    if settings.launcher_visibility == visibility {
        return Ok(());
    }
    settings.launcher_visibility = visibility;
    persist_settings(&state.settings_path, &settings)?;
    let _ = app.emit_to("launcher", "launcher-visibility-updated", visibility);
    Ok(())
}

#[tauri::command]
pub fn update_ai_settings(
    mut settings: AiSettingsInput,
    state: State<'_, AppState>,
) -> Result<AiSettings, String> {
    settings.base_url = settings.base_url.trim().to_string();
    settings.model = settings.model.trim().to_string();
    let mut api_key = settings.api_key.trim().to_string();
    let has_new_key = !api_key.is_empty();
    let has_saved_key = state
        .ai_secrets
        .load(crate::ai::API_KEY_SECRET_NAME)
        .map_err(|_| "无法读取本机保存的 AI API Key".to_string())?
        .is_some();
    if settings.enabled {
        crate::ai::normalize_base_url(&settings.base_url)?;
        if settings.model.is_empty() {
            return Err("请选择 AI 模型".into());
        }
    }
    if has_new_key {
        state
            .ai_secrets
            .save(crate::ai::API_KEY_SECRET_NAME, api_key.as_bytes())
            .map_err(|_| "无法安全保存 AI API Key".to_string())?;
    }
    zeroize::Zeroize::zeroize(&mut api_key);
    zeroize::Zeroize::zeroize(&mut settings.api_key);
    let configured = has_saved_key || has_new_key;
    if settings.enabled && !configured {
        return Err("启用 AI 智能分析前请先配置 API Key".into());
    }
    let saved = AiSettings {
        enabled: settings.enabled,
        base_url: settings.base_url,
        model: settings.model,
        api_key_configured: configured,
    };
    let mut app_settings = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    app_settings.ai = saved.clone();
    persist_settings(&state.settings_path, &app_settings)?;
    Ok(saved)
}

#[tauri::command]
pub async fn list_ai_models(
    settings: AiSettingsInput,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut api_key = settings.api_key.trim().to_string();
    if api_key.is_empty() {
        api_key = state
            .ai_secrets
            .load(crate::ai::API_KEY_SECRET_NAME)
            .map_err(|_| "无法读取本机保存的 AI API Key".to_string())?
            .and_then(|value| String::from_utf8(value).ok())
            .ok_or_else(|| "请先输入或保存 AI API Key".to_string())?;
    }
    let result = crate::ai::fetch_models(&settings.base_url, &api_key).await;
    zeroize::Zeroize::zeroize(&mut api_key);
    result
}

#[tauri::command]
pub fn update_debug_reports(enabled: bool, state: State<'_, AppState>) -> Result<Settings, String> {
    let mut setting = state
        .debug_reports
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    *setting = enabled;
    if let Ok(mut settings) = state.settings.lock() {
        settings.debug_reports = enabled;
    }
    Ok(Settings {
        debug_reports: *setting,
    })
}

#[tauri::command]
pub async fn update_settings(
    mut settings: AppSettings,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<AppSettings, String> {
    settings.download_directory = settings.download_directory.trim().to_string();
    let download_directory = PathBuf::from(&settings.download_directory);
    if settings.download_directory.is_empty() || !download_directory.is_absolute() {
        return Err("下载目录必须是绝对路径".into());
    }
    std::fs::create_dir_all(&download_directory)
        .map_err(|error| format!("无法创建下载目录：{error}"))?;
    settings.proxy.host = settings.proxy.host.trim().to_string();
    settings.proxy.username = settings.proxy.username.trim().to_string();
    proxy_url(&settings.proxy)?;
    settings.aria2.rpc_token = settings.aria2.rpc_token.trim().to_string();
    settings.aria2.validate()?;

    let previous = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?
        .clone();
    settings.ai = previous.ai;
    if settings.aria2 != previous.aria2 {
        state
            .runtime
            .reconfigure_aria2_settings(settings.aria2.clone())
            .await?;
    }
    persist_settings(&state.settings_path, &settings)?;
    *state
        .debug_reports
        .lock()
        .map_err(|_| "设置锁不可用".to_string())? = settings.debug_reports;
    *state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())? = settings.clone();
    let _ = app.emit_to(
        "launcher",
        "launcher-visibility-updated",
        settings.launcher_visibility,
    );
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cloud::{share::CloudShareEntry, share::CloudSharePlan, CloudProvider},
        models::{NewCloudDirectoryGroup, NewCloudDirectoryGroupItem},
        DuplicateDownloadState, NewTask,
    };

    #[test]
    fn cloud_directory_group_projection_nests_linked_and_virtual_leaves() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Baidu,
                    "remote-season-1",
                    "Season 1",
                    "C:\\Downloads",
                ),
                &[
                    NewCloudDirectoryGroupItem::new("active", &["Extras"], "active.mkv", Some(100)),
                    NewCloudDirectoryGroupItem::new(
                        "complete",
                        &["Extras"],
                        "complete.mkv",
                        Some(200),
                    ),
                    NewCloudDirectoryGroupItem::new("failed", &["Extras"], "failed.mkv", Some(300)),
                ],
            )
            .unwrap();
        let items = store.cloud_directory_group_items(group.id).unwrap();

        let active = store
            .create(NewTask::direct("https://example.test/active"))
            .unwrap();
        let active = store
            .apply_transfer_snapshot(DownloadTask {
                name: "active.mkv".into(),
                status: TaskStatus::Downloading,
                progress: 25,
                total_length: Some(100),
                completed_length: 25,
                download_speed: 10,
                connections: 2,
                ..active
            })
            .unwrap();
        let complete = store
            .create(NewTask::direct("https://example.test/complete"))
            .unwrap();
        let complete = store
            .apply_transfer_snapshot(DownloadTask {
                name: "complete.mkv".into(),
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(200),
                completed_length: 200,
                ..complete
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, items[0].id, active.id)
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, items[1].id, complete.id)
            .unwrap();
        store
            .fail_cloud_directory_group_item(
                group.id,
                items[2].id,
                "submit_failed",
                "不能加入下载队列",
            )
            .unwrap();

        let active_scope =
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("active")).unwrap();

        assert_eq!(active_scope.len(), 1);
        assert_eq!(active_scope[0].kind, "folder");
        assert_eq!(active_scope[0].name, "Season 1");
        assert_eq!(active_scope[0].status, TaskStatus::Downloading);
        assert_eq!(active_scope[0].total_length, Some(600));
        assert_eq!(active_scope[0].completed_length, 225);
        assert_eq!(active_scope[0].download_speed, 10);
        assert_eq!(active_scope[0].connections, 2);
        assert_eq!(active_scope[0].progress, 37);
        assert_eq!(active_scope[0].children[0].name, "Extras");
        assert_eq!(active_scope[0].children[0].children.len(), 3);
        assert!(active_scope[0].children[0]
            .children
            .iter()
            .any(|child| child.status == TaskStatus::Failed));

        store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(100),
                completed_length: 100,
                download_speed: 0,
                connections: 0,
                ..active
            })
            .unwrap();

        assert!(
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("active"))
                .unwrap()
                .is_empty()
        );
        let terminal_scope =
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("terminal"))
                .unwrap();
        assert_eq!(terminal_scope.len(), 1);
        assert_eq!(terminal_scope[0].status, TaskStatus::Failed);
    }

    #[test]
    fn cloud_directory_group_projection_ignores_unknown_size_progress() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Baidu,
                    "remote-directory",
                    "Unknown size",
                    "C:\\Downloads",
                ),
                &[
                    NewCloudDirectoryGroupItem::new(
                        "known",
                        &[] as &[&str],
                        "known.bin",
                        Some(100),
                    ),
                    NewCloudDirectoryGroupItem::new("unknown", &[] as &[&str], "unknown.bin", None),
                ],
            )
            .unwrap();
        let items = store.cloud_directory_group_items(group.id).unwrap();
        let known = store
            .create(NewTask::direct("https://example.test/known"))
            .unwrap();
        let known = store
            .apply_transfer_snapshot(DownloadTask {
                total_length: Some(100),
                completed_length: 25,
                progress: 25,
                status: TaskStatus::Downloading,
                ..known
            })
            .unwrap();
        let unknown = store
            .create(NewTask::direct("https://example.test/unknown"))
            .unwrap();
        let unknown = store
            .apply_transfer_snapshot(DownloadTask {
                total_length: None,
                completed_length: 900,
                progress: 0,
                status: TaskStatus::Downloading,
                ..unknown
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, items[0].id, known.id)
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, items[1].id, unknown.id)
            .unwrap();

        let projected =
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("active")).unwrap();

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].total_length, Some(100));
        assert_eq!(projected[0].completed_length, 25);
        assert_eq!(projected[0].progress, 25);
    }

    #[test]
    fn directory_member_update_emits_its_recursive_group_root() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "remote-directory",
                    "课程",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-file",
                    &["资料"],
                    "讲义.pdf",
                    Some(100),
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let mut new_task = NewTask::direct("cloud://quark/remote-file");
        new_task.name = "讲义.pdf".into();
        let task = store.create(new_task).unwrap();
        let task = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Downloading,
                total_length: Some(100),
                completed_length: 50,
                progress: 50,
                ..task
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        let TaskUpdatePayload::Queue(payload) = task_update_payload(&store, None, task).unwrap()
        else {
            panic!("a cloud-directory member update must not emit a top-level leaf");
        };
        assert_eq!(payload.id, group.id.to_string());
        assert_eq!(payload.progress, 50);
        assert_eq!(payload.children[0].name, "资料");
        assert_eq!(payload.children[0].children[0].name, "讲义.pdf");
    }

    #[test]
    fn local_action_outcome_for_a_directory_member_returns_its_group_root() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "remote-directory",
                    "课程",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-file",
                    &[] as &[&str],
                    "讲义.pdf",
                    Some(100),
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let task = store
            .create(NewTask::direct("cloud://quark/remote-file"))
            .unwrap();
        let task = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Paused,
                total_length: Some(100),
                completed_length: 40,
                progress: 40,
                ..task
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        let action = task_action_outcome(&store, task, None).unwrap();
        let root = action
            .queue_task
            .expect("a local directory member action must return its group root");
        assert_eq!(root.id, group.id.to_string());
        assert_eq!(root.progress, 40);
        assert_eq!(root.children[0].name, "remote-file");
    }

    #[cfg(windows)]
    #[test]
    fn cloud_directory_group_recycles_and_purges_its_real_and_virtual_members() {
        let output = tempfile::tempdir().unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "remote-directory",
                    "Season 1",
                    output.path(),
                ),
                &[
                    NewCloudDirectoryGroupItem::new(
                        "remote-complete",
                        &[] as &[&str],
                        "episode.mkv",
                        Some(100),
                    ),
                    NewCloudDirectoryGroupItem::new(
                        "remote-failed",
                        &[] as &[&str],
                        "missing.mkv",
                        Some(100),
                    ),
                ],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let task = store
            .create(NewTask::direct("cloud://quark/remote-complete"))
            .unwrap();
        let task = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(100),
                completed_length: 100,
                ..task
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();
        let failed = store.cloud_directory_group_items(group.id).unwrap()[1].id;
        store
            .fail_cloud_directory_group_item(group.id, failed, "provider_error", "link failed")
            .unwrap();

        assert_eq!(
            cloud_directory_group_output_root(&group).unwrap(),
            output.path().join("Season 1")
        );
        let recycled = recycle_cloud_directory_group(&store, group.id).unwrap();
        assert_eq!(recycled.status, TaskStatus::Recycled);
        assert_eq!(
            cloud_directory_group_queue_task(&store, group.id)
                .unwrap()
                .status,
            TaskStatus::Recycled
        );

        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(output.path().to_path_buf()).unwrap(),
        );
        purge_cloud_directory_group(&store, &plans, group.id, false).unwrap();
        assert!(store.cloud_directory_group(group.id).unwrap().is_none());
        assert!(matches!(
            store.find(task.id),
            Err(crate::StoreError::NotFound(_))
        ));
    }

    #[test]
    fn unsubmitted_active_task_can_be_force_cancelled_without_engine() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("not-a-download-link"))
            .unwrap();
        assert_eq!(task.status, TaskStatus::Parsing);
        assert!(task.aria_gid.is_none());

        let cancelled = force_cancel_unsubmitted_task(&store, task).unwrap();

        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(cancelled.download_speed, 0);
        assert_eq!(cancelled.upload_speed, 0);
        assert_eq!(
            store.find(cancelled.id).unwrap().status,
            TaskStatus::Cancelled
        );
    }

    #[cfg(windows)]
    #[test]
    fn permanent_task_delete_unlinks_only_after_sqlite_delete_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "delete-order".into(),
            None,
            vec![CloudShareEntry::new(
                "delete-order",
                "entry".into(),
                "file.bin".into(),
                vec![],
                Some(1),
            )],
            serde_json::json!({}),
        );
        let source = plan.source_for("entry");
        plans.upsert(&plan).unwrap();
        plans
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create(NewTask::direct(&source)).unwrap();
        plans.link_local_task(&source, task.id).unwrap();

        store.remove_task_record(task.id).unwrap();
        assert!(remove_task_record_and_unlink_share(&store, &plans, &task).is_err());
        assert!(plans
            .resolve_entry(&source)
            .unwrap()
            .plan
            .linked_local_tasks
            .values()
            .any(|ids| ids.contains(&task.id)));
    }

    #[cfg(windows)]
    #[test]
    fn permanent_task_delete_uses_original_exact_source_and_id_for_unlink() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let mut plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "exact-delete".into(),
            None,
            vec![CloudShareEntry::new(
                "exact-delete",
                "first".into(),
                "first.bin".into(),
                vec![],
                Some(1),
            )],
            serde_json::json!({}),
        );
        plan.entries.push(CloudShareEntry::new(
            "exact-delete",
            "second".into(),
            "second.bin".into(),
            vec![],
            Some(2),
        ));
        let first_source = plan.source_for("first");
        let second_source = plan.source_for("second");
        plans.upsert(&plan).unwrap();
        plans
            .link_local_sources(&[first_source.clone(), second_source.clone()])
            .unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let first = store.create(NewTask::direct(&first_source)).unwrap();
        let second = store.create(NewTask::direct(&second_source)).unwrap();
        plans.link_local_task(&first_source, first.id).unwrap();
        plans.link_local_task(&second_source, second.id).unwrap();

        remove_task_record_and_unlink_share(&store, &plans, &first).unwrap();

        assert!(matches!(
            store.find(first.id),
            Err(crate::StoreError::NotFound(_))
        ));
        assert!(store.find(second.id).is_ok());
        let remaining = plans.resolve_entry(&second_source).unwrap().plan;
        assert_eq!(remaining.linked_local_tasks.len(), 1);
        assert!(remaining
            .linked_local_tasks
            .values()
            .any(|ids| ids.contains(&second.id)));
    }

    #[cfg(windows)]
    #[test]
    fn permanent_purge_unlinks_each_recycled_cloud_share_task_after_sqlite_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "purge".into(),
            None,
            vec![CloudShareEntry::new(
                "purge",
                "entry".into(),
                "file.bin".into(),
                vec![],
                Some(1),
            )],
            serde_json::json!({}),
        );
        let source = plan.source_for("entry");
        plans.upsert(&plan).unwrap();
        plans
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create(NewTask::direct(&source)).unwrap();
        let task = store.fail(task.id, "fixture").unwrap();
        let task = store.transition(task.id, TaskStatus::Recycled).unwrap();
        plans.link_local_task(&source, task.id).unwrap();

        purge_recycled_task_records(&store, &plans, &[task.id], false).unwrap();

        assert!(matches!(
            store.find(task.id),
            Err(crate::StoreError::NotFound(_))
        ));
        assert!(plans
            .resolve_entry(&source)
            .unwrap()
            .plan
            .linked_local_tasks
            .is_empty());
    }

    #[test]
    fn disabled_proxy_does_not_leak_process_proxy_settings_into_aria2() {
        assert_eq!(proxy_url(&ProxySettings::default()).unwrap(), None);
    }

    #[test]
    fn automatic_share_submission_preserves_the_existing_duplicate_outcome() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("cloud-share://baidu/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .unwrap();
        let action = task_action_outcome(
            &store,
            task.clone(),
            Some(DuplicateDownloadState::ExistingActive),
        )
        .unwrap();

        let ParseDownloadOutcome::Task(parsed) = share_action_parse_outcome(action) else {
            panic!("an automatic share must return the ordinary task parse branch")
        };
        assert_eq!(parsed.task.id, task.id);
        assert_eq!(
            parsed.duplicate,
            Some(DuplicateDownloadState::ExistingActive)
        );
        assert!(!parsed.requires_confirmation);
        assert!(parsed.resources.is_empty());
    }

    #[test]
    fn cloud_share_resubmission_reserves_the_original_task_before_engine_or_provider_work() {
        let production = include_str!("commands.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        let body = production
            .split_once("async fn resubmit_task(")
            .expect("resubmit task exists")
            .1
            .split_once("fn task_output_directory")
            .expect("resubmit task has a stable end")
            .0;
        let reserve = body
            .find("reserve_cloud_share_resubmission")
            .expect("cloud-share retry must reserve its original task");
        let engine = body
            .find("runtime\n        .service()")
            .expect("resubmit obtains the engine");
        let provider = body
            .find("resolve_stable_cloud_share_download")
            .expect("resubmit resolves the provider URL");
        assert!(reserve < engine);
        assert!(reserve < provider);
        assert!(body.contains("share_resolution_failure_message"));
        assert!(!body.contains("无法重新获取网盘下载地址：{error}"));
    }

    #[tokio::test]
    async fn cloud_share_batch_keeps_successes_before_and_after_a_typed_failure() {
        let store = TaskStore::open_in_memory().unwrap();
        let first = task_action_outcome(
            &store,
            store
                .create(NewTask::direct("https://example.test/first"))
                .unwrap(),
            None,
        )
        .unwrap();
        let last = task_action_outcome(
            &store,
            store
                .create(NewTask::direct("https://example.test/last"))
                .unwrap(),
            None,
        )
        .unwrap();
        let mut submissions = std::collections::VecDeque::from([
            Ok(first),
            Err(crate::cloud::QuarkError::WebAuthorizationRequired),
            Ok(last),
        ]);

        let batch = submit_cloud_share_batch_with(
            vec![
                ("first".into(), "source-1".into()),
                ("failed".into(), "source-2".into()),
                ("last".into(), "source-3".into()),
            ],
            |_| std::future::ready(submissions.pop_front().unwrap()),
        )
        .await;

        assert_eq!(batch.outcomes.len(), 2);
        assert_eq!(batch.failures.len(), 1);
        assert_eq!(batch.failures[0].selected_id, "failed");
        assert_eq!(batch.failures[0].code, "web_auth_required");
        assert_eq!(
            batch.failures[0].message,
            "此登录方式需要网页验证，请点击网页授权完成登录"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cloud_share_resubmission_reservation_is_idempotent_for_the_original_task() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "retry-reservation".into(),
            None,
            vec![CloudShareEntry::new(
                "retry-reservation",
                "entry".into(),
                "file.bin".into(),
                vec![],
                Some(1),
            )],
            serde_json::json!({}),
        );
        let source = plan.source_for("entry");
        plans.upsert(&plan).unwrap();
        let tasks = TaskStore::open_in_memory().unwrap();
        let task = tasks.create(NewTask::direct(&source)).unwrap();

        reserve_cloud_share_resubmission(&plans, &task).unwrap();
        reserve_cloud_share_resubmission(&plans, &task).unwrap();

        let current = plans.resolve_entry(&source).unwrap().plan;
        let linked = current.linked_local_tasks.values().next().unwrap();
        assert_eq!(linked.len(), 1);
        assert!(linked.contains(&task.id));
    }

    #[cfg(windows)]
    #[test]
    fn task_payload_projects_cleanup_pending_from_the_exact_plan_journal() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "payload-cleanup".into(),
            None,
            vec![CloudShareEntry::new(
                "payload-cleanup",
                "entry".into(),
                "file.bin".into(),
                vec![],
                Some(1),
            )],
            serde_json::json!({}),
        );
        let source = plan.source_for("entry");
        plans.upsert(&plan).unwrap();
        plans
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create(NewTask::direct(&source)).unwrap();
        plans.link_local_task(&source, task.id).unwrap();
        plans.record_transfer(&source, "created-id").unwrap();
        plans
            .mark_local_completion(&source, false, |_, _| true)
            .unwrap();
        let completed = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                ..task
            })
            .unwrap();

        let payload = task_payload_with_cleanup(&store, Some(&plans), completed).unwrap();
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["cleanup_pending"], true);

        let ordinary = store
            .create(NewTask::direct("https://example.test/file.bin"))
            .unwrap();
        let ordinary = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                ..ordinary
            })
            .unwrap();
        let ordinary = task_payload_with_cleanup(&store, Some(&plans), ordinary).unwrap();
        assert!(!ordinary.cleanup_pending);
    }

    #[cfg(windows)]
    #[test]
    fn task_payload_does_not_clear_cleanup_retry_when_the_plan_state_is_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct(&format!(
                "cloud-share://baidu/{}/{}",
                "a".repeat(64),
                "b".repeat(64)
            )))
            .unwrap();
        let completed = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                ..task
            })
            .unwrap();

        assert!(task_payload_with_cleanup(&store, Some(&plans), completed).is_err());
    }

    #[test]
    fn authenticated_proxy_is_validated_and_encoded_for_aria2() {
        let proxy = ProxySettings {
            enabled: true,
            protocol: "https".into(),
            host: "127.0.0.1".into(),
            port: 1080,
            username: "user@example.com".into(),
            password: "p@ss word".into(),
        };
        let value = proxy_url(&proxy).unwrap().unwrap();
        assert_eq!(
            value,
            "https://user%40example.com:p%40ss%20word@127.0.0.1:1080/"
        );
    }

    #[test]
    fn unbound_torrent_confirmation_is_rejected_instead_of_becoming_uri_download() {
        let store = TaskStore::open_in_memory().unwrap();
        let mut new_task = NewTask::direct("C:\\Downloads\\game.torrent");
        new_task.kind = "torrent".into();
        let task = store.create(new_task).unwrap();

        assert!(unbound_torrent_confirmation_error(&task)
            .unwrap()
            .contains("尚未完成解析"));
    }

    #[test]
    fn failed_retry_with_payload_without_control_file_requires_overwrite() {
        let root = std::env::temp_dir().join(format!("duplicate-payload-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("fixture.bin");
        std::fs::write(&payload, [1_u8, 2, 3]).unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.test/fixture.bin"))
            .unwrap();
        store
            .upsert_files(
                task.id,
                &[(0, payload.to_string_lossy().into_owned(), 3, 1, true)],
            )
            .unwrap();
        let failed = store.fail(task.id, "previous transfer stopped").unwrap();

        assert_eq!(
            retry_duplicate_state(&store, &failed).unwrap(),
            Some(DuplicateDownloadState::OverwriteRequired)
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_task_directory_comes_from_the_recorded_output_path() {
        let root = std::env::temp_dir().join(format!("task-directory-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("fixture.bin");
        std::fs::write(&payload, b"payload").unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.test/fixture.bin"))
            .unwrap();
        store
            .upsert_files(
                task.id,
                &[(0, payload.to_string_lossy().into_owned(), 7, 7, true)],
            )
            .unwrap();

        assert_eq!(task_output_directory(&store, task.id).unwrap(), root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_existing_video_files_are_collected_for_default_playback() {
        let root = std::env::temp_dir().join(format!("task-videos-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let video = root.join("episode.MKV");
        let document = root.join("notes.txt");
        let skipped_video = root.join("trailer.mp4");
        std::fs::write(&video, b"video").unwrap();
        std::fs::write(&document, b"notes").unwrap();
        std::fs::write(&skipped_video, b"trailer").unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.test/episode.MKV"))
            .unwrap();
        store
            .upsert_files(
                task.id,
                &[
                    (0, video.to_string_lossy().into_owned(), 5, 5, true),
                    (1, document.to_string_lossy().into_owned(), 5, 5, true),
                    (2, skipped_video.to_string_lossy().into_owned(), 7, 0, false),
                ],
            )
            .unwrap();

        assert_eq!(task_video_paths(&store, task.id).unwrap(), vec![video]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn permanent_recycle_cleanup_removes_only_recorded_payload_and_sidecar() {
        let root = std::env::temp_dir().join(format!("recycle-cleanup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("fixture.bin");
        let sidecar = std::path::PathBuf::from(format!("{}.aria2", payload.display()));
        let neighbor = root.join("keep.bin");
        std::fs::write(&payload, b"payload").unwrap();
        std::fs::write(&sidecar, b"control").unwrap();
        std::fs::write(&neighbor, b"keep").unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.test/fixture.bin"))
            .unwrap();
        store
            .upsert_files(
                task.id,
                &[(0, payload.to_string_lossy().into_owned(), 7, 7, true)],
            )
            .unwrap();

        remove_task_payload_files(&store, task.id).unwrap();

        assert!(!payload.exists());
        assert!(!sidecar.exists());
        assert!(neighbor.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
