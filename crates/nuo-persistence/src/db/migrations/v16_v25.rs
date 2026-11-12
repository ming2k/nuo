//! Schema definitions and data migrations for context lifecycle and thread clean break (v21-v25).
use rusqlite::Result;

use super::runner::{table_columns, table_exists};

/// Canonical context-lifecycle substrate,
/// applied by migration 22. Creates the immutable fact tables, branch cursors,
/// derived views/checkpoints, artifact manifests and references, request
/// manifests, tombstones, and the idempotency ledger.
///
/// The `facts` triggers enforce `INV-FACT-02` at the SQL layer: a payload or
/// ancestry may never be updated, and a fact may never be deleted. The one
/// sanctioned payload replacement is a purge, which flips `deletion` in the
/// same statement (the tombstone path). DDL is idempotent so a
/// database that already carries the tables passes through unchanged.
pub fn apply_context_lifecycle_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        -- Immutable execution facts (ADR-0275 §2). Payload and ancestry are
        -- write-once; `deletion` is the only mutable column.
        CREATE TABLE IF NOT EXISTS facts (
            session_id       TEXT NOT NULL,
            id               TEXT NOT NULL,
            branch_origin    TEXT NOT NULL,
            seq              INTEGER NOT NULL,
            round_id         TEXT NOT NULL,
            turn_id          TEXT NOT NULL,
            parent_ids       TEXT NOT NULL DEFAULT '[]',
            payload_json     TEXT NOT NULL,
            payload_hash     TEXT NOT NULL DEFAULT '',
            source_authority TEXT NOT NULL,
            sensitivity      TEXT NOT NULL,
            artifact_refs    TEXT NOT NULL DEFAULT '[]',
            deletion         TEXT NOT NULL DEFAULT 'present'
                             CHECK (deletion IN ('present','delete_pending','purged')),
            created_at_ms    INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (session_id, id),
            UNIQUE (session_id, seq)
        );
        "#,
    )?;

    let fact_col =
        if table_exists(tx, "facts")? && table_columns(tx, "facts")?.contains("thread_id") {
            "thread_id"
        } else {
            "session_id"
        };

    let idx_sql = if fact_col == "thread_id" {
        "CREATE INDEX IF NOT EXISTS idx_facts_thread_seq ON facts(thread_id, seq ASC);"
    } else {
        "CREATE INDEX IF NOT EXISTS idx_facts_session_seq ON facts(session_id, seq ASC);"
    };
    tx.execute_batch(idx_sql)?;

    let trg_sql = format!(
        r#"
        DROP TRIGGER IF EXISTS trg_facts_immutable_payload;
        CREATE TRIGGER trg_facts_immutable_payload
        BEFORE UPDATE ON facts
        WHEN NEW.id <> OLD.id
          OR NEW.{fact_col} <> OLD.{fact_col}
          OR NEW.seq <> OLD.seq
          OR NEW.branch_origin <> OLD.branch_origin
          OR NEW.parent_ids <> OLD.parent_ids
          OR NEW.round_id <> OLD.round_id
          OR NEW.turn_id <> OLD.turn_id
          OR NEW.source_authority <> OLD.source_authority
          OR NEW.sensitivity <> OLD.sensitivity
          OR NEW.created_at_ms <> OLD.created_at_ms
          OR NEW.payload_hash <> OLD.payload_hash
          OR ((NEW.payload_json <> OLD.payload_json OR NEW.artifact_refs <> OLD.artifact_refs)
              AND NOT (OLD.deletion <> 'purged' AND NEW.deletion = 'purged'
                  AND NEW.payload_json = '{{"termination":{{"reason":"purged"}}}}'
                  AND NEW.artifact_refs = '[]'))
        BEGIN
            SELECT RAISE(ABORT, 'facts are immutable: only payload erasure during purge is allowed');
        END;
        "#
    );
    tx.execute_batch(&trg_sql)?;

    tx.execute_batch(
        r#"
        CREATE TRIGGER IF NOT EXISTS trg_facts_deletion_monotonic
        BEFORE UPDATE OF deletion ON facts
        WHEN NOT (
            (OLD.deletion = 'present' AND NEW.deletion IN ('present','delete_pending','purged'))
            OR (OLD.deletion = 'delete_pending' AND NEW.deletion IN ('delete_pending','purged'))
            OR (OLD.deletion = 'purged' AND NEW.deletion = 'purged')
        )
        BEGIN
            SELECT RAISE(ABORT, 'facts are immutable: deletion state must not revert (INV-FACT-02)');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_facts_no_delete
        BEFORE DELETE ON facts
        BEGIN
            SELECT RAISE(ABORT, 'facts are immutable: use a sanctioned purge, never a delete (INV-FACT-02)');
        END;

        -- Minimal deletion tombstones (ADR-0279 §6): structure without payload.
        CREATE TABLE IF NOT EXISTS tombstones (
            session_id  TEXT NOT NULL,
            fact_id     TEXT NOT NULL,
            parent_ids  TEXT NOT NULL DEFAULT '[]',
            purged_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, fact_id)
        );

        -- Branch cursor and view pointer (ADR-0275 §2). `revision` is the
        -- compare-and-swap token for every commit.
        CREATE TABLE IF NOT EXISTS branches (
            branch_id        TEXT NOT NULL,
            session_id       TEXT NOT NULL,
            head_fact_id     TEXT,
            revision         INTEGER NOT NULL DEFAULT 0,
            active_task_id   TEXT,
            active_view_id   TEXT,
            execution_cursor TEXT,
            PRIMARY KEY (session_id, branch_id)
        );
        CREATE INDEX IF NOT EXISTS idx_branches_session ON branches(session_id);

        -- Cross-round task state (ADR-0275 §1). A requirement fact is a GC
        -- root while the task is open (ADR-0279 §6).
        CREATE TABLE IF NOT EXISTS task_revisions (
            session_id      TEXT NOT NULL,
            task_id         TEXT NOT NULL,
            revision        INTEGER NOT NULL,
            objective_fact_id TEXT NOT NULL,
            requirements    TEXT NOT NULL DEFAULT '[]',
            open_items      TEXT NOT NULL DEFAULT '[]',
            open            INTEGER NOT NULL DEFAULT 1,
            created_at_ms   INTEGER NOT NULL,
            PRIMARY KEY (session_id, task_id, revision)
        );

        -- Derived context views (ADR-0275 §2, ADR-0278). A view writes no facts.
        CREATE TABLE IF NOT EXISTS context_views (
            session_id        TEXT NOT NULL,
            view_id           TEXT NOT NULL,
            branch_id         TEXT NOT NULL,
            basis_revision    INTEGER NOT NULL,
            checkpoint_id     TEXT,
            tail_after_fact_id TEXT,
            representations   TEXT NOT NULL DEFAULT '[]',
            policy_revision   INTEGER NOT NULL,
            created_at_ms     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (session_id, view_id)
        );
        CREATE INDEX IF NOT EXISTS idx_context_views_branch ON context_views(branch_id);

        -- Derived checkpoints (ADR-0278 §1). Reference a source interval; no
        -- parent edge, never an execution node.
        CREATE TABLE IF NOT EXISTS checkpoints (
            session_id          TEXT NOT NULL,
            checkpoint_id       TEXT NOT NULL,
            branch_id           TEXT NOT NULL,
            prior_checkpoint_id TEXT,
            source_manifest     TEXT NOT NULL,
            summary             TEXT NOT NULL,
            mandatory_fact_refs TEXT NOT NULL DEFAULT '[]',
            source_authority    TEXT NOT NULL,
            created_at_ms       INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (session_id, checkpoint_id)
        );
        CREATE INDEX IF NOT EXISTS idx_checkpoints_branch ON checkpoints(branch_id);

        -- Published raw artifact manifests (ADR-0276). Written only after the
        -- artifact is durably published (INV-CAP-01).
        CREATE TABLE IF NOT EXISTS artifact_manifests (
            session_id   TEXT NOT NULL,
            artifact_id  TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            media_type   TEXT NOT NULL,
            stream       TEXT NOT NULL,
            capture      TEXT NOT NULL,
            byte_count   INTEGER NOT NULL,
            retention    TEXT NOT NULL,
            sensitivity  TEXT NOT NULL,
            deletion     TEXT NOT NULL DEFAULT 'present'
                         CHECK (deletion IN ('present','delete_pending','purged')),
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, artifact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_artifact_manifests_hash ON artifact_manifests(content_hash);

        -- Durable artifact references: a blob is live iff a reference exists
        -- (ADR-0187 ledger extended to the fact substrate). GC roots.
        CREATE TABLE IF NOT EXISTS artifact_refs (
            session_id  TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            fact_id     TEXT NOT NULL,
            PRIMARY KEY (session_id, artifact_id, fact_id)
        );

        -- Immutable request manifests (ADR-0277). No authentication headers.
        CREATE TABLE IF NOT EXISTS request_manifests (
            session_id      TEXT NOT NULL,
            request_id      TEXT NOT NULL,
            branch_id       TEXT NOT NULL,
            basis_revision  INTEGER NOT NULL,
            policy_revision INTEGER NOT NULL,
            blocks          TEXT NOT NULL DEFAULT '[]',
            input_ceiling   INTEGER NOT NULL,
            created_at_ms   INTEGER NOT NULL,
            PRIMARY KEY (session_id, request_id)
        );

        -- Idempotency ledger: an operation ID is applied at most once, so a
        -- retry after a crash replays instead of double-applying (ADR-0275 §7).
        CREATE TABLE IF NOT EXISTS commit_operations (
            session_id       TEXT NOT NULL,
            operation_id     TEXT NOT NULL,
            kind             TEXT NOT NULL,
            applied_revision INTEGER NOT NULL,
            at_ms            INTEGER NOT NULL,
            PRIMARY KEY (session_id, operation_id)
        );
        "#,
    )?;
    Ok(())
}

