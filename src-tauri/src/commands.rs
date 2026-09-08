use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::cloud::commands::{resolve_download, safe_filename, CloudState, ResolvedCloudDownload};
use crate::cloud::{CloudProvider, QuarkError, ReqwestTransport};
use crate::{
    confirm_download, prepare_download, AppSettings, Aria2Engine, Aria2Status, DownloadFileState,
    DownloadTask, DuplicateDownloadState, ParseOutcome, ProxySettings, StoredDownloadFile,
    TaskStatus, TaskStore,
};

pub struct AppState {
    pub store: Arc<TaskStore>,
    pub engine: Arc<Aria2Engine>,
    pub settings: Mutex<AppSettings>,
    pub settings_path: PathBuf,
    pub cloud: Arc<CloudState>,
    pub recovering: Mutex<HashSet<Uuid>>,
    pub cloud_refresh_attempts: Mutex<HashSet<Uuid>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutomaticRecovery {
    MissingGid,
    RefreshCloudLink,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskActionOutcome {
    task: DownloadTask,
    #[serde(skip_serializing_if = "Option::is_none")]
    duplicate: Option<DuplicateDownloadState>,
}

struct TaskOperation<'a> {
    state: &'a AppState,
    ids: Vec<Uuid>,
}
impl Drop for TaskOperation<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.state.recovering.lock() {
            for id in &self.ids {
                pending.remove(id);
            }
        }
    }
}

fn begin_task_operation(state: &AppState, id: Uuid) -> Result<TaskOperation<'_>, String> {
    let mut pending = state
        .recovering
        .lock()
        .map_err(|_| "任务操作锁不可用".to_string())?;
    if !pending.insert(id) {
        return Err("任务正在恢复或处理操作，请稍后重试".into());
    }
    Ok(TaskOperation {
        state,
        ids: vec![id],
    })
}

pub fn prepare_recovery(state: &AppState) -> Result<Vec<DownloadTask>, String> {
    let mut pending = state
        .recovering
        .lock()
        .map_err(|_| "任务操作锁不可用".to_string())?;
    let stored_tasks = state
        .store
        .list()
        .map_err(|error| error.to_string())?;
    let mut tasks = Vec::new();
    for task in stored_tasks {
        if matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Paused
        ) {
            tasks.push(task);
        } else if task.status == TaskStatus::Failed
            && (task.error.as_deref().is_some_and(is_missing_gid_error)
                || is_refreshable_cloud_failure(&task))
        {
            tasks.push(
                state
                    .store
                    .prepare_retry(task.id)
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    pending.extend(tasks.iter().map(|task| task.id));
    Ok(tasks)
}

fn parse_cloud_source(source: &str) -> Result<Option<(CloudProvider, String)>, String> {
    if !source
        .split(':')
        .next()
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("cloud"))
    {
        return Ok(None);
    }
    let invalid = || "网盘任务来源无效，请从网盘重新创建下载".to_string();
    let (provider, encoded) = source
        .strip_prefix("cloud://")
        .and_then(|s| s.split_once('/'))
        .ok_or_else(invalid)?;
    let provider = match provider {
        "quark" => CloudProvider::Quark,
        "pikpak" => CloudProvider::Pikpak,
        _ => return Err(invalid()),
    };
    if encoded.is_empty() || encoded.contains(['/', '?', '#', '&', '=']) || !encoded.is_ascii() {
        return Err(invalid());
    }
    for (index, byte) in encoded.bytes().enumerate() {
        if byte == b'%'
            && !encoded
                .as_bytes()
                .get(index + 1..index + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(invalid());
        }
    }
    let query = format!("id={encoded}");
    let id = url::form_urlencoded::parse(query.as_bytes())
        .next()
        .ok_or_else(invalid)?
        .1
        .into_owned();
    if id.trim().is_empty() || id.chars().any(|c| c.is_control() || c == '\u{fffd}') {
        return Err(invalid());
    }
    Ok(Some((provider, id)))
}

fn cloud_download_error(error: QuarkError) -> String {
    match error {
        QuarkError::AuthorizationExpired
        | QuarkError::WebAuthorizationRequired
        | QuarkError::AuthorizationPending
        | QuarkError::Storage => error.to_string(),
        QuarkError::Transport(_) => "网盘网络请求失败，请检查网络后重试".into(),
        _ => "网盘无法重新获取下载地址，请检查文件权限或重新登录授权".into(),
    }
}

fn enqueue_task_source(
    source: &str,
    resolve: impl FnOnce(CloudProvider, &str) -> Result<ResolvedCloudDownload, QuarkError>,
    enqueue: impl FnOnce(&str, &[(String, String)], Option<&str>) -> Result<String, String>,
) -> Result<String, String> {
    match parse_cloud_source(source)? {
        Some((provider, id)) => {
            let resolved = resolve(provider, &id).map_err(cloud_download_error)?;
            // Signed URLs and credentials are transient; only the stable source is in TaskStore.
            let filename = safe_filename(&resolved.name);
            enqueue(&resolved.url, &resolved.headers, Some(&filename))
                .map_err(|_| "aria2 无法创建网盘下载任务，请重试".into())
        }
        None => enqueue(source, &[], None),
    }
}

fn enqueue_task(
    task: &DownloadTask,
    state: &AppState,
    directory: &Path,
    proxy: &ProxySettings,
    gid: Option<&str>,
    allow_overwrite: bool,
) -> Result<String, String> {
    enqueue_task_source(
        &task.source,
        |provider, id| {
            state.cloud.with_provider(provider, |credentials| {
                resolve_download(provider, id, credentials, &ReqwestTransport::new()?)
            })
        },
        |url, headers, filename| {
            state
                .engine
                .add_uri_with_options_and_overwrite(
                    url,
                    directory,
                    proxy,
                    gid,
                    headers,
                    filename,
                    allow_overwrite,
                )
                .map_err(|error| error.to_string())
        },
    )
}

fn record_start_failure(task: &DownloadTask, store: &TaskStore, message: &str) {
    let _ = store.sync_transfer(
        task.id,
        TaskStatus::Failed,
        task.progress,
        None,
        task.size.clone(),
        Some(message.into()),
    );
}

fn download_options(state: &AppState) -> Result<(PathBuf, ProxySettings), String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    Ok((
        PathBuf::from(&settings.download_directory),
        settings.proxy.clone(),
    ))
}

