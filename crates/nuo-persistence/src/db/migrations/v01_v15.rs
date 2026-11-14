//! Schema definitions and data migrations for database versions 1 through 20.
use rusqlite::{Connection, OptionalExtension, Result, params};
use std::path::Path;
use tracing::info;

use super::runner::{sessions_columns, table_exists};
use crate::db::{unix_ms, upsert_attempt};

/// Persistence v2 schema repair, applied by migration 7. Every
/// step is conditional: fresh databases, final version-5 databases,
/// and interim version-5 databases (no transcript tables) must all converge.
pub fn apply_persistence_v2_schema(tx: &rusqlite::Transaction) -> Result<()> {
    // Dead ledgers: the event log never had a writer or a replay path, and
    // the CAS lives on the filesystem.
    tx.execute_batch("DROP TABLE IF EXISTS events; DROP TABLE IF EXISTS session_blobs;")?;

    if !table_exists(tx, "sessions")? {
        return Ok(());
    }

    let existing = sessions_columns(tx)?;
    // Honest units: sessions store seconds despite the `_ms` suffix.
    if existing.contains("created_at_ms") {
        tx.execute_batch("ALTER TABLE sessions RENAME COLUMN created_at_ms TO created_at_s;")?;
    }
    if existing.contains("updated_at_ms") {
        tx.execute_batch("ALTER TABLE sessions RENAME COLUMN updated_at_ms TO updated_at_s;")?;
    }
    // Restore the listing indexes the v5 table rebuild dropped. The
    // project-root indexes exist only while `project_root` is still a column:
    // a schema already migrated to `scope_key` (an artificial
    // re-stamp) must not fail here.
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at_s DESC);",
    )?;
    if sessions_columns(tx)?.contains("project_root") {
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_root);
             CREATE INDEX IF NOT EXISTS idx_sessions_project_updated ON sessions(project_root, updated_at_s DESC);",
        )?;
    }
    // Working state that v5 lost on the floor (regressions).
    if !existing.contains("digest_anchor") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN digest_anchor INTEGER;")?;
    }
    if !existing.contains("tree") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN tree TEXT NOT NULL DEFAULT '{}';")?;
    }
    if !existing.contains("transcript_generation") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN transcript_generation TEXT;")?;
    }
    // High-water mark with no reader or writer.
    if existing.contains("applied_seq") {
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN applied_seq;")?;
    }

    // The transcript ledger tables exist only in databases; an
    // interim version-5 database converges later, when the tables appear.
    if !table_exists(tx, "entries")? || !table_exists(tx, "entry_memberships")? {
        return Ok(());
    }

    // Drop the old FTS triggers before any table rename: ALTER TABLE RENAME
    // reparses every trigger body, and the old entry-insert trigger selects
    // from the table being swapped (it fired before the membership row
    // existed anyway, leaving fts_entries silently empty).
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS trg_entries_ai; DROP TRIGGER IF EXISTS trg_entries_ad;",
    )?;

    // Projections: the kind CHECK blocked forward-compatible extension;
    // created_at_ms was written (with seconds!) and never read.
    tx.execute_batch(
        "CREATE TABLE projections_new (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            kind       TEXT NOT NULL,
            up_to_seq  INTEGER NOT NULL,
            payload    TEXT NOT NULL,
            PRIMARY KEY (session_id, seq)
        );
        INSERT INTO projections_new (session_id, seq, kind, up_to_seq, payload)
            SELECT session_id, seq, kind, up_to_seq, payload FROM projections;
        DROP TABLE projections;
        ALTER TABLE projections_new RENAME TO projections;",
    )?;

    // Memberships: added_by was written and never read.
    tx.execute_batch(
        "CREATE TABLE entry_memberships_new (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            entry_id   TEXT NOT NULL REFERENCES entries(id),
            PRIMARY KEY (session_id, seq)
        );
        INSERT INTO entry_memberships_new (session_id, seq, entry_id)
            SELECT session_id, seq, entry_id FROM entry_memberships;
        DROP TABLE entry_memberships;
        ALTER TABLE entry_memberships_new RENAME TO entry_memberships;
        CREATE INDEX IF NOT EXISTS idx_memberships_entry ON entry_memberships(entry_id);",
    )?;

    // Durable GC roots: a blob is live iff some session's transcript
    // references it. Maintained in the same transaction as the save that
    // introduces the reference; session deletion cascades.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS blob_refs (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            hash       TEXT NOT NULL,
            PRIMARY KEY (session_id, hash)
        );
        INSERT OR IGNORE INTO blob_refs (session_id, hash)
            SELECT m.session_id, json_extract(e.payload, '$.content_blob')
            FROM entries e JOIN entry_memberships m ON m.entry_id = e.id
            WHERE json_extract(e.payload, '$.content_blob') IS NOT NULL;",
    )?;

    // Re-anchor FTS on memberships and backfill the index.
    tx.execute_batch(
        "CREATE TRIGGER trg_memberships_ai AFTER INSERT ON entry_memberships BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.entry_id, new.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
            FROM entries e WHERE e.id = new.entry_id;
        END;
        CREATE TRIGGER trg_memberships_ad AFTER DELETE ON entry_memberships BEGIN
            DELETE FROM fts_entries WHERE entry_id = old.entry_id AND session_id = old.session_id;
        END;
        DELETE FROM fts_entries;
        INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT m.entry_id, m.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
            FROM entry_memberships m JOIN entries e ON e.id = m.entry_id;",
    )?;
    Ok(())
}

