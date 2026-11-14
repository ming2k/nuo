//! Migrations 31+.
use rusqlite::Result;

use super::runner::{table_columns, table_exists};

/// Migration 31: remove the FTS write triggers migration 28 intended to drop.
///
/// Migration 28 dropped `trg_memberships_ai`/`trg_memberships_ad` and
/// `trg_entries_ai`/`trg_entries_ad`, but the thread-canonical rebuild
/// (migration 24) had re-created the membership pair under
/// `trg_entries_membership_ai`/`trg_entries_membership_ad`. Those survived, so
/// every transcript commit still wrote `fts_entries` on the writer thread
/// despite `[INV-DERIVED-01]`, and a full rewrite's
/// `DELETE FROM fts_entries WHERE session_id = ?` scanned the entire FTS
/// content table (seconds on a live database; the visible `saving request`
/// stall).
///
/// The named drops cover every trigger spelling this catalog has ever created.
/// The trailing sweep is deliberate: any *other* trigger on the transcript that
/// writes the derived FTS table violates the same invariant, and leaving it in
/// place would reintroduce the cliff under a name this migration did not know.
pub fn apply_drop_legacy_fts_triggers_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS trg_memberships_ai;
         DROP TRIGGER IF EXISTS trg_memberships_ad;
         DROP TRIGGER IF EXISTS trg_entries_ai;
         DROP TRIGGER IF EXISTS trg_entries_ad;
         DROP TRIGGER IF EXISTS trg_entries_membership_ai;
         DROP TRIGGER IF EXISTS trg_entries_membership_ad;",
    )?;

    let mut stmt = tx.prepare(
        "SELECT name FROM sqlite_master
          WHERE type = 'trigger'
            AND tbl_name IN ('entries', 'entry_memberships')
            AND lower(sql) LIKE '%fts_entries%'",
    )?;
    let leftover: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<_>>()?;
    drop(stmt);
    for name in leftover {
        // The identifier comes from `sqlite_master`, never from user input.
        // Doubling embedded quotes keeps the DDL well-formed even for a name a
        // future migration might have added.
        let quoted = name.replace('"', "\"\"");
        tx.execute_batch(&format!("DROP TRIGGER IF EXISTS \"{quoted}\";"))?;
    }
    Ok(())
}

/// Migration 32: a thread's role and workspace binding are write-once.
///
/// `ThreadScope = (role, workspace)` is the thread's durable identity, not a
/// mutable preference (ADR-0170). `/role` switches to (or starts) a thread of
/// the target role; it never rebinds the current one. The trigger enforces the
/// same rule for every writer — including a future code path that bypasses the
/// CLI — so a save that disagrees with the stored scope fails loudly instead of
/// silently moving a conversation between history buckets.
pub fn apply_thread_scope_immutability_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TRIGGER IF NOT EXISTS trg_thread_policy_scope_immutable
        BEFORE UPDATE OF system_role, workspace_root ON thread_policies
        WHEN NEW.system_role <> OLD.system_role
          OR NEW.workspace_root <> OLD.workspace_root
        BEGIN
            SELECT RAISE(ABORT, 'thread role/workspace is immutable');
        END;
        "#,
    )?;
    Ok(())
}

