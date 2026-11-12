//! Schema definitions and data migrations for workspace effects, scrub, roles, and derived index (v26-v30).
use rusqlite::Result;

use super::runner::table_exists;

/// Workspace effect journal, applied by migration 26 (ADR-0125).
///
/// One row per tool call that declared *any* effect outside the conversation.
/// The row is the harness's evidence for two distinct questions:
///
/// 1. **What did this thread change?** — the `/diff` projection answers from
///    `path` + `before_hash` / `after_hash`, for every class.
/// 2. **Can it be undone?** — only `effect_class = 'file_bytes'` rows carry a
///    usable pre-image; everything else is recorded-but-irreversible, and
///    `/undo` refuses those rows with the reason the class carries rather than
///    silently skipping them.
///
/// ## The three-phase lifecycle, and why it is three phases
///
/// A file mutation is journaled **write-ahead**: the row is appended after the
/// pre-image is durably stored but *before* the mutation runs, then completed
/// once the call returns.
///
/// ```text
///   append   (before_hash set, completed_at_ms NULL)   ← pre-image durable
///      ↓
///   mutate   (the tool writes the file)
///      ↓
///   complete (after_hash set, completed_at_ms set)     ← frozen observation
///      ↓
///   reverse  (reversed_at_ms set)                      ← one-shot, terminal
/// ```
///
/// The order is load-bearing. Appending *after* the mutation would mean a crash
/// mid-write leaves changed bytes with no record at all — the exact failure the
/// journal exists to prevent. Appending before costs nothing: if the process
/// dies before the tool runs, the row is still *true* (the pre-image is what was
/// there), it simply never reached `complete`.
///
/// An incomplete row is therefore **not** evidence that the mutation happened.
/// It is evidence that the mutation *may* have happened, and reversal refuses it
/// rather than guessing: writing the pre-image back would be correct only if this
/// call was the thing that changed the file, and that is exactly what an
/// incomplete record cannot tell us.
///
/// ## Immutability
///
/// Enforced in SQL, not by convention. A journal a bug can rewrite is not
/// evidence, and `/undo` reads `before_hash` as its sole authority on what the
/// file contained. Exactly two column groups may transition, each once and
/// monotonically: the completion stamp (`after_hash`, `completed_at_ms`) and the
/// reversal stamp (`reversed_at_ms`, `reversal_note`). Nothing may ever be
/// cleared, so replaying `/undo` cannot write stale bytes back over newer
/// content.
pub fn apply_workspace_effect_journal_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "threads")? {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS workspace_effects (
            -- Monotonic journal identity. Not the tool call id: a call id is
            -- provider-scoped and repeats across retries, while a journal row
            -- is one observed effect of one execution.
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id       TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            -- Turn coordinates, matching `facts`. `round_id`/`turn_id` are
            -- text (the harness allows non-numeric turn numbering on resume);
            -- `seq` is the journal-local observation order, which is the only
            -- honest ordering when several calls in one turn touch the same
            -- path.
            round_id        TEXT NOT NULL,
            turn_id         TEXT NOT NULL,
            call_id         TEXT NOT NULL,
            seq             INTEGER NOT NULL,
            tool_name       TEXT NOT NULL,
            -- Absolute, lexically normalized path. NULL for classes that touch
            -- no single file (shell, network, process). Storing it absolute is
            -- deliberate: a relative path is only interpretable against the
            -- workspace root in force at write time, which a later `/undo` in a
            -- different cwd cannot reconstruct.
            path            TEXT,
            -- `create` | `edit` | `delete`. `create` pairs with a NULL
            -- before_hash, so reversal removes the file instead of writing
            -- empty content.
            op              TEXT NOT NULL
                            CHECK (op IN ('create', 'edit', 'delete')),
            effect_class    TEXT NOT NULL
                            CHECK (effect_class IN ('none','file_bytes','external_effect',
                                                    'process_effect','opaque')),
            -- CAS hashes of the pre-image and (post-completion) the post-image.
            -- For `create` there was no pre-image, so `before_hash` is NULL.
            before_hash     TEXT,
            after_hash      TEXT,
            -- Why the effect was not reversible, when it was not. Copied from
            -- the class at write time so a future class rename cannot rewrite
            -- history's explanation.
            irreversible_reason TEXT,
            -- When the call returned and the post-image was observed. NULL means
            -- "journaled but never confirmed": the mutation may or may not have
            -- run, so reversal refuses it instead of guessing.
            completed_at_ms INTEGER,
            -- One-shot reversal bookkeeping. `reversed_at_ms` non-NULL means the
            -- pre-image was written back; `reversal_note` records why the
            -- reversal was partial or refused at the moment it ran.
            reversed_at_ms  INTEGER,
            reversal_note   TEXT,
            -- Retention. Non-NULL means the pre-image has aged out of the
            -- retention window: the *record* of the change survives (it is
            -- evidence), but its bytes are no longer live for the collector, so
            -- `/undo` refuses it with a reason instead of finding a missing blob.
            -- One-way and monotonic, like every other transition on this table.
            expired_at_ms   INTEGER,
            created_at_ms   INTEGER NOT NULL
        );

        -- The `/diff` read: one thread's rows in observation order.
        CREATE INDEX IF NOT EXISTS idx_workspace_effects_thread_seq
            ON workspace_effects(thread_id, seq);

        -- The `/undo` read: the newest not-yet-reversed reversible rows of one
        -- thread, newest first. Partial so the index stays proportional to the
        -- undoable set rather than the whole journal.
        CREATE INDEX IF NOT EXISTS idx_workspace_effects_undoable
            ON workspace_effects(thread_id, id DESC)
            WHERE effect_class = 'file_bytes' AND reversed_at_ms IS NULL;

        -- Per-path history, for a future "what touched this file" inspection.
        CREATE INDEX IF NOT EXISTS idx_workspace_effects_path
            ON workspace_effects(thread_id, path);

        -- Immutability: the observation is write-once. Completion and reversal
        -- are separate triggers below, each with its own one-way rule.
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

        -- Completion is one-shot: a NULL stamp may be filled exactly once, and a
        -- filled stamp is frozen. This is what lets an incomplete row stay
        -- interpretable forever ("may or may not have run").
        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_completion_monotonic
        BEFORE UPDATE OF completed_at_ms ON workspace_effects
        WHEN OLD.completed_at_ms IS NOT NULL
          AND NEW.completed_at_ms IS NOT OLD.completed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal completion is one-shot (INV-EFFECT-03)');
        END;

        -- The post-image may only be filled at completion time, never rewritten.
        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_after_hash_monotonic
        BEFORE UPDATE OF after_hash ON workspace_effects
        WHEN OLD.after_hash IS NOT NULL
          AND NEW.after_hash IS NOT OLD.after_hash
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal post-image is write-once (INV-EFFECT-03)');
        END;

        -- Reversal is one-shot and monotonic: once stamped, it cannot be
        -- cleared or re-stamped, so replaying `/undo` cannot write stale bytes
        -- back over newer content.
        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_reversal_monotonic
        BEFORE UPDATE OF reversed_at_ms ON workspace_effects
        WHEN OLD.reversed_at_ms IS NOT NULL
          AND NEW.reversed_at_ms IS NOT OLD.reversed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal reversal is one-shot (INV-EFFECT-02)');
        END;

        -- Retention expiry is one-way: it may be set once and never cleared, so
        -- an expired pre-image cannot be resurrected by a later write that would
        -- reference bytes the collector has already reclaimed.
        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_expiry_monotonic
        BEFORE UPDATE OF expired_at_ms ON workspace_effects
        WHEN OLD.expired_at_ms IS NOT NULL
          AND NEW.expired_at_ms IS NOT OLD.expired_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal expiry is one-way (INV-EFFECT-05)');
        END;

        -- A row cannot be reversed before it is completed: an unconfirmed
        -- mutation is not something the journal may act on.
        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_no_early_reversal
        BEFORE UPDATE OF reversed_at_ms ON workspace_effects
        WHEN NEW.reversed_at_ms IS NOT NULL
          AND NEW.completed_at_ms IS NULL
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal refuses to reverse an unconfirmed effect (INV-EFFECT-04)');
        END;

        -- Rows are append-only evidence: no delete path, by construction.
        CREATE TRIGGER IF NOT EXISTS trg_workspace_effects_no_delete
        BEFORE DELETE ON workspace_effects
        BEGIN
            SELECT RAISE(ABORT, 'workspace journal rows are append-only; prune by retention policy, not delete (INV-EFFECT-01)');
        END;
        "#,
    )?;
    Ok(())
}