/// Retrieval leases, deletion jobs, and GC bookkeeping,
/// applied by migration 23.
///
/// A read lease protects an artifact from concurrent collection; a deletion job
/// makes a purge observable and resumable across a crash (commit state first,
/// then reclaim). DDL is idempotent.
pub fn apply_retrieval_and_gc_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        -- Read leases: while a lease is live, GC must not collect the artifact
        -- (INV-RET-04). Expired leases are reclaimable.
        CREATE TABLE IF NOT EXISTS read_leases (
            session_id  TEXT NOT NULL,
            lease_id    TEXT NOT NULL,
            artifact_id TEXT,
            fact_id     TEXT,
            expires_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, lease_id)
        );
        CREATE INDEX IF NOT EXISTS idx_read_leases_artifact
            ON read_leases(session_id, artifact_id);

        -- Deletion jobs: state is committed first, reclamation follows; a crash
        -- resumes the same job (INV-RET-03).
        CREATE TABLE IF NOT EXISTS deletion_jobs (
            session_id     TEXT NOT NULL,
            job_id         TEXT NOT NULL,
            target_kind    TEXT NOT NULL CHECK (target_kind IN ('fact','artifact')),
            target_id      TEXT NOT NULL,
            scope          TEXT NOT NULL,
            state          TEXT NOT NULL CHECK (state IN ('pending','reclaiming','done')),
            created_at_ms  INTEGER NOT NULL,
            updated_at_ms  INTEGER NOT NULL,
            PRIMARY KEY (session_id, job_id)
        );
        CREATE INDEX IF NOT EXISTS idx_deletion_jobs_state ON deletion_jobs(state);

        -- GC continuation cursor: a bounded batch saves where it stopped.
        CREATE TABLE IF NOT EXISTS gc_continuations (
            session_id     TEXT PRIMARY KEY,
            last_seq       INTEGER NOT NULL DEFAULT 0,
            updated_at_ms  INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )?;
    Ok(())
}

