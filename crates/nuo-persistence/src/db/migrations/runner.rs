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
use super::catalog::NUO_SCHEMA;
use super::history;
use super::schema::Schema;
use super::verify;

/// Open a database file and bring it to the current schema.
pub(crate) fn initialize_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut conn = Connection::open(db_path)?;
    configure_connection(&mut conn)?;
    migrate_schema_with(&mut conn, &NUO_SCHEMA, Some(db_path))
        .map_err(|error| with_database_path(error, db_path))?;

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
    initialize_schema(conn, &NUO_SCHEMA, db_path)
}

/// Apply one schema's standard catalog to an already-open connection. Used by
/// secondary durable databases (`role_memory.db`) so they share the same
/// baseline/ledger/verification path (ADR-0192).
pub(crate) fn initialize_schema(
    conn: &mut Connection,
    schema: &Schema,
    db_path: Option<&Path>,
) -> Result<()> {
    configure_connection(conn)?;
    migrate_schema_with(conn, schema, db_path).map_err(|error| match db_path {
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

/// Bring `nuo.db` to the current schema, or refuse it.
pub fn migrate_schema(conn: &mut Connection) -> Result<()> {
    migrate_schema_with(conn, &NUO_SCHEMA, None)
}

/// Bring one schema to its current version.
pub(crate) fn migrate_schema_with(
    conn: &mut Connection,
    schema: &Schema,
    db_path: Option<&Path>,
) -> Result<()> {
    let version = user_version(conn)?;
    let ledger = history::ledger_exists(conn)?;

    if version == 0 && !ledger {
        if has_user_schema(conn)? {
            return Err(history::incompatible(format!(
                "{}: database has tables but no migration ledger; it was written by the retired \
                 pre-baseline migration chain",
                schema.name
            )));
        }
        apply_pending(conn, schema, db_path)?;
    } else {
        if !ledger {
            return Err(history::incompatible(format!(
                "{}: PRAGMA user_version is {version} but the migration ledger is missing; \
                 the database was written by the retired pre-baseline migration chain",
                schema.name
            )));
        }
        if version > schema.current_version {
            return Err(history::incompatible(format!(
                "{}: database schema v{version} is newer than this binary (v{}); \
                 upgrade nuo to open it",
                schema.name, schema.current_version
            )));
        }
        history::validate(conn, version, schema)?;
        apply_pending(conn, schema, db_path)?;
        let version = user_version(conn)?;
        history::validate(conn, version, schema)?;
    }

    verify::verify(conn, schema)
}

/// Apply every migration above the ledger's last row in one transaction.
fn apply_pending(conn: &mut Connection, schema: &Schema, db_path: Option<&Path>) -> Result<()> {
    let applied = history::read_applied(conn)?;
    let pending = history::next_pending(&applied, schema);
    if pending.is_empty() {
        return Ok(());
    }

    // A brand-new database needs no backup: the baseline is the first state.
    // An existing database is snapshotted before any pending migration, and a
    // failed backup aborts the upgrade rather than risking the only copy.
    if !applied.is_empty()
        && let Some(path) = db_path
    {
        backup_before_migration(conn, path, pending[0].version)?;
    }

    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let outcome = apply_pending_transaction(conn, schema);
    // Enforcement is a per-connection invariant installed by
    // `configure_connection`; restore it even when the migration fails.
    let restore = conn.pragma_update(None, "foreign_keys", "ON");
    outcome?;
    restore?;

    assert_referential_integrity(conn)?;
    assert_quick_check(conn)?;
    Ok(())
}

fn apply_pending_transaction(conn: &mut Connection, schema: &Schema) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // The ledger is infrastructure, created in the same transaction as the
    // baseline it records: a failed first migration leaves no half-created
    // database state.
    history::ensure_ledger(&tx)?;

    // Re-read inside the write lock: a concurrent opener may have advanced the
    // ledger while this process waited.
    let version = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let applied = history::read_applied(&tx)?;
    history::validate(&tx, version, schema)?;

    let pending = history::next_pending(&applied, schema);
    if pending.is_empty() {
        return Ok(());
    }

    for migration in pending {
        info!(
            schema = schema.name,
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
        schema = schema.name,
        version = schema.current_version,
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

/// Test-only: build a database at an earlier catalog version without the
/// latest-schema verification. Use this to create an upgrade fixture:
///
/// ```ignore
/// migrate_to_for_tests(&mut conn, CURRENT_DB_VERSION - 1)?;
/// // insert rows that the next migration must transform
/// migrate_schema(&mut conn)?;
/// ```
#[cfg(test)]
pub(crate) fn migrate_to_for_tests(conn: &mut Connection, through: u32) -> Result<()> {
    configure_connection(conn)?;
    history::ensure_ledger(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for migration in NUO_SCHEMA
        .migrations
        .iter()
        .filter(|migration| migration.version <= through)
    {
        execute_body(&tx, &migration.body)?;
        history::insert_applied(&tx, migration, &migration.checksum(), now_ms(), 0)?;
        tx.pragma_update(None, "user_version", migration.version)?;
    }
    tx.commit()
}

/// Snapshot the database before a pending migration. `VACUUM INTO` is a
/// consistent copy while the connection is idle and is safe with WAL. The
/// backup keeps the pre-migration `user_version` and ledger, so restoring it is
/// a plain file replacement.
fn backup_before_migration(conn: &Connection, db_path: &Path, next_version: u32) -> Result<()> {
    let stamp = now_ms();
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "nuo.db".to_string());
    let backup = db_path.with_file_name(format!("{file_name}.pre-v{next_version}.{stamp}.bak"));
    let backup_str = backup.to_str().ok_or_else(|| {
        history::incompatible(format!(
            "pre-migration backup path is not valid UTF-8: {}",
            backup.display()
        ))
    })?;

    conn.execute("VACUUM INTO ?1", params![backup_str])
        .map_err(|error| {
            history::incompatible(format!(
                "could not create pre-migration backup at {}: {error}; refusing to migrate",
                backup.display()
            ))
        })?;
    info!(
        backup = %backup.display(),
        next_version,
        "created pre-migration database backup"
    );
    Ok(())
}

fn with_database_path(error: rusqlite::Error, db_path: &Path) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(format!("{error}; database: {}", db_path.display()))
}