/// Schema for the online derived-index scrub (migration 29).
///
/// The table is deliberately tiny: a resumable scan cursor over session ids.
/// It is **not** a second index and carries no facts — deleting it only restarts
/// the scrub. The `active_leaf` reset is part of the same migration because a
/// main-line session's lineage position is the transcript's newest dialogue
/// fact, not a value the old IR-commit path persisted.
pub fn apply_derived_scrub_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS derived_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        INSERT OR IGNORE INTO derived_meta (key, value) VALUES ('scrub_cursor', '');
        "#,
    )?;
    if table_exists(tx, "threads")? {
        tx.execute_batch("UPDATE threads SET active_leaf = NULL WHERE fork_kind = 'trunk';")?;
    }
    Ok(())
}

/// Migration 30: role-scoped thread history.
///
/// `general` was the pre-role placeholder, never a user-selectable role. The
/// default role has always been `developer`, so legacy rows are folded into
/// it instead of forming a phantom role bucket. The covering index matches the
/// `(system_role, workspace_root)` predicate every scoped list/resume/purge
/// query now uses.
pub fn apply_role_scoped_thread_scope_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "thread_policies")? {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        UPDATE thread_policies
           SET system_role = 'developer'
         WHERE system_role IS NULL OR trim(system_role) = '' OR system_role = 'general';

        CREATE INDEX IF NOT EXISTS idx_thread_policies_role_scope
            ON thread_policies(system_role, workspace_root, updated_at_ms DESC);
        "#,
    )?;
    Ok(())
}