/// Migration 33: the workspace-effect journal outlives its thread.
///
/// `workspace_effects` is append-only evidence (`INV-EFFECT-01`): a thread
/// delete must not cascade into it, and the no-delete trigger correctly refuses
/// that cascade. The original foreign key was nevertheless declared
/// `ON DELETE CASCADE`, so deleting any thread that had journaled a file
/// mutation failed with "workspace journal rows are append-only". Rebuild the
/// table without the reference; the journal keeps its `thread_id` as evidence,
/// while `/diff` and `/undo` are simply unreachable once the thread itself is
/// gone. Pre-image bytes are expired separately when the owner is deleted.
pub fn apply_workspace_effect_detach_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "workspace_effects")? {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        CREATE TABLE workspace_effects_new (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            -- The owning thread's id is retained as evidence, but deliberately
            -- has no foreign key: the journal must survive the thread it
            -- describes (INV-EFFECT-01).
            thread_id       TEXT NOT NULL,
            round_id        TEXT NOT NULL,
            turn_id         TEXT NOT NULL,
            call_id         TEXT NOT NULL,
            seq             INTEGER NOT NULL,
            tool_name       TEXT NOT NULL,
            path            TEXT,
            op              TEXT NOT NULL
                            CHECK (op IN ('create', 'edit', 'delete')),
            effect_class    TEXT NOT NULL
                            CHECK (effect_class IN ('none','file_bytes','external_effect',
                                                    'process_effect','opaque')),
            before_hash     TEXT,
            after_hash      TEXT,
            irreversible_reason TEXT,
            completed_at_ms INTEGER,
            reversed_at_ms  INTEGER,
            reversal_note   TEXT,
            expired_at_ms   INTEGER,
            created_at_ms   INTEGER NOT NULL
        );

        INSERT INTO workspace_effects_new (
            id, thread_id, round_id, turn_id, call_id, seq, tool_name, path, op,
            effect_class, before_hash, after_hash, irreversible_reason,
            completed_at_ms, reversed_at_ms, reversal_note, expired_at_ms,
            created_at_ms
        )
        SELECT
            id, thread_id, round_id, turn_id, call_id, seq, tool_name, path, op,
            effect_class, before_hash, after_hash, irreversible_reason,
            completed_at_ms, reversed_at_ms, reversal_note, expired_at_ms,
            created_at_ms
        FROM workspace_effects;

        DROP TABLE workspace_effects;
        ALTER TABLE workspace_effects_new RENAME TO workspace_effects;

        CREATE INDEX IF NOT EXISTS idx_workspace_effects_thread_seq
            ON workspace_effects(thread_id, seq);

        CREATE INDEX IF NOT EXISTS idx_workspace_effects_undoable
            ON workspace_effects(thread_id, id DESC)
            WHERE effect_class = 'file_bytes' AND reversed_at_ms IS NULL;

        CREATE INDEX IF NOT EXISTS idx_workspace_effects_path
            ON workspace_effects(thread_id, path);

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_immutable
        BEFORE UPDATE ON workspace_effects
        WHEN NEW.id <> OLD.id
          OR NEW.thread_id <> OLD.thread_id
          OR NEW.round_id <> OLD.round_id
          OR NEW.turn_id <> OLD.turn_id
          OR NEW.call_id <> OLD.call_id
          OR NEW.seq <> OLD.seq
          OR NEW.tool_name <> OLD.tool_name
          OR NEW.path IS NOT OLD.path
          OR NEW.op <> OLD.op
          OR NEW.effect_class <> OLD.effect_class
          OR NEW.before_hash IS NOT OLD.before_hash
          OR NEW.irreversible_reason IS NOT OLD.irreversible_reason
          OR NEW.created_at_ms <> OLD.created_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal observations are write-once (INV-EFFECT-01)');
        END;

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_completion_monotonic
        BEFORE UPDATE OF completed_at_ms ON workspace_effects
        WHEN OLD.completed_at_ms IS NOT NULL
          AND NEW.completed_at_ms IS NOT OLD.completed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal completion is one-shot (INV-EFFECT-03)');
        END;

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_after_hash_monotonic
        BEFORE UPDATE OF after_hash ON workspace_effects
        WHEN OLD.after_hash IS NOT NULL
          AND NEW.after_hash IS NOT OLD.after_hash
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal post-image is write-once (INV-EFFECT-03)');
        END;

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_reversal_monotonic
        BEFORE UPDATE OF reversed_at_ms ON workspace_effects
        WHEN OLD.reversed_at_ms IS NOT NULL
          AND NEW.reversed_at_ms IS NOT OLD.reversed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal reversal is one-shot (INV-EFFECT-02)');
        END;

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_expiry_monotonic
        BEFORE UPDATE OF expired_at_ms ON workspace_effects
        WHEN OLD.expired_at_ms IS NOT NULL
          AND NEW.expired_at_ms IS NOT OLD.expired_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal expiry is one-way (INV-EFFECT-05)');
        END;

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_no_early_reversal
        BEFORE UPDATE OF reversed_at_ms ON workspace_effects
        WHEN NEW.reversed_at_ms IS NOT NULL
          AND NEW.completed_at_ms IS NULL
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal refuses to reverse an unconfirmed effect (INV-EFFECT-04)');
        END;

        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_no_delete
        BEFORE DELETE ON workspace_effects
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal rows are append-only; prune by retention policy, not delete (INV-EFFECT-01)');
        END;
        "#,
    )?;
    Ok(())
}