fn task_download_directory(task: &DownloadTask, default_directory: &Path) -> PathBuf {
    if let Some(directory) = task.directory.as_deref() {
        return PathBuf::from(directory);
    }
    default_directory.to_path_buf()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentPresence {
    AllPresent,
    Missing,
    Partial,
}

fn content_presence(task: &DownloadTask) -> ContentPresence {
    if task.content_files.is_empty() {
        let has_legacy_content = task
            .directory
            .as_deref()
            .map(PathBuf::from)
            .zip(
                Path::new(&task.name)
                    .file_name()
                    .filter(|name| !name.is_empty())
                    .map(PathBuf::from),
            )
            .is_some_and(|(directory, name)| directory.join(name).exists());
        return if has_legacy_content {
            ContentPresence::AllPresent
        } else {
            ContentPresence::Missing
        };
    }

    let present = task
        .content_files
        .iter()
        .filter(|file| {
            std::fs::metadata(&file.path)
                .map(|metadata| file.length == 0 || metadata.len() == file.length)
                .unwrap_or(false)
        })
        .count();
    if present == task.content_files.len() {
        ContentPresence::AllPresent
    } else if present == 0 {
        ContentPresence::Missing
    } else {
        ContentPresence::Partial
    }
}

fn payload_path(task: &DownloadTask, file_path: &str) -> PathBuf {
    let path = PathBuf::from(file_path);
    if path.is_absolute() {
        path
    } else if let Some(directory) = task.directory.as_deref() {
        PathBuf::from(directory).join(path)
    } else {
        path
    }
}

fn can_resume(task: &DownloadTask) -> bool {
    !task.content_files.is_empty()
        && task.content_files.iter().all(|file| {
            let payload = PathBuf::from(&file.path);
            payload.is_file() && PathBuf::from(format!("{}.aria2", payload.to_string_lossy())).is_file()
        })
}

fn has_existing_payload(task: &DownloadTask) -> bool {
    if task.content_files.is_empty() {
        return task
            .directory
            .as_deref()
            .map(PathBuf::from)
            .zip(
                Path::new(&task.name)
                    .file_name()
                    .filter(|name| !name.is_empty())
                    .map(PathBuf::from),
            )
            .is_some_and(|(directory, name)| directory.join(name).is_file());
    }
    task.content_files
        .iter()
        .any(|file| PathBuf::from(&file.path).is_file())
}

fn retry_duplicate_state(task: &DownloadTask) -> Option<DuplicateDownloadState> {
    let state = duplicate_state(task);
    (state == DuplicateDownloadState::ResumeAvailable
        || (state == DuplicateDownloadState::OverwriteRequired && has_existing_payload(task)))
        .then_some(state)
}

fn duplicate_state(task: &DownloadTask) -> DuplicateDownloadState {
    match task.status {
        TaskStatus::Draft
        | TaskStatus::Parsing
        | TaskStatus::AwaitingUser
        | TaskStatus::Queued
        | TaskStatus::Downloading
        | TaskStatus::Processing => DuplicateDownloadState::ExistingActive,
        TaskStatus::Completed => match content_presence(task) {
            ContentPresence::AllPresent => DuplicateDownloadState::CompletedFilesPresent,
            ContentPresence::Missing => DuplicateDownloadState::CompletedFilesMissing,
            ContentPresence::Partial => DuplicateDownloadState::OverwriteRequired,
        },
        TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled if can_resume(task) => {
            DuplicateDownloadState::ResumeAvailable
        }
        TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled => {
            DuplicateDownloadState::OverwriteRequired
        }
    }
}

fn automatic_recovery_reason(
    task: &DownloadTask,
    status: Option<&Aria2Status>,
    status_error: Option<&str>,
    cloud_link_was_refreshed: bool,
) -> Option<AutomaticRecovery> {
    if task.source.starts_with("cloud://")
        && !cloud_link_was_refreshed
        && status.is_some_and(|status| status.status == "error" && status.error_code == "22")
    {
        return Some(AutomaticRecovery::RefreshCloudLink);
    }
    is_missing_gid_error(status_error?).then_some(AutomaticRecovery::MissingGid)
}

fn is_missing_gid_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("gid") && (error.contains("not found") || error.contains("does not exist"))
}

fn is_refreshable_cloud_failure(task: &DownloadTask) -> bool {
    task.source.starts_with("cloud://")
        && task
            .error
            .as_deref()
            .is_some_and(|error| error.contains("aria2 错误码 22"))
}

fn cloud_link_was_refreshed(state: &AppState, id: Uuid) -> Result<bool, String> {
    state
        .cloud_refresh_attempts
        .lock()
        .map(|attempts| attempts.contains(&id))
        .map_err(|_| "网盘重试状态锁不可用".to_string())
}

fn clear_cloud_link_refresh(state: &AppState, id: Uuid) {
    if let Ok(mut attempts) = state.cloud_refresh_attempts.lock() {
        attempts.remove(&id);
    }
}

fn recovery_snapshot_is_current(task: &DownloadTask, store: &TaskStore) -> Result<bool, String> {
    let current = store.find(task.id).map_err(|error| error.to_string())?;
    Ok(
        matches!(
            current.status,
            TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Paused
        ) && current.aria2_gid == task.aria2_gid,
    )
}

fn recover_task_automatically(
    task: DownloadTask,
    reason: AutomaticRecovery,
    state: &AppState,
) -> Result<DownloadTask, String> {
    let _operation = begin_task_operation(state, task.id)?;
    if !recovery_snapshot_is_current(&task, &state.store)? {
        return Err("下载任务状态已变化，不再自动恢复".into());
    }
    if reason == AutomaticRecovery::RefreshCloudLink {
        let mut attempts = state
            .cloud_refresh_attempts
            .lock()
            .map_err(|_| "网盘重试状态锁不可用".to_string())?;
        if !attempts.insert(task.id) {
            return Err("网盘临时链接已经自动刷新过一次".into());
        }
    }
    let mut recovered = start_task(task, state)?;
    recovered.status_message = Some(match reason {
        AutomaticRecovery::MissingGid => "aria2 已重启，正在恢复下载".into(),
        AutomaticRecovery::RefreshCloudLink => "网盘下载请求异常，正在刷新地址后重试".into(),
    });
    Ok(recovered)
}

fn start_task(task: DownloadTask, state: &AppState) -> Result<DownloadTask, String> {
    start_task_with_overwrite(task, state, false)
}

fn start_task_with_overwrite(
    task: DownloadTask,
    state: &AppState,
    allow_overwrite: bool,
) -> Result<DownloadTask, String> {
    let (directory, proxy) = download_options(state)?;
    let directory = task_download_directory(&task, &directory);
    let gid = match enqueue_task(&task, state, &directory, &proxy, None, allow_overwrite) {
        Ok(gid) => gid,
        Err(error) => {
            let message = format!("创建下载任务失败：{error}");
            record_start_failure(&task, &state.store, &message);
            return Err(message);
        }
    };
    state
        .store
        .start_download(task.id, &gid, &directory)
        .map_err(|error| error.to_string())
}