/// Migration 28: move both derived indexes off the commit path.
///
/// Dropping the membership triggers is the load-bearing part: from here on the
/// only writer of `fts_entries` is the derived-index worker, and the only
/// writer of `causal_nodes` is that worker plus an explicit IR fold. The
/// cursors seed from the state those paths left behind, so the switch neither
/// re-indexes the whole database nor loses rows.
pub fn apply_derived_index_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS trg_memberships_ai;
        DROP TRIGGER IF EXISTS trg_memberships_ad;
        DROP TRIGGER IF EXISTS trg_entries_ai;
        DROP TRIGGER IF EXISTS trg_entries_ad;

        CREATE TABLE IF NOT EXISTS derived_cursors (
            session_id TEXT PRIMARY KEY,
            causal_seq INTEGER NOT NULL DEFAULT -1,
            fts_seq    INTEGER NOT NULL DEFAULT -1
        );
        CREATE TABLE IF NOT EXISTS derived_pending (
            session_id     TEXT PRIMARY KEY,
            through_seq    INTEGER NOT NULL,
            enqueued_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_derived_pending_age
            ON derived_pending(enqueued_at_ms ASC);
        "#,
    )?;

    // Seed only sessions that do not have a cursor yet, so a re-run (or a
    // database that already drained) is never rolled back to the pre-worker
    // state. A synthetic or partially-migrated schema may not carry both
    // tables yet; the cursor then starts empty and the first drain on a real
    // session fills it.
    if !table_exists(tx, "threads")? || !table_exists(tx, "entry_memberships")? {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        INSERT INTO derived_cursors (session_id, causal_seq, fts_seq)
        SELECT t.id,
               COALESCE((SELECT MAX(c.seq) FROM causal_nodes c
                          WHERE c.session_id = t.id AND c.kind <> 'compaction'), -1),
               COALESCE((SELECT MAX(m.seq) FROM entry_memberships m
                          WHERE m.session_id = t.id), -1)
          FROM threads t
         WHERE true
        ON CONFLICT(session_id) DO NOTHING;
        "#,
    )?;
    Ok(())
}

/// Backfill `causal_nodes` from the transcript (migration 27).
///
/// `entry_memberships` already holds the conversation's identity, order and
/// parent-free sequence; the live mirror writes `dialogue` nodes keyed by the same
/// entry id at the same `seq`. `INSERT OR IGNORE` therefore completes a
/// partially-mirrored database and leaves a complete one untouched.
///
/// Parent edges are left NULL on purpose. The true causal parent of an entry is
/// the previous entry *within one lineage*, and a database holding forked branches
/// cannot answer that without risking a cross-lineage edge — a wrong edge is worse
/// than none, because `linear_path` would follow it. The live mirror repairs the
/// edge for every session that commits again (it writes `parent_id` on conflict: a
/// fact's payload is immutable, its lineage was simply never recorded). Until then
/// `newest_leaf` and `read_horizon` order by `seq`, so such a graph is usable
/// rather than silently empty.
///
/// `state` entries are projections with no node kind, so they are excluded —
/// exactly as the live mirror excludes them. A `checkpoint` entry becomes a
/// `compaction` node, matching the serializer.
pub fn apply_causal_fact_backfill_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "entry_memberships")? || !table_exists(tx, "entries")? {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        INSERT OR IGNORE INTO causal_nodes (
            id, session_id, parent_id, seq, kind, payload_json, timestamp_ms
        )
        SELECT
            e.id,
            m.session_id,
            NULL,
            m.seq,
            CASE WHEN e.origin = 'checkpoint' THEN 'compaction' ELSE 'dialogue' END,
            -- `NodePayload` is internally tagged (`#[serde(tag = "type",
            -- rename_all = "snake_case")]`), so the durable shape is
            -- `{"type":"message","message":{...}}`. A node in any other shape is
            -- not merely unreadable: `load_session_ir` fails the whole hydrate on
            -- it, which is the failure mode this backfill exists to end.
            --
            -- `Role` carries NO `rename_all`, so its wire form is capitalized
            -- (`User` / `Assistant` / `System` / `Tool`). Lowercase here would
            -- produce exactly the undecodable node described above.
            json_object(
                'type', 'message',
                'message', json_object(
                    'role', CASE lower(COALESCE(e.role, 'user'))
                                WHEN 'user' THEN 'User'
                                WHEN 'assistant' THEN 'Assistant'
                                WHEN 'system' THEN 'System'
                                WHEN 'tool' THEN 'Tool'
                                ELSE 'User'
                            END,
                    'content', COALESCE(e.content, '')
                )
            ),
            COALESCE(e.created_at_ms, 0)
        FROM entry_memberships m
        JOIN entries e ON e.id = m.entry_id
        WHERE e.kind = 'message'
          AND NOT EXISTS (
              SELECT 1 FROM causal_nodes c
               WHERE c.session_id = m.session_id AND c.id = e.id
          );
        "#,
    )?;
    Ok(())
}
