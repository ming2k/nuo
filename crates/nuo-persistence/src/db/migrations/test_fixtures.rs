//! Test fixture builders and read-only inspectors for integration and E2E
//! migration suites.
//!
//! Lives inside `nuo_persistence::db::migrations` so external tests under `tests/`
//! can construct historical on-disk SQLite databases and verify migration
//! outcomes without bypassing the one-door rule.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, Result, TransactionBehavior, params};

use super::apply::execute_body;
use super::catalog::NUO_SCHEMA;
use super::history;
use super::runner::{assert_referential_integrity, configure_connection};

/// Snapshot of a SQLite file's schema metadata after inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbInspection {
    pub user_version: u32,
    pub ledger: Vec<(u32, String, String)>,
    pub tables: Vec<String>,
    pub views: Vec<String>,
    pub indexes: Vec<String>,
    pub triggers: Vec<String>,
}

/// One row read from `thread_list_view`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadListRow {
    pub id: String,
    pub title: String,
    pub msg_count: i64,
    pub last_user_prompt: Option<String>,
}

/// Create an on-disk `nuo.db` migrated through `through` (`0..=CURRENT_DB_VERSION`),
/// optionally executing `seed_sql` inside the database at that version.
pub fn create_nuo_db_at_version(path: &Path, through: u32, seed_sql: Option<&str>) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(path)?;
    configure_connection(&mut conn)?;
    history::ensure_ledger(&conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for migration in NUO_SCHEMA
        .migrations
        .iter()
        .filter(|migration| migration.version <= through)
    {
        execute_body(&tx, &migration.body)?;
        history::insert_applied(&tx, migration, &migration.checksum(), 1_700_000_000_000, 1)?;
        tx.pragma_update(None, "user_version", migration.version)?;
    }
    tx.commit()?;

    if let Some(sql) = seed_sql {
        conn.execute_batch(sql)?;
    }

    // Flush WAL into the main file so the fixture mirrors a cleanly closed
    // historical database.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

/// Create a pre-baseline `nuo.db` without `schema_migrations` (retired chain).
pub fn create_pre_baseline_nuo_db(path: &Path, user_version: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(&format!(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT);
         INSERT INTO threads(id, title) VALUES ('legacy-t1', 'legacy title');
         PRAGMA user_version = {user_version};"
    ))?;
    Ok(())
}

/// Create a pre-baseline `role_memory.db` without `schema_migrations`.
pub fn create_pre_baseline_role_memory_db(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE role_memories (
             id TEXT PRIMARY KEY,
             role TEXT NOT NULL,
             user_prompt TEXT NOT NULL,
             role_response TEXT NOT NULL
         );
         INSERT INTO role_memories(id, role, user_prompt, role_response)
         VALUES ('m1', 'developer', 'hi', 'hello');",
    )?;
    Ok(())
}

/// Tamper with a recorded checksum in `schema_migrations`.
pub fn tamper_migration_checksum(path: &Path, version: u32, fake_checksum: &str) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.execute(
        "UPDATE schema_migrations SET checksum = ?1 WHERE version = ?2",
        params![fake_checksum, version],
    )?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

/// Execute arbitrary DDL against an existing fixture database to simulate
/// out-of-band schema drift.
pub fn execute_raw_sql(path: &Path, sql: &str) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch(sql)?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

/// Stamp `PRAGMA user_version` directly on a fixture database.
pub fn set_user_version(path: &Path, version: u32) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "user_version", version)?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

/// Inspect a SQLite file read-only without mutating or creating it.
pub fn inspect_db(path: &Path) -> Result<DbInspection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let user_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let ledger = history::read_applied(&conn)?
        .into_iter()
        .map(|row| (row.version, row.name, row.checksum))
        .collect();

    let tables = list_objects_by_type(&conn, "table")?;
    let views = list_objects_by_type(&conn, "view")?;
    let indexes = list_objects_by_type(&conn, "index")?;
    let triggers = list_objects_by_type(&conn, "trigger")?;

    Ok(DbInspection {
        user_version,
        ledger,
        tables,
        views,
        indexes,
        triggers,
    })
}

fn list_objects_by_type(conn: &Connection, kind: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
          WHERE type = ?1 AND name NOT LIKE 'sqlite_%'
          ORDER BY name ASC",
    )?;
    let rows = stmt.query_map(params![kind], |row| row.get(0))?;
    rows.collect()
}

/// Read `thread_list_view` rows from an existing database read-only.
pub fn query_thread_list_rows(path: &Path) -> Result<Vec<ThreadListRow>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT id, title, msg_count, last_user_prompt
           FROM thread_list_view
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ThreadListRow {
            id: row.get(0)?,
            title: row.get(1)?,
            msg_count: row.get(2)?,
            last_user_prompt: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Execute a single-integer read-only query against a SQLite file.
pub fn query_scalar_i64(path: &Path, sql: &str) -> Result<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.query_row(sql, [], |row| row.get(0))
}

/// Verify both `PRAGMA foreign_key_check` and `PRAGMA quick_check` read-only.
pub fn verify_db_integrity(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    assert_referential_integrity(&conn)?;
    let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "quick_check failed: {check}"
        )));
    }
    Ok(())
}

/// Find pre-migration backup files (`<db>.pre-v<next_version>.*.bak`) beside `db_path`.
pub fn find_pre_migration_backups(
    db_path: &Path,
    next_version: u32,
) -> std::io::Result<Vec<PathBuf>> {
    let Some(parent) = db_path.parent() else {
        return Ok(Vec::new());
    };
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{file_name}.pre-v{next_version}.");
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".bak") {
            matches.push(entry.path());
        }
    }
    matches.sort();
    Ok(matches)
}