/// Usage ledger schema repair, applied by migration 8: create the
/// key-addressed `usage_records` table and migrate the session-row JSON
/// column into it when that column exists.
pub fn apply_usage_ledger_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_records (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            actor_id   TEXT NOT NULL,
            round      INTEGER NOT NULL,
            turn       INTEGER NOT NULL,
            attempt    INTEGER NOT NULL,
            payload    TEXT NOT NULL,
            PRIMARY KEY (session_id, actor_id, round, turn, attempt)
        );",
    )?;
    if !table_exists(tx, "sessions")? {
        return Ok(());
    }
    let existing = sessions_columns(tx)?;
    if !existing.contains("request_usage_records") {
        return Ok(());
    }
    let mut stmt = tx.prepare(
        "SELECT id, request_usage_records FROM sessions WHERE request_usage_records IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    drop(stmt);
    for (session_id, json) in rows {
        let Ok(records) = serde_json::from_str::<Vec<nuo_protocol::RequestUsageRecord>>(&json)
        else {
            continue;
        };
        for record in &records {
            insert_legacy_usage_record_tx(tx, &session_id, record)?;
        }
    }
    tx.execute_batch("ALTER TABLE sessions DROP COLUMN request_usage_records;")?;
    Ok(())
}

/// Migration 10 repair: rewrite `entries` with the corrected `origin`/`hidden`
/// CHECKs (integrity fix). The whole step runs in the caller's outer
/// transaction with foreign-key enforcement disabled by the migration subagent
/// (sqlite.org/lang_altertable procedure): the rebuild briefly drops
/// `entries`, the parent of `entry_memberships`' foreign key, and re-creating
/// the equivalent rows under a new table name cannot decrement SQLite's
/// deferred-constraint counter — a `defer_foreign_keys` workaround COMMITs
/// with `FOREIGN KEY constraint failed` on a database whose final state is
/// perfectly consistent. Data and FTS triggers are preserved: rows are
/// copied byte-for-byte, the FTS-insert trigger is dropped before the rename
/// (ALTER TABLE RENAME reparses trigger bodies) and re-anchored after.
pub fn apply_entries_integrity_schema(tx: &rusqlite::Transaction) -> Result<()> {
    // The `entries` table exists only in databases that ran the
    // transcript migration. An interim version-5 database (no transcript
    // tables) converges later, when the entries/entry_memberships tables
    // appear; skip the repair here exactly like the v2 schema guard does.
    if !table_exists(tx, "entries")? || !table_exists(tx, "entry_memberships")? {
        return Ok(());
    }
    // `ALTER TABLE entries_new RENAME TO entries` re-parses every surviving
    // trigger body that references `entries`; the migration-7 membership
    // insert trigger selects from it, so it is dropped first and re-created
    // byte-identically after the swap (mirrors the v2 FTS re-anchor). The
    // old `entries` FTS triggers are dropped too (their bodies reference the
    // renamed table) and re-anchored identically.
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS trg_memberships_ai;
         DROP TRIGGER IF EXISTS trg_entries_ai;
         DROP TRIGGER IF EXISTS trg_entries_ad;
         DROP TRIGGER IF EXISTS trg_entries_membership_ai;
         DROP TRIGGER IF EXISTS trg_entries_membership_ad;
         CREATE TABLE entries_new (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
            role          TEXT CHECK (role IN ('user','assistant','system','tool')),
            content       TEXT,
            origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
            hidden        INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            CHECK ( hidden = 0 OR origin IS NOT NULL ),
            CHECK ( origin IS NULL OR hidden = 1 OR origin <> 'checkpoint' ),
            CHECK ( kind <> 'message' OR role IS NOT NULL )
        );
        INSERT INTO entries_new (id, kind, role, content, origin, hidden, created_at_ms, payload)
            SELECT id, kind, role, content, origin, hidden, created_at_ms, payload FROM entries;
        DROP TABLE entries;
        ALTER TABLE entries_new RENAME TO entries;
        -- Re-create the membership/entry FTS triggers with the exact pre-merge
        -- definitions so the resulting schema matches a fresh migration 7 run.
        CREATE TRIGGER trg_memberships_ai AFTER INSERT ON entry_memberships BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.entry_id, new.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
            FROM entries e WHERE e.id = new.entry_id;
        END;
        CREATE TRIGGER trg_entries_ai AFTER INSERT ON entries BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.id, m.session_id, COALESCE(new.role, ''), COALESCE(new.content, '')
            FROM entry_memberships m WHERE m.entry_id = new.id;
        END;
        CREATE TRIGGER trg_entries_ad AFTER DELETE ON entries BEGIN
            DELETE FROM fts_entries WHERE entry_id = old.id;
        END;",
    )?;
    Ok(())
}