fn parse_u64(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

fn aria2_http_status(error: &str) -> Option<u16> {
    let status = error
        .to_ascii_lowercase()
        .split("status=")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    status.parse().ok()
}

fn cloud_transfer_error(status: &Aria2Status) -> String {
    let aria2_code = parse_u64(&status.error_code);
    if let Some(http_status) = aria2_http_status(&status.error_message) {
        format!(
            "网盘下载请求被服务器拒绝（HTTP {http_status}；aria2 错误码 {aria2_code}），请重新授权后重试"
        )
    } else {
        format!(
            "网盘下载请求异常（aria2 错误码 {aria2_code}），请重新授权后重试"
        )
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}小时{}分", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}分{}秒", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}秒")
    }
}

fn is_download_payload(path: &str) -> bool {
    !PathBuf::from(path)
        .file_name()
        .map(|name| name.to_string_lossy().starts_with("[METADATA]"))
        .unwrap_or(false)
}

fn sync_status(
    task: &DownloadTask,
    status: Aria2Status,
    store: &TaskStore,
) -> Result<DownloadTask, String> {
    if let Some(next_gid) = status.followed_by.first() {
        return store
            .replace_gid(task.id, next_gid)
            .map_err(|error| error.to_string());
    }
    let total = parse_u64(&status.total_length);
    let completed = parse_u64(&status.completed_length);
    let speed = parse_u64(&status.download_speed);
    let fetching_metadata = status
        .files
        .iter()
        .any(|file| !is_download_payload(&file.path));
    let progress = if total == 0 || fetching_metadata {
        0
    } else {
        ((completed.saturating_mul(100) / total).min(100)) as u8
    };
    let mapped = match status.status.as_str() {
        "active" | "waiting" => TaskStatus::Downloading,
        "paused" => TaskStatus::Paused,
        "complete" => TaskStatus::Completed,
        "error" => TaskStatus::Failed,
        "removed" => TaskStatus::Cancelled,
        _ => task.status,
    };
    let error = if mapped == TaskStatus::Failed {
        Some(if task.source.starts_with("cloud://") {
            cloud_transfer_error(&status)
        } else if status.error_message.is_empty() {
            format!("aria2 错误码 {}", status.error_code)
        } else {
            status.error_message
        })
    } else {
        None
    };
    let child_status = mapped;
    let children = status
        .files
        .iter()
        .filter(|file| file.selected != "false" && is_download_payload(&file.path))
        .map(|file| {
            let length = parse_u64(&file.length);
            let completed = parse_u64(&file.completed_length);
            let progress = if length == 0 {
                0
            } else {
                ((completed.saturating_mul(100) / length).min(100)) as u8
            };
            DownloadFileState {
                id: format!("{}-file-{}", task.id, file.index),
                name: PathBuf::from(&file.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.path.clone()),
                kind: "file".into(),
                status: if progress == 100 {
                    TaskStatus::Completed
                } else {
                    child_status
                },
                progress,
                size: if length > 0 {
                    Some(format!(
                        "{} / {}",
                        format_bytes(completed),
                        format_bytes(length)
                    ))
                } else {
                    None
                },
            }
        })
        .collect::<Vec<_>>();
    let content_files = status
        .files
        .iter()
        .filter(|file| file.selected != "false" && is_download_payload(&file.path))
        .map(|file| StoredDownloadFile {
            path: payload_path(task, &file.path).to_string_lossy().into_owned(),
            length: parse_u64(&file.length),
        })
        .collect::<Vec<_>>();
    if !content_files.is_empty() {
        store
            .store_content_files(task.id, &content_files)
            .map_err(|error| error.to_string())?;
    }
    let mut updated = store
        .sync_transfer(
            task.id,
            mapped,
            if mapped == TaskStatus::Completed {
                100
            } else {
                progress.min(99)
            },
            Some(format!("{}/s", format_bytes(speed))),
            if total > 0 && !fetching_metadata {
                Some(format!(
                    "{} / {}",
                    format_bytes(completed),
                    format_bytes(total)
                ))
            } else {
                None
            },
            error,
        )
        .map_err(|error| error.to_string())?;
    updated.connections = status.connections.parse().ok();
    updated.speed_bytes = speed;
    updated.eta = if speed > 0 && completed < total && !fetching_metadata {
        Some(format_duration((total - completed) / speed))
    } else {
        None
    };
    updated.status_message = if fetching_metadata && status.status == "active" {
        Some(match updated.connections {
            Some(0) => "正在获取种子信息，暂无节点连接".into(),
            Some(count) => format!("正在获取种子信息，已连接 {count} 个节点"),
            None => "正在获取种子信息".into(),
        })
    } else {
        None
    };
    updated.children = children;
    updated.content_files = content_files;
    if updated.children.len() > 1 {
        updated.kind = "folder".into();
    }
    if let Some(name) = status
        .bittorrent
        .and_then(|torrent| torrent.info)
        .map(|info| info.name)
        .filter(|name| !name.is_empty())
    {
        updated.name = name;
    }
    Ok(updated)
}

pub fn recover_downloads(state: &AppState, tasks: Vec<DownloadTask>) {
    let options = download_options(state);
    for task in tasks.into_iter().filter(|task| {
        matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Paused
        )
    }) {
        let _operation = TaskOperation {
            state,
            ids: vec![task.id],
        };
        let (default_directory, proxy) = match &options {
            Ok(options) => options,
            Err(error) => {
                record_start_failure(&task, &state.store, error);
                continue;
            }
        };
        let directory = task_download_directory(&task, default_directory);
        match enqueue_task(
            &task,
            state,
            &directory,
            proxy,
            task.aria2_gid.as_deref(),
            false,
        ) {
            Ok(gid) => {
                let _ = state.store.start_download(task.id, &gid, &directory);
                if task.status == TaskStatus::Paused {
                    if state.engine.pause(&gid).is_err() {
                        record_start_failure(&task, &state.store, "恢复暂停状态失败，请重试");
                    }
                }
            }
            Err(error) => {
                let _ = state.store.sync_transfer(
                    task.id,
                    TaskStatus::Failed,
                    task.progress,
                    None,
                    task.size,
                    Some(format!("恢复下载失败：{error}")),
                );
            }
        }
    }
}

