//! Database open, migration execution, and integrity gates.
//!
//! The database has one durable schema lineage: the immutable baseline plus
//! numbered migrations (ADR-0189). A database is either exactly at a version
//! this binary knows, or it is refused. There is no replay of a retired
//! migration history and no automatic repair path.
//!
//! One transaction applies every pending migration and writes its ledger row
//! and `user_version` stamp. `BEGIN IMMEDIATE` serializes concurrent openers.
//! Structural verification against the compiled catalog runs on every open
//! (ADR-0190).

use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Result, TransactionBehavior, params};
use tracing::{info, warn};

use super::apply::execute_body;
use super::catalog::CURRENT_DB_VERSION;
use super::history;
use super::verify;

/// Open a database file and bring it to the current schema.
pub(crate) fn initialize_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut conn = Connection::open(db_path)?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn).map_err(|error| with_database_path(error, db_path))?;

    Ok(conn)
}

/// Initialize an in-memory SQLite database for tests and ephemeral workflows.
#[cfg(test)]
pub(crate) fn initialize_in_memory_db() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;
    Ok(conn)
}

/// Apply the standard schema to an already-open connection.
///
/// `db_path` is optional because in-memory and embedded callers have no file;
/// when present it is included in incompatibility errors so the user knows
/// which file to move aside.
pub(crate) fn initialize_connection_schema(
    conn: &mut Connection,
    db_path: Option<&Path>,
) -> Result<()> {
    configure_connection(conn)?;
    migrate_schema(conn).map_err(|error| match db_path {
        Some(path) => with_database_path(error, path),
        None => error,
    })
}

/// Open a fully-migrated in-memory database for integration tests.
#[doc(hidden)]
pub fn open_in_memory_for_tests() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    initialize_connection_schema(&mut conn, None)?;
    Ok(conn)
}

/// Standard connection configuration applied to every connection.
pub fn configure_connection(conn: &mut Connection) -> Result<()> {
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // `PRAGMA journal_mode=WAL` does not invoke the busy handler. Under
    // concurrent first-open a writer mid-migration makes it return SQLITE_BUSY
    // immediately; the winning process sets WAL and every later connection
    // observes it, so a transient refusal is harmless.
    if let Err(error) = conn.pragma_update(None, "journal_mode", "WAL") {
        warn!(%error, "could not set WAL journal mode; continuing");
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_size_limit", 16777216)?; // 16MB WAL recycling
    // Checkpointing is owned by the storage-maintenance thread; SQLite's
    // automatic checkpoint runs inside the writer's commit path.
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// Does the named table exist in the database?
pub fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![name],
        |row| row.get::<_, i64>(0),
    )? == 1)
}

fn user_version(conn: &Connection) -> Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn has_user_schema(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
         )",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// Bring `conn` to the current schema, or refuse it.
pub fn migrate_schema(conn: &mut Connection) -> Result<()> {
    let version = user_version(conn)?;
    let ledger = history::ledger_exists(conn)?;

    if version == 0 && !ledger {
        if has_user_schema(conn)? {
            return Err(history::incompatible(
                "database has tables but no migration ledger; it was written by the retired \
                 pre-baseline migration chain",
            ));
        }
        history::ensure_ledger(conn)?;
        apply_pending(conn)?;
    } else {
        if !ledger {
            return Err(history::incompatible(format!(
                "PRAGMA user_version is {version} but the migration ledger is missing; \
                 the database was written by the retired pre-baseline migration chain"
            )));
        }
        if version > CURRENT_DB_VERSION {
            return Err(history::incompatible(format!(
                "database schema v{version} is newer than this binary (v{CURRENT_DB_VERSION}); \
                 upgrade nuo to open it"
            )));
        }
        history::validate(conn, version)?;
        apply_pending(conn)?;
        let version = user_version(conn)?;
        history::validate(conn, version)?;
    }

    verify::verify(conn)
}

/// Apply every migration above the ledger's last row in one transaction.
fn apply_pending(conn: &mut Connection) -> Result<()> {
    let applied = history::read_applied(conn)?;
    if history::next_pending(&applied).is_empty() {
        return Ok(());
    }

    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let outcome = apply_pending_transaction(conn);
    // Enforcement is a per-connection invariant installed by
    // `configure_connection`; restore it even when the migration fails.
    let restore = conn.pragma_update(None, "foreign_keys", "ON");
    outcome?;
    restore?;

    assert_referential_integrity(conn)?;
    assert_quick_check(conn)?;
    Ok(())
}

fn apply_pending_transaction(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Re-read inside the write lock: a concurrent opener may have advanced the
    // ledger while this process waited.
    let version = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let applied = history::read_applied(&tx)?;
    history::validate(&tx, version)?;

    let pending = history::next_pending(&applied);
    if pending.is_empty() {
        return Ok(());
    }

    for migration in pending {
        info!(
            version = migration.version,
            name = migration.name,
            "applying database migration"
        );
        let started = Instant::now();
        execute_body(&tx, &migration.body)?;
        let checksum = migration.checksum();
        history::insert_applied(
            &tx,
            migration,
            &checksum,
            now_ms(),
            started.elapsed().as_millis() as u64,
        )?;
        tx.pragma_update(None, "user_version", migration.version)?;
    }

    tx.commit()?;
    info!(
        version = CURRENT_DB_VERSION,
        "database schema is up-to-date"
    );
    Ok(())
}

/// Post-migration integrity gate: every declared foreign key must resolve.
pub fn assert_referential_integrity(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let violation = stmt
        .query_row([], |row| {
            Ok(format!(
                "table '{}' row {} references missing parent '{}' row {:?}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .optional()?;
    drop(stmt);
    if let Some(detail) = violation {
        return Err(history::incompatible(format!(
            "post-migration referential integrity check failed: {detail}"
        )));
    }
    Ok(())
}

/// SQLite's bounded page-level integrity check. Run only when this open
/// actually applied migrations, not on every launch.
fn assert_quick_check(conn: &Connection) -> Result<()> {
    let result: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if result != "ok" {
        return Err(history::incompatible(format!(
            "database quick_check failed after migration: {result}"
        )));
    }
    Ok(())
}

fn with_database_path(error: rusqlite::Error, db_path: &Path) -> rusqlite::Error {
    match error {
        rusqlite::Error::InvalidParameterName(message) => rusqlite::Error::InvalidParameterName(
            format!("{message}; database: {}", db_path.display()),
        ),
        other => other,
    }
}
