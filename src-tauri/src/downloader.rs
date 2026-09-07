use std::{path::PathBuf, process::Command, sync::Arc};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{classify_input, CandidateKind, DownloadTask, NewTask, StoreError, TaskStatus, TaskStore};

#[derive(Clone, Debug, Serialize)]
pub struct ParseOutcome { pub task: DownloadTask, pub requires_confirmation: bool }

#[derive(Debug, Error)]
pub enum DownloadError { #[error(transparent)] Store(#[from] StoreError) }

pub fn aria2_binary_name() -> &'static str { "aria2c-x86_64-pc-windows-msvc.exe" }

pub fn prepare_download(input: &str, store: &TaskStore) -> Result<ParseOutcome, DownloadError> {
    let candidate = classify_input(input);
    let mut new_task = NewTask::direct(candidate.uri);
    new_task.kind = match candidate.kind { CandidateKind::Magnet => "magnet", CandidateKind::Torrent => "torrent", CandidateKind::QuarkShare | CandidateKind::PikPakShare => "cloud", _ => "file" }.to_string();
    let initial = store.create(new_task)?;
    if candidate.auto_start {
        let queued = store.transition(initial.id, TaskStatus::Queued)?;
        return Ok(ParseOutcome { task: store.transition(queued.id, TaskStatus::Downloading)?, requires_confirmation: false });
    }
    Ok(ParseOutcome { task: store.transition(initial.id, TaskStatus::AwaitingUser)?, requires_confirmation: true })
}

pub fn confirm_download(id: Uuid, store: &TaskStore) -> Result<DownloadTask, DownloadError> {
    let queued = store.transition(id, TaskStatus::Queued)?;
    Ok(store.transition(queued.id, TaskStatus::Downloading)?)
}

pub fn start_aria2(task: DownloadTask, store: Arc<TaskStore>, sidecar: PathBuf, destination: PathBuf) {
    std::thread::spawn(move || {
        if std::fs::create_dir_all(&destination).is_err() { let _ = store.fail(task.id, "无法创建下载目录"); return; }
        let status = Command::new(&sidecar).args(["--continue=true", "--summary-interval=0", "--max-connection-per-server=8", "--split=8", "--dir"])
            .arg(&destination).arg(&task.source).status();
        match status {
            Ok(status) if status.success() => {
                if store.transition(task.id, TaskStatus::Processing).is_ok() { let _ = store.transition(task.id, TaskStatus::Completed); }
            }
            Ok(status) => { let _ = store.fail(task.id, format!("aria2 退出码 {}", status.code().unwrap_or(-1))); }
            Err(error) => { let _ = store.fail(task.id, format!("无法启动 aria2：{error}")); }
        }
    });
}