/// Canonical Thread persistence schema (ADR-0059), applied by migration 24.
///
/// Establishes `threads` as the root entity, `thread_policies` with normalized
/// first-class columns (`workspace_root`, `system_role`), and `thread_list_view`
/// as the authoritative CQRS projection, eliminating legacy junk drawers.
pub fn apply_thread_canonical_clean_break_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        r#"
        -- 0. Drop views first so altering/dropping underlying tables never marks views invalid
        DROP VIEW IF EXISTS session_list_view;
        DROP VIEW IF EXISTS thread_list_view;

        -- 1. Canonical threads root table (ADR-0059)
        CREATE TABLE IF NOT EXISTS threads (
            id                    TEXT PRIMARY KEY,
            title                 TEXT NOT NULL DEFAULT '',
            title_manual          INTEGER NOT NULL DEFAULT 0 CHECK(title_manual IN (0, 1)),
            status                TEXT NOT NULL DEFAULT 'idle'
                                  CHECK(status IN ('idle', 'running', 'suspended', 'archived', 'terminated')),
            fork_kind             TEXT NOT NULL DEFAULT 'trunk'
                                  CHECK(fork_kind IN ('trunk', 'fork', 'subagent')),
            parent_thread_id      TEXT REFERENCES threads(id) ON DELETE SET NULL,
            fork_fact_id          TEXT,
            active_branch_id      TEXT NOT NULL DEFAULT 'main',
            active_leaf           TEXT,
            suspension_payload    TEXT,
            pending_notifications TEXT NOT NULL DEFAULT '[]',
            round_counter         INTEGER NOT NULL DEFAULT 0,
            msg_count             INTEGER NOT NULL DEFAULT 0,
            last_user_prompt      TEXT,
            tree                  TEXT NOT NULL DEFAULT '{}',
            transcript_generation TEXT,
            digest                TEXT,
            digest_anchor         INTEGER,
            checksum              INTEGER,
            created_at_ms         INTEGER NOT NULL,
            updated_at_ms         INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_threads_parent ON threads(parent_thread_id);
        CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at_ms DESC);

        -- 2. Normalized thread_policies table (ADR-0059)
        CREATE TABLE IF NOT EXISTS thread_policies (
            thread_id                  TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
            workspace_root             TEXT NOT NULL,
            system_role                TEXT NOT NULL DEFAULT 'general',
            unattended                 INTEGER NOT NULL DEFAULT 0 CHECK(unattended IN (0, 1)),
            provider_pin               TEXT,
            max_context_tokens         INTEGER NOT NULL DEFAULT 128000,
            compaction_trigger_tokens  INTEGER NOT NULL DEFAULT 96000,
            disabled_tools_json        TEXT NOT NULL DEFAULT '[]',
            additional_roots_json      TEXT NOT NULL DEFAULT '[]',
            commands_json              TEXT NOT NULL DEFAULT '[]',
            round_interrupts_json      TEXT NOT NULL DEFAULT '[]',
            retry_resolutions_json     TEXT NOT NULL DEFAULT '[]',
            retry_pending              TEXT,
            role_manifest              TEXT,
            rules_json                 TEXT NOT NULL DEFAULT '{}',
            capabilities_json          TEXT NOT NULL DEFAULT '{}',
            guardrails_json            TEXT NOT NULL DEFAULT '{}',
            budget_json                TEXT NOT NULL DEFAULT '{}',
            updated_at_ms              INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_thread_policies_workspace ON thread_policies(workspace_root);
        "#,
    )?;

    if table_exists(tx, "sessions_v2")? {
        let title_subquery = if table_exists(tx, "sessions")? {
            "COALESCE((SELECT s.title FROM sessions s WHERE s.id = s2.id), '')"
        } else {
            "''"
        };
        tx.execute_batch(&format!(
            r#"
            INSERT OR IGNORE INTO threads (
                id, title, title_manual, status, fork_kind, parent_thread_id,
                active_branch_id, active_leaf, suspension_payload, pending_notifications,
                round_counter, created_at_ms, updated_at_ms
            )
            SELECT
                s2.id,
                {title_subquery},
                0,
                CASE WHEN s2.status IN ('idle', 'running', 'suspended', 'archived', 'terminated') THEN s2.status ELSE 'idle' END,
                'trunk',
                s2.parent_session_id,
                'main',
                s2.active_leaf,
                s2.suspension_payload,
                s2.pending_notifications,
                s2.round_counter,
                s2.created_at_s * 1000,
                s2.updated_at_s * 1000
            FROM sessions_v2 s2;
            "#
        ))?;
    }

    if table_exists(tx, "sessions")? {
        tx.execute_batch(
            r#"
            INSERT OR IGNORE INTO threads (
                id, title, title_manual, status, fork_kind, parent_thread_id,
                active_branch_id, round_counter, msg_count, last_user_prompt,
                tree, transcript_generation, digest, digest_anchor, checksum,
                created_at_ms, updated_at_ms
            )
            SELECT
                id,
                COALESCE(title, ''),
                0,
                'idle',
                CASE WHEN fork_kind IN ('trunk', 'fork', 'subagent') THEN fork_kind ELSE 'trunk' END,
                parent_id,
                'main',
                round_counter,
                msg_count,
                last_user_prompt,
                tree,
                transcript_generation,
                digest,
                digest_anchor,
                checksum,
                created_at_s * 1000,
                updated_at_s * 1000
            FROM sessions;
            "#,
        )?;
    }

    if table_exists(tx, "session_policies")? {
        tx.execute_batch(
            r#"
            INSERT OR IGNORE INTO thread_policies (
                thread_id, workspace_root, system_role, unattended, provider_pin,
                max_context_tokens, compaction_trigger_tokens, disabled_tools_json,
                additional_roots_json, commands_json, round_interrupts_json,
                retry_resolutions_json, retry_pending, role_manifest, rules_json,
                capabilities_json, guardrails_json, budget_json, updated_at_ms
            )
            SELECT
                session_id,
                COALESCE(json_extract(rules_json, '$.workspace_root'), ''),
                COALESCE(json_extract(rules_json, '$.system_persona'), 'general'),
                CASE WHEN json_extract(guardrails_json, '$.unattended') = 1 THEN 1 ELSE 0 END,
                json_extract(capabilities_json, '$.provider_pin'),
                COALESCE(json_extract(budget_json, '$.max_context_tokens'), 128000),
                COALESCE(json_extract(budget_json, '$.compaction_trigger_tokens'), 96000),
                COALESCE(json_extract(capabilities_json, '$.disabled_tools'), '[]'),
                '[]',
                '[]',
                '[]',
                '[]',
                NULL,
                NULL,
                rules_json,
                capabilities_json,
                guardrails_json,
                budget_json,
                updated_at_s * 1000
            FROM session_policies;
            "#,
        )?;
    }

    if table_exists(tx, "sessions")? {
        tx.execute_batch(
            r#"
            INSERT OR IGNORE INTO thread_policies (
                thread_id, workspace_root, system_role, unattended, provider_pin,
                max_context_tokens, compaction_trigger_tokens, disabled_tools_json,
                additional_roots_json, commands_json, round_interrupts_json,
                retry_resolutions_json, retry_pending, role_manifest, updated_at_ms
            )
            SELECT
                id,
                COALESCE(workspace_root, ''),
                COALESCE(persona, 'general'),
                CASE WHEN unattended != 0 THEN 1 ELSE 0 END,
                provider_connection,
                128000,
                96000,
                COALESCE(disabled_tools, '[]'),
                '[]',
                commands,
                round_interrupts,
                retry_resolutions,
                retry_pending,
                role_manifest,
                updated_at_s * 1000
            FROM sessions;
            "#,
        )?;
    }

    // 4. Rebuild legacy parent references to point to threads instead of sessions
    if table_exists(tx, "entry_memberships")? {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS entry_memberships_clean (
                session_id            TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                seq                   INTEGER NOT NULL,
                entry_id              TEXT NOT NULL REFERENCES entries(id),
                PRIMARY KEY (session_id, seq)
            );
            INSERT OR IGNORE INTO entry_memberships_clean SELECT * FROM entry_memberships;
            DROP TABLE entry_memberships;
            ALTER TABLE entry_memberships_clean RENAME TO entry_memberships;
            CREATE INDEX IF NOT EXISTS idx_memberships_entry ON entry_memberships(entry_id);

            CREATE TRIGGER IF NOT EXISTS trg_entries_membership_ai AFTER INSERT ON entry_memberships BEGIN
                INSERT INTO fts_entries(rowid, entry_id, session_id, role, content)
                SELECT new.rowid, new.entry_id, new.session_id, e.role, e.content
                FROM entries e WHERE e.id = new.entry_id;
            END;

            CREATE TRIGGER IF NOT EXISTS trg_entries_membership_ad AFTER DELETE ON entry_memberships BEGIN
                DELETE FROM fts_entries WHERE rowid = old.rowid;
            END;
            "#,
        )?;
    }

    if table_exists(tx, "projections")? {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projections_clean (
                session_id            TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                seq                   INTEGER NOT NULL,
                kind                  TEXT NOT NULL,
                up_to_seq             INTEGER NOT NULL,
                payload               TEXT NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT OR IGNORE INTO projections_clean SELECT * FROM projections;
            DROP TABLE projections;
            ALTER TABLE projections_clean RENAME TO projections;
            "#,
        )?;
    }

    if table_exists(tx, "blob_refs")? {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS blob_refs_clean (
                session_id            TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                hash                  TEXT NOT NULL,
                PRIMARY KEY (session_id, hash)
            );
            INSERT OR IGNORE INTO blob_refs_clean SELECT session_id, hash FROM blob_refs;
            DROP TABLE blob_refs;
            ALTER TABLE blob_refs_clean RENAME TO blob_refs;
            "#,
        )?;
    }

    if table_exists(tx, "request_projections")? {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS request_projections_clean (
                session_id            TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                round                 INTEGER NOT NULL,
                turn                  INTEGER NOT NULL,
                created_at_ms         INTEGER NOT NULL,
                payload               TEXT NOT NULL,
                PRIMARY KEY (session_id, round, turn)
            );
            INSERT OR IGNORE INTO request_projections_clean SELECT * FROM request_projections;
            DROP TABLE request_projections;
            ALTER TABLE request_projections_clean RENAME TO request_projections;
            CREATE INDEX IF NOT EXISTS idx_request_projections_recency ON request_projections(session_id, created_at_ms DESC);
            "#,
        )?;
    }

    if table_exists(tx, "causal_nodes")? {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS causal_nodes_clean (
                id                    TEXT NOT NULL,
                session_id            TEXT NOT NULL,
                parent_id             TEXT,
                seq                   INTEGER NOT NULL,
                kind                  TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','system_notice','observation')),
                payload_json          TEXT NOT NULL,
                timestamp_ms          INTEGER NOT NULL,
                PRIMARY KEY (session_id, id),
                UNIQUE(session_id, seq)
            );

            INSERT OR IGNORE INTO causal_nodes_clean (id, session_id, parent_id, seq, kind, payload_json, timestamp_ms)
            SELECT id, session_id, parent_id, seq, kind, payload_json, timestamp_ms FROM causal_nodes;

            DROP TABLE causal_nodes;
            ALTER TABLE causal_nodes_clean RENAME TO causal_nodes;
            CREATE INDEX IF NOT EXISTS idx_causal_nodes_session_seq ON causal_nodes(session_id, seq ASC);
            CREATE INDEX IF NOT EXISTS idx_causal_nodes_parent ON causal_nodes(session_id, parent_id);
            "#,
        )?;
    }

    // 5. Physical purge of retired zombie tables and duplicate ledgers (ADR-0059)
    tx.execute_batch(
        r#"
        DROP VIEW IF EXISTS session_list_view;
        DROP TABLE IF EXISTS sessions;
        DROP TABLE IF EXISTS sessions_v2;
        DROP TABLE IF EXISTS session_policies;
        DROP TABLE IF EXISTS session_events;
        DROP TABLE IF EXISTS session_blobs;
        DROP TABLE IF EXISTS causal_nodes_v2;
        DROP TABLE IF EXISTS messages;
        DROP TABLE IF EXISTS commands;
        "#,
    )?;

    // 6. Uncompromising schema standardization: rename session_id -> thread_id
    for table in &[
        "facts",
        "branches",
        "checkpoints",
        "context_views",
        "artifact_manifests",
        "artifact_refs",
        "request_manifests",
        "commit_operations",
        "read_leases",
        "deletion_jobs",
        "gc_continuations",
        "tombstones",
        "task_revisions",
    ] {
        if table_exists(tx, table)? {
            let cols = table_columns(tx, table)?;
            if cols.contains("session_id") {
                tx.execute_batch(&format!(
                    "ALTER TABLE {table} RENAME COLUMN session_id TO thread_id;"
                ))?;
            }
        }
    }

    // 7. Rebuild fact immutability triggers aligning to thread_id
    if table_exists(tx, "facts")? {
        tx.execute_batch(
            r#"
            DROP TRIGGER IF EXISTS trg_facts_immutable_payload;
            CREATE TRIGGER trg_facts_immutable_payload
            BEFORE UPDATE ON facts
            WHEN NEW.id <> OLD.id
              OR NEW.thread_id <> OLD.thread_id
              OR NEW.seq <> OLD.seq
              OR NEW.branch_origin <> OLD.branch_origin
              OR NEW.parent_ids <> OLD.parent_ids
              OR NEW.round_id <> OLD.round_id
              OR NEW.turn_id <> OLD.turn_id
              OR NEW.source_authority <> OLD.source_authority
              OR NEW.sensitivity <> OLD.sensitivity
              OR NEW.created_at_ms <> OLD.created_at_ms
              OR NEW.payload_hash <> OLD.payload_hash
              OR ((NEW.payload_json <> OLD.payload_json OR NEW.artifact_refs <> OLD.artifact_refs)
                  AND NOT (OLD.deletion <> 'purged' AND NEW.deletion = 'purged'
                      AND NEW.payload_json = '{"termination":{"reason":"purged"}}'
                      AND NEW.artifact_refs = '[]'))
            BEGIN
                SELECT RAISE(ABORT, 'facts are immutable: only payload erasure during purge is allowed');
            END;

            DROP VIEW IF EXISTS thread_list_view;
            CREATE VIEW thread_list_view AS
            SELECT
                t.id,
                t.parent_thread_id AS parent_id,
                t.fork_kind,
                t.title,
                t.created_at_ms / 1000 AS created_at_s,
                t.updated_at_ms / 1000 AS updated_at_s,
                p.workspace_root,
                p.system_role AS persona,
                CASE
                    WHEN EXISTS (SELECT 1 FROM facts f WHERE f.thread_id = t.id AND f.deletion = 'present')
                    THEN (SELECT COUNT(*) FROM facts f WHERE f.thread_id = t.id AND f.deletion = 'present')
                    ELSE t.msg_count
                END AS msg_count,
                COALESCE(
                    (SELECT COALESCE(
                        json_extract(f.payload_json, '$.user_message.text'),
                        json_extract(f.payload_json, '$.message.content')
                     )
                     FROM facts f
                     WHERE f.thread_id = t.id AND f.deletion = 'present'
                     ORDER BY f.seq DESC LIMIT 1),
                    t.last_user_prompt
                ) AS last_user_prompt,
                t.digest,
                t.status,
                t.round_counter
            FROM threads t
            LEFT JOIN thread_policies p ON p.thread_id = t.id;
            "#,
        )?;
    }

    Ok(())
}

