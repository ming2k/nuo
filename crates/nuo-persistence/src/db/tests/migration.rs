//! Schema baseline, ledger, and clean-break tests.

use super::*;

#[test]
fn fresh_db_migrates_to_baseline() {
    let conn = initialize_in_memory_db().unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    assert_eq!(CURRENT_DB_VERSION, 1);

    let (name, checksum): (String, String) = conn
        .query_row(
            "SELECT name, checksum FROM schema_migrations WHERE version = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "baseline");
    assert_eq!(checksum, crate::db::migrations::catalog::BASELINE_CHECKSUM);

    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ledger_rows, 1);
}

#[test]
fn migration_catalog_is_contiguous() {
    let versions: Vec<u32> = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(
        versions,
        (1..=CURRENT_DB_VERSION).collect::<Vec<_>>(),
        "migration versions must be contiguous and ordered"
    );
}

/// The baseline is immutable: editing it would silently stamp fresh databases
/// with a schema that no existing database can reproduce.
#[test]
fn baseline_checksum_is_stable() {
    assert_eq!(
        MIGRATIONS[0].checksum(),
        crate::db::migrations::catalog::BASELINE_CHECKSUM,
        "the baseline SQL changed; ship the change as a new numbered migration"
    );
}

#[test]
fn reopening_a_baseline_database_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nuo.db");

    drop(initialize_db(&path).unwrap());
    let conn = initialize_db(&path).unwrap();

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
    assert_eq!(ledger_rows, 1);
}

#[test]
fn upgrade_fixture_builder_can_start_before_the_baseline() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_to_for_tests(&mut conn, 0).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let ledger_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_rows, 0);

    migrate_schema(&mut conn).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_DB_VERSION);
}

#[test]
fn retired_pre_baseline_database_is_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY);
         PRAGMA user_version = 38;",
    )
    .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(error.contains("migration ledger is missing"), "{error}");
    assert!(error.contains("ADR-0189"), "{error}");
}

#[test]
fn unversioned_tables_are_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    configure_connection(&mut conn).unwrap();
    conn.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY);")
        .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(error.contains("tables but no migration ledger"), "{error}");
}

#[test]
fn checksum_drift_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute(
        "UPDATE schema_migrations SET checksum = 'tampered' WHERE version = 1",
        [],
    )
    .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(error.contains("checksum mismatch"), "{error}");
    assert!(error.contains("edited after it shipped"), "{error}");
}

#[test]
fn schema_drift_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.execute_batch("CREATE TABLE unexpected_object (id TEXT PRIMARY KEY);")
        .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(
        error.contains("unexpected_object"),
        "the verifier must name the object that is not in the catalog: {error}"
    );
}

#[test]
fn newer_database_is_refused() {
    let mut conn = initialize_in_memory_db().unwrap();
    conn.pragma_update(None, "user_version", CURRENT_DB_VERSION + 1)
        .unwrap();

    let error = migrate_schema(&mut conn).unwrap_err().to_string();
    assert!(
        error.contains("newer than this binary"),
        "a newer database must be refused: {error}"
    );
}