/// Session-scope rebuild, applied by migration 12. Rebuilds
/// `sessions` so the partition key becomes `scope_kind` + namespaced
/// `scope_key` with an optional `workspace_root`, enforcing the NOT NULL
/// constraints `ALTER TABLE` cannot add. Existing rows backfill as workspace
/// scopes (`scope_key = 'ws:' || project_root`) with the same root bound as
/// their workspace, losslessly. Foreign-key enforcement is disabled by the
/// migration driver for the table swap; the post-commit integrity gate
/// re-verifies every declared reference.
pub fn apply_session_scope_schema(tx: &rusqlite::Transaction) -> Result<()> {
    let existing = sessions_columns(tx)?;
    if !existing.contains("project_root") {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        CREATE TABLE sessions_new (
            id                    TEXT PRIMARY KEY,
            parent_id             TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            fork_kind             TEXT NOT NULL DEFAULT 'trunk'
                                  CHECK (fork_kind IN ('trunk','fork','aside','subagent')),
            title                 TEXT,
            created_at_s          INTEGER NOT NULL,
            updated_at_s          INTEGER NOT NULL,
            scope_kind            TEXT NOT NULL
                                  CHECK (scope_kind IN ('workspace','persona','ephemeral')),
            scope_key             TEXT NOT NULL,
            workspace_root        TEXT,
            additional_roots      TEXT NOT NULL DEFAULT '[]',
            msg_count             INTEGER NOT NULL DEFAULT 0,
            last_user_prompt      TEXT,
            digest                TEXT,
            digest_anchor         INTEGER,
            tree                  TEXT NOT NULL DEFAULT '{}',
            transcript_generation TEXT,
            provider_connection   TEXT,
            round_counter         INTEGER NOT NULL DEFAULT 0,
            unattended            INTEGER NOT NULL DEFAULT 0,
            disabled_tools        TEXT NOT NULL DEFAULT '[]',
            commands              TEXT NOT NULL DEFAULT '[]',
            round_interrupts      TEXT NOT NULL DEFAULT '[]',
            retry_resolutions     TEXT NOT NULL DEFAULT '[]',
            retry_pending         TEXT,
            checksum              INTEGER,
            schema_version        INTEGER NOT NULL DEFAULT 14
        );
        INSERT INTO sessions_new (
            id, parent_id, fork_kind, title, created_at_s, updated_at_s,
            scope_kind, scope_key, workspace_root, additional_roots,
            msg_count, last_user_prompt, digest, digest_anchor, tree,
            transcript_generation, provider_connection, round_counter, unattended,
            disabled_tools, commands, round_interrupts, retry_resolutions,
            retry_pending, checksum, schema_version
        )
            SELECT
                id, parent_id, fork_kind, title, created_at_s, updated_at_s,
                'workspace', 'ws:' || project_root, project_root, '[]',
                msg_count, last_user_prompt, digest, digest_anchor, tree,
                transcript_generation, provider_connection, round_counter, unattended,
                disabled_tools, commands, round_interrupts, retry_resolutions,
                retry_pending, checksum, schema_version
            FROM sessions;
        DROP TABLE sessions;
        ALTER TABLE sessions_new RENAME TO sessions;
        CREATE INDEX idx_sessions_scope ON sessions(scope_key, updated_at_s DESC);
        CREATE INDEX idx_sessions_updated ON sessions(updated_at_s DESC);
        "#,
    )?;
    Ok(())
}

