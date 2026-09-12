use std::path::Path;

use crate::{LegacyPurgeReport, StoreError, TaskStore};

const ARIA2_NEXT_MIGRATION_MARKER: &str = "engine-migrations/aria2-next-2.7.5";

pub fn run_aria2_next_migration(
    store: &TaskStore,
    data_dir: &Path,
    download_root: &Path,
) -> Result<Option<LegacyPurgeReport>, StoreError> {
    let marker = data_dir.join(ARIA2_NEXT_MIGRATION_MARKER);
    if marker.is_file() {
        return Ok(None);
    }
    let report = store.purge_incompatible_engine_tasks(download_root)?;
    std::fs::write(data_dir.join("aria2.session"), [])?;
    let parent = marker.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid migration marker")
    })?;
    std::fs::create_dir_all(parent)?;
    std::fs::write(&marker, b"aria2-next-2.7.5\n")?;
    Ok(Some(report))
}