/// Migration 34: Thread/Branch/Fact runtime substrate (RFC-0003 / ADR-0173).
///
/// The durable `Thread` aggregate needs three pieces of storage the legacy
/// transcript/IR schema did not have:
///
/// - a monotonic thread revision for optimistic concurrency and idempotent
///   replay;
/// - branch cursors (`head`, `forked_from`, `compaction_horizon`, policy
///   overrides) independent of the legacy `active_leaf`/`timelines` fields;
/// - per-fact execution context (`round`, `turn`) and a dedicated thread-policy
///   JSON column, so neither is squeezed into transcript-shaped columns.
pub fn apply_thread_branch_and_runtime_schema(tx: &rusqlite::Transaction) -> Result<()> {
    // Some legacy fixtures already carry one or more of these columns; the
    // migration must converge them without failing on a duplicate. The schema
    // version is still the authority, but the DDL is idempotent.
    if !table_columns(tx, "threads")?.contains("revision") {
        tx.execute_batch("ALTER TABLE threads ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;")?;
    }
    if !table_columns(tx, "thread_policies")?.contains("thread_policy_json") {
        tx.execute_batch(
            "ALTER TABLE thread_policies ADD COLUMN thread_policy_json TEXT NOT NULL DEFAULT '{}';",
        )?;
    }
    if !table_columns(tx, "causal_nodes")?.contains("context_json") {
        tx.execute_batch(
            "ALTER TABLE causal_nodes ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}';",
        )?;
    }

    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS thread_branches (
            thread_id                    TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            branch_id                    TEXT NOT NULL,
            name                         TEXT NOT NULL,
            head_fact_id                 TEXT,
            forked_from_fact_id          TEXT,
            compaction_horizon_fact_id   TEXT,
            policy_json                  TEXT NOT NULL DEFAULT '{}',
            created_at_ms                INTEGER NOT NULL,
            updated_at_ms                INTEGER NOT NULL,
            PRIMARY KEY (thread_id, branch_id)
        );

        CREATE INDEX IF NOT EXISTS idx_thread_branches_thread
            ON thread_branches(thread_id);
        "#,
    )?;

    // Backfill the main branch for every existing thread. The legacy
    // `active_leaf` is the only durable cursor the old schema had.
    tx.execute(
        r#"
        INSERT OR IGNORE INTO thread_branches (
            thread_id, branch_id, name, head_fact_id, forked_from_fact_id,
            compaction_horizon_fact_id, policy_json, created_at_ms, updated_at_ms
        )
        SELECT
            id, 'main', 'main', active_leaf, NULL, NULL, '{}', created_at_ms, updated_at_ms
        FROM threads
        "#,
        [],
    )?;

    Ok(())
}