/// Grouping rebuild, applied by migration 13. Adds `space`, drops
/// `scope_kind`/`scope_key`, and re-indexes on the concrete grouping columns.
/// Conditional so a database already at the derived-grouping schema passes
/// through unchanged.
pub fn apply_session_grouping_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "sessions")? {
        return Ok(());
    }
    let existing = sessions_columns(tx)?;
    if existing.contains("space") && !existing.contains("scope_key") {
        return Ok(());
    }
    if !existing.contains("space") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN space TEXT;")?;
    }
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_sessions_scope;
         DROP INDEX IF EXISTS idx_sessions_project;
         DROP INDEX IF EXISTS idx_sessions_project_updated;",
    )?;
    if existing.contains("scope_kind") {
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN scope_kind;")?;
    }
    if existing.contains("scope_key") {
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN scope_key;")?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_workspace ON sessions(workspace_root, updated_at_s DESC);
         CREATE INDEX IF NOT EXISTS idx_sessions_space ON sessions(space, updated_at_s DESC);",
    )?;
    Ok(())
}

/// Workspace-only partition (revised), applied by migration 14:
/// drops the `space` column and adds `persona` metadata.
pub fn apply_workspace_partition_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "sessions")? {
        return Ok(());
    }
    let existing = sessions_columns(tx)?;
    if existing.contains("persona") && !existing.contains("space") {
        return Ok(());
    }
    if existing.contains("space") {
        // Drop the index that references the column before dropping it;
        // SQLite refuses to drop a column an index still references.
        tx.execute_batch("DROP INDEX IF EXISTS idx_sessions_space;")?;
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN space;")?;
    }
    if !existing.contains("persona") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN persona TEXT;")?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_persona ON sessions(persona, workspace_root, updated_at_s DESC);",
    )?;
    Ok(())
}