#[tauri::command]
pub fn parse_download(input: String, state: State<'_, AppState>) -> Result<ParseOutcome, String> {
    let candidate = crate::classify_input(&input);
    let content_key = crate::models::content_key_for(&candidate.uri);
    if let Some(task) = state
        .store
        .find_by_content_key(&content_key)
        .map_err(|error| error.to_string())?
    {
        return Ok(ParseOutcome {
            duplicate: Some(duplicate_state(&task)),
            task,
            requires_confirmation: false,
        });
    }
    let mut outcome = prepare_download(&input, &state.store).map_err(|error| error.to_string())?;
    if !outcome.requires_confirmation {
        outcome.task = start_task(outcome.task, &state)?;
    }
    Ok(outcome)
}

#[tauri::command]
pub fn resolve_duplicate_download(
    task_id: String,
    action: String,
    state: State<'_, AppState>,
) -> Result<DownloadTask, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let _operation = begin_task_operation(&state, id)?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    let resolution = duplicate_state(&current);
    let (task, allow_overwrite) = match (action.as_str(), resolution) {
        ("resume", DuplicateDownloadState::ResumeAvailable) => {
            if current.status == TaskStatus::Paused {
                if let Some(gid) = current.aria2_gid.as_deref() {
                    state.engine.unpause(gid).map_err(|error| error.to_string())?;
                    return state
                        .store
                        .transition(current.id, TaskStatus::Downloading)
                        .map_err(|error| error.to_string());
                }
            }
            (
                state
                    .store
                    .prepare_retry(id)
                    .map_err(|error| error.to_string())?,
                false,
            )
        }
        ("restart_missing", DuplicateDownloadState::CompletedFilesMissing) => (
            state
                .store
                .restart_download(id)
                .map_err(|error| error.to_string())?,
            false,
        ),
        ("overwrite", DuplicateDownloadState::OverwriteRequired) => {
            let restarted = if current.status == TaskStatus::Completed {
                state.store.restart_download(id)
            } else {
                state.store.prepare_retry(id)
            }
            .map_err(|error| error.to_string())?;
            (restarted, true)
        }
        _ => return Err("重复下载状态已变化，请重新提交下载链接".into()),
    };
    start_task_with_overwrite(task, &state, allow_overwrite)
}

#[tauri::command]
pub fn confirm_download_command(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<DownloadTask, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = confirm_download(id, &state.store).map_err(|error| error.to_string())?;
    start_task(task, &state)
}

#[tauri::command]
pub async fn list_tasks(app: AppHandle) -> Result<Vec<DownloadTask>, String> {
    tauri::async_runtime::spawn_blocking(move || refresh_tasks(&app.state::<AppState>()))
        .await
        .map_err(|_| "读取下载任务中断".to_string())?
}

fn refresh_tasks(state: &AppState) -> Result<Vec<DownloadTask>, String> {
    // Hold this short RPC/snapshot boundary so an action cannot invalidate a GID mid-poll.
    // Cloud HTTPS resolution never holds this lock.
    let pending = {
        let pending = state
            .recovering
            .lock()
            .map_err(|_| "任务操作锁不可用".to_string())?;
        pending.clone()
    };
    let tasks = state.store.list().map_err(|error| error.to_string())?;
    let mut refreshed = Vec::with_capacity(tasks.len());
    for mut task in tasks {
        if pending.contains(&task.id) {
            task.status_message = Some("正在恢复下载或处理任务操作，请稍候".into());
            refreshed.push(task);
            continue;
        }
        if matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Paused
        ) {
            if let Some(gid) = task.aria2_gid.as_deref() {
                match state.engine.status(gid) {
                    Ok(status) => {
                        let refreshed_link = cloud_link_was_refreshed(state, task.id)?;
                        if let Some(reason) = automatic_recovery_reason(
                            &task,
                            Some(&status),
                            None,
                            refreshed_link,
                        ) {
                            match recover_task_automatically(task.clone(), reason, state) {
                                Ok(recovered) => {
                                    refreshed.push(recovered);
                                    continue;
                                }
                                Err(_) => {
                                    refreshed.push(
                                        state
                                            .store
                                            .find(task.id)
                                            .map_err(|error| error.to_string())?,
                                    );
                                    continue;
                                }
                            }
                        }
                        if status.status != "error" {
                            clear_cloud_link_refresh(state, task.id);
                        }
                        refreshed.push(sync_status(&task, status, &state.store)?);
                        continue;
                    }
                    Err(error) => {
                        if automatic_recovery_reason(
                            &task,
                            None,
                            Some(&error.to_string()),
                            false,
                        ) == Some(AutomaticRecovery::MissingGid)
                        {
                            match recover_task_automatically(
                                task.clone(),
                                AutomaticRecovery::MissingGid,
                                state,
                            ) {
                                Ok(recovered) => {
                                    refreshed.push(recovered);
                                    continue;
                                }
                                Err(_) => {
                                    refreshed.push(
                                        state
                                            .store
                                            .find(task.id)
                                            .map_err(|error| error.to_string())?,
                                    );
                                    continue;
                                }
                            }
                        }
                        refreshed.push(
                            state
                                .store
                                .sync_transfer(
                                    task.id,
                                    TaskStatus::Failed,
                                    task.progress,
                                    None,
                                    task.size.clone(),
                                    Some(if task.source.starts_with("cloud://") {
                                        "读取网盘下载状态失败，请右键重试".into()
                                    } else {
                                        format!("读取 aria2 状态失败：{error}")
                                    }),
                                )
                                .map_err(|error| error.to_string())?,
                        );
                        continue;
                    }
                }
            }
        }
        refreshed.push(task);
    }
    Ok(refreshed)
}

#[tauri::command]
pub async fn task_action(
    task_id: String,
    action: String,
    app: AppHandle,
) -> Result<TaskActionOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        task_action_blocking(task_id, action, &app.state::<AppState>())
    })
    .await
    .map_err(|_| "下载任务操作中断，请重试".to_string())?
}

