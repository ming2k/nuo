//! The durable migration ledger (`schema_migrations`).
//!
//! The ledger is authoritative history; `PRAGMA user_version` is the fast gate
//! and must agree with it (ADR-0189). Rows are only written inside the
//! transaction that applies their migration, so a rolled-back migration leaves
//! no row and no version stamp.

use rusqlite::{Connection, Result, params};

use super::catalog::Migration;
use super::schema::Schema;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: u32,
    pub name: String,
    pub checksum: String,
}

pub fn ledger_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1)
}

/// Create the infrastructure ledger. The baseline SQL deliberately does not
/// contain this table: the runner owns history, migrations own domain schema.
pub fn ensure_ledger(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version       INTEGER PRIMARY KEY,
            name          TEXT NOT NULL,
            checksum      TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL,
            execution_ms  INTEGER NOT NULL
         ) STRICT;",
    )
}

pub fn read_applied(conn: &Connection) -> Result<Vec<AppliedMigration>> {
    if !ledger_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT version, name, checksum
           FROM schema_migrations
          ORDER BY version ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(AppliedMigration {
            version: row.get(0)?,
            name: row.get(1)?,
            checksum: row.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn insert_applied(
    conn: &Connection,
    migration: &Migration,
    checksum: &str,
    applied_at_ms: u64,
    execution_ms: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO schema_migrations(version, name, checksum, applied_at_ms, execution_ms)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![
            migration.version,
            migration.name,
            checksum,
            applied_at_ms as i64,
            execution_ms as i64
        ],
    )?;
    Ok(())
}

/// Validate the ledger against one schema's catalog and version gate.
///
/// The applied set must be an exact prefix of the catalog: no gaps, no unknown
/// versions, no checksum drift. A database that fails this test was written by
/// a different binary (or edited by hand); it is not repaired silently.
pub fn validate(conn: &Connection, user_version: u32, schema: &Schema) -> Result<()> {
    let applied = read_applied(conn)?;

    if applied.len() > schema.migrations.len() {
        return Err(incompatible(format!(
            "{}: migration ledger has {} rows but this binary knows only {} migrations",
            schema.name,
            applied.len(),
            schema.migrations.len()
        )));
    }

    for (index, row) in applied.iter().enumerate() {
        let expected = &schema.migrations[index];
        if row.version != expected.version {
            return Err(incompatible(format!(
                "{}: migration ledger row {} is v{} but the catalog expects v{}; \
                 the database was written by a different migration history",
                schema.name,
                index + 1,
                row.version,
                expected.version
            )));
        }
        if row.name != expected.name {
            return Err(incompatible(format!(
                "{}: migration v{} is named '{}' in the ledger but '{}' in this binary",
                schema.name, expected.version, row.name, expected.name
            )));
        }
        let checksum = expected.checksum();
        if row.checksum != checksum {
            return Err(incompatible(format!(
                "{}: migration v{} ({}) checksum mismatch: database has {}, binary expects {}; \
                 the migration body was edited after it shipped",
                schema.name, expected.version, expected.name, row.checksum, checksum
            )));
        }
    }

    let ledger_version = applied.last().map_or(0, |row| row.version);
    if user_version != ledger_version {
        return Err(incompatible(format!(
            "{}: PRAGMA user_version is {user_version} but the migration ledger ends at \
             v{ledger_version}; the database is internally inconsistent",
            schema.name
        )));
    }

    if ledger_version > schema.current_version {
        return Err(incompatible(format!(
            "{}: database schema v{ledger_version} is newer than this binary (v{}); \
             upgrade nuo to open it",
            schema.name, schema.current_version
        )));
    }

    Ok(())
}

pub fn next_pending(applied: &[AppliedMigration], schema: &Schema) -> &'static [Migration] {
    &schema.migrations[applied.len().min(schema.migrations.len())..]
}

pub fn incompatible(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(format!(
        "{}; no automatic migration or repair is provided (ADR-0189)",
        message.into()
    ))
}