/// Fast FTS entry triggers (fix), applied by migration 16: replaces
/// the unindexed `trg_memberships_ad` trigger (which scanned the entire virtual
/// table on every deleted entry, stalling session persistence for seconds) with
/// an O(1) `rowid`-addressed delete, drops legacy `trg_entries_ai`/`trg_entries_ad`
/// triggers, and ensures `fts_entries.rowid` aligns with `entry_memberships.rowid`.
pub fn apply_fast_fts_triggers_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "entry_memberships")? || !table_exists(tx, "fts_entries")? {
        return Ok(());
    }

    tx.execute_batch(
        "DROP TRIGGER IF EXISTS trg_memberships_ai;
         DROP TRIGGER IF EXISTS trg_memberships_ad;
         DROP TRIGGER IF EXISTS trg_entries_ai;
         DROP TRIGGER IF EXISTS trg_entries_ad;

         CREATE TRIGGER trg_memberships_ai AFTER INSERT ON entry_memberships BEGIN
             INSERT INTO fts_entries(rowid, entry_id, session_id, role, content)
             SELECT new.rowid, new.entry_id, new.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
             FROM entries e WHERE e.id = new.entry_id;
         END;

         CREATE TRIGGER trg_memberships_ad AFTER DELETE ON entry_memberships BEGIN
             DELETE FROM fts_entries WHERE rowid = old.rowid;
         END;",
    )?;

    let misaligned: i64 = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM entry_memberships m
            LEFT JOIN fts_entries f ON m.rowid = f.rowid
            WHERE f.rowid IS NULL OR m.entry_id != f.entry_id OR m.session_id != f.session_id
            LIMIT 1
        )",
        [],
        |r| r.get(0),
    )?;

    if misaligned == 1 {
        info!("rebuilding fts_entries to align rowids with entry_memberships");
        tx.execute_batch(
            "DELETE FROM fts_entries;
             INSERT INTO fts_entries(rowid, entry_id, session_id, role, content)
                 SELECT m.rowid, m.entry_id, m.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
                 FROM entry_memberships m JOIN entries e ON e.id = m.entry_id;",
        )?;
    }

    Ok(())
}

