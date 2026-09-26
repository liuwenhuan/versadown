use rusqlite::{params, Connection, OpenFlags, Transaction};
use std::{ffi::OsStr, fs, path::Path};

const CURRENT_APP_DATA_DIRECTORY: &str = "com.versadown.desktop";
const LEGACY_APP_DATA_DIRECTORY: &str = "com.alldownloader.desktop";
const MIGRATION_MARKER: &str = "engine-migrations/ed2k-discovery-v1";
const ED2K_STATE_PATH: &str = "aria2-next-state/ed2k/state.db";

#[derive(Debug, Default, Eq, PartialEq)]
pub struct Ed2kDiscoveryMigrationReport {
    pub servers: usize,
    pub source_seeds: usize,
    pub credits: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum Ed2kDiscoveryMigrationError {
    #[error("eD2K state migration storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("eD2K state migration database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Imports discovery data left behind by the pre-rename application directory.
///
/// Old eD2K download GIDs are intentionally not imported: the durable task
/// database owns the current engine identities. Only data that helps the new
/// engine rediscover sources is copied.
pub fn migrate_legacy_ed2k_discovery_state(
    data_dir: &Path,
) -> Result<Ed2kDiscoveryMigrationReport, Ed2kDiscoveryMigrationError> {
    if data_dir.file_name() != Some(OsStr::new(CURRENT_APP_DATA_DIRECTORY)) {
        return Ok(Ed2kDiscoveryMigrationReport::default());
    }

    let marker = data_dir.join(MIGRATION_MARKER);
    if marker.is_file() {
        return Ok(Ed2kDiscoveryMigrationReport::default());
    }

    let Some(app_data_root) = data_dir.parent() else {
        return Ok(Ed2kDiscoveryMigrationReport::default());
    };
    let legacy_state = app_data_root
        .join(LEGACY_APP_DATA_DIRECTORY)
        .join(ED2K_STATE_PATH);
    let report = if legacy_state.is_file() {
        let target_state = data_dir.join(ED2K_STATE_PATH);
        let parent = target_state
            .parent()
            .ok_or_else(|| std::io::Error::other("eD2K state path has no parent"))?;
        fs::create_dir_all(parent)?;

        let legacy = Connection::open_with_flags(&legacy_state, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut current = Connection::open(target_state)?;
        prepare_discovery_tables(&current)?;
        let transaction = current.transaction()?;
        let report = import_discovery_records(&legacy, &transaction)?;
        transaction.commit()?;
        report
    } else {
        Ed2kDiscoveryMigrationReport::default()
    };

    let marker_parent = marker
        .parent()
        .ok_or_else(|| std::io::Error::other("eD2K migration marker has no parent"))?;
    fs::create_dir_all(marker_parent)?;
    fs::write(marker, b"v1\n")?;
    Ok(report)
}

fn prepare_discovery_tables(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS credits(
            user_hash BLOB PRIMARY KEY,
            uploaded INTEGER NOT NULL,
            downloaded INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS servers(endpoint TEXT PRIMARY KEY, payload BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS source_seeds(
            file_hash BLOB NOT NULL,
            file_size INTEGER NOT NULL,
            ordinal INTEGER NOT NULL,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            crypt_options INTEGER NOT NULL,
            user_hash BLOB NOT NULL,
            PRIMARY KEY(file_hash, file_size, ordinal)
        );
        ",
    )
}

fn import_discovery_records(
    legacy: &Connection,
    current: &Transaction<'_>,
) -> rusqlite::Result<Ed2kDiscoveryMigrationReport> {
    let mut report = Ed2kDiscoveryMigrationReport::default();

    let mut servers = legacy.prepare("SELECT endpoint, payload FROM servers")?;
    for row in servers.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
    })? {
        let (endpoint, payload) = row?;
        report.servers += current.execute(
            "INSERT OR IGNORE INTO servers(endpoint, payload) VALUES (?1, ?2)",
            params![endpoint, payload],
        )?;
    }

    let mut source_seeds = legacy.prepare(
        "SELECT file_hash, file_size, ordinal, host, port, crypt_options, user_hash FROM source_seeds",
    )?;
    for row in source_seeds.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Vec<u8>>(6)?,
        ))
    })? {
        let (file_hash, file_size, ordinal, host, port, crypt_options, user_hash) = row?;
        report.source_seeds += current.execute(
            "INSERT OR IGNORE INTO source_seeds(file_hash, file_size, ordinal, host, port, crypt_options, user_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![file_hash, file_size, ordinal, host, port, crypt_options, user_hash],
        )?;
    }

    let mut credits = legacy.prepare("SELECT user_hash, uploaded, downloaded FROM credits")?;
    for row in credits.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })? {
        let (user_hash, uploaded, downloaded) = row?;
        report.credits += current.execute(
            "INSERT OR IGNORE INTO credits(user_hash, uploaded, downloaded) VALUES (?1, ?2, ?3)",
            params![user_hash, uploaded, downloaded],
        )?;
    }

    Ok(report)
}
