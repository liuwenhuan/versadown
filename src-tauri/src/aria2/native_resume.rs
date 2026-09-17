//! Read-only adapter for the bundled Next engine's persisted HTTP ranges.
//! A preallocated payload is never evidence of downloaded data.
use crate::{DownloadTask, TaskStore};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub(crate) struct NativeCheckpoint {
    pub uri: String,
    pub path: std::path::PathBuf,
}

pub(crate) fn has_legacy_control(store: &TaskStore, task: &DownloadTask) -> bool {
    store.files(task.id).is_ok_and(|files| {
        files.iter().any(|file| {
            let path = Path::new(&file.path);
            let mut control = path.as_os_str().to_os_string();
            control.push(".aria2");
            path.is_file() && Path::new(&control).is_file()
        })
    })
}

pub(crate) fn checkpoint(
    store: &TaskStore,
    task: &DownloadTask,
    gid: &str,
) -> Option<NativeCheckpoint> {
    if gid.len() != 16 || !gid.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let files = store.files(task.id).ok()?;
    if files.len() != 1 {
        return None;
    }
    let payload = Path::new(&files[0].path);
    let metadata = payload.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let connection = Connection::open_with_flags(
        store.native_stream_state_path()?,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let (uri, path, total, completed, ranges): (String, String, u64, u64, String) = connection.query_row(
        "SELECT uri,path,total_length,completed_length,completed_ranges FROM downloads WHERE gid=?1",
        [gid], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).ok()?;
    let parsed = url::Url::parse(&uri).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || Path::new(&path).canonicalize().ok()? != payload.canonicalize().ok()?
        || total == 0
        || completed == 0
        || completed >= total
        || task.total_length.is_some_and(|known| known != total)
        || (files[0].length != 0 && files[0].length != total)
        || ranges.is_empty()
        || ranges.len() > 1_048_576
    {
        return None;
    }
    let mut previous_end = 0;
    let mut counted = 0u64;
    for range in ranges.split(',') {
        let (start, end) = range.split_once('-')?;
        let start = start.parse::<u64>().ok()?;
        let end = end.parse::<u64>().ok()?;
        if start < previous_end || start >= end || end > total || end > metadata.len() {
            return None;
        }
        counted = counted.checked_add(end - start)?;
        previous_end = end;
    }
    (counted == completed).then_some(NativeCheckpoint {
        uri,
        path: path.into(),
    })
}