/// Session IR Clean Break, applied by migration 17:
/// Initializes headless `sessions_v2`, `session_policies`, `causal_nodes` schema,
/// migrates historical data out of legacy tables, and establishes `session_list_view`.
pub fn apply_session_ir_clean_break_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sessions_v2 (
            id TEXT PRIMARY KEY,
            parent_session_id TEXT REFERENCES sessions_v2(id) ON DELETE SET NULL,
            active_leaf TEXT,
            status TEXT NOT NULL DEFAULT 'idle',
            suspension_payload TEXT,
            pending_notifications TEXT NOT NULL DEFAULT '[]',
            round_counter INTEGER NOT NULL DEFAULT 0,
            created_at_s INTEGER NOT NULL,
            updated_at_s INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS session_policies (
            session_id TEXT PRIMARY KEY REFERENCES sessions_v2(id) ON DELETE CASCADE,
            rules_json TEXT NOT NULL,
            capabilities_json TEXT NOT NULL,
            guardrails_json TEXT NOT NULL,
            budget_json TEXT NOT NULL,
            updated_at_s INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS causal_nodes (
            id TEXT NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions_v2(id) ON DELETE CASCADE,
            parent_id TEXT,
            seq INTEGER NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','system_notice')),
            payload_json TEXT NOT NULL,
            timestamp_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, id),
            UNIQUE(session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_causal_nodes_session_seq ON causal_nodes(session_id, seq ASC);
        CREATE INDEX IF NOT EXISTS idx_causal_nodes_parent ON causal_nodes(session_id, parent_id);
        "#,
    )?;

    // Create the CQRS Read Model projection view
    tx.execute_batch(
        r#"
        CREATE VIEW IF NOT EXISTS session_list_view AS
        SELECT
            s.id,
            s.parent_session_id AS parent_id,
            'trunk' AS fork_kind,
            NULL AS title,
            s.created_at_s,
            s.updated_at_s,
            (SELECT json_extract(rules_json, '$.workspace_root') FROM session_policies p WHERE p.session_id = s.id) AS workspace_root,
            (SELECT json_extract(rules_json, '$.system_persona') FROM session_policies p WHERE p.session_id = s.id) AS persona,
            COALESCE((SELECT COUNT(*) FROM causal_nodes c WHERE c.session_id = s.id AND c.kind = 'dialogue'), 0) AS msg_count,
            (SELECT json_extract(c.payload_json, '$.message.content')
             FROM causal_nodes c
             WHERE c.session_id = s.id AND json_extract(c.payload_json, '$.message.role') = 'user'
             ORDER BY c.seq DESC LIMIT 1) AS last_user_prompt,
            NULL AS digest
        FROM sessions_v2 s;
        "#,
    )?;

    // Migrate any existing sessions in legacy `sessions` table into `sessions_v2` and `session_policies`
    if table_exists(tx, "sessions")? {
        tx.execute_batch(
            r#"
            INSERT OR IGNORE INTO sessions_v2 (
                id, parent_session_id, active_leaf, status, suspension_payload,
                pending_notifications, round_counter, created_at_s, updated_at_s
            )
            SELECT
                id,
                parent_id,
                CASE WHEN json_valid(tree) THEN json_extract(tree, '$.active_leaf_id') ELSE NULL END,
                CASE WHEN retry_pending IS NOT NULL THEN 'suspended' ELSE 'idle' END,
                retry_pending,
                '[]',
                round_counter,
                created_at_s,
                updated_at_s
            FROM sessions;

            INSERT OR IGNORE INTO session_policies (
                session_id, rules_json, capabilities_json, guardrails_json, budget_json, updated_at_s
            )
            SELECT
                id,
                json_object('system_persona', persona, 'workspace_root', workspace_root, 'project_rules', json('[]')),
                json_object('enabled_tools', json('[]'), 'disabled_tools', json(disabled_tools), 'provider_pin', provider_connection),
                json_object('unattended', unattended != 0, 'require_approval_tools', json('[]')),
                json_object('max_context_tokens', 128000, 'compaction_trigger_tokens', 96000, 'max_tool_output_tokens', 8000),
                updated_at_s
            FROM sessions;
            "#,
        )?;
    }

    // Migrate existing entries into causal_nodes if `entries` and `entry_memberships` exist
    if table_exists(tx, "entries")? && table_exists(tx, "entry_memberships")? {
        tx.execute_batch(
            r#"
            INSERT OR IGNORE INTO causal_nodes (
                id, session_id, parent_id, seq, kind, payload_json, timestamp_ms
            )
            SELECT
                m.entry_id,
                m.session_id,
                NULL,
                m.seq,
                CASE WHEN e.origin = 'checkpoint' THEN 'compaction' ELSE 'dialogue' END,
                CASE 
                    WHEN e.kind = 'message' THEN json_object('type', 'message', 'message', json_object('role', e.role, 'content', COALESCE(e.content, '')))
                    ELSE json_object('type', 'system_notice', 'source', 'transcript', 'notice_type', 'state', 'content', COALESCE(e.content, ''))
                END,
                e.created_at_ms
            FROM entry_memberships m
            JOIN entries e ON e.id = m.entry_id;
            "#,
        )?;
    }

    Ok(())
}

pub fn insert_legacy_usage_record_tx(
    tx: &rusqlite::Connection,
    session_id: &str,
    record: &nuo_protocol::RequestUsageRecord,
) -> rusqlite::Result<()> {
    let payload = serde_json::to_string(record)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    tx.execute(
        "INSERT OR REPLACE INTO usage_records (session_id, actor_id, round, turn, attempt, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            record.key.actor_id,
            record.key.round as i64,
            record.key.turn as i64,
            record.key.attempt as i64,
            payload,
        ],
    )?;
    Ok(())
}

/// Universal Asset Attestation Schema, applied by migration 18:
/// Initializes the `asset_attestations` table for process and endpoint trust.
pub fn apply_asset_attestation_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS asset_attestations (
            fingerprint TEXT PRIMARY KEY,
            asset_type TEXT NOT NULL,
            summary TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at_s INTEGER NOT NULL,
            updated_at_s INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_asset_attestations_status ON asset_attestations(status);
        "#,
    )?;
    Ok(())
}

/// Migration 19: Session Role Manifest Snapshotting.
/// Adds `role_manifest TEXT` JSON column to `sessions` table.
pub fn apply_role_manifest_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "sessions")? {
        return Ok(());
    }
    let existing = sessions_columns(tx)?;
    if !existing.contains("role_manifest") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN role_manifest TEXT;")?;
    }
    Ok(())
}

