use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::downloader::load_torrent_base64;
use crate::{
    confirm_download, prepare_download, DownloadAction, DownloadService, DownloadServiceError,
    DownloadTask, ParseOutcome, ParseResource, TaskStatus, TaskStore, TransferFile,
};

pub struct AppState {
    pub store: Arc<TaskStore>,
    pub service: Arc<DownloadService>,
    pub sync_cancel: tokio::sync::watch::Sender<bool>,
    pub debug_reports: Mutex<bool>,
}

#[derive(Clone, Serialize)]
pub struct DownloadTaskPayload {
    #[serde(flatten)]
    pub task: DownloadTask,
    pub files: Vec<TransferFile>,
}

pub(crate) fn task_payload(
    store: &TaskStore,
    task: DownloadTask,
) -> Result<DownloadTaskPayload, String> {
    let files = store.files(task.id).map_err(|error| error.to_string())?;
    Ok(DownloadTaskPayload { task, files })
}

#[derive(Serialize)]
pub struct Settings {
    pub debug_reports: bool,
}

#[tauri::command]
pub async fn parse_download(
    input: String,
    state: State<'_, AppState>,
) -> Result<ParseOutcome, String> {
    let mut outcome = prepare_download(&input, &state.store).map_err(|error| error.to_string())?;
    if outcome.task.kind == "torrent" {
        let encoded = match load_torrent_base64(&outcome.task.source).await {
            Ok(encoded) => encoded,
            Err(error) => {
                let message = error.to_string();
                let _ = state.store.fail(outcome.task.id, message.clone());
                return Err(message);
            }
        };
        outcome.task = match state
            .service
            .submit_torrent(outcome.task.id, encoded, vec![], Default::default())
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
        outcome.task = state
            .service
            .submit_uri(
                outcome.task.id,
                vec![outcome.task.source.clone()],
                Default::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
    } else if outcome.task.kind == "magnet" {
        outcome.task = state
            .service
            .prepare_magnet(outcome.task.id, outcome.task.source.clone())
            .await
            .map_err(|error| error.to_string())?;
        match state
            .service
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
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    if current.aria_gid.is_some() {
        let indices = selected_ids
            .into_iter()
            .map(|value| value.parse::<u32>().map_err(|_| "文件索引无效".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let task = state
            .service
            .select_files(id, indices)
            .await
            .map_err(|error| error.to_string())?;
        return task_payload(&state.store, task);
    }
    if let Some(error) = unbound_torrent_confirmation_error(&current) {
        return Err(error.to_string());
    }
    let task = confirm_download(id, &state.store).map_err(|error| error.to_string())?;
    let task = state
        .service
        .submit_uri(task.id, vec![task.source], Default::default())
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
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let action = match action.as_str() {
        "pause" => DownloadAction::Pause,
        "resume" => DownloadAction::Resume,
        "cancel" => DownloadAction::Cancel,
        _ => return Err("当前任务不能执行此操作".to_string()),
    };
    let task = state
        .service
        .control(id, action)
        .await
        .map_err(|error| error.to_string())?;
    task_payload(&state.store, task)
}

#[tauri::command]
pub async fn select_files(
    task_id: String,
    indices: Vec<u32>,
    state: State<'_, AppState>,
) -> Result<DownloadTaskPayload, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = state
        .service
        .select_files(id, indices)
        .await
        .map_err(|error| error.to_string())?;
    task_payload(&state.store, task)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(Settings {
        debug_reports: *state
            .debug_reports
            .lock()
            .map_err(|_| "设置锁不可用".to_string())?,
    })
}

#[tauri::command]
pub fn update_debug_reports(enabled: bool, state: State<'_, AppState>) -> Result<Settings, String> {
    let mut setting = state
        .debug_reports
        .lock()
        .map_err(|_| "设置锁不可用".to_string())?;
    *setting = enabled;
    Ok(Settings {
        debug_reports: *setting,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NewTask;

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
}
