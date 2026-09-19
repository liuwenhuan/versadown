use std::{
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{StoreError, TaskStore};

/// Chooses and durably reserves a unique output path for one non-BT task.
///
/// aria2 may not create a payload until it has contacted the remote server,
/// so checking only the file system allows two independent submissions to
/// target the same file. The task-scoped reservation closes that gap and also
/// survives an application restart.
pub(crate) fn reserve_unique_output_path(
    store: &TaskStore,
    task_id: Uuid,
    directory: &Path,
    preferred_name: &str,
) -> Result<PathBuf, StoreError> {
    if let Some(path) = store.reserved_output_path(task_id)? {
        return Ok(path);
    }

    if let Some(path) = store
        .files(task_id)?
        .into_iter()
        .map(|file| PathBuf::from(file.path))
        .find(|path| path.is_absolute())
    {
        if store.reserve_output_path(task_id, &path)? {
            return Ok(path);
        }
        return Err(StoreError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "下载任务的原输出路径已被其他任务占用",
        )));
    }

    for suffix in 0..=9_999_u32 {
        let name = suffixed_output_name(preferred_name, suffix);
        let path = directory.join(name);
        if output_or_control_file_exists(&path) {
            continue;
        }
        if store.reserve_output_path(task_id, &path)? {
            return Ok(path);
        }
    }

    Err(StoreError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "无法为下载任务分配未被占用的文件名",
    )))
}

fn output_or_control_file_exists(path: &Path) -> bool {
    path.exists() || PathBuf::from(format!("{}.aria2", path.to_string_lossy())).exists()
}

fn suffixed_output_name(preferred_name: &str, suffix: u32) -> String {
    if suffix == 0 {
        return preferred_name.to_owned();
    }
    let path = Path::new(preferred_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(preferred_name);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    format!("{stem} ({suffix}){extension}")
}

#[cfg(test)]
mod tests {
    use super::{reserve_unique_output_path, suffixed_output_name};
    use crate::{NewTask, TaskStore};

    #[test]
    fn suffixes_names_before_the_last_extension() {
        assert_eq!(
            suffixed_output_name("archive.tar.gz", 1),
            "archive.tar (1).gz"
        );
        assert_eq!(suffixed_output_name("README", 2), "README (2)");
    }

    #[test]
    fn separately_sourced_tasks_receive_distinct_persisted_paths() {
        let directory = tempfile::tempdir().unwrap();
        let store = TaskStore::open_in_memory().unwrap();
        let first = store
            .create(NewTask::direct("https://one.example.test/archive.zip"))
            .unwrap();
        let second = store
            .create(NewTask::direct("ftp://two.example.test/archive.zip"))
            .unwrap();

        let first_path =
            reserve_unique_output_path(&store, first.id, directory.path(), "archive.zip").unwrap();
        let second_path =
            reserve_unique_output_path(&store, second.id, directory.path(), "archive.zip").unwrap();

        assert_eq!(first_path, directory.path().join("archive.zip"));
        assert_eq!(second_path, directory.path().join("archive (1).zip"));
        assert_eq!(
            store.reserved_output_path(second.id).unwrap(),
            Some(second_path)
        );
    }
}
