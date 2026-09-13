use std::{
    collections::{HashMap, HashSet},
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
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
    pub privacy_session: crate::privacy::PrivacySession,
    pub cloud_download_sources: crate::cloud::commands::CloudDownloadSourceGate,
}

#[derive(Clone, Serialize)]
pub struct DownloadTaskPayload {
    #[serde(flatten)]
    pub task: DownloadTask,
    pub files: Vec<TransferFile>,
    pub cleanup_pending: bool,
    /// A queue-safe action hint for callers that receive a task-action result
    /// before the next list projection arrives.
    pub reauthorization_required: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyTaskVisibility {
    pub enabled: bool,
    pub unlocked: bool,
}

pub(crate) fn privacy_task_visibility(state: &AppState) -> Result<PrivacyTaskVisibility, String> {
    let accepting_new_private_downloads = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?
        .privacy_download_enabled;
    // Disabling new private downloads must never unlock historical private
    // tasks. Keep their protection and unlock control available until every
    // private task has been removed.
    let enabled = accepting_new_private_downloads
        || state
            .store
            .has_private_tasks()
            .map_err(|error| error.to_string())?;
    Ok(PrivacyTaskVisibility {
        enabled,
        unlocked: enabled && state.privacy_session.is_unlocked(),
    })
}

/// Private task names must not cross the event boundary while the session is
/// locked. Fail closed if settings state cannot be read.
pub(crate) fn should_emit_task_update(state: &AppState, is_private: bool) -> bool {
    !is_private
        || privacy_task_visibility(state)
            .map(|visibility| visibility.unlocked)
            .unwrap_or(false)
}

pub(crate) fn ensure_private_task_access(
    state: &AppState,
    task: &DownloadTask,
) -> Result<(), String> {
    if task.is_private && !privacy_task_visibility(state)?.unlocked {
        return Err("隐私任务已锁定，请先解锁".into());
    }
    Ok(())
}

pub(crate) fn ensure_private_group_access(
    state: &AppState,
    group: &CloudDirectoryGroup,
) -> Result<(), String> {
    if group.is_private && !privacy_task_visibility(state)?.unlocked {
        return Err("隐私任务已锁定，请先解锁".into());
    }
    Ok(())
}

pub(crate) fn ensure_private_download_enabled(
    state: &AppState,
    private_download: bool,
) -> Result<(), String> {
    if private_download
        && !state
            .settings
            .lock()
            .map_err(|_| "设置锁不可用".to_string())?
            .privacy_download_enabled
    {
        return Err("请先在设置中开启隐私下载".into());
    }
    Ok(())
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
    mut task: DownloadTask,
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
    let reauthorization_required = crate::cloud::commands::requires_cloud_reauthorization_failure(
        task.failure_code.as_deref(),
    );
    redact_ftp_credentials(&mut task);
    Ok(DownloadTaskPayload {
        task,
        files,
        cleanup_pending,
        reauthorization_required,
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

fn fail_invalid_task_submission(store: &TaskStore, task: &DownloadTask) -> Result<(), String> {
    match crate::validate_task_source(&task.kind, &task.source) {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = error.to_string();
            let _ = store.fail(task.id, message.clone());
            Err(message)
        }
    }
}

fn download_engine_submission_message(error: &DownloadServiceError) -> String {
    match error {
        DownloadServiceError::Input(error) => error.to_string(),
        _ => safe_engine_submission_message(&error.to_string()),
    }
}

fn safe_engine_submission_message(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("http status 400") {
        return "下载引擎拒绝请求（HTTP 400）".into();
    }
    if ["uri", "magnet", "parameter", "invalid"]
        .iter()
        .any(|word| lower.contains(word))
    {
        return "下载引擎拒绝该链接：链接格式无效".into();
    }
    "下载引擎未能接受下载任务".into()
}

fn fail_engine_submission(
    store: &TaskStore,
    task: &DownloadTask,
    error: DownloadServiceError,
) -> String {
    let message = download_engine_submission_message(&error);
    let _ = store.fail(task.id, message.clone());
    message
}

fn fail_engine_submission_text(store: &TaskStore, task: &DownloadTask, error: String) -> String {
    let message = safe_engine_submission_message(&error);
    let _ = store.fail(task.id, message.clone());
    message
}

#[tauri::command]
pub async fn parse_download(
    app: AppHandle,
    input: String,
    access_code: Option<String>,
    require_confirmation: Option<bool>,
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
            crate::cloud::share::ShareConfirmationOutcome::AutoSubmit {
                source,
                confirmation,
            } => {
                if require_confirmation.unwrap_or(false) {
                    return Ok(ParseDownloadOutcome::ShareConfirmation(confirmation));
                }
                let configured = download_submission_options(&state)?;
                let action = crate::cloud::commands::submit_cloud_share_file(
                    app, &state, source, configured, false, None,
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
    let mut outcome = prepare_download(&input, &state.store).map_err(|error| error.to_string())?;
    if outcome.duplicate.is_some() {
        redact_ftp_credentials(&mut outcome.task);
        ensure_private_task_access(&state, &outcome.task)?;
        return Ok(ParseDownloadOutcome::Task(outcome));
    }
    if require_confirmation.unwrap_or(false) && !outcome.requires_confirmation {
        outcome.task = state
            .store
            .transition(outcome.task.id, TaskStatus::AwaitingUser)
            .map_err(|error| error.to_string())?;
        outcome.requires_confirmation = true;
    }
    if outcome.task.kind == "torrent" {
        let service = state
            .runtime
            .service()
            .await
            .map_err(|error| error.to_string())?;
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
                let message = fail_engine_submission(&state.store, &outcome.task, error);
                return Err(message);
            }
        };
        outcome.resources = parse_resources(&state.store, outcome.task.id)?;
    } else if !outcome.requires_confirmation {
        let options = download_submission_options(&state)?;
        outcome.task = match state
            .runtime
            .submit_uri_with_options(outcome.task.id, vec![outcome.task.source.clone()], options)
            .await
        {
            Ok(task) => task,
            Err(error) => {
                return Err(fail_engine_submission_text(&state.store, &outcome.task, error))
            }
        };
    } else if outcome.task.kind == "magnet" {
        let service = state
            .runtime
            .service()
            .await
            .map_err(|error| error.to_string())?;
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
                .map_err(|error| fail_engine_submission(&state.store, &outcome.task, error))?;
        } else {
            outcome.task = service
                .prepare_magnet_with_options(outcome.task.id, outcome.task.source.clone(), options)
                .await
                .map_err(|error| fail_engine_submission(&state.store, &outcome.task, error))?;
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
    redact_ftp_credentials(&mut outcome.task);
    Ok(ParseDownloadOutcome::Task(outcome))
}

#[tauri::command]
pub async fn confirm_cloud_share_download(
    app: AppHandle,
    plan_source: String,
    selected_ids: Vec<String>,
    destination_directory: Option<String>,
    private_download: Option<bool>,
    on_created: tauri::ipc::Channel<QueueTaskPayload>,
    state: State<'_, AppState>,
) -> Result<CloudShareBatchOutcome, crate::cloud::QuarkError> {
    let cloud = app
        .try_state::<Arc<crate::cloud::commands::CloudState>>()
        .ok_or(crate::cloud::QuarkError::Storage)?;
    let selection = crate::cloud::share::selected_share_download(
        &cloud.share_plans,
        &plan_source,
        &selected_ids,
    )?;
    let configured =
        download_submission_options_for_directory(&state, destination_directory.as_deref())
            .map_err(crate::cloud::QuarkError::InvalidInput)?;
    let private_download = private_download.unwrap_or(false);
    ensure_private_download_enabled(&state, private_download)
        .map_err(crate::cloud::QuarkError::InvalidInput)?;
    if selection.directory_name.is_some() {
        let plan = crate::cloud::commands::plan_cloud_share_directory_download(&selection)?;
        let store = Arc::clone(&state.store);
        let provider = selection.provider;
        let group_source = selection.plan_source.clone();
        let submit_app = app.clone();
        let root_event_app = app.clone();
        let emit_root = !private_download
            || privacy_task_visibility(&state)
                .map(|visibility| visibility.unlocked)
                .unwrap_or(false);
        let outcome = crate::cloud::commands::submit_cloud_directory_plan_with_group_event(
            &store,
            provider,
            &group_source,
            plan,
            configured,
            private_download,
            move |root| {
                let _ = on_created.send(root.clone());
                if emit_root {
                    use tauri::Emitter;
                    let _ = root_event_app.emit("download-task-updated", root);
                }
                Ok(())
            },
            move |_, source, options, context| {
                let app = submit_app.clone();
                async move {
                    let state = app.state::<AppState>();
                    crate::cloud::commands::submit_cloud_share_directory_leaf(
                        app.clone(),
                        &state,
                        source,
                        options,
                        context,
                        private_download,
                    )
                    .await
                }
            },
        )
        .await?;
        let mut result = CloudShareBatchOutcome {
            outcomes: vec![outcome.task],
            failures: Vec::new(),
        };
        if private_download {
            mark_cloud_share_outcomes_private(&state.store, &mut result)?;
        }
        return Ok(result);
    }
    let selections = selection
        .entries
        .into_iter()
        .map(|entry| (entry.selected_id, entry.source))
        .collect();
    let state = &*state;
    let mut result = submit_cloud_share_batch_with(selections, |source| {
        let app = app.clone();
        let configured = configured.clone();
        let on_created = on_created.clone();
        async move {
            let outcome = crate::cloud::commands::submit_cloud_share_file(
                app,
                state,
                source,
                configured,
                private_download,
                Some(on_created.clone()),
            )
            .await?;
            if let Ok(task) =
                queue_task_payload_from_task(&state.store, outcome.payload.task.clone())
            {
                let _ = on_created.send(task);
            }
            Ok(outcome)
        }
    })
    .await;
    if private_download {
        mark_cloud_share_outcomes_private(&state.store, &mut result)?;
    }
    Ok(result)
}

fn mark_cloud_share_outcomes_private(
    store: &TaskStore,
    result: &mut CloudShareBatchOutcome,
) -> Result<(), crate::cloud::QuarkError> {
    for outcome in &mut result.outcomes {
        let id = outcome.payload.task.id;
        if let Some(group) = store
            .cloud_directory_group(id)
            .map_err(|_| crate::cloud::QuarkError::Storage)?
        {
            store
                .set_cloud_directory_group_private(group.id, true)
                .map_err(|_| crate::cloud::QuarkError::Storage)?;
            for item in store
                .cloud_directory_group_items(group.id)
                .map_err(|_| crate::cloud::QuarkError::Storage)?
            {
                if let Some(task_id) = item.task_id {
                    store
                        .set_private(task_id, true)
                        .map_err(|_| crate::cloud::QuarkError::Storage)?;
                }
            }
            outcome.payload.task.is_private = true;
            outcome.queue_task = Some(
                cloud_directory_group_queue_task(store, group.id)
                    .map_err(crate::cloud::QuarkError::Transport)?,
            );
        } else {
            let task = store
                .set_private(id, true)
                .map_err(|_| crate::cloud::QuarkError::Storage)?;
            outcome.payload =
                task_payload(store, task).map_err(crate::cloud::QuarkError::Transport)?;
        }
    }
    Ok(())
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
        QuarkError::PikpakOperationVerificationRequired(_) => {
            "pikpak_operation_verification_required"
        }
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
    private_download: Option<bool>,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let private_download = private_download.unwrap_or(false);
    ensure_private_download_enabled(&state, private_download)?;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    ensure_private_task_access(&state, &current)?;
    fail_invalid_task_submission(&state.store, &current)?;
    let current = state
        .store
        .set_private(id, private_download)
        .map_err(|error| error.to_string())?;
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
pub async fn list_tasks(
    state: State<'_, AppState>,
    app: AppHandle,
    scope: Option<String>,
) -> Result<Vec<QueueTaskPayload>, String> {
    let cloud = app
        .try_state::<Arc<crate::cloud::commands::CloudState>>()
        .map(|cloud| Arc::clone(cloud.inner()));
    let visibility = privacy_task_visibility(&state)?;
    let include_private = visibility.unlocked;
    list_queue_tasks_in_background(Arc::clone(&state.store), cloud, scope, include_private).await
}

async fn list_queue_tasks_in_background(
    store: Arc<TaskStore>,
    cloud: Option<Arc<crate::cloud::commands::CloudState>>,
    scope: Option<String>,
    include_private: bool,
) -> Result<Vec<QueueTaskPayload>, String> {
    // SQLite and the encrypted share journal can block. Keep the complete
    // projection off the window thread, including focus-triggered refreshes.
    tauri::async_runtime::spawn_blocking(move || {
        // Restore share-cleanup messages before reading the same projected list that
        // the command returns. The reusable helper below deliberately has no
        // application-state dependency, so the durable directory grouping boundary
        // can be exercised outside a running Tauri webview.
        if let Some(cloud) = cloud.as_ref() {
            let _ = cloud.share_plans.restore_cleanup_pending_messages(&store);
        }
        let mut tasks =
            list_queue_tasks_for_scope_with_privacy(&store, scope.as_deref(), include_private)?;
        if let Some(plans) = cloud.as_ref().map(|cloud| &cloud.share_plans) {
            apply_queue_cleanup_projection(&mut tasks, plans)?;
        }
        Ok(tasks)
    })
    .await
    .map_err(|error| format!("后台读取任务失败: {error}"))?
}

/// Returns the durable queue projection used by [`list_tasks`] for a scope.
/// Cloud-directory children are removed from the top level and nested beneath
/// their one synthetic folder root.
pub fn list_queue_tasks_for_scope(
    store: &TaskStore,
    scope: Option<&str>,
) -> Result<Vec<QueueTaskPayload>, String> {
    list_queue_tasks_for_scope_with_privacy(store, scope, true)
}

pub fn list_queue_tasks_for_scope_with_privacy(
    store: &TaskStore,
    scope: Option<&str>,
    include_private: bool,
) -> Result<Vec<QueueTaskPayload>, String> {
    let leaves = list_tasks_with_cleanup_projection_for_scope(store, None, None)?;
    let tasks = project_cloud_directory_groups(store, leaves, scope)?;
    Ok(if include_private {
        tasks
    } else {
        without_private_queue_tasks(tasks)
    })
}

fn without_private_queue_tasks(tasks: Vec<QueueTaskPayload>) -> Vec<QueueTaskPayload> {
    tasks
        .into_iter()
        .filter(|task| !task.is_private)
        .map(|mut task| {
            task.children = without_private_queue_tasks(task.children);
            task
        })
        .collect()
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

pub(crate) fn queue_task_payload_from_task(
    store: &TaskStore,
    mut task: DownloadTask,
) -> Result<QueueTaskPayload, String> {
    let files = store.files(task.id).map_err(|error| error.to_string())?;
    let reauthorization_required = crate::cloud::commands::requires_cloud_reauthorization_failure(
        task.failure_code.as_deref(),
    );
    redact_ftp_credentials(&mut task);
    Ok(QueueTaskPayload {
        id: task.id.to_string(),
        source: task.source,
        name: task.name,
        kind: task.kind,
        status: task.status,
        progress: task.progress,
        error: task.error,
        failure_code: task.failure_code,
        reauthorization_required,
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
        is_private: task.is_private,
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
    failure_code: Option<String>,
) -> QueueTaskPayload {
    let status = if matches!(
        group.status,
        TaskStatus::Paused | TaskStatus::Cancelled | TaskStatus::Recycled
    ) {
        group.status
    } else if error.is_some() {
        TaskStatus::Failed
    } else {
        TaskStatus::Queued
    };
    let reauthorization_required =
        crate::cloud::commands::requires_cloud_reauthorization_failure(failure_code.as_deref());
    QueueTaskPayload {
        id: item.id.to_string(),
        source: format!(
            "cloud-directory-item://{}/{}",
            group.provider.key(),
            item.id
        ),
        name: item.output_name.clone(),
        kind: "file".to_string(),
        status,
        progress: 0,
        error,
        failure_code,
        reauthorization_required,
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
        is_private: group.is_private,
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
        status: TaskStatus::Queued,
        progress: 0,
        error: None,
        failure_code: None,
        reauthorization_required: false,
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
        is_private: group.is_private,
        files: Vec::new(),
        cleanup_pending: false,
        children: Vec::new(),
    }
}

fn linked_cloud_directory_item_payload(
    store: &TaskStore,
    item: &CloudDirectoryGroupItem,
    task: DownloadTask,
) -> Result<QueueTaskPayload, String> {
    let mut payload = queue_task_payload_from_task(store, task)?;
    if payload.total_length.is_none() {
        payload.total_length = selected_transfer_files_total_length(&payload.files)
            .or(item.size)
            .or_else(|| {
                (payload.status == TaskStatus::Completed)
                    .then(|| completed_cloud_directory_item_file_size(store, item))
                    .flatten()
            });
    }
    if payload.status == TaskStatus::Completed {
        // Older snapshots could retain a cloud-share failure after aria2 later
        // reported the same local task as complete. The authoritative local
        // completion wins when rendering the durable directory membership.
        payload.error = None;
        payload.failure_code = None;
        payload.aria_error_code = None;
        if let Some(total_length) = payload.total_length {
            payload.completed_length = total_length;
        }
    }
    Ok(payload)
}

fn selected_transfer_files_total_length(files: &[TransferFile]) -> Option<u64> {
    files
        .iter()
        .filter(|file| file.selected)
        .map(|file| file.length.max(file.completed_length))
        .reduce(u64::saturating_add)
        .filter(|total_length| *total_length > 0)
}

fn completed_cloud_directory_item_file_size(
    store: &TaskStore,
    item: &CloudDirectoryGroupItem,
) -> Option<u64> {
    let group = store.cloud_directory_group(item.group_id).ok()??;
    let (root, descendants) = item.relative_directory.split_first()?;
    if root != &group.relative_directory {
        return None;
    }
    let mut output_path = cloud_directory_group_output_root(&group).ok()?;
    for component in descendants.iter().chain(std::iter::once(&item.output_name)) {
        let mut path_components = Path::new(component).components();
        let Some(Component::Normal(component)) = path_components.next() else {
            return None;
        };
        if path_components.next().is_some() {
            return None;
        }
        output_path.push(component);
    }
    let metadata = fs::metadata(output_path).ok()?;
    metadata.is_file().then_some(metadata.len())
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
    let mut root = cloud_directory_folder_payload(group, &[]);
    root.id = group.id.to_string();
    root.children = children;
    aggregate_cloud_directory_folder(group, root)
}

fn aggregate_cloud_directory_folder(
    group: &CloudDirectoryGroup,
    folder: QueueTaskPayload,
) -> QueueTaskPayload {
    // Every directory, not just the queue root, derives its transfer state
    // from all descendant files. Keep directory identities stable across updates.
    let children: Vec<_> = folder
        .children
        .into_iter()
        .map(|child| {
            if child.kind == "folder" {
                aggregate_cloud_directory_folder(group, child)
            } else {
                child
            }
        })
        .collect();
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
    let status = if group.status == TaskStatus::Recycled || all_linked_recycled {
        TaskStatus::Recycled
    } else if group.status == TaskStatus::Paused && !all_completed {
        TaskStatus::Paused
    } else if has_active {
        leaves
            .iter()
            .find(|item| task_is_active(item.status))
            .map(|item| item.status)
            .unwrap_or(TaskStatus::Queued)
    } else if group.status == TaskStatus::Cancelled {
        TaskStatus::Cancelled
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
    let progress = if all_completed {
        100
    } else if total_length == 0 {
        0
    } else {
        // Bytes alone cannot confirm completion: unknown-size files or
        // finalization may still be pending after all known bytes arrive.
        ((completed_length.saturating_mul(100) / total_length).min(99)) as u8
    };
    let updated_at = leaves
        .iter()
        .map(|item| item.updated_at)
        .max()
        .unwrap_or(group.updated_at)
        .max(group.updated_at);
    QueueTaskPayload {
        id: folder.id,
        source: folder.source,
        name: folder.name,
        kind: "folder".to_string(),
        status,
        progress,
        error: (status == TaskStatus::Failed)
            .then(|| leaves.iter().find_map(|item| item.error.clone()))
            .flatten(),
        failure_code: None,
        reauthorization_required: status == TaskStatus::Failed
            && leaves.iter().any(|item| item.reauthorization_required),
        aria_gid: None,
        followed_by_gid: None,
        metadata_gid: None,
        total_length: has_known_total.then_some(total_length),
        completed_length,
        download_speed: leaves.iter().map(|item| item.download_speed).sum(),
        upload_speed: leaves.iter().map(|item| item.upload_speed).sum(),
        connections: leaves.iter().map(|item| item.connections).sum(),
        eta_seconds: leaves.iter().filter_map(|item| item.eta_seconds).max(),
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
        is_private: group.is_private,
        files: Vec::new(),
        cleanup_pending: false,
        children,
    }
}

fn legacy_share_directory_root(
    group: &CloudDirectoryGroup,
    items: &[CloudDirectoryGroupItem],
) -> Option<String> {
    if group.relative_directory
        != crate::cloud::share::provider_share_directory_name(group.provider)
    {
        return None;
    }
    let root = items.first()?.relative_directory.get(1)?;
    (!root.trim().is_empty()
        && items.iter().all(|item| {
            item.relative_directory.first() == Some(&group.relative_directory)
                && item.relative_directory.get(1) == Some(root)
        }))
    .then(|| root.to_owned())
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
        let legacy_share_root = legacy_share_directory_root(&group, &items);
        let mut root = cloud_directory_folder_payload(&group, &[]);
        root.id = group.id.to_string();
        root.name = legacy_share_root
            .clone()
            .unwrap_or_else(|| group.relative_directory.clone());
        for item in items {
            let leaf = match item.task_id {
                Some(task_id) => match task_by_id.remove(&task_id) {
                    Some(task) => linked_cloud_directory_item_payload(store, &item, task)?,
                    None => virtual_cloud_directory_item_payload(
                        &group,
                        &item,
                        Some("关联的下载任务不存在，需重新提交网盘文件".to_string()),
                        None,
                    ),
                },
                None => virtual_cloud_directory_item_payload(
                    &group,
                    &item,
                    item.failure_message.clone(),
                    item.failure_code.clone(),
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
            let visible_components = legacy_share_root
                .as_ref()
                .and_then(|root| visible_components.strip_prefix(std::slice::from_ref(root)))
                .unwrap_or(visible_components);
            insert_cloud_directory_leaf(
                &group,
                &mut root,
                visible_components,
                &mut Vec::new(),
                leaf,
            );
        }
        let mut root = aggregate_cloud_directory_root(&group, root.children);
        if let Some(root_name) = legacy_share_root {
            root.name = root_name;
        }
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
        ensure_private_group_access(&state, &group)?;
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
            ensure_private_group_access(&state, &group)?;
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
    ensure_private_group_access(state, group)?;
    if action == "cancel" {
        return crate::cloud::commands::cancel_cloud_directory_group_with(
            &state.store,
            group.id,
            |id| {
                let app = app.clone();
                async move {
                    run_local_task_action(id.to_string(), "cancel".to_string(), app)
                        .await
                        .map(|_| ())
                }
            },
        )
        .await;
    }
    if !matches!(action, "pause" | "resume" | "cancel" | "retry") {
        return Err("当前任务不能执行此操作".to_string());
    }
    if matches!(action, "pause" | "resume") {
        state
            .store
            .set_cloud_directory_paused(group.id, action == "pause")
            .map_err(|error| error.to_string())?;
        use tauri::Emitter;
        let root = cloud_directory_group_queue_task(&state.store, group.id)?;
        if !group.is_private
            || privacy_task_visibility(state)
                .map(|v| v.unlocked)
                .unwrap_or(false)
        {
            let _ = app.emit("download-task-updated", root);
        }
        let items = state
            .store
            .cloud_directory_group_items(group.id)
            .map_err(|e| e.to_string())?;
        let targets = items
            .iter()
            .filter_map(|item| item.task_id)
            .map(|id| state.store.find(id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|task| task.aria_gid.is_some() && task_is_active(task.status));
        // Dispatch every interrupt before waiting for any child's reconciliation.
        let targets = targets.collect::<Vec<_>>();
        let mut results = Vec::new();
        if !targets.is_empty() {
            let service = state.runtime.service().await.map_err(|e| e.to_string())?;
            let control = if action == "pause" {
                DownloadAction::Pause
            } else {
                DownloadAction::Resume
            };
            results = futures_util::future::join_all(
                targets
                    .iter()
                    .map(|task| service.control_directory_member(task.id, control)),
            )
            .await;
        }
        if action == "resume" {
            let app = app.clone();
            let group_id = group.id;
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                // Linked preparations can survive an application exit without
                // ever receiving a GID. resubmit_task waits for any live resolver
                // and rechecks the record before doing provider work.
                for id in items.iter().filter_map(|item| item.task_id) {
                    if let Ok(task) = state.store.find(id) {
                        if task.aria_gid.is_none()
                            && matches!(task.status, TaskStatus::Parsing | TaskStatus::Paused)
                        {
                            if let Err(error) =
                                resubmit_task(app.clone(), &state, task, Default::default()).await
                            {
                                if let Ok(current) = state.store.find(id) {
                                    let still_running = state
                                        .store
                                        .cloud_directory_group(group_id)
                                        .ok()
                                        .flatten()
                                        .is_some_and(|group| {
                                            !matches!(
                                                group.status,
                                                TaskStatus::Paused
                                                    | TaskStatus::Cancelled
                                                    | TaskStatus::Recycled
                                            )
                                        });
                                    if still_running
                                        && current.aria_gid.is_none()
                                        && matches!(
                                            current.status,
                                            TaskStatus::Parsing | TaskStatus::Paused
                                        )
                                    {
                                        let _ = state.store.fail(id, &error);
                                    }
                                }
                            }
                        }
                    }
                }
                if items.iter().any(|item| item.task_id.is_none()) {
                    let _ =
                        crate::cloud::commands::retry_cloud_directory_group(app.clone(), group_id)
                            .await;
                }
                if let Ok(root) = cloud_directory_group_queue_task(&state.store, group_id) {
                    if !root.is_private
                        || privacy_task_visibility(&state)
                            .map(|v| v.unlocked)
                            .unwrap_or(false)
                    {
                        let _ = app.emit("download-task-updated", root);
                    }
                }
            });
        }
        if let Some(error) = results.into_iter().find_map(Result::err) {
            return Err(format!("部分目录文件操作失败，请重试：{error}"));
        }
        return crate::cloud::commands::cloud_directory_group_task_outcome(&state.store, group)
            .map_err(|error| error.to_string());
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
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    ensure_private_task_access(&state, &current)?;
    if action == "retry_cleanup" {
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
    if action == "cancel" && current.aria_gid.is_none() && current.kind != "ftp" {
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
    let task = state
        .runtime
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
    ensure_private_task_access(&state, &current)?;
    let resolution = duplicate_state(&state.store, &current).map_err(|error| error.to_string())?;
    let task = match (action.as_str(), resolution) {
        ("resume", DuplicateDownloadState::ResumeAvailable)
            if current.status == TaskStatus::Paused && current.aria_gid.is_some() =>
        {
            state
                .runtime
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

pub(crate) async fn resubmit_task(
    app: AppHandle,
    state: &AppState,
    mut task: DownloadTask,
    mut options: UriSubmissionOptions,
) -> Result<DownloadTask, String> {
    fail_invalid_task_submission(&state.store, &task)?;
    // Match the cleanup worker's source gate for both retry and overwrite.
    let _directory_submission =
        crate::cloud::commands::lock_directory_member_submission(&state.store, task.id)
            .await
            .map_err(|error| error.to_string())?;
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
    if _directory_submission.is_some()
        && task.aria_gid.is_none()
        && matches!(task.status, TaskStatus::Parsing | TaskStatus::Paused)
    {
        task = crate::cloud::commands::recover_interrupted_directory_preparation(
            &state.store,
            task.id,
        )
        .map_err(|error| error.to_string())?;
        if task.aria_gid.is_some() || task.status != TaskStatus::Paused {
            return Ok(task);
        }
    }
    if task.source.starts_with("cloud-share://") {
        let cloud = app
            .try_state::<Arc<crate::cloud::commands::CloudState>>()
            .ok_or_else(|| "网盘分享下载状态不可用，请重新解析分享链接".to_string())?;
        reserve_cloud_share_resubmission(&cloud.share_plans, &task)?;
    }
    options = crate::cloud::commands::cloud_directory_member_retry_options(
        &state.store,
        task.id,
        options,
    )
    .map_err(|error| error.to_string())?;
    let configured = download_submission_options(state)?;
    if options.directory.is_none() {
        options.directory = configured.directory;
    }
    if options.proxy.is_none() {
        options.proxy = configured.proxy;
    }
    if task.source.starts_with("cloud://") || task.source.starts_with("cloud-share://") {
        let owner = state
            .store
            .cloud_download_account(task.id)
            .map_err(|error| error.to_string())?;
        let provider = if task.source.starts_with("cloud://") {
            Some(
                crate::cloud::commands::parse_stable_cloud_source(&task.source)
                    .map_err(|error| error.to_string())?
                    .0,
            )
        } else {
            None
        };
        if let (Some(provider), Some(owner)) = (provider, owner.as_deref()) {
            crate::cloud::commands::require_download_account(&app, provider, owner)
                .map_err(|error| error.to_string())?;
        }
        let service = state
            .runtime
            .service()
            .await
            .map_err(|error| error.to_string())?;
        let share = task.source.starts_with("cloud-share://");
        let resolved = if share {
            crate::cloud::commands::resolve_stable_cloud_share_download(app.clone(), &task.source)
                .await
        } else {
            crate::cloud::commands::resolve_stable_cloud_download(app.clone(), &task.source).await
        };
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                let message = crate::cloud::commands::share_resolution_failure_message(&error);
                // A failed cloud task remains the same durable task while a
                // retry discovers that its session now needs verification.
                // Preserve a typed reason for the queue instead of making
                // the frontend guess from this localized message.
                if task.status == TaskStatus::Failed {
                    let _ = state.store.update_failed_failure(
                        task.id,
                        Some(crate::cloud::commands::cloud_failure_code(&error)),
                        message,
                    );
                    if let Ok(updated) = state.store.find(task.id) {
                        if should_emit_task_update(state, updated.is_private) {
                            if let Ok(payload) = task_update_payload(&state.store, None, updated) {
                                let _ = app.emit("download-task-updated", payload);
                            }
                        }
                    }
                }
                return Err(message.to_string());
            }
        };
        if let (Some(provider), Some(owner)) = (provider, owner.as_deref()) {
            crate::cloud::commands::require_download_account(&app, provider, owner)
                .map_err(|error| error.to_string())?;
        }
        if share {
            return crate::cloud::share::resubmit_current_share_download(
                &service, &task, resolved, options,
            )
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "任务状态已变化，本次重试已跳过".into());
        }
        return crate::cloud::share::resubmit_current_cloud_download(
            &service, &task, resolved, options,
        )
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "任务状态已变化，本次重试已跳过".into());
    }
    if task.kind == "torrent" {
        return Err("种子任务请重新添加后再选择文件下载".to_string());
    }
    if task.kind == "magnet" {
        let service = state
            .runtime
            .service()
            .await
            .map_err(|error| error.to_string())?;
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
    state
        .runtime
        .resubmit_uri_with_options(task.id, vec![task.source], options)
        .await
        .map_err(|error| error.to_string())
}

fn redact_ftp_credentials(task: &mut DownloadTask) {
    if task.kind != "ftp" {
        return;
    }
    if let Ok(target) = crate::parse_ftp_url(&task.source) {
        task.source = target.display_source().to_owned();
    }
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
pub(crate) fn schedule_cloud_download_refresh(app: AppHandle, task: &DownloadTask) {
    if task.status != TaskStatus::Failed {
        return;
    }
    let Some(cloud) = app.try_state::<Arc<crate::cloud::commands::CloudState>>() else {
        return;
    };
    let share = task.source.starts_with("cloud-share://");
    let claimed = if share {
        cloud
            .share_refresh_budget
            .claim(&task.source, task.aria_error_code.as_deref())
            .unwrap_or(false)
    } else {
        cloud
            .cloud_download_refresh_budget
            .claim(&task.source, task.aria_error_code.as_deref())
    };
    if !claimed {
        return;
    }
    let failed = task.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let Ok(_directory_submission) =
            crate::cloud::commands::lock_directory_member_submission(&state.store, failed.id).await
        else {
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
        // One attempt only. If refresh identifies an expired session or an
        // official verification challenge, preserve that durable reason for
        // the Failed tab instead of leaving the stale aria2 error there.
        let resolution = if share {
            crate::cloud::commands::resolve_stable_cloud_share_download(
                app.clone(),
                &current.source,
            )
            .await
        } else {
            crate::cloud::commands::resolve_stable_cloud_download(app.clone(), &current.source)
                .await
        };
        let resolved = match resolution {
            Ok(resolved) => resolved,
            Err(error) => {
                let code = crate::cloud::commands::cloud_failure_code(&error);
                if crate::cloud::commands::requires_cloud_reauthorization_failure(Some(code)) {
                    let _ = state.store.update_failed_failure(
                        current.id,
                        Some(code),
                        crate::cloud::commands::share_resolution_failure_message(&error),
                    );
                    if let Ok(task) = state.store.find(current.id) {
                        if should_emit_task_update(&state, task.is_private) {
                            if let Ok(payload) = task_update_payload(&state.store, None, task) {
                                let _ = app.emit("download-task-updated", payload);
                            }
                        }
                    }
                }
                return;
            }
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
        let _ = crate::cloud::share::resubmit_current_cloud_download(
            &service, &failed, resolved, options,
        )
        .await;
    });
}

const PLAYABLE_MEDIA_EXTENSIONS: &[&str] = &[
    "3gp", "asf", "avi", "flv", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "mts", "ts",
    "webm", "wmv", "aac", "aiff", "alac", "ape", "flac", "m4a", "mka", "mp3", "oga", "ogg", "opus",
    "wav", "wma",
];

const ASSOCIATED_FILE_EXTENSIONS: &[&str] = &[
    "avif", "bmp", "cfg", "conf", "csv", "css", "doc", "docm", "docx", "dot", "dotm", "dotx",
    "gif", "heic", "htm", "html", "ico", "ini", "jpeg", "jpg", "json", "log", "markdown", "md",
    "odp", "ods", "odt", "pdf", "png", "pot", "potm", "potx", "pps", "ppsm", "ppsx", "ppt", "pptm",
    "pptx", "rst", "rtf", "svg", "text", "tif", "tiff", "toml", "tsv", "txt", "webp", "wps", "xls",
    "xlsb", "xlsm", "xlsx", "xlt", "xltm", "xltx", "xml", "yaml", "yml",
];

fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            PLAYABLE_MEDIA_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

fn is_associated_file_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ASSOCIATED_FILE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
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
        Some(group) => {
            ensure_private_group_access(&state, &group)?;
            cloud_directory_group_output_root(&group)?
        }
        None => {
            let task = state.store.find(id).map_err(|error| error.to_string())?;
            ensure_private_task_access(&state, &task)?;
            task_output_directory(&state.store, id)?
        }
    };
    tauri_plugin_opener::open_path(&directory, None::<&str>)
        .map_err(|error| format!("无法打开下载目录：{error}"))
}

fn task_associated_file_path(store: &TaskStore, task_id: Uuid) -> Result<PathBuf, String> {
    let task = store.find(task_id).map_err(|error| error.to_string())?;
    if task.status != TaskStatus::Completed {
        return Err("仅已完成任务可以打开文件".to_string());
    }
    let files = store
        .files(task_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|file| file.selected)
        .map(|file| PathBuf::from(file.path))
        .collect::<Vec<_>>();
    let [path] = files.as_slice() else {
        return Err("该任务包含多个文件，请打开下载目录".to_string());
    };
    if !path.is_absolute() {
        return Err("下载文件路径不是本地绝对路径，无法打开".to_string());
    }
    if !is_associated_file_path(path) {
        return Err("该文件类型不支持使用系统默认应用打开".to_string());
    }
    if !path.is_file() {
        return Err("下载文件已不存在，无法打开".to_string());
    }
    Ok(path.to_path_buf())
}

#[tauri::command]
pub fn open_task_file(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = state.store.find(id).map_err(|error| error.to_string())?;
    ensure_private_task_access(&state, &task)?;
    let path = task_associated_file_path(&state.store, id)?;
    tauri_plugin_opener::open_path(&path, None::<&str>)
        .map_err(|error| format!("无法使用系统默认应用打开文件：{error}"))
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
    ensure_private_task_access(&state, &task)?;
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
    ensure_private_task_access(&state, &task)?;
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
pub async fn recycle_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<QueueTaskPayload, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    if let Some(group) = state
        .store
        .cloud_directory_group(id)
        .map_err(|error| error.to_string())?
    {
        ensure_private_group_access(&state, &group)?;
        let _submission = crate::cloud::commands::lock_directory_submission(id)
            .await
            .map_err(|error| error.to_string())?;
        return recycle_cloud_directory_group(&state.store, id);
    }
    let task = state.store.find(id).map_err(|error| error.to_string())?;
    ensure_private_task_access(&state, &task)?;
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
            if member.failure_message.is_none()
                && !matches!(group.status, TaskStatus::Cancelled | TaskStatus::Recycled)
            {
                return Err("请先取消目录中的活动下载任务，再移入回收站".to_string());
            }
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
    for task in tasks {
        if task.status != TaskStatus::Recycled {
            store
                .transition(task.id, TaskStatus::Recycled)
                .map_err(|error| error.to_string())?;
        }
    }
    store
        .recycle_cloud_directory_group(group.id)
        .map_err(|error| error.to_string())?;
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
        if file.path.trim().is_empty() {
            continue;
        }
        let path = PathBuf::from(file.path);
        remove_recorded_download_file(&path)?;
        let mut sidecar = path.into_os_string();
        sidecar.push(".aria2");
        remove_recorded_download_file(Path::new(&sidecar))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn purge_recycled_tasks(
    task_ids: Vec<String>,
    delete_files: bool,
    state: State<'_, AppState>,
    cloud: State<'_, Arc<crate::cloud::commands::CloudState>>,
) -> Result<(), String> {
    for task_id in &task_ids {
        let id = Uuid::parse_str(task_id).map_err(|_| "任务 ID 无效".to_string())?;
        if let Some(group) = state
            .store
            .cloud_directory_group(id)
            .map_err(|error| error.to_string())?
        {
            ensure_private_group_access(&state, &group)?;
        } else {
            let task = state.store.find(id).map_err(|error| error.to_string())?;
            ensure_private_task_access(&state, &task)?;
        }
    }
    purge_recycled_task_batch(
        Arc::clone(&state.store),
        Arc::clone(cloud.inner()),
        task_ids,
        delete_files,
    )
    .await
}

async fn purge_recycled_task_batch(
    store: Arc<TaskStore>,
    cloud: Arc<crate::cloud::commands::CloudState>,
    task_ids: Vec<String>,
    delete_files: bool,
) -> Result<(), String> {
    // A cancelled root can be visible before its in-flight URL resolution
    // returns. Do not delete membership while that submission is still linking.
    let mut locked_groups = Vec::new();
    for task_id in &task_ids {
        let id = Uuid::parse_str(task_id).map_err(|_| "任务 ID 无效".to_string())?;
        if store
            .cloud_directory_group(id)
            .map_err(|error| error.to_string())?
            .is_some()
        {
            locked_groups.push(id);
        } else if let Some(group) = store
            .cloud_directory_group_for_task(id)
            .map_err(|error| error.to_string())?
        {
            locked_groups.push(group.id);
        }
    }
    locked_groups.sort_unstable();
    locked_groups.dedup();
    let mut submission_guards = Vec::new();
    for id in locked_groups {
        submission_guards.push(
            crate::cloud::commands::lock_directory_submission(id)
                .await
                .map_err(|error| error.to_string())?,
        );
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _submission_guards = submission_guards;
        let mut ids = Vec::with_capacity(task_ids.len());
        let mut group_ids = Vec::new();
        let mut seen_task_ids = HashSet::with_capacity(task_ids.len());
        let mut seen_group_ids = HashSet::new();
        for task_id in task_ids {
            let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
            if store
                .cloud_directory_group(id)
                .map_err(|error| error.to_string())?
                .is_some()
            {
                if seen_group_ids.insert(id) {
                    group_ids.push(id);
                }
            } else if seen_task_ids.insert(id) {
                ids.push(id);
            }
        }
        let mut groups = Vec::with_capacity(group_ids.len());
        for group_id in group_ids {
            let (group, tasks) = prepare_cloud_directory_group_for_purge(&store, group_id)?;
            for task in tasks {
                if seen_task_ids.insert(task.id) {
                    ids.push(task.id);
                }
            }
            groups.push(group);
        }
        purge_recycled_task_records(&store, &cloud.share_plans, &ids, delete_files)?;
        for group in groups {
            store
                .remove_cloud_directory_group(group.id)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|_| "后台删除任务中断".to_string())?
}

fn prepare_cloud_directory_group_for_purge(
    store: &TaskStore,
    group_id: Uuid,
) -> Result<(CloudDirectoryGroup, Vec<DownloadTask>), String> {
    let mut group = store
        .cloud_directory_group(group_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "目录下载任务不存在".to_string())?;
    if group.status == TaskStatus::Cancelled {
        // Only runs after an explicit purge request; completion records/files
        // stay untouched by cancellation itself. This validates all members
        // are stopped before transitioning terminal siblings to the recycle bin.
        recycle_cloud_directory_group(store, group_id)?;
        group = store
            .cloud_directory_group(group_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "目录下载任务不存在".to_string())?;
    }
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
    if tasks.is_empty() && group.status != TaskStatus::Recycled {
        return Err("目录下载任务中没有可彻底删除的文件".to_string());
    }
    Ok((group, tasks))
}

#[cfg(test)]
fn purge_cloud_directory_group(
    store: &TaskStore,
    plans: &crate::cloud::share::CloudSharePlanStore,
    group_id: Uuid,
    delete_files: bool,
) -> Result<(), String> {
    let (group, tasks) = prepare_cloud_directory_group_for_purge(store, group_id)?;
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
    let linked_cloud_tasks = tasks
        .iter()
        .filter(|task| task.source.starts_with("cloud-share://"))
        .map(|task| (task.source.clone(), task.id))
        .collect::<Vec<_>>();
    plans
        .unlink_local_tasks(&linked_cloud_tasks)
        .map_err(|error| error.to_string())?;
    if let Err(error) = store.remove_task_records(ids) {
        let message = error.to_string();
        if let Err(restore_error) = plans.link_local_tasks(&linked_cloud_tasks) {
            return Err(format!("{message}；网盘关联恢复失败：{restore_error}"));
        }
        return Err(message);
    }
    Ok(())
}

#[tauri::command]
pub async fn select_files(
    task_id: String,
    indices: Vec<u32>,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    ensure_private_task_access(&state, &current)?;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
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
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    ensure_private_task_access(&state, &current)?;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
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

#[tauri::command]
pub fn get_privacy_task_visibility(
    state: State<'_, AppState>,
) -> Result<PrivacyTaskVisibility, String> {
    privacy_task_visibility(&state)
}

#[tauri::command]
pub fn configure_privacy_download(
    enabled: bool,
    password: Option<String>,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    crate::privacy::require_unlocked_configuration(&state.privacy_session, &state.ai_secrets)?;
    if let Some(password) = password {
        crate::privacy::configure_password(&state.ai_secrets, password)?;
    }
    let configured = crate::privacy::password_is_configured(&state.ai_secrets)?;
    if enabled && !configured {
        return Err("开启隐私下载前请先设置隐私模式密码".into());
    }
    state.privacy_session.lock();
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    settings.privacy_download_enabled = enabled;
    settings.privacy_password_configured = configured;
    persist_settings(&state.settings_path, &settings)?;
    Ok(settings.clone())
}

#[tauri::command]
pub fn unlock_privacy_tasks(
    password: String,
    state: State<'_, AppState>,
) -> Result<PrivacyTaskVisibility, String> {
    if !privacy_task_visibility(&state)?.enabled {
        return Err("请先开启隐私下载".into());
    }
    if !state.privacy_session.unlock(&state.ai_secrets, password)? {
        return Err("隐私模式密码不正确".into());
    }
    privacy_task_visibility(&state)
}

#[tauri::command]
pub fn lock_privacy_tasks(state: State<'_, AppState>) -> Result<PrivacyTaskVisibility, String> {
    state.privacy_session.lock();
    privacy_task_visibility(&state)
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
    settings.privacy_download_enabled = previous.privacy_download_enabled;
    settings.privacy_password_configured = previous.privacy_password_configured;
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
        cloud::{
            share::CloudShareEntry, share::CloudSharePlan, CloudProvider, MemoryCredentialStore,
        },
        models::{NewCloudDirectoryGroup, NewCloudDirectoryGroupItem},
        DuplicateDownloadState, NewTask,
    };

    #[test]
    fn private_visibility_requires_the_configured_password_for_this_session() {
        let credentials = MemoryCredentialStore::default();
        let session = crate::privacy::PrivacySession::default();

        crate::privacy::configure_password(&credentials, "correct horse battery staple".into())
            .unwrap();
        assert!(!session.is_unlocked());
        assert!(!session
            .unlock(&credentials, "wrong password".into())
            .unwrap());
        assert!(!session.is_unlocked());
        assert!(session
            .unlock(&credentials, "correct horse battery staple".into())
            .unwrap());
        assert!(session.is_unlocked());

        session.lock();
        assert!(!session.is_unlocked());
    }

    #[test]
    fn failed_cloud_authentication_task_projects_a_reauthorization_action() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("cloud://baidu/blocked-file"))
            .unwrap();
        store
            .fail_with_code(
                task.id,
                Some("web_auth_required"),
                "网盘要求完成官方操作验证后重试",
            )
            .unwrap();

        let projected = list_queue_tasks_for_scope(&store, None).unwrap();

        assert_eq!(projected.len(), 1);
        assert_eq!(
            projected[0].failure_code.as_deref(),
            Some("web_auth_required")
        );
        assert!(projected[0].reauthorization_required);
        assert_eq!(
            serde_json::to_value(&projected[0]).unwrap()["reauthorization_required"],
            true
        );
    }

    #[test]
    fn locked_queue_projection_excludes_private_tasks_and_private_directory_roots() {
        let store = TaskStore::open_in_memory().unwrap();
        let ordinary = store
            .create(NewTask::direct("https://example.test/ordinary.zip"))
            .unwrap();
        let private = store
            .create(NewTask::direct("https://example.test/private.zip"))
            .unwrap();
        store.set_private(private.id, true).unwrap();
        let private_group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "private-directory",
                    "私密目录",
                    "C:\\Downloads",
                )
                .with_private_download(true),
                &[],
            )
            .unwrap();

        let locked = list_queue_tasks_for_scope_with_privacy(&store, None, false).unwrap();
        assert_eq!(
            locked
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec![ordinary.id.to_string()]
        );
        assert!(!locked
            .iter()
            .any(|task| task.id == private_group.id.to_string()));

        let unlocked = list_queue_tasks_for_scope_with_privacy(&store, None, true).unwrap();
        assert_eq!(unlocked.len(), 3);
        assert!(unlocked
            .iter()
            .any(|task| task.id == private.id.to_string() && task.is_private));
        assert!(unlocked
            .iter()
            .any(|task| task.id == private_group.id.to_string() && task.is_private));
    }

    #[tokio::test]
    async fn background_queue_read_preserves_scopes() {
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let active = store
            .create(NewTask::direct("https://example.test/active.zip"))
            .unwrap();
        let completed = store
            .create(NewTask::direct("https://example.test/done.zip"))
            .unwrap();
        store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                ..completed
            })
            .unwrap();
        let active_tasks =
            list_queue_tasks_in_background(Arc::clone(&store), None, Some("active".into()), true)
                .await
                .unwrap();
        assert_eq!(active_tasks.len(), 1);
        assert_eq!(active_tasks[0].id, active.id.to_string());
        let terminal =
            list_queue_tasks_in_background(Arc::clone(&store), None, Some("terminal".into()), true)
                .await
                .unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn background_queue_read_yields_while_database_is_locked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tasks.sqlite");
        let store = Arc::new(TaskStore::open_path(&path).unwrap());
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let mut read = Box::pin(list_queue_tasks_in_background(store, None, None, true));
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(100), &mut read).await;
        // The caller can keep servicing events even while SQLite is waiting.
        blocker.execute_batch("ROLLBACK").unwrap();
        assert!(
            waiting.is_err(),
            "database waiting must yield to the caller"
        );
        assert!(read.await.unwrap().is_empty());
    }

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
                eta_seconds: Some(75),
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
        assert_eq!(active_scope[0].eta_seconds, Some(75));
        assert_eq!(active_scope[0].connections, 2);
        assert_eq!(active_scope[0].progress, 37);
        assert_eq!(active_scope[0].children[0].name, "Extras");
        assert_eq!(active_scope[0].children[0].status, TaskStatus::Downloading);
        assert_eq!(active_scope[0].children[0].progress, 37);
        assert_eq!(active_scope[0].children[0].total_length, Some(600));
        assert_eq!(active_scope[0].children[0].completed_length, 225);
        assert_eq!(active_scope[0].children[0].download_speed, 10);
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
    fn cloud_directory_group_progress_tracks_every_nested_file() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Baidu,
                    "remote",
                    "Series",
                    "C:\\Downloads",
                ),
                &[
                    NewCloudDirectoryGroupItem::new("small", &["Season"], "small.mp4", Some(100)),
                    NewCloudDirectoryGroupItem::new(
                        "large",
                        &["Season", "Extras"],
                        "large.mp4",
                        Some(900),
                    ),
                ],
            )
            .unwrap();
        let items = store.cloud_directory_group_items(group.id).unwrap();
        let initial = cloud_directory_group_queue_task(&store, group.id).unwrap();
        assert_eq!(initial.children[0].progress, 0);
        assert_eq!(initial.children[0].status, TaskStatus::Queued);
        assert_eq!(initial.children[0].children[1].progress, 0);
        let season_id = initial.children[0].id.clone();
        let extras_id = initial.children[0].children[1].id.clone();

        let small = store
            .create(NewTask::direct("https://example.test/small"))
            .unwrap();
        let large = store
            .create(NewTask::direct("https://example.test/large"))
            .unwrap();
        for (remote, task_id) in [("small", small.id), ("large", large.id)] {
            let item = items
                .iter()
                .find(|item| item.remote_file_id == remote)
                .unwrap();
            store
                .link_cloud_directory_group_item_task(group.id, item.id, task_id)
                .unwrap();
        }
        store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(100),
                completed_length: 100,
                ..small
            })
            .unwrap();
        for (status, bytes, expected_root, expected_inner) in [
            (TaskStatus::Downloading, 0, 10, 0),
            (TaskStatus::Downloading, 450, 55, 50),
            (TaskStatus::Paused, 450, 55, 50),
            (TaskStatus::Failed, 450, 55, 50),
            (TaskStatus::Completed, 900, 100, 100),
        ] {
            let updated = store
                .apply_transfer_snapshot(DownloadTask {
                    status,
                    total_length: Some(900),
                    completed_length: bytes,
                    progress: expected_inner,
                    ..large.clone()
                })
                .unwrap();
            let TaskUpdatePayload::Queue(root) =
                task_update_payload(&store, None, updated).unwrap()
            else {
                panic!("directory updates must publish the aggregate root");
            };
            let season = &root.children[0];
            let extras = &season.children[1];
            assert_eq!(root.progress, expected_root);
            assert_eq!(season.progress, expected_root);
            assert_eq!(extras.progress, expected_inner);
            assert_eq!(season.status, status);
            assert_eq!(extras.status, status);
            assert_eq!(season.total_length, Some(1000));
            assert_eq!(season.completed_length, 100 + bytes);
            assert_eq!(extras.total_length, Some(900));
            assert_eq!(extras.completed_length, bytes);
            assert_eq!(season.id, season_id);
            assert_eq!(extras.id, extras_id);
            assert_eq!(season.name, "Season");
            assert_eq!(extras.name, "Extras");
            assert_eq!(
                season.completed_at.is_some(),
                status == TaskStatus::Completed
            );
        }
    }

    #[test]
    fn cloud_directory_queue_serializes_transfer_metrics_with_download_task_field_names() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Pikpak,
                    "cloud-share://pikpak/share/root",
                    "分享目录",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "cloud-share://pikpak/share/file",
                    &["分享目录"],
                    "视频.mp4",
                    Some(1_000),
                )],
            )
            .unwrap();
        let item = store.cloud_directory_group_items(group.id).unwrap()[0].clone();
        let task = store
            .create(NewTask::direct("cloud-share://pikpak/share/file"))
            .unwrap();
        store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Downloading,
                progress: 25,
                total_length: Some(1_000),
                completed_length: 250,
                download_speed: 64,
                eta_seconds: Some(12),
                ..task.clone()
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        let json = serde_json::to_value(cloud_directory_group_queue_task(&store, group.id).unwrap())
            .unwrap();

        for node in [&json, &json["children"][0]] {
            assert_eq!(node["total_length"], 1_000);
            assert_eq!(node["completed_length"], 250);
            assert_eq!(node["download_speed"], 64);
            assert_eq!(node["eta_seconds"], 12);
            assert!(node.get("totalLength").is_none());
            assert!(node.get("completedLength").is_none());
            assert!(node.get("downloadSpeed").is_none());
            assert!(node.get("etaSeconds").is_none());
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelled_directory_can_purge_records_while_retaining_completed_files() {
        for include_completed in [false, true] {
            let output = tempfile::tempdir().unwrap();
            let store = TaskStore::open_in_memory().unwrap();
            let group = store
                .create_cloud_directory_group(
                    NewCloudDirectoryGroup::new(
                        CloudProvider::Quark,
                        "remote",
                        "Series",
                        output.path(),
                    ),
                    &[
                        NewCloudDirectoryGroupItem::new(
                            "first",
                            &["Series"],
                            "first.mp4",
                            Some(100),
                        ),
                        NewCloudDirectoryGroupItem::new(
                            "pending",
                            &["Series"],
                            "pending.mp4",
                            Some(200),
                        ),
                    ],
                )
                .unwrap();
            let file = output.path().join("first.mp4");
            fs::write(&file, b"downloaded file").unwrap();
            if include_completed {
                let task = store
                    .create(NewTask::direct("https://example.test/first"))
                    .unwrap();
                store
                    .apply_transfer_snapshot(DownloadTask {
                        status: TaskStatus::Completed,
                        ..task.clone()
                    })
                    .unwrap();
                store
                    .link_cloud_directory_group_item_task(
                        group.id,
                        store.cloud_directory_group_items(group.id).unwrap()[0].id,
                        task.id,
                    )
                    .unwrap();
            }
            crate::cloud::commands::cancel_cloud_directory_group_with(
                &store,
                group.id,
                |_| async { panic!("no active member") },
            )
            .await
            .unwrap();
            let plans = crate::cloud::share::CloudSharePlanStore::new(
                DpapiCredentialStore::new(output.path().to_path_buf()).unwrap(),
            );
            purge_cloud_directory_group(&store, &plans, group.id, false).unwrap();
            assert!(store.cloud_directory_group(group.id).unwrap().is_none());
            assert!(store.list().unwrap().is_empty());
            assert_eq!(fs::read(file).unwrap(), b"downloaded file");
        }
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

        // Known-size bytes reaching 100% must not hide an unfinished unknown-size file.
        store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                completed_length: 100,
                ..known
            })
            .unwrap();
        for (status, expected_progress) in [
            (TaskStatus::Downloading, 99),
            (TaskStatus::Failed, 99),
            (TaskStatus::Completed, 100),
        ] {
            store
                .apply_transfer_snapshot(DownloadTask {
                    status,
                    ..unknown.clone()
                })
                .unwrap();
            let root = cloud_directory_group_queue_task(&store, group.id).unwrap();
            assert_eq!(root.progress, expected_progress);
            assert_eq!(root.status, status);
        }
    }

    #[test]
    fn cloud_directory_group_uses_aria_file_lengths_when_the_task_total_is_missing() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "remote-directory",
                    "YM-JFSMBYHS",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-file",
                    &["YM-JFSMBYHS"],
                    "episode.mp4",
                    None,
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let task = store
            .create(NewTask::direct("cloud-share://quark/share/episode"))
            .unwrap();
        let task = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Downloading,
                progress: 25,
                total_length: None,
                completed_length: 250,
                ..task
            })
            .unwrap();
        store
            .upsert_files(
                task.id,
                &[(
                    0,
                    "C:\\Downloads\\YM-JFSMBYHS\\episode.mp4".into(),
                    1_000,
                    250,
                    true,
                )],
            )
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        let projected =
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("active")).unwrap();

        assert_eq!(projected[0].total_length, Some(1_000));
        assert_eq!(projected[0].completed_length, 250);
        assert_eq!(projected[0].progress, 25);
    }

    #[test]
    fn legacy_share_directory_group_promotes_its_parsed_root_in_the_queue() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "remote-directory",
                    "夸克分享目录",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-file",
                    &["夸克分享目录", "YM-JFSMBYHS"],
                    "episode.mp4",
                    Some(1_000),
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let task = store
            .create(NewTask::direct("cloud-share://quark/share/episode"))
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        let projected =
            project_cloud_directory_groups(&store, store.list().unwrap(), None).unwrap();

        assert_eq!(projected[0].name, "YM-JFSMBYHS");
        assert_eq!(projected[0].children.len(), 1);
        assert_eq!(projected[0].children[0].kind, "file");
    }

    #[test]
    fn completed_cloud_directory_member_clears_stale_failure_and_uses_planned_size() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Baidu,
                    "remote-directory",
                    "Season 1",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-episode",
                    &[] as &[&str],
                    "episode.mp4",
                    Some(1_572_864),
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let task = store
            .create(NewTask::direct("cloud-share://baidu/share/episode"))
            .unwrap();
        let completed = store
            .apply_transfer_snapshot(DownloadTask {
                name: "episode.mp4".into(),
                status: TaskStatus::Completed,
                progress: 100,
                error: Some("网盘分享下载失败，请重试或重新解析分享链接".into()),
                total_length: None,
                completed_length: 0,
                ..task
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, completed.id)
            .unwrap();

        let projected =
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("terminal"))
                .unwrap();
        let leaf = &projected[0].children[0];

        assert_eq!(projected[0].status, TaskStatus::Completed);
        assert_eq!(leaf.status, TaskStatus::Completed);
        assert_eq!(leaf.error, None);
        assert_eq!(leaf.total_length, Some(1_572_864));
        assert_eq!(leaf.completed_length, 1_572_864);
    }

    #[test]
    fn completed_cloud_directory_member_uses_its_downloaded_file_size_when_metadata_is_missing() {
        let root = std::env::temp_dir().join(format!("cloud-directory-size-{}", Uuid::new_v4()));
        let payload_path = root.join("Season 1").join("Extras").join("episode.mp4");
        std::fs::create_dir_all(payload_path.parent().unwrap()).unwrap();
        std::fs::write(&payload_path, vec![0_u8; 1_572_864]).unwrap();

        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Pikpak,
                    "remote-directory",
                    "Season 1",
                    &root,
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-episode",
                    &["Season 1", "Extras"],
                    "episode.mp4",
                    None,
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let task = store
            .create(NewTask::direct("cloud-share://pikpak/share/episode"))
            .unwrap();
        let completed = store
            .apply_transfer_snapshot(DownloadTask {
                name: "episode.mp4".into(),
                status: TaskStatus::Completed,
                progress: 100,
                total_length: None,
                completed_length: 0,
                ..task
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, completed.id)
            .unwrap();

        let projected =
            project_cloud_directory_groups(&store, store.list().unwrap(), Some("terminal"))
                .unwrap();
        let leaf = &projected[0].children[0].children[0];

        assert_eq!(leaf.total_length, Some(1_572_864));
        assert_eq!(leaf.completed_length, 1_572_864);
        std::fs::remove_dir_all(root).unwrap();
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
    fn failed_share_directory_stays_one_root_after_moving_to_recycle() {
        let store = TaskStore::open_in_memory().unwrap();
        let share_hash = "a".repeat(64);
        let first_hash = "b".repeat(64);
        let second_hash = "c".repeat(64);
        let first_source = format!("cloud-share://xunlei/{share_hash}/{first_hash}");
        let second_source = format!("cloud-share://xunlei/{share_hash}/{second_hash}");
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Xunlei,
                    first_source.clone(),
                    "迅雷云盘分享目录",
                    "C:\\Downloads",
                ),
                &[
                    NewCloudDirectoryGroupItem::new(
                        first_source.clone(),
                        &["迅雷云盘分享目录"],
                        "first.bin",
                        Some(10),
                    ),
                    NewCloudDirectoryGroupItem::new(
                        second_source.clone(),
                        &["迅雷云盘分享目录", "nested"],
                        "second.bin",
                        Some(20),
                    ),
                ],
            )
            .unwrap();
        let items = store.cloud_directory_group_items(group.id).unwrap();
        let first = store.create(NewTask::direct(first_source)).unwrap();
        let first = store
            .fail(first.id, "provider rejected the transfer")
            .unwrap();
        let second = store.create(NewTask::direct(second_source)).unwrap();
        let second = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(20),
                completed_length: 20,
                ..second
            })
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, items[0].id, first.id)
            .unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, items[1].id, second.id)
            .unwrap();

        let failed = list_queue_tasks_for_scope(&store, Some("terminal")).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, group.id.to_string());
        assert_eq!(failed[0].status, TaskStatus::Failed);
        assert_eq!(failed[0].children.len(), 2);

        let recycled = recycle_cloud_directory_group(&store, group.id).unwrap();
        assert_eq!(recycled.id, group.id.to_string());
        assert_eq!(recycled.status, TaskStatus::Recycled);
        let recycle_scope = list_queue_tasks_for_scope(&store, Some("terminal")).unwrap();
        assert_eq!(recycle_scope.len(), 1);
        assert_eq!(recycle_scope[0].id, group.id.to_string());
        assert_eq!(recycle_scope[0].status, TaskStatus::Recycled);
        assert_eq!(recycle_scope[0].children.len(), 2);
    }

    #[test]
    fn virtual_failed_directory_group_moves_to_recycle_without_local_members() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "remote-wallpapers",
                    "夸克精选壁纸",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "remote-wallpaper",
                    &["夸克精选壁纸"],
                    "wallpaper.jpg",
                    Some(100),
                )],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        store
            .fail_cloud_directory_group_item(
                group.id,
                item.id,
                "duplicate_active",
                "已存在下载任务，未重复入队",
            )
            .unwrap();

        let recycled = recycle_cloud_directory_group(&store, group.id).unwrap();

        assert_eq!(recycled.status, TaskStatus::Recycled);
        let (_, local_members) = prepare_cloud_directory_group_for_purge(&store, group.id).unwrap();
        assert!(local_members.is_empty());
    }

    #[test]
    fn cancelled_virtual_directory_recycles_without_queued_children() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "root",
                    "Series",
                    "C:\\Downloads",
                ),
                &[NewCloudDirectoryGroupItem::new(
                    "pending",
                    &["Series", "Season"],
                    "pending.mp4",
                    Some(100),
                )],
            )
            .unwrap();
        store.cancel_cloud_directory_group(group.id).unwrap();
        let recycled = recycle_cloud_directory_group(&store, group.id).unwrap();
        assert_eq!(recycled.status, TaskStatus::Recycled);
        assert_eq!(recycled.children[0].status, TaskStatus::Recycled);
        assert_eq!(
            recycled.children[0].children[0].status,
            TaskStatus::Recycled
        );
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

    #[cfg(windows)]
    #[test]
    fn permanent_purge_keeps_a_task_when_its_cloud_share_plan_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let source = format!("cloud-share://baidu/{}/{}", "a".repeat(64), "b".repeat(64));
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create(NewTask::direct(&source)).unwrap();
        let task = store.fail(task.id, "fixture").unwrap();
        let task = store.transition(task.id, TaskStatus::Recycled).unwrap();

        assert!(purge_recycled_task_records(&store, &plans, &[task.id], false).is_err());

        assert!(store.find(task.id).is_ok());
    }

    #[test]
    fn background_purge_batch_removes_only_its_supplied_recycled_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let cloud = Arc::new(
            crate::cloud::commands::CloudState::new(directory.path().join("cloud")).unwrap(),
        );
        let first = store
            .create(NewTask::direct("https://example.test/first"))
            .unwrap();
        let first = store.fail(first.id, "fixture").unwrap();
        let first = store.transition(first.id, TaskStatus::Recycled).unwrap();
        let second = store
            .create(NewTask::direct("https://example.test/second"))
            .unwrap();
        let second = store.fail(second.id, "fixture").unwrap();
        let second = store.transition(second.id, TaskStatus::Recycled).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(purge_recycled_task_batch(
                Arc::clone(&store),
                Arc::clone(&cloud),
                vec![first.id.to_string()],
                false,
            ))
            .unwrap();

        assert!(matches!(
            store.find(first.id),
            Err(crate::StoreError::NotFound(_))
        ));
        assert!(store.find(second.id).is_ok());
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
            .split_once("pub(crate) async fn resubmit_task(")
            .expect("resubmit task exists")
            .1
            .split_once("fn task_output_directory")
            .expect("resubmit task has a stable end")
            .0;
        let reserve = body
            .find("reserve_cloud_share_resubmission")
            .expect("cloud-share retry must reserve its original task");
        let engine = body
            .find(".service()")
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
    fn resolves_only_a_completed_single_existing_task_file_for_associated_application() {
        let root = std::env::temp_dir().join(format!("task-open-file-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("notes.txt");
        std::fs::write(&payload, b"notes").unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/notes.txt"))
            .unwrap();
        store
            .upsert_files(
                task.id,
                &[(0, payload.to_string_lossy().into_owned(), 5, 5, true)],
            )
            .unwrap();

        assert_eq!(
            task_associated_file_path(&store, task.id).unwrap_err(),
            "仅已完成任务可以打开文件"
        );

        let completed = store
            .apply_transfer_snapshot(DownloadTask {
                name: "notes.txt".into(),
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(5),
                completed_length: 5,
                ..task
            })
            .unwrap();
        assert_eq!(
            task_associated_file_path(&store, completed.id).unwrap(),
            payload
        );

        std::fs::remove_file(&payload).unwrap();
        assert_eq!(
            task_associated_file_path(&store, completed.id).unwrap_err(),
            "下载文件已不存在，无法打开"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn associated_application_rejects_relative_and_unsupported_file_paths() {
        let root = std::env::temp_dir().join(format!("task-open-file-policy-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("notes.txt#1.exe");
        std::fs::write(&executable, b"binary").unwrap();
        let store = TaskStore::open_in_memory().unwrap();

        let relative_task = store
            .create(NewTask::direct("https://example.com/notes.txt"))
            .unwrap();
        store
            .upsert_files(relative_task.id, &[(0, "notes.txt".into(), 5, 5, true)])
            .unwrap();
        let relative_task = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(5),
                completed_length: 5,
                ..relative_task
            })
            .unwrap();
        assert_eq!(
            task_associated_file_path(&store, relative_task.id).unwrap_err(),
            "下载文件路径不是本地绝对路径，无法打开"
        );

        let executable_task = store
            .create(NewTask::direct("https://example.com/notes.txt#1.exe"))
            .unwrap();
        store
            .upsert_files(
                executable_task.id,
                &[(0, executable.to_string_lossy().into_owned(), 6, 6, true)],
            )
            .unwrap();
        let executable_task = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                progress: 100,
                total_length: Some(6),
                completed_length: 6,
                ..executable_task
            })
            .unwrap();
        assert_eq!(
            task_associated_file_path(&store, executable_task.id).unwrap_err(),
            "该文件类型不支持使用系统默认应用打开"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_existing_media_files_are_collected_for_default_playback() {
        let root = std::env::temp_dir().join(format!("task-videos-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let video = root.join("episode.MKV");
        let audio = root.join("commentary.MP3");
        let document = root.join("notes.txt");
        let skipped_video = root.join("trailer.mp4");
        std::fs::write(&video, b"video").unwrap();
        std::fs::write(&audio, b"audio").unwrap();
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
                    (1, audio.to_string_lossy().into_owned(), 5, 5, true),
                    (2, document.to_string_lossy().into_owned(), 5, 5, true),
                    (3, skipped_video.to_string_lossy().into_owned(), 7, 0, false),
                ],
            )
            .unwrap();

        assert_eq!(
            task_video_paths(&store, task.id).unwrap(),
            vec![video, audio]
        );
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

    #[cfg(windows)]
    #[test]
    fn permanent_recycle_cleanup_removes_metadata_only_task_without_a_local_path() {
        let directory = tempfile::tempdir().unwrap();
        let plans = crate::cloud::share::CloudSharePlanStore::new(
            DpapiCredentialStore::new(directory.path().to_path_buf()).unwrap(),
        );
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("magnet:?xt=urn:btih:metadata-only"))
            .unwrap();
        store
            .upsert_files(task.id, &[(0, String::new(), 0, 0, true)])
            .unwrap();
        let task = store.fail(task.id, "获取文件信息失败").unwrap();
        let task = store.transition(task.id, TaskStatus::Recycled).unwrap();

        purge_recycled_task_records(&store, &plans, &[task.id], true).unwrap();

        assert!(matches!(
            store.find(task.id),
            Err(crate::StoreError::NotFound(_))
        ));
    }
}
