//! Migrations 31+.
use rusqlite::Result;

use super::runner::table_exists;

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