/// Thread lineage graph (ADR-0120), applied by migration 25.
///
/// Promotes fork provenance from an unpopulated column (`fork_fact_id`) and a
/// display-string convention (`"{parent} (fork)"`) to two typed, persisted
/// fields on the child thread, and republishes `thread_list_view` so every
/// projection reads them without inferring anything:
///
/// - `fork_round` — the parent's 1-indexed user round at the cut, or `NULL`
///   for a trunk.
/// - `fork_turn`  — the 0-indexed turn inside that round, or `NULL` when the
///   cut is the round boundary itself.
///
/// `fork_fact_id` is dropped: nothing wrote it, nothing read it, and a node id
/// is not an address a user can act on. The full-resolution causal anchor stays
/// where it belongs — the child `SessionIR`'s `active_leaf` (ADR-0104 §3).
pub fn apply_thread_lineage_graph_schema(tx: &rusqlite::Transaction) -> Result<()> {
    if !table_exists(tx, "threads")? {
        return Ok(());
    }
    let existing = table_columns(tx, "threads")?;

    // 1. The typed fork point. `ADD COLUMN` is a cheap metadata-only operation
    //    in SQLite, so this stays conditional rather than rebuilding the row.
    if !existing.contains("fork_round") {
        tx.execute_batch("ALTER TABLE threads ADD COLUMN fork_round INTEGER;")?;
    }
    if !existing.contains("fork_turn") {
        tx.execute_batch("ALTER TABLE threads ADD COLUMN fork_turn INTEGER;")?;
    }

    // 2. Purge the never-written provenance column. It documented a capability
    //    the system never had (ADR-0059's zero-legacy discipline).
    if existing.contains("fork_fact_id") {
        tx.execute_batch("ALTER TABLE threads DROP COLUMN fork_fact_id;")?;
    }

    // 3. Purge the smuggled provenance (ADR-0120 §3, `[INV-LINEAGE-04]`). The
    //    retired fork primitive wrote `"{parent title} (fork)"` — and bare
    //    `"fork"` when the parent had no title — so a branch's divergence point
    //    lived in a user-editable display string. Those titles are machine data,
    //    not authored names, and every branch they described now has a typed
    //    `fork_round`/`fork_turn` record instead (or, for branches predating
    //    this migration, an honest `NULL`).
    //
    //    The strip is deliberately narrow: a title is only treated as derived
    //    when it is byte-for-byte what the retired primitive would have
    //    produced from the parent's *current* title. A user who typed "fork" or
    //    "... (fork)" themselves keeps it.
    tx.execute_batch(
        r#"
        UPDATE threads
           SET title = ''
         WHERE fork_kind = 'fork'
           AND title = 'fork'
           AND (parent_thread_id IS NULL
                OR (SELECT COALESCE(NULLIF(parent.title, ''), 'fork')
                      FROM threads parent
                     WHERE parent.id = threads.parent_thread_id) = 'fork');

        UPDATE threads
           SET title = ''
         WHERE fork_kind = 'fork'
           AND title <> 'fork'
           AND title = (
                SELECT COALESCE(NULLIF(parent.title, ''), 'fork') || ' (fork)'
                  FROM threads parent
                 WHERE parent.id = threads.parent_thread_id
           );
        "#,
    )?;

    // 4. Republish the read projection with the lineage pair. Ordering is
    //    deterministic — `parent_thread_id, created_at_ms, id` — so a browser
    //    that sorts by lineage and creation never sees rows move (INV-LINEAGE-03).
    tx.execute_batch(
        r#"
        DROP VIEW IF EXISTS thread_list_view;
        CREATE VIEW thread_list_view AS
        SELECT
            t.id,
            t.parent_thread_id AS parent_id,
            t.fork_kind,
            t.fork_round,
            t.fork_turn,
            t.title,
            t.created_at_ms / 1000 AS created_at_s,
            t.updated_at_ms / 1000 AS updated_at_s,
            p.workspace_root,
            p.system_role AS persona,
            CASE
                WHEN EXISTS (SELECT 1 FROM facts f WHERE f.thread_id = t.id AND f.deletion = 'present')
                THEN (SELECT COUNT(*) FROM facts f WHERE f.thread_id = t.id AND f.deletion = 'present')
                ELSE t.msg_count
            END AS msg_count,
            COALESCE(
                (SELECT COALESCE(
                    json_extract(f.payload_json, '$.user_message.text'),
                    json_extract(f.payload_json, '$.message.content')
                 )
                 FROM facts f
                 WHERE f.thread_id = t.id AND f.deletion = 'present'
                 ORDER BY f.seq DESC LIMIT 1),
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