fn task_action_blocking(
    task_id: String,
    action: String,
    state: &AppState,
) -> Result<TaskActionOutcome, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let _operation = begin_task_operation(state, id)?;
    if action == "retry" {
        let current = state.store.find(id).map_err(|error| error.to_string())?;
        if let Some(duplicate) = retry_duplicate_state(&current) {
            return Ok(TaskActionOutcome {
                task: current,
                duplicate: Some(duplicate),
            });
        }
        clear_cloud_link_refresh(state, id);
        let task = state
            .store
            .prepare_retry(id)
            .map_err(|error| error.to_string())?;
        return start_task(task, &state).map(|task| TaskActionOutcome {
            task,
            duplicate: None,
        });
    }
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    let gid = current.aria2_gid.as_deref();
    let next = match action.as_str() {
        "pause" if current.status == TaskStatus::Downloading => {
            state
                .engine
                .pause(gid.ok_or("任务缺少 aria2 GID")?)
                .map_err(|error| error.to_string())?;
            TaskStatus::Paused
        }
        "resume" if current.status == TaskStatus::Paused => {
            state
                .engine
                .unpause(gid.ok_or("任务缺少 aria2 GID")?)
                .map_err(|error| error.to_string())?;
            TaskStatus::Downloading
        }
        "cancel" => {
            if let Some(gid) = gid {
                state
                    .engine
                    .remove(gid)
                    .map_err(|error| error.to_string())?;
            }
            TaskStatus::Cancelled
        }
        _ => return Err("当前任务不能执行此操作".to_string()),
    };
    state
        .store
        .transition(id, next)
        .map(|task| TaskActionOutcome {
            task,
            duplicate: None,
        })
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn open_task_directory(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = state.store.find(id).map_err(|error| error.to_string())?;
    let directory = match task.directory {
        Some(directory) if !directory.trim().is_empty() => PathBuf::from(directory),
        _ => download_options(&state)?.0,
    };
    std::fs::create_dir_all(&directory).map_err(|error| format!("无法访问下载目录：{error}"))?;
    tauri_plugin_opener::open_path(&directory, None::<&str>)
        .map_err(|error| format!("无法打开下载目录：{error}"))
}

