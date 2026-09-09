use std::{
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
use crate::{
    confirm_download, duplicate_state, prepare_download, retry_duplicate_state, AppSettings,
    DownloadAction, DownloadServiceError, DownloadTask, DuplicateDownloadState, ParseOutcome,
    ParseResource, ProxySettings, TaskStatus, TaskStore, TransferFile, UriSubmissionOptions,
};

pub struct AppState {
    pub store: Arc<TaskStore>,
    pub runtime: Arc<crate::EngineRuntime>,
    pub debug_reports: Mutex<bool>,
    pub settings: Mutex<AppSettings>,
    pub settings_path: PathBuf,
}

#[derive(Clone, Serialize)]
pub struct DownloadTaskPayload {
    #[serde(flatten)]
    pub task: DownloadTask,
    pub files: Vec<TransferFile>,
}

#[derive(Clone, Serialize)]
pub struct TaskActionOutcome {
    #[serde(flatten)]
    pub payload: DownloadTaskPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<DuplicateDownloadState>,
}

pub(crate) fn task_payload(
    store: &TaskStore,
    task: DownloadTask,
) -> Result<DownloadTaskPayload, String> {
    let files = store.files(task.id).map_err(|error| error.to_string())?;
    Ok(DownloadTaskPayload { task, files })
}

pub(crate) fn task_action_outcome(
    store: &TaskStore,
    task: DownloadTask,
    duplicate: Option<DuplicateDownloadState>,
) -> Result<TaskActionOutcome, String> {
    Ok(TaskActionOutcome {
        payload: task_payload(store, task)?,
        duplicate,
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

pub(crate) fn download_submission_options(
    state: &AppState,
) -> Result<UriSubmissionOptions, String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    let directory = PathBuf::from(settings.download_directory.trim());
    if !directory.is_absolute() {
        return Err("下载目录必须是绝对路径".into());
    }
    Ok(UriSubmissionOptions {
        directory: Some(directory),
        proxy: proxy_url(&settings.proxy)?,
        ..Default::default()
    })
}

#[tauri::command]
pub async fn parse_download(
    input: String,
    state: State<'_, AppState>,
) -> Result<ParseOutcome, String> {
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let mut outcome = prepare_download(&input, &state.store).map_err(|error| error.to_string())?;
    if outcome.duplicate.is_some() {
        return Ok(outcome);
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
        outcome.resources = parse_resources(&state.store, outcome.task.id)?;
    }
    Ok(outcome)
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
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    if current.aria_gid.is_some() {
        let indices = selected_ids
            .into_iter()
            .map(|value| value.parse::<u32>().map_err(|_| "文件索引无效".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let task = service
            .select_files(id, indices)
            .await
            .map_err(|error| error.to_string())?;
        return task_payload(&state.store, task);
    }
    if let Some(error) = unbound_torrent_confirmation_error(&current) {
        return Err(error.to_string());
    }
    let task = confirm_download(id, &state.store).map_err(|error| error.to_string())?;
    let options = download_submission_options(&state)?;
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
pub fn list_tasks(state: State<'_, AppState>) -> Result<Vec<DownloadTaskPayload>, String> {
    state
        .store
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|task| task_payload(&state.store, task))
        .collect()
}

#[tauri::command]
pub async fn task_action(
    task_id: String,
    action: String,
    app: AppHandle,
) -> Result<TaskActionOutcome, String> {
    let state = app.state::<AppState>();
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| error.to_string())?;
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
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
    if task.source.starts_with("cloud://") {
        let resolved = crate::cloud::commands::resolve_stable_cloud_download(app, &task.source)
            .await
            .map_err(|error| format!("无法重新获取网盘下载地址：{error}"))?;
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
        return Err("磁力任务请重新添加后再选择文件下载".to_string());
    }
    options.output_name = None;
    service
        .resubmit_uri_with_options(task.id, vec![task.source], options)
        .await
        .map_err(|error| error.to_string())
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

#[tauri::command]
pub fn open_task_directory(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let directory = task_output_directory(&state.store, id)?;
    tauri_plugin_opener::open_path(&directory, None::<&str>)
        .map_err(|error| format!("无法打开下载目录：{error}"))
}

#[tauri::command]
pub fn remove_task_record(task_id: String, state: State<'_, AppState>) -> Result<(), String> {
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
    state
        .store
        .remove_task_record(id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn recycle_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
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
    task_payload(&state.store, recycled)
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
) -> Result<(), String> {
    let mut ids = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    for id in &ids {
        let task = state.store.find(*id).map_err(|error| error.to_string())?;
        if !matches!(task.status, TaskStatus::Recycled | TaskStatus::Cancelled) {
            return Err("只有回收站中的任务可以彻底删除".to_string());
        }
    }
    if delete_files {
        for id in &ids {
            remove_task_payload_files(&state.store, *id)?;
        }
    }
    for id in ids {
        state
            .store
            .remove_task_record(id)
            .map_err(|error| error.to_string())?;
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
pub fn update_settings(
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

    let json =
        serde_json::to_vec_pretty(&settings).map_err(|error| format!("无法保存设置：{error}"))?;
    std::fs::write(&state.settings_path, json).map_err(|error| format!("无法保存设置：{error}"))?;

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
    use crate::{DuplicateDownloadState, NewTask};

    #[test]
    fn disabled_proxy_does_not_leak_process_proxy_settings_into_aria2() {
        assert_eq!(proxy_url(&ProxySettings::default()).unwrap(), None);
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