/// Adds covering index on `sessions(persona, updated_at_s DESC)` for workspace-free role partitions.
pub fn apply_role_anchored_session_partition_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "sessions")? {
        return Ok(());
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_role_partition \
         ON sessions(persona, updated_at_s DESC) \
         WHERE workspace_root IS NULL AND fork_kind <> 'subagent';",
    )?;
    Ok(())
}

/// Upgrades `causal_nodes` table primary key to composite `(session_id, id)`.
pub fn apply_causal_nodes_composite_pk_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "causal_nodes")? {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        DROP VIEW IF EXISTS session_list_view;

        CREATE TABLE IF NOT EXISTS causal_nodes_v2 (
            id                    TEXT NOT NULL,
            session_id            TEXT NOT NULL REFERENCES sessions_v2(id) ON DELETE CASCADE,
            parent_id             TEXT,
            seq                   INTEGER NOT NULL,
            kind                  TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','system_notice')),
            payload_json          TEXT NOT NULL,
            timestamp_ms          INTEGER NOT NULL,
            PRIMARY KEY (session_id, id),
            UNIQUE(session_id, seq)
        );

        INSERT OR IGNORE INTO causal_nodes_v2 (id, session_id, parent_id, seq, kind, payload_json, timestamp_ms)
        SELECT id, session_id, parent_id, seq, kind, payload_json, timestamp_ms FROM causal_nodes;

        DROP TABLE causal_nodes;
        ALTER TABLE causal_nodes_v2 RENAME TO causal_nodes;

        CREATE INDEX IF NOT EXISTS idx_causal_nodes_session_seq ON causal_nodes(session_id, seq ASC);
        CREATE INDEX IF NOT EXISTS idx_causal_nodes_parent ON causal_nodes(session_id, parent_id);

        CREATE VIEW IF NOT EXISTS session_list_view AS
        SELECT
            s.id,
            s.parent_session_id AS parent_id,
            'trunk' AS fork_kind,
            NULL AS title,
            s.created_at_s,
            s.updated_at_s,
            (SELECT json_extract(rules_json, '$.workspace_root') FROM session_policies p WHERE p.session_id = s.id) AS workspace_root,
            (SELECT json_extract(rules_json, '$.system_persona') FROM session_policies p WHERE p.session_id = s.id) AS persona,
            COALESCE((SELECT COUNT(*) FROM causal_nodes c WHERE c.session_id = s.id AND c.kind = 'dialogue'), 0) AS msg_count,
            (SELECT json_extract(c.payload_json, '$.message.content')
             FROM causal_nodes c
             WHERE c.session_id = s.id AND json_extract(c.payload_json, '$.message.role') = 'user'
             ORDER BY c.seq DESC LIMIT 1) AS last_user_prompt,
            NULL AS digest
        FROM sessions_v2 s;
        "#,
    )?;
    Ok(())
}

pub fn insert_usage_record_tx(
    conn: &Connection,
    session_id: &str,
    record: &nuo_protocol::RequestUsageRecord,
) -> Result<()> {
    if record.key.session_id != session_id {
        return Err(rusqlite::Error::InvalidParameterName(
            "usage belongs to another session".into(),
        ));
    }
    let root: Option<String> = conn
        .query_row(
            "SELECT workspace_root FROM thread_policies WHERE thread_id=?1",
            [session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let project = root
        .map(|p| nuo_host::paths::project_bucket_name(Path::new(&p)))
        .unwrap_or_default();
    let at = if record.started_at_ms > 0 {
        record.started_at_ms
    } else {
        unix_ms()
    };
    upsert_attempt(
        conn,
        &nuo_protocol::usage_stats::UsageStatRecord {
            day: nuo_protocol::usage_stats::day_key_from_epoch_ms(at),
            recorded_at_ms: at,
            project,
            record: record.clone(),
        },
    )?;
    Ok(())
}
