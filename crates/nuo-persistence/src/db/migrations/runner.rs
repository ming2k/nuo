//! Schema migration runner, connection initialization, and integrity checks.
use rusqlite::{Connection, OptionalExtension, Result, TransactionBehavior, params};
use std::path::Path;
use tracing::{info, warn};

use super::catalog::MIGRATIONS;
use super::types::*;
use super::v01_v15::*;
use super::v16_v25::*;
use super::v26_v30::*;
use super::v31_v35::*;
use crate::db::{FastSummaryProbe, apply_durable_attempt_schema};

/// Initialize and return a connection to the SQLite database.
/// Configures WAL mode, synchronous=FULL for robustness, busy timeout, and turns on foreign keys.
pub(crate) fn initialize_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut conn = Connection::open(db_path)?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;

    Ok(conn)
}

/// Initialize an in-memory SQLite database for testing and ephemeral workflows.
#[cfg(test)]
pub(crate) fn initialize_in_memory_db() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;
    Ok(conn)
}

/// Apply the full schema to an already-open connection.
///
/// Used by the offline migration tool to prepare a target database without
/// going through the server's single-writer door (the tool is a separate
/// process).
pub(crate) fn initialize_connection_schema(conn: &mut Connection) -> Result<()> {
    configure_connection(conn)?;
    migrate_schema(conn)
}

/// Open a fully-migrated in-memory database for integration tests.
///
/// Test support only: exposes the real migration path to cross-crate
/// integration tests without a filesystem. Not part of the runtime API.
#[doc(hidden)]
pub fn open_in_memory_for_tests() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    initialize_connection_schema(&mut conn)?;
    Ok(conn)
}

/// Standard connection configurations applied to every connection (reader & writer).
pub fn configure_connection(conn: &mut Connection) -> Result<()> {
    // busy_timeout first: every later lock attempt (including the migration
    // transaction) must wait rather than fail immediately.
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // `PRAGMA journal_mode=WAL` does not invoke the busy handler. Under
    // concurrent first-open — parallel test processes sharing one database —
    // a writer mid-migration makes it return SQLITE_BUSY immediately.
    // Best-effort: the winning process sets WAL and every later connection
    // observes it, so a transient refusal here is harmless.
    if let Err(error) = conn.pragma_update(None, "journal_mode", "WAL") {
        warn!(%error, "could not set WAL journal mode; continuing");
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_size_limit", 16777216)?; // 16MB WAL recycling
    // Checkpointing is owned by the storage-maintenance thread, which runs on
    // its own connection with a short busy timeout. SQLite's automatic
    // checkpoint runs inside the *writer's* commit path; against a pinned WAL
    // it then walks the whole wal-index on every commit, which is exactly the
    // latency cliff that turned ordinary turns into 20–55 second stalls.
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

pub fn sessions_columns(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    Ok(columns)
}

pub fn table_columns(
    conn: &Connection,
    table_name: &str,
) -> Result<std::collections::HashSet<String>> {
    let pragma = format!("PRAGMA table_info({table_name})");
    let mut stmt = conn.prepare(&pragma)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    Ok(columns)
}

/// Reconcile an interim version-5 `sessions` table to the final
/// schema: drop retired columns, add any missing working-state column. Every
/// step is conditional because both version-5 incarnations (interim and
/// final) must converge through this repair.
pub fn repair_intermediate_sessions_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "sessions")? {
        return Ok(());
    }
    let existing = sessions_columns(tx)?;
    for retired in SESSIONS_RETIRED_COLUMNS {
        if existing.contains(*retired) {
            tx.execute_batch(&format!("ALTER TABLE sessions DROP COLUMN {retired};"))?;
        }
    }
    for (name, ddl) in SESSIONS_WORKING_STATE_COLUMNS {
        if !existing.contains(*name) {
            tx.execute_batch(&format!("ALTER TABLE sessions ADD COLUMN {name} {ddl};"))?;
        }
    }
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

/// Fail-fast guard: verify the `sessions` table carries exactly the columns
/// the current runtime SQL writes, so a schema/runtime mismatch surfaces as
/// one clear startup error instead of scattered per-statement failures
/// (`table sessions has no column named ...`) during persistence.
pub fn verify_sessions_schema(conn: &Connection) -> Result<()> {
    if !conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'sessions')",
            [],
            |r| r.get::<_, i64>(0),
        )?
        .eq(&1)
    {
        return Ok(());
    }
    let existing = sessions_columns(conn)?;
    let mut expected: std::collections::HashSet<String> = SESSIONS_IDENTITY_COLUMNS
        .iter()
        .map(|c| c.to_string())
        .collect();
    expected.extend(
        SESSIONS_WORKING_STATE_COLUMNS
            .iter()
            .map(|(c, _)| c.to_string()),
    );
    // Column owned by migration 9 (not in the conditional repair list, see
    // the note there) — the startup guard still requires it.
    expected.insert("retry_resolutions".to_string());
    expected.extend(SESSIONS_V2_COLUMNS.iter().map(|(c, _)| c.to_string()));
    // Columns migration 8 retired from the row (their data moved to the
    // usage ledger table) must not be required here.
    expected.remove("request_usage_records");
    let missing: Vec<&str> = expected
        .iter()
        .filter(|c| !existing.contains(c.as_str()))
        .map(|c| c.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(rusqlite::Error::InvalidColumnName(format!(
            "sessions table is missing column(s) {}; the database predates a \
             skipped schema migration — restore a backup or recreate the state",
            missing.join(", ")
        )));
    }
    // Table owned by migration 11. In version 24+ (ADR-0059),
    // request_projections is retired; only enforce for legacy databases.
    if !table_exists(conn, "threads")? && !table_exists(conn, "request_projections")? {
        return Err(rusqlite::Error::InvalidParameterName(
            "request_projections table is missing; the database predates schema \
             migration 11 — restore a backup or recreate the state"
                .to_string(),
        ));
    }
    Ok(())
}

