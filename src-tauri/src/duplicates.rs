use std::path::Path;

use crate::{DownloadTask, DuplicateDownloadState, StoreError, TaskStatus, TaskStore};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentPresence {
    AllPresent,
    Missing,
    Partial,
}

fn content_presence(store: &TaskStore, task: &DownloadTask) -> Result<ContentPresence, StoreError> {
    let files = store.files(task.id)?;
    if files.is_empty() {
        return Ok(ContentPresence::Missing);
    }

    let complete = files
        .iter()
        .map(|file| {
            std::fs::metadata(&file.path)
                .map(|metadata| {
                    metadata.is_file() && (file.length == 0 || metadata.len() == file.length)
                })
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if complete.iter().all(|present| *present) {
        Ok(ContentPresence::AllPresent)
    } else if files.iter().any(|file| Path::new(&file.path).is_file()) {
        Ok(ContentPresence::Partial)
    } else {
        Ok(ContentPresence::Missing)
    }
}

fn has_existing_payload(store: &TaskStore, task: &DownloadTask) -> Result<bool, StoreError> {
    Ok(store
        .files(task.id)?
        .iter()
        .any(|file| Path::new(&file.path).is_file()))
}

fn can_resume(store: &TaskStore, task: &DownloadTask) -> Result<bool, StoreError> {
    let files = store.files(task.id)?;
    Ok(task.aria_gid.is_some()
        && !files.is_empty()
        && files.iter().any(|file| Path::new(&file.path).is_file()))
}

/// Classifies an existing task without contacting aria2. The caller must show
/// the corresponding user choice before changing any files on disk.
pub fn duplicate_state(
    store: &TaskStore,
    task: &DownloadTask,
) -> Result<DuplicateDownloadState, StoreError> {
    match task.status {
        TaskStatus::Draft
        | TaskStatus::Parsing
        | TaskStatus::AwaitingUser
        | TaskStatus::Queued
        | TaskStatus::Downloading
        | TaskStatus::Processing => Ok(DuplicateDownloadState::ExistingActive),
        TaskStatus::Completed => Ok(match content_presence(store, task)? {
            ContentPresence::AllPresent => DuplicateDownloadState::CompletedFilesPresent,
            ContentPresence::Missing => DuplicateDownloadState::CompletedFilesMissing,
            ContentPresence::Partial => DuplicateDownloadState::OverwriteRequired,
        }),
        TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled => {
            if can_resume(store, task)? {
                Ok(DuplicateDownloadState::ResumeAvailable)
            } else {
                Ok(DuplicateDownloadState::OverwriteRequired)
            }
        }
        // Recycled records are kept only for user recovery. Callers avoid treating
        // them as active duplicates, and this defensive fallback never resumes one.
        TaskStatus::Recycled => Ok(DuplicateDownloadState::OverwriteRequired),
    }
}

/// Returns a decision only if retrying might overwrite a payload or can resume
/// it. If every prior output is gone, the caller may submit a new transfer.
pub fn retry_duplicate_state(
    store: &TaskStore,
    task: &DownloadTask,
) -> Result<Option<DuplicateDownloadState>, StoreError> {
    let state = duplicate_state(store, task)?;
    match state {
        DuplicateDownloadState::ResumeAvailable => Ok(Some(state)),
        DuplicateDownloadState::OverwriteRequired if has_existing_payload(store, task)? => {
            Ok(Some(state))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{duplicate_state, retry_duplicate_state};
    use crate::{DownloadTask, DuplicateDownloadState, NewTask, TaskStatus, TaskStore};

    fn task_with_status(store: &TaskStore, source: &str, status: TaskStatus) -> DownloadTask {
        let task = store.create(NewTask::direct(source)).unwrap();
        store
            .apply_transfer_snapshot(DownloadTask { status, ..task })
            .unwrap()
    }

    #[test]
    fn distinguishes_completed_present_missing_partial_and_resumable_outputs() {
        let root = std::env::temp_dir().join(format!("duplicate-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = TaskStore::open_in_memory().unwrap();

        let completed = task_with_status(
            &store,
            "https://example.test/complete",
            TaskStatus::Completed,
        );
        let complete_file = root.join("complete.bin");
        std::fs::write(&complete_file, b"abc").unwrap();
        store
            .upsert_files(
                completed.id,
                &[(0, complete_file.to_string_lossy().into_owned(), 3, 3, true)],
            )
            .unwrap();
        assert_eq!(
            duplicate_state(&store, &completed).unwrap(),
            DuplicateDownloadState::CompletedFilesPresent
        );
        std::fs::remove_file(&complete_file).unwrap();
        assert_eq!(
            duplicate_state(&store, &completed).unwrap(),
            DuplicateDownloadState::CompletedFilesMissing
        );

        let partial = task_with_status(
            &store,
            "https://example.test/partial",
            TaskStatus::Completed,
        );
        let partial_file = root.join("partial.bin");
        std::fs::write(&partial_file, b"a").unwrap();
        store
            .upsert_files(
                partial.id,
                &[(0, partial_file.to_string_lossy().into_owned(), 3, 1, true)],
            )
            .unwrap();
        assert_eq!(
            duplicate_state(&store, &partial).unwrap(),
            DuplicateDownloadState::OverwriteRequired
        );

        let resumable = task_with_status(&store, "https://example.test/resume", TaskStatus::Failed);
        let resumable = store.bind_gid(resumable.id, "native-resume-gid").unwrap();
        let resumable_file = root.join("resume.bin");
        std::fs::write(&resumable_file, b"a").unwrap();
        store
            .upsert_files(
                resumable.id,
                &[(0, resumable_file.to_string_lossy().into_owned(), 3, 1, true)],
            )
            .unwrap();
        assert_eq!(
            duplicate_state(&store, &resumable).unwrap(),
            DuplicateDownloadState::ResumeAvailable
        );
        assert_eq!(
            retry_duplicate_state(&store, &resumable).unwrap(),
            Some(DuplicateDownloadState::ResumeAvailable)
        );

        let recycled = task_with_status(
            &store,
            "https://example.test/recycled",
            TaskStatus::Recycled,
        );
        assert_eq!(
            duplicate_state(&store, &recycled).unwrap(),
            DuplicateDownloadState::OverwriteRequired,
            "a recycled task must never be resumed implicitly"
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
