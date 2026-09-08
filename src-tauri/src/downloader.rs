use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    classify_input, CandidateKind, DownloadTask, NewTask, StoreError, TaskStatus, TaskStore,
};

#[derive(Clone, Debug, Serialize)]
pub struct ParseResource {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub size: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParseOutcome {
    pub task: DownloadTask,
    pub requires_confirmation: bool,
    pub resources: Vec<ParseResource>,
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
        return Ok(ParseOutcome {
            task: initial,
            requires_confirmation: false,
            resources: Vec::new(),
        });
    }
    Ok(ParseOutcome {
        task: store.transition(initial.id, TaskStatus::AwaitingUser)?,
        requires_confirmation: true,
        resources: Vec::new(),
    })
}

pub fn confirm_download(id: Uuid, store: &TaskStore) -> Result<DownloadTask, DownloadError> {
    Ok(store.transition(id, TaskStatus::Queued)?)
}