/// Migration 35: durable thread commands, writer leases, and blob references
/// (RFC-0003 §6.18–6.19).
///
/// The prompt model needs four things the pre-RFC schema did not carry:
///
/// - an idempotency receipt per `(thread, command_id)`, so a retried command
///   returns the original ack instead of applying twice;
/// - the single-writer fencing lease per thread;
/// - a content-addressed blob table and a reference ledger that GC can sweep;
/// - thread-level schema/spawn lineage columns on the aggregate row.
pub fn apply_thread_commands_leases_and_blobs_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_columns(tx, "threads")?.contains("spawned_from_fact_id") {
        tx.execute_batch("ALTER TABLE threads ADD COLUMN spawned_from_fact_id TEXT;")?;
    }

    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS thread_commands (
            thread_id      TEXT NOT NULL,
            command_id     TEXT NOT NULL,
            ack_json       TEXT NOT NULL,
            created_at_ms  INTEGER NOT NULL,
            PRIMARY KEY (thread_id, command_id)
        );

        CREATE TABLE IF NOT EXISTS thread_leases (
            thread_id       TEXT PRIMARY KEY,
            owner_id        TEXT NOT NULL,
            fencing_token   INTEGER NOT NULL,
            expires_at_ms   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS thread_lease_counters (
            thread_id       TEXT PRIMARY KEY,
            last_token      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS thread_blobs (
            hash            TEXT PRIMARY KEY,
            size            INTEGER NOT NULL,
            mime            TEXT NOT NULL,
            data            BLOB NOT NULL,
            preview         TEXT,
            trust           TEXT NOT NULL DEFAULT 'trusted',
            created_at_ms   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS thread_blob_refs (
            thread_id       TEXT NOT NULL,
            fact_id         TEXT NOT NULL,
            hash            TEXT NOT NULL,
            size            INTEGER NOT NULL,
            mime            TEXT NOT NULL,
            preview         TEXT,
            trust           TEXT NOT NULL DEFAULT 'trusted',
            PRIMARY KEY (thread_id, fact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_thread_blob_refs_hash
            ON thread_blob_refs(hash);
        "#,
    )?;
    Ok(())
}

/// Migration 36: facts are the conversation SSOT (RFC-0003 / ADR-0173).
///
/// Converts every existing transcript into `ThreadFact` rows, rebuilds the
/// branch cursors, seeds the derived FTS index from those facts, and then drops
/// the transcript tables. The old `causal_nodes` payloads were a different
/// `NodePayload` shape and 0-based; the conversion rewrites them through the
/// same projection the live store uses, so a pre-RFC database opens with the
/// same conversation it had.
pub fn apply_thread_facts_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_thread_facts USING fts5(
            fact_id UNINDEXED,
            thread_id UNINDEXED,
            role UNINDEXED,
            content,
            tokenize = 'porter unicode61'
        );
        "#,
    )?;

    if table_exists(tx, "entries")? {
        let thread_ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM threads ORDER BY id ASC")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<_>>()?
        };
        for thread_id in thread_ids {
            let Some(data) = crate::db::thread_session::load_session_full(tx, &thread_id)? else {
                continue;
            };
            let thread = crate::thread::thread_from_session_data(&data);
            crate::db::thread_store::persist_thread_facts(tx, &thread, true).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "thread fact migration failed for {thread_id}: {error}"
                ))
            })?;
        }
    }

    tx.execute_batch(
        r#"
        DROP TABLE IF EXISTS entry_memberships;
        DROP TABLE IF EXISTS entries;
        DROP TABLE IF EXISTS projections;
        DROP TABLE IF EXISTS blob_refs;
        DROP TABLE IF EXISTS fts_entries;
        "#,
    )?;
    Ok(())
}