/// Verify presence of canonical Thread SSOT schema tables (ADR-0059).
pub fn verify_thread_canonical_schema(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "threads")? {
        return Err(rusqlite::Error::InvalidParameterName(
            "threads table is missing; the database predates schema migration 24 — restore a backup or recreate state"
                .to_string(),
        ));
    }
    if !table_exists(conn, "thread_policies")? {
        return Err(rusqlite::Error::InvalidParameterName(
            "thread_policies table is missing; the database predates schema migration 24 — restore a backup or recreate state"
                .to_string(),
        ));
    }
    if !table_exists(conn, "workspace_effects")? {
        return Err(rusqlite::Error::InvalidParameterName(
            "workspace_effects table is missing; the database predates schema migration 26 — restore a backup or recreate state"
                .to_string(),
        ));
    }
    Ok(())
}

/// Run all outstanding migrations in a single transactional loop, then
/// verify the resulting schema. Verification also covers databases that
/// short-circuit the loop (already at `CURRENT_DB_VERSION`) so a schema a
/// retired migration produced cannot reach runtime SQL unnoticed.
///
/// Foreign-key enforcement is disabled for the duration of the migration
/// transaction and restored afterwards, per the canonical table-rebuild
/// procedure (sqlite.org/lang_altertable): migrations 5 and 10 drop parent
/// tables (`sessions`, `entries`) that other tables reference and swap a
/// rebuilt replacement into place. With enforcement ON, SQLite's deferred
/// constraint counter counts the dropped parent rows and is never
/// reconciled by the rename, so COMMIT fails with `FOREIGN KEY constraint
/// failed` even though the post-migration state is perfectly referentially
/// consistent — and because the failure rolls the whole transaction back,
/// the database never advances past the broken migration and every future
/// open retries and fails it forever. `PRAGMA foreign_keys` is a no-op
/// inside an open transaction, hence the flip happens before `BEGIN`. A
/// `foreign_key_check` gate after COMMIT fails loud on a database whose
/// final state violates its own declared references.
pub fn migrate_schema(conn: &mut Connection) -> Result<()> {
    let current_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    // A database written by a newer binary may carry schema this build cannot
    // interpret; proceeding would silently degrade or destroy data. Fail loud
    // and let every caller surface the refusal.
    if current_version > CURRENT_DB_VERSION {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database schema v{current_version} is newer than this binary (v{CURRENT_DB_VERSION}); \
             refusing to open — upgrade nuo to work with this state"
        )));
    }

    if current_version < CURRENT_DB_VERSION {
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        let outcome = apply_migrations(conn, current_version);
        // Enforcement is a per-connection invariant installed by
        // `configure_connection`; restore it before propagating any
        // migration failure so the connection is never handed back in a
        // weaker security posture than it started with.
        let restore = conn.pragma_update(None, "foreign_keys", "ON");
        outcome?;
        restore?;
        assert_referential_integrity(conn)?;
    }

    if !table_exists(conn, "threads")? {
        verify_sessions_schema(conn)?;
    }
    verify_thread_canonical_schema(conn)
}

