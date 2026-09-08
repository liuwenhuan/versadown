use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    classify_input, CandidateKind, DownloadTask, NewTask, StoreError, TaskStatus, TaskStore,
};

#[derive(Clone, Debug, Serialize)]
pub struct ParseOutcome {
    pub task: DownloadTask,
    pub requires_confirmation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<DuplicateDownloadState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateDownloadState {
    ExistingActive,
    ResumeAvailable,
    CompletedFilesPresent,
    CompletedFilesMissing,
    OverwriteRequired,
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub fn aria2_binary_name() -> &'static str {
    "aria2c-x86_64-pc-windows-msvc.exe"
}

pub fn prepare_download(input: &str, store: &TaskStore) -> Result<ParseOutcome, DownloadError> {
    let candidate = classify_input(input);
    let mut new_task = NewTask::direct(candidate.uri);
    new_task.kind = match candidate.kind {
        CandidateKind::Magnet => "magnet",
        CandidateKind::Torrent => "torrent",
        CandidateKind::QuarkShare | CandidateKind::PikPakShare => "cloud",
        _ => "file",
    }
    .to_string();
    let initial = store.create(new_task)?;
    if candidate.auto_start {
        let queued = store.transition(initial.id, TaskStatus::Queued)?;
        return Ok(ParseOutcome {
            task: store.transition(queued.id, TaskStatus::Downloading)?,
            requires_confirmation: false,
            duplicate: None,
        });
    }
    Ok(ParseOutcome {
        task: store.transition(initial.id, TaskStatus::AwaitingUser)?,
        requires_confirmation: true,
        duplicate: None,
    })
}

pub fn confirm_download(id: Uuid, store: &TaskStore) -> Result<DownloadTask, DownloadError> {
    let queued = store.transition(id, TaskStatus::Queued)?;
    Ok(store.transition(queued.id, TaskStatus::Downloading)?)
}