#[tauri::command]
pub fn remove_task_record(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    state
        .store
        .remove_record(id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    Ok(state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?
        .clone())
}

#[tauri::command]
pub fn update_settings(
    mut settings: AppSettings,
    state: State<'_, AppState>,
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
    if settings.proxy.enabled {
        if !matches!(settings.proxy.protocol.as_str(), "http" | "https") {
            return Err("aria2 仅支持 HTTP/HTTPS 代理".into());
        }
        if settings.proxy.host.is_empty() || settings.proxy.host.contains("://") {
            return Err("代理服务器地址无效".into());
        }
        if settings.proxy.port == 0 {
            return Err("代理端口无效".into());
        }
    }
    let json =
        serde_json::to_vec_pretty(&settings).map_err(|error| format!("无法序列化设置：{error}"))?;
    std::fs::write(&state.settings_path, json).map_err(|error| format!("无法保存设置：{error}"))?;
    *state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())? = settings.clone();
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::{Arc, Mutex}};

    use super::{
        automatic_recovery_reason, duplicate_state, format_bytes, is_download_payload,
        recovery_snapshot_is_current, start_task, sync_status, task_action_blocking, AppState,
        AutomaticRecovery,
    };
    use crate::{
        Aria2Engine, Aria2Status, AppSettings, DuplicateDownloadState, NewTask,
        StoredDownloadFile, TaskStatus, TaskStore,
    };

    #[test]
    fn duplicate_state_distinguishes_present_missing_and_resumable_content() {
        let root = std::env::temp_dir().join(format!("duplicate-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/fixture.mkv"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let task = store.start_download(task.id, "0123456789abcdef", &root).unwrap();
        let file = root.join("fixture.mkv");
        std::fs::write(&file, vec![0_u8; 3]).unwrap();
        store
            .store_content_files(
                task.id,
                &[StoredDownloadFile {
                    path: file.to_string_lossy().into_owned(),
                    length: 3,
                }],
            )
            .unwrap();
        let processing = store.transition(task.id, TaskStatus::Processing).unwrap();
        let completed = store.transition(processing.id, TaskStatus::Completed).unwrap();

        assert_eq!(
            duplicate_state(&store.find(completed.id).unwrap()),
            DuplicateDownloadState::CompletedFilesPresent
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            duplicate_state(&store.find(completed.id).unwrap()),
            DuplicateDownloadState::CompletedFilesMissing
        );

        let task = store
            .create(NewTask::direct("https://example.com/partial.mkv"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let task = store.start_download(task.id, "1111111111111111", &root).unwrap();
        let present = root.join("present.mkv");
        std::fs::write(&present, vec![0_u8; 3]).unwrap();
        store
            .store_content_files(
                task.id,
                &[
                    StoredDownloadFile {
                        path: present.to_string_lossy().into_owned(),
                        length: 3,
                    },
                    StoredDownloadFile {
                        path: root.join("missing.mkv").to_string_lossy().into_owned(),
                        length: 3,
                    },
                ],
            )
            .unwrap();
        let processing = store.transition(task.id, TaskStatus::Processing).unwrap();
        let completed = store.transition(processing.id, TaskStatus::Completed).unwrap();
        assert_eq!(
            duplicate_state(&store.find(completed.id).unwrap()),
            DuplicateDownloadState::OverwriteRequired
        );

        let task = store
            .create(NewTask::direct("https://example.com/resumable.mkv"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let task = store.start_download(task.id, "fedcba9876543210", &root).unwrap();
        let resumable = root.join("resumable.mkv");
        std::fs::write(&resumable, vec![0_u8; 1]).unwrap();
        std::fs::write(format!("{}.aria2", resumable.to_string_lossy()), []).unwrap();
        store
            .store_content_files(
                task.id,
                &[StoredDownloadFile {
                    path: resumable.to_string_lossy().into_owned(),
                    length: 3,
                }],
            )
            .unwrap();
        store.fail(task.id, "interrupted").unwrap();

        assert_eq!(
            duplicate_state(&store.find(task.id).unwrap()),
            DuplicateDownloadState::ResumeAvailable
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retry_stops_before_overwriting_an_orphaned_payload() {
        let root = std::env::temp_dir().join(format!("orphaned-payload-{}", uuid::Uuid::new_v4()));
        let downloads = root.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let payload = downloads.join("fixture.mkv");
        std::fs::write(&payload, b"incomplete data without an aria2 control file").unwrap();
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask::direct("http://127.0.0.1:9/fixture.mkv"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store
            .start_download(task.id, "0123456789abcdef", &downloads)
            .unwrap();
        store
            .store_content_files(
                task.id,
                &[StoredDownloadFile {
                    path: payload.to_string_lossy().into_owned(),
                    length: 0,
                }],
            )
            .unwrap();
        store.fail(task.id, "interrupted").unwrap();
        let state = AppState {
            store: store.clone(),
            engine: Arc::new(
                Aria2Engine::start(
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("binaries")
                        .join(crate::aria2_binary_name()),
                    &root,
                )
                .unwrap(),
            ),
            settings: Mutex::new(AppSettings {
                download_directory: downloads.to_string_lossy().into_owned(),
                ..Default::default()
            }),
            settings_path: root.join("settings.json"),
            cloud: Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: Mutex::new(HashSet::new()),
            cloud_refresh_attempts: Default::default(),
        };

        let outcome = task_action_blocking(task.id.to_string(), "retry".into(), &state).unwrap();

        assert_eq!(outcome.duplicate, Some(DuplicateDownloadState::OverwriteRequired));
        assert_eq!(store.find(task.id).unwrap().status, TaskStatus::Failed);
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retries_a_missing_gid_and_only_the_first_expired_cloud_link() {
        let store = TaskStore::open_in_memory().unwrap();
        let direct = store
            .create(NewTask::direct("https://example.com/fixture.mkv"))
            .unwrap();
        let cloud = store
            .create(NewTask {
                source: "cloud://quark/fixture".into(),
                name: "fixture.mkv".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        let expired: Aria2Status = serde_json::from_value(serde_json::json!({
            "status": "error", "errorCode": "22"
        }))
        .unwrap();

        assert_eq!(
            automatic_recovery_reason(
                &direct,
                None,
                Some("aria2 RPC error: GID 32d25ddfe21b not found"),
                false,
            ),
            Some(AutomaticRecovery::MissingGid)
        );
        assert_eq!(
            automatic_recovery_reason(&cloud, Some(&expired), None, false),
            Some(AutomaticRecovery::RefreshCloudLink)
        );
        assert_eq!(
            automatic_recovery_reason(&cloud, Some(&expired), None, true),
            None
        );
    }

    #[test]
    fn polling_recreates_a_task_when_aria2_has_lost_its_gid() {
        let root = std::env::temp_dir().join(format!("stale-gid-poll-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let engine = Aria2Engine::start(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries")
                .join(crate::aria2_binary_name()),
            &root,
        )
        .unwrap();
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask::direct("http://127.0.0.1:1/stale-gid-fixture"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let stale = "32d25ddfe21b0000";
        store.start_download(task.id, stale, &root).unwrap();
        let state = AppState {
            store: store.clone(),
            engine: Arc::new(engine),
            settings: Mutex::new(AppSettings {
                download_directory: root.to_string_lossy().into_owned(),
                ..Default::default()
            }),
            settings_path: root.join("settings.json"),
            cloud: Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: Default::default(),
            cloud_refresh_attempts: Default::default(),
        };

        let refreshed = super::refresh_tasks(&state).unwrap();

        assert_eq!(refreshed[0].status, TaskStatus::Downloading);
        assert_ne!(refreshed[0].aria2_gid.as_deref(), Some(stale));
        assert!(refreshed[0].error.is_none());
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn automatic_recovery_does_not_restart_a_task_that_was_cancelled_after_polling_began() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask::direct("https://example.com/cancelled-fixture"))
            .unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let task = store
            .start_download(task.id, "32d25ddfe21b0000", std::path::Path::new("C:/Downloads"))
            .unwrap();

        assert!(recovery_snapshot_is_current(&task, &store).unwrap());
        store.transition(task.id, TaskStatus::Cancelled).unwrap();

        assert!(!recovery_snapshot_is_current(&task, &store).unwrap());
    }

    #[test]
    fn startup_requeues_only_failures_caused_by_a_missing_aria2_gid() {
        let root = std::env::temp_dir().join(format!("stale-gid-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask::direct("https://example.com/startup-fixture"))
            .unwrap();
        store
            .sync_transfer(
                task.id,
                TaskStatus::Failed,
                0,
                None,
                None,
                Some("读取 aria2 状态失败：aria2 RPC error: GID 32d25ddfe21b not found".into()),
            )
            .unwrap();
        let state = AppState {
            store: store.clone(),
            engine: Arc::new(
                Aria2Engine::start(
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("binaries")
                        .join(crate::aria2_binary_name()),
                    &root,
                )
                .unwrap(),
            ),
            settings: Mutex::new(AppSettings::default()),
            settings_path: root.join("settings.json"),
            cloud: Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: Default::default(),
            cloud_refresh_attempts: Default::default(),
        };

        let recovery = super::prepare_recovery(&state).unwrap();

        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].status, TaskStatus::Queued);
        assert_eq!(store.find(task.id).unwrap().status, TaskStatus::Queued);
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn startup_requeues_a_cloud_failure_with_aria2_error_22_to_refresh_its_link() {
        let root = std::env::temp_dir().join(format!("cloud-22-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask {
                source: "cloud://quark/fixture".into(),
                name: "fixture.mkv".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        store
            .sync_transfer(
                task.id,
                TaskStatus::Failed,
                0,
                None,
                None,
                Some("网盘下载失败（aria2 错误码 22），请右键重试以刷新下载地址".into()),
            )
            .unwrap();
        let state = AppState {
            store: store.clone(),
            engine: Arc::new(
                Aria2Engine::start(
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("binaries")
                        .join(crate::aria2_binary_name()),
                    &root,
                )
                .unwrap(),
            ),
            settings: Mutex::new(AppSettings::default()),
            settings_path: root.join("settings.json"),
            cloud: Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: Default::default(),
            cloud_refresh_attempts: Default::default(),
        };

        let recovery = super::prepare_recovery(&state).unwrap();

        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].status, TaskStatus::Queued);
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn magnet_task_uses_the_configured_download_directory() {
        let root = std::env::temp_dir().join(format!("magnet-directory-{}", uuid::Uuid::new_v4()));
        let downloads = root.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let store = Arc::new(TaskStore::open_in_memory().unwrap());
        let mut new_task = NewTask::direct(
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=fixture",
        );
        new_task.kind = "magnet".into();
        let task = store.create(new_task).unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let task = store.transition(task.id, TaskStatus::Downloading).unwrap();
        let state = AppState {
            store: store.clone(),
            engine: Arc::new(
                Aria2Engine::start(
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("binaries")
                        .join(crate::aria2_binary_name()),
                    &root,
                )
                .unwrap(),
            ),
            settings: Mutex::new(AppSettings {
                download_directory: downloads.to_string_lossy().into_owned(),
                ..Default::default()
            }),
            settings_path: root.join("settings.json"),
            cloud: Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: Mutex::new(HashSet::new()),
            cloud_refresh_attempts: Default::default(),
        };

        let started = start_task(task, &state).unwrap();

        assert_eq!(
            started.directory,
            Some(downloads.to_string_lossy().into_owned())
        );
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retrying_a_legacy_magnet_task_keeps_its_original_download_directory() {
        let downloads = std::path::PathBuf::from(r"C:\Downloads");
        let store = TaskStore::open_in_memory().unwrap();
        let mut new_task = NewTask::direct(
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=fixture",
        );
        new_task.kind = "magnet".into();
        let task = store.create(new_task).unwrap();
        let task = store.transition(task.id, TaskStatus::Queued).unwrap();
        let legacy = store.start_download(task.id, "0123456789abcdef", &downloads).unwrap();

        assert_eq!(
            super::task_download_directory(&legacy, &downloads),
            downloads
        );
    }

    #[test]
    fn cloud_recovery_reports_expired_authorization_without_sending_cloud_uri_to_aria2() {
        let root = std::env::temp_dir().join(format!("cloud-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let engine = crate::Aria2Engine::start(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries")
                .join(crate::aria2_binary_name()),
            &root,
        )
        .unwrap();
        let store = std::sync::Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask {
                source: "cloud://quark/file%2Fid".into(),
                name: "fixture.txt".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        store.transition(task.id, TaskStatus::Queued).unwrap();
        let mut recovery_ids = vec![task.id];
        for desired in [TaskStatus::Downloading, TaskStatus::Paused] {
            let extra = store
                .create(NewTask {
                    source: "cloud://pikpak/another-id".into(),
                    name: "fixture.txt".into(),
                    kind: "cloud".into(),
                })
                .unwrap();
            store.transition(extra.id, TaskStatus::Queued).unwrap();
            store
                .start_download(extra.id, "0123456789abcdef", &root)
                .unwrap();
            if desired == TaskStatus::Paused {
                store.transition(extra.id, desired).unwrap();
            }
            recovery_ids.push(extra.id);
        }
        let state = super::AppState {
            store: store.clone(),
            engine: std::sync::Arc::new(engine),
            settings: std::sync::Mutex::new(crate::AppSettings::default()),
            settings_path: root.join("settings.json"),
            cloud: std::sync::Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: Default::default(),
            cloud_refresh_attempts: Default::default(),
        };
        super::recover_downloads(&state, super::prepare_recovery(&state).unwrap());
        let restored = store.find(task.id).unwrap();
        assert!(state.recovering.lock().unwrap().is_empty());
        for id in recovery_ids {
            let failed = store.find(id).unwrap();
            assert_eq!(failed.status, TaskStatus::Failed);
            assert!(failed.error.unwrap().contains("重新登录授权"));
        }
        let retry_error =
            super::task_action_blocking(task.id.to_string(), "retry".into(), &state).unwrap_err();
        assert!(retry_error.contains("重新登录授权"));
        assert_eq!(store.find(task.id).unwrap().status, TaskStatus::Failed);
        assert!(state.recovering.lock().unwrap().is_empty());
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(restored.status, TaskStatus::Failed);
        assert_eq!(restored.source, "cloud://quark/file%2Fid");
        assert!(restored.error.unwrap().contains("重新登录授权"));
    }

    #[test]
    fn cloud_source_decodes_provider_id_without_changing_direct_uris() {
        use crate::cloud::CloudProvider;
        assert_eq!(
            super::parse_cloud_source("cloud://quark/file%2Fid%2Btest+space").unwrap(),
            Some((CloudProvider::Quark, "file/id+test space".into()))
        );
        assert_eq!(
            super::parse_cloud_source("cloud://pikpak/abc").unwrap(),
            Some((CloudProvider::Pikpak, "abc".into()))
        );
        for source in [
            "https://example.test/file?sig=kept",
            "magnet:?xt=urn:btih:123",
        ] {
            assert_eq!(super::parse_cloud_source(source).unwrap(), None);
        }
        for source in [
            "cloud://unknown/id",
            "cloud://quark/",
            "cloud://quark/a/b",
            "cloud://quark/a%ZZ",
            "cloud://quark/a?token=x",
            "CLOUD://quark/id",
        ] {
            assert!(super::parse_cloud_source(source).is_err(), "{source}");
        }
    }

    #[test]
    fn cloud_retry_uses_fresh_headers_and_filename_but_only_persists_stable_source() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask {
                source: "cloud://quark/file%2Fid".into(),
                name: "fixture.txt".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        store.fail(task.id, "previous failure").unwrap();
        let task = store.prepare_retry(task.id).unwrap();
        let directory = std::path::Path::new("C:/Downloads");
        let gid = super::enqueue_task_source(
            &task.source,
            |provider, id| {
                assert_eq!(provider, crate::cloud::CloudProvider::Quark);
                assert_eq!(id, "file/id");
                Ok(crate::cloud::commands::ResolvedCloudDownload {
                    url: "https://cdn.example.test/f?token=temporary-secret".into(),
                    name: "fixture:name.txt".into(),
                    headers: vec![("Cookie".into(), "session=secret".into())],
                })
            },
            |url, headers, filename| {
                assert_eq!(url, "https://cdn.example.test/f?token=temporary-secret");
                assert_eq!(headers, &[("Cookie".into(), "session=secret".into())]);
                assert_eq!(filename, Some("fixture_name.txt"));
                Ok("0123456789abcdef".into())
            },
        )
        .unwrap();
        let saved = store.start_download(task.id, &gid, directory).unwrap();
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(saved.source, "cloud://quark/file%2Fid");
        assert!(!json.contains("temporary-secret"));
        assert!(!json.contains("session=secret"));
        let direct = super::enqueue_task_source(
            "https://example.test/file",
            |_, _| panic!("direct URL must not resolve cloud credentials"),
            |url, headers, filename| {
                assert_eq!(url, "https://example.test/file");
                assert!(headers.is_empty());
                assert!(filename.is_none());
                Ok("direct".into())
            },
        )
        .unwrap();
        assert_eq!(direct, "direct");
    }

    #[test]
    fn cloud_errors_do_not_expose_provider_secrets() {
        let error = super::enqueue_task_source(
            "cloud://pikpak/id",
            |_, _| {
                Err(crate::cloud::QuarkError::Transport(
                    "https://cdn.example.test/?secret=token Cookie: password".into(),
                ))
            },
            |_, _, _| panic!("failed resolution must not enqueue"),
        )
        .unwrap_err();
        assert!(error.contains("网络"));
        assert!(!error.contains("secret"));
        assert!(!error.contains("password"));
    }

    #[test]
    fn cloud_transfer_errors_do_not_persist_aria2_signed_url_or_cookie() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask {
                source: "cloud://quark/id".into(),
                name: "fixture.txt".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        let status: Aria2Status = serde_json::from_value(serde_json::json!({"status": "error", "errorCode": "22", "errorMessage": "https://cdn.example.test/f?secret=token Cookie: password"})).unwrap();
        let result = sync_status(&task, status, &store).unwrap();
        let saved = serde_json::to_string(&result).unwrap();
        assert!(!saved.contains("secret=token"));
        assert!(!saved.contains("password"));
        assert!(result.error.unwrap().contains("重试"));
    }

    #[test]
    fn quark_http_rejection_is_not_reported_as_an_expired_link() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store
            .create(NewTask {
                source: "cloud://quark/id".into(),
                name: "fixture.jpg".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        let status: Aria2Status = serde_json::from_value(serde_json::json!({
            "status": "error",
            "errorCode": "22",
            "errorMessage": "The response status is not successful. status=412 URI=https://cdn.example.test/f?secret=token"
        }))
        .unwrap();

        let result = sync_status(&task, status, &store).unwrap();
        let error = result.error.unwrap();

        assert!(error.contains("HTTP 412"));
        assert!(!error.contains("链接已过期"));
        assert!(!error.contains("secret=token"));
    }

    #[test]
    fn startup_recovery_does_not_let_polling_fail_a_task_with_an_old_gid() {
        let root =
            std::env::temp_dir().join(format!("cloud-recovery-poll-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let engine = crate::Aria2Engine::start(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries")
                .join(crate::aria2_binary_name()),
            &root,
        )
        .unwrap();
        let store = std::sync::Arc::new(TaskStore::open_in_memory().unwrap());
        let task = store
            .create(NewTask {
                source: "cloud://pikpak/id".into(),
                name: "fixture.txt".into(),
                kind: "cloud".into(),
            })
            .unwrap();
        store.transition(task.id, TaskStatus::Queued).unwrap();
        store
            .start_download(task.id, "0123456789abcdef", &root)
            .unwrap();
        let state = super::AppState {
            store: store.clone(),
            engine: std::sync::Arc::new(engine),
            settings: std::sync::Mutex::new(crate::AppSettings::default()),
            settings_path: root.join("settings.json"),
            cloud: std::sync::Arc::new(
                crate::cloud::commands::CloudState::new(root.join("cloud-sessions")).unwrap(),
            ),
            recovering: std::sync::Mutex::new([task.id].into_iter().collect()),
            cloud_refresh_attempts: Default::default(),
        };
        let tasks = super::refresh_tasks(&state).unwrap();
        let persisted = store.find(task.id).unwrap();
        assert!(
            super::task_action_blocking(task.id.to_string(), "retry".into(), &state)
                .unwrap_err()
                .contains("稍后重试")
        );
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(tasks[0].status, TaskStatus::Downloading);
        assert!(tasks[0].status_message.as_ref().unwrap().contains("恢复"));
        assert_eq!(persisted.status, TaskStatus::Downloading);
    }

    fn magnet_task(store: &TaskStore) -> crate::DownloadTask {
        let mut new_task = NewTask::direct(
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=LegalFixture",
        );
        new_task.kind = "magnet".into();
        let task = store.create(new_task).unwrap();
        store.transition(task.id, TaskStatus::Queued).unwrap();
        store.transition(task.id, TaskStatus::Downloading).unwrap()
    }

    #[test]
    fn magnet_without_peers_explains_that_metadata_has_not_arrived() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = magnet_task(&store);
        let status: Aria2Status = serde_json::from_value(serde_json::json!({
            "gid": "metadata", "status": "active", "totalLength": "0",
            "completedLength": "0", "downloadSpeed": "0", "connections": "0",
            "bittorrent": {"announceList": []},
            "files": [{"index": "1", "path": "[METADATA]LegalFixture", "length": "0", "completedLength": "0", "selected": "true"}]
        })).unwrap();
        let updated = sync_status(&task, status, &store).unwrap();
        assert_eq!(updated.status, TaskStatus::Downloading);
        assert_eq!(updated.speed_bytes, 0);
        assert_eq!(updated.connections, Some(0));
        assert!(updated.children.is_empty());
        assert_eq!(
            serde_json::to_value(updated).unwrap()["statusMessage"],
            "正在获取种子信息，暂无节点连接"
        );
    }

    #[test]
    fn magnet_with_peers_reports_metadata_phase_without_payload_progress() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = magnet_task(&store);
        let status: Aria2Status = serde_json::from_value(serde_json::json!({
            "status": "active", "totalLength": "4096", "completedLength": "2048",
            "downloadSpeed": "1024", "connections": "2",
            "files": [{"index": "1", "path": "[METADATA]LegalFixture", "length": "4096", "completedLength": "2048", "selected": "true"}]
        })).unwrap();
        let updated = sync_status(&task, status, &store).unwrap();
        assert_eq!(
            updated.progress, 0,
            "metadata bytes are not payload progress"
        );
        assert!(updated.size.is_none());
        assert!(updated.eta.is_none());
        assert_eq!(updated.speed_bytes, 1024);
        assert_eq!(
            serde_json::to_value(updated).unwrap()["statusMessage"],
            "正在获取种子信息，已连接 2 个节点"
        );
    }

    #[test]
    fn payload_sync_uses_real_speed_connections_and_selected_file_progress() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = magnet_task(&store);
        let status: Aria2Status = serde_json::from_value(serde_json::json!({
            "status": "active", "totalLength": "8192", "completedLength": "4096",
            "downloadSpeed": "1024", "connections": "3", "bittorrent": {"info": {"name": "LegalFixture"}},
            "files": [
                {"index": "1", "path": "LegalFixture/a.txt", "length": "8192", "completedLength": "4096", "selected": "true"},
                {"index": "2", "path": "LegalFixture/skipped.txt", "length": "1024", "completedLength": "0", "selected": "false"}
            ]
        })).unwrap();
        let updated = sync_status(&task, status, &store).unwrap();
        assert_eq!(updated.speed_bytes, 1024);
        assert_eq!(updated.speed.as_deref(), Some("1.0 KB/s"));
        assert_eq!(updated.connections, Some(3));
        assert_eq!(updated.progress, 50);
        assert_eq!(updated.eta.as_deref(), Some("4秒"));
        assert_eq!(updated.children.len(), 1);
        assert_eq!(updated.children[0].name, "a.txt");
        assert_eq!(updated.children[0].progress, 50);
        let response = serde_json::to_value(updated).unwrap();
        assert_eq!(
            response["speedBytes"], 1024,
            "the frontend totals the camelCase speedBytes field"
        );
        assert!(response["statusMessage"].is_null());
    }

    #[test]
    fn formats_transfer_sizes_for_the_download_ui() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
    }

    #[test]
    fn hides_aria2_magnet_metadata_placeholder_from_the_file_list() {
        assert!(!is_download_payload("[METADATA]Lanterns.S01E04"));
        assert!(is_download_payload("Lanterns.S01E04/video.mkv"));
    }
}
