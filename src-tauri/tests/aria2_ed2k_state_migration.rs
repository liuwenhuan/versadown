use rusqlite::{params, Connection};
use std::path::Path;

fn create_ed2k_schema(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "
            CREATE TABLE credits(
                user_hash BLOB PRIMARY KEY,
                uploaded INTEGER NOT NULL,
                downloaded INTEGER NOT NULL
            );
            CREATE TABLE downloads(
                gid TEXT PRIMARY KEY,
                file_hash BLOB NOT NULL,
                file_size INTEGER NOT NULL,
                link TEXT NOT NULL,
                path TEXT NOT NULL,
                modified_time INTEGER NOT NULL,
                paused INTEGER NOT NULL,
                complete INTEGER NOT NULL,
                sharing_time INTEGER NOT NULL,
                bitfield BLOB NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE servers(endpoint TEXT PRIMARY KEY, payload BLOB NOT NULL);
            CREATE TABLE source_seeds(
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
        .unwrap();
}

#[test]
fn imports_legacy_ed2k_discovery_records_without_copying_download_gids() {
    let root = tempfile::tempdir().unwrap();
    let legacy_data = root.path().join("com.alldownloader.desktop");
    let current_data = root.path().join("com.versadown.desktop");
    let legacy_state = legacy_data.join("aria2-next-state/ed2k");
    let current_state = current_data.join("aria2-next-state/ed2k");
    std::fs::create_dir_all(&legacy_state).unwrap();
    std::fs::create_dir_all(&current_state).unwrap();

    let legacy_path = legacy_state.join("state.db");
    create_ed2k_schema(&legacy_path);
    let legacy = Connection::open(&legacy_path).unwrap();
    let file_hash = vec![0x3d_u8; 16];
    let user_hash = vec![0x7a_u8; 16];
    legacy
        .execute(
            "INSERT INTO servers(endpoint, payload) VALUES (?1, ?2)",
            params!["91.208.162.87:4232", vec![1_u8, 2, 3]],
        )
        .unwrap();
    legacy
        .execute(
            "INSERT INTO source_seeds VALUES (?1, 3389035, 0, '203.0.113.10', 4662, 0, ?2)",
            params![&file_hash, &user_hash],
        )
        .unwrap();
    legacy
        .execute(
            "INSERT INTO credits VALUES (?1, 30, 10)",
            params![&user_hash],
        )
        .unwrap();
    legacy
        .execute(
            "INSERT INTO downloads VALUES ('legacy-gid', ?1, 3389035, 'ed2k://legacy', 'legacy.exe', 0, 0, 0, 0, X'00', 0)",
            params![&file_hash],
        )
        .unwrap();
    drop(legacy);

    let current_path = current_state.join("state.db");
    create_ed2k_schema(&current_path);
    let current = Connection::open(&current_path).unwrap();
    current
        .execute(
            "INSERT INTO downloads VALUES ('current-gid', X'01', 1, 'ed2k://current', 'current.exe', 0, 0, 0, 0, X'00', 0)",
            [],
        )
        .unwrap();
    drop(current);

    let report = versadown_lib::aria2::migrate_legacy_ed2k_discovery_state(&current_data)
        .expect("legacy discovery state should migrate");

    assert_eq!(report.servers, 1);
    assert_eq!(report.source_seeds, 1);
    assert_eq!(report.credits, 1);

    let current = Connection::open(&current_path).unwrap();
    assert_eq!(
        current
            .query_row(
                "SELECT payload FROM servers WHERE endpoint = ?1",
                ["91.208.162.87:4232"],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
        vec![1, 2, 3]
    );
    assert_eq!(
        current
            .query_row(
                "SELECT host FROM source_seeds WHERE file_size = 3389035",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "203.0.113.10"
    );
    assert_eq!(
        current
            .query_row("SELECT uploaded FROM credits", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        30
    );
    assert_eq!(
        current
            .query_row(
                "SELECT count(*) FROM downloads WHERE gid = 'legacy-gid'",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        current
            .query_row(
                "SELECT count(*) FROM downloads WHERE gid = 'current-gid'",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}
