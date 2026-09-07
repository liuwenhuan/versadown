use std::{path::PathBuf, sync::{Arc, Mutex}};

use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::{confirm_download, prepare_download, start_aria2, DownloadTask, ParseOutcome, TaskStatus, TaskStore};

pub struct AppState { pub store: Arc<TaskStore>, pub sidecar: PathBuf, pub destination: PathBuf, pub debug_reports: Mutex<bool> }

#[derive(Serialize)]
pub struct Settings { pub debug_reports: bool }

#[tauri::command]
pub fn parse_download(input: String, state: State<'_, AppState>) -> Result<ParseOutcome, String> {
    let outcome = prepare_download(&input, &state.store).map_err(|error| error.to_string())?;
    if !outcome.requires_confirmation { start_aria2(outcome.task.clone(), Arc::clone(&state.store), state.sidecar.clone(), state.destination.clone()); }
    Ok(outcome)
}

#[tauri::command]
pub fn confirm_download_command(task_id: String, state: State<'_, AppState>) -> Result<DownloadTask, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let task = confirm_download(id, &state.store).map_err(|error| error.to_string())?;
    start_aria2(task.clone(), Arc::clone(&state.store), state.sidecar.clone(), state.destination.clone());
    Ok(task)
}

#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>) -> Result<Vec<DownloadTask>, String> {
    state.store.list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn task_action(task_id: String, action: String, state: State<'_, AppState>) -> Result<DownloadTask, String> {
    let id = Uuid::parse_str(&task_id).map_err(|_| "任务 ID 无效".to_string())?;
    let current = state.store.find(id).map_err(|error| error.to_string())?;
    let next = match action.as_str() {
        "pause" if current.status == TaskStatus::Downloading => TaskStatus::Paused,
        "resume" if current.status == TaskStatus::Paused => TaskStatus::Downloading,
        "cancel" => TaskStatus::Cancelled,
        _ => return Err("当前任务不能执行此操作".to_string()),
    };
    state.store.transition(id, next).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(Settings { debug_reports: *state.debug_reports.lock().map_err(|_| "设置锁不可用".to_string())? })
}

#[tauri::command]
pub fn update_debug_reports(enabled: bool, state: State<'_, AppState>) -> Result<Settings, String> {
    let mut setting = state.debug_reports.lock().map_err(|_| "设置锁不可用".to_string())?;
    *setting = enabled;
    Ok(Settings { debug_reports: *setting })
}