/// Migration 37: working-state mirror for the thread.
///
/// Todos are client/runtime working state, not conversation facts. They live
/// beside the thread policy so they survive a reload without becoming a
/// transcript row or a fact.
pub fn apply_thread_working_state_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_columns(tx, "thread_policies")?.contains("todos_json") {
        tx.execute_batch(
            "ALTER TABLE thread_policies ADD COLUMN todos_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    Ok(())
}

/// Migration 38: thread nomenclature and dead-table cleanup.
///
/// The pre-RFC schema named the thread origin `fork_kind`, carried a second
/// conversation tree (`threads.tree`, `fork_round`, `fork_turn`), and kept a
/// derived-index queue whose consumer is gone. The live model has one origin
/// (`trunk` / `subagent`), one fact table (`causal_nodes`), and one revision
/// row per thread.
pub fn apply_thread_nomenclature_cleanup_schema(tx: &rusqlite::Transaction) -> Result<()> {
    // SQLite refuses to drop a column a view still references.
    tx.execute_batch("DROP VIEW IF EXISTS thread_list_view;")?;
    tx.execute_batch("DROP VIEW IF EXISTS session_list_view;")?;
    if table_exists(tx, "threads")? {
        let columns = table_columns(tx, "threads")?;
        if columns.contains("fork_kind") && !columns.contains("origin") {
            tx.execute_batch("ALTER TABLE threads RENAME COLUMN fork_kind TO origin;")?;
        }
        for column in ["fork_round", "fork_turn", "tree"] {
            if columns.contains(column) {
                tx.execute_batch(&format!("ALTER TABLE threads DROP COLUMN {column};"))?;
            }
        }
    }
    if table_exists(tx, "thread_policies")? {
        let columns = table_columns(tx, "thread_policies")?;
        for column in [
            "rules_json",
            "capabilities_json",
            "guardrails_json",
            "budget_json",
        ] {
            if columns.contains(column) {
                tx.execute_batch(&format!(
                    "ALTER TABLE thread_policies DROP COLUMN {column};"
                ))?;
            }
        }
    }
    if table_exists(tx, "session_revisions")? {
        tx.execute_batch("ALTER TABLE session_revisions RENAME TO thread_revisions;")?;
        if table_columns(tx, "thread_revisions")?.contains("session_id") {
            tx.execute_batch(
                "ALTER TABLE thread_revisions RENAME COLUMN session_id TO thread_id;",
            )?;
        }
    }
    tx.execute_batch(
        r#"
        DROP TABLE IF EXISTS derived_pending;
        DROP TABLE IF EXISTS derived_cursors;
        DROP TABLE IF EXISTS derived_meta;
        DROP TABLE IF EXISTS facts;
        DROP TABLE IF EXISTS branches;
        DROP TABLE IF EXISTS checkpoints;
        DROP TABLE IF EXISTS context_views;
        DROP TABLE IF EXISTS artifact_manifests;
        DROP TABLE IF EXISTS artifact_refs;
        DROP TABLE IF EXISTS request_manifests;
        DROP TABLE IF EXISTS commit_operations;
        DROP TABLE IF EXISTS read_leases;
        DROP TABLE IF EXISTS deletion_jobs;
        DROP TABLE IF EXISTS gc_continuations;
        DROP TABLE IF EXISTS tombstones;
        DROP TABLE IF EXISTS task_revisions;

        DROP VIEW IF EXISTS thread_list_view;
        CREATE VIEW thread_list_view AS
        SELECT
            t.id,
            t.parent_thread_id AS parent_id,
            t.origin,
            t.title,
            t.created_at_ms / 1000 AS created_at_s,
            t.updated_at_ms / 1000 AS updated_at_s,
            p.workspace_root,
            p.system_role AS persona,
            COALESCE(
                (SELECT COUNT(*) FROM causal_nodes cn
                  WHERE cn.session_id = t.id AND cn.kind IN ('dialogue','observation')),
                t.msg_count
            ) AS msg_count,
            COALESCE(
                (SELECT json_extract(cn.payload_json, '$.message.content')
                   FROM causal_nodes cn
                  WHERE cn.session_id = t.id
                    AND cn.kind = 'dialogue'
                    AND json_extract(cn.payload_json, '$.message.role') = 'user'
                  ORDER BY cn.seq DESC LIMIT 1),
                t.last_user_prompt
            ) AS last_user_prompt,
            t.digest,
            t.status,
            t.round_counter
        FROM threads t
        LEFT JOIN thread_policies p ON p.thread_id = t.id;
        "#,
    )?;
    Ok(())
}