/// Apply every migration above `observed_version` inside one immediate
/// transaction, then stamp `PRAGMA user_version`. `BEGIN IMMEDIATE` takes
/// the write lock up front; `user_version` is re-read inside the lock so a
/// concurrent opener that observed a stale version while waiting serializes
/// into a no-op instead of racing a duplicate rebuild.
pub fn apply_migrations(conn: &mut Connection, observed_version: u32) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_version: u32 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current_version > CURRENT_DB_VERSION {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database schema v{current_version} is newer than this binary (v{CURRENT_DB_VERSION}); \
             refusing to open — upgrade nuo to work with this state"
        )));
    }

    if current_version == observed_version {
        for migration in MIGRATIONS {
            if migration.version > current_version {
                info!(
                    version = migration.version,
                    "Applying SQLite schema migration"
                );
                if !migration.sql.is_empty() {
                    if !table_exists(&tx, "sessions")?
                        && migration.sql.contains("ALTER TABLE sessions")
                    {
                        // Obsolete sessions table alterations skipped because sessions is dropped
                    } else {
                        tx.execute_batch(migration.sql)?;
                    }
                }

                // Migration 9 is a no-op when the column already exists — a
                // database stamped by an intermediate dev build (via the
                // startup repair, or a partially-applied v9) must pass
                // through unchanged instead of failing on a duplicate
                // `ADD COLUMN` (mirrors the migration-6 conditional repair).
                if migration.version == 9 && table_exists(&tx, "sessions")? {
                    let has_column = sessions_columns(&tx)?
                        .iter()
                        .any(|column| column == "retry_resolutions");
                    if !has_column {
                        tx.execute_batch(
                            "ALTER TABLE sessions ADD COLUMN retry_resolutions TEXT NOT NULL DEFAULT '[]';",
                        )?;
                    }
                }

                if migration.version == 3 && table_exists(&tx, "sessions")? {
                    let mut stmt =
                        tx.prepare("SELECT id, data FROM sessions WHERE data IS NOT NULL")?;
                    let rows = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                    let mut updates = Vec::new();
                    for item in rows {
                        let (id, data_str) = item?;
                        if let Ok(probe) = serde_json::from_str::<FastSummaryProbe>(&data_str) {
                            let count = probe.model_window.len() + probe.archived_transcript.len();
                            let last_prompt = probe
                                .model_window
                                .iter()
                                .rev()
                                .chain(probe.archived_transcript.iter().rev())
                                .find(|m| {
                                    let is_echo = m.origin.as_ref().is_some_and(|o| {
                                        o.kind == nuo_protocol::InjectionKind::CommandEcho
                                    });
                                    m.role == nuo_protocol::Role::User && !m.hidden && !is_echo
                                })
                                .map(|m| m.content.clone());
                            let digest_json = probe
                                .digest
                                .as_ref()
                                .and_then(|d| serde_json::to_string(d).ok());
                            updates.push((id, count as i64, last_prompt, digest_json));
                        }
                    }
                    drop(stmt);
                    let mut update_stmt = tx.prepare(
                        "UPDATE sessions SET msg_count = ?1, last_user_prompt = ?2, digest = ?3 WHERE id = ?4",
                    )?;
                    for (id, count, last_prompt, digest_json) in updates {
                        update_stmt.execute(params![count, last_prompt, digest_json, id])?;
                    }
                }

                if migration.version == 6 {
                    repair_intermediate_sessions_schema(&tx)?;
                }
                if migration.version == 7 {
                    apply_persistence_v2_schema(&tx)?;
                }
                if migration.version == 8 {
                    apply_usage_ledger_schema(&tx)?;
                }
                if migration.version == 9 && table_exists(&tx, "sessions")? {
                    // Conditional add (see the migration entry's note): the
                    // column already exists on every database whose version-5
                    // SQL was finalized after the feature landed.
                    let existing = sessions_columns(&tx)?;
                    if !existing.contains("retry_resolutions") {
                        tx.execute_batch(
                            "ALTER TABLE sessions ADD COLUMN retry_resolutions TEXT NOT NULL DEFAULT '[]';",
                        )?;
                    }
                }
                if migration.version == 10 {
                    apply_entries_integrity_schema(&tx)?;
                }
                if migration.version == 12 {
                    apply_session_scope_schema(&tx)?;
                }
                if migration.version == 13 {
                    apply_session_grouping_schema(&tx)?;
                }
                if migration.version == 14 {
                    apply_workspace_partition_schema(&tx)?;
                }
                if migration.version == 15 {
                    apply_durable_attempt_schema(&tx)?;
                }
                if migration.version == 16 {
                    apply_fast_fts_triggers_schema(&tx)?;
                }
                if migration.version == 17 {
                    apply_session_ir_clean_break_schema(&tx)?;
                }
                if migration.version == 18 {
                    apply_asset_attestation_schema(&tx)?;
                }
                if migration.version == 19 {
                    apply_role_manifest_schema(&tx)?;
                }
                if migration.version == 20 {
                    apply_role_anchored_session_partition_schema(&tx)?;
                }
                if migration.version == 21 {
                    apply_causal_nodes_composite_pk_schema(&tx)?;
                }
                if migration.version == 22 {
                    apply_context_lifecycle_schema(&tx)?;
                }
                if migration.version == 23 {
                    apply_retrieval_and_gc_schema(&tx)?;
                }
                if migration.version == 24 {
                    apply_thread_canonical_clean_break_schema(&tx)?;
                }
                if migration.version == 25 {
                    apply_thread_lineage_graph_schema(&tx)?;
                }
                if migration.version == 26 {
                    apply_workspace_effect_journal_schema(&tx)?;
                }
                if migration.version == 27 {
                    apply_causal_fact_backfill_schema(&tx)?;
                }
                if migration.version == 28 {
                    apply_derived_index_schema(&tx)?;
                }
                if migration.version == 29 {
                    apply_derived_scrub_schema(&tx)?;
                }
                if migration.version == 30 {
                    apply_role_scoped_thread_scope_schema(&tx)?;
                }
                if migration.version == 31 {
                    apply_drop_legacy_fts_triggers_schema(&tx)?;
                }
                if migration.version == 32 {
                    apply_thread_scope_immutability_schema(&tx)?;
                }
                if migration.version == 33 {
                    apply_workspace_effect_detach_schema(&tx)?;
                }
            }
        }

        // Update schema version pragma
        let pragma_sql = format!("PRAGMA user_version = {CURRENT_DB_VERSION}");
        tx.execute_batch(&pragma_sql)?;

        tx.commit()?;
        info!(
            version = CURRENT_DB_VERSION,
            "SQLite database schema is up-to-date"
        );
    }

    Ok(())
}

/// Post-migration integrity gate: scan every declared foreign key in the
/// schema and fail loud on dangling references. Runs after COMMIT with
/// enforcement restored, so a migration that produced an inconsistent state
/// surfaces as one clear startup error instead of scattered per-statement
/// failures in runtime SQL.
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
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "post-migration referential integrity check failed: {detail}; \
             restore a backup or recreate the state"
        )));
    }
    Ok(())
}
