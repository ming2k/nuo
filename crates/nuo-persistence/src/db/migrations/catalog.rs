//! The chronological catalog of database migrations.
use super::types::Migration;

/// The chronological sequence of schema migrations.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: r#"
        -- Sessions table
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            parent_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            fork_kind TEXT CHECK(fork_kind IN ('trunk', 'fork', 'aside')) NOT NULL DEFAULT 'trunk',
            title TEXT,
            title_manual BOOLEAN NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            project_root TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_root);
        CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at_ms DESC);

        -- Strict Monotonic Event Ledger (replacing legacy .jsonl files)
        CREATE TABLE IF NOT EXISTS session_events (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY(session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_events_session_seq ON session_events(session_id, seq ASC);

        -- Materialized Messages table
        CREATE TABLE IF NOT EXISTS messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL,
            role TEXT CHECK(role IN ('user', 'assistant', 'system', 'tool')) NOT NULL,
            content TEXT NOT NULL,
            content_blob_hash TEXT,
            reasoning_content TEXT,
            provider TEXT,
            model TEXT,
            created_at_ms INTEGER NOT NULL,
            UNIQUE(session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq ASC);

        -- Command execution audit ledger (ADR-0091)
        CREATE TABLE IF NOT EXISTS commands (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            arguments TEXT NOT NULL,
            result TEXT,
            status TEXT CHECK(status IN ('running', 'ok', 'failed', 'cancelled')) NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_commands_session ON commands(session_id, created_at_ms ASC);

        -- Unified Key-Value Store table
        CREATE TABLE IF NOT EXISTS kv_store (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );

        -- Full-Text Search Index (FTS5) for messages
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_messages USING fts5(
            message_id UNINDEXED,
            session_id UNINDEXED,
            role UNINDEXED,
            content,
            reasoning_content,
            tokenize = 'porter unicode61'
        );

        -- Triggers to synchronize fts_messages with messages table
        CREATE TRIGGER IF NOT EXISTS trg_messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO fts_messages(message_id, session_id, role, content, reasoning_content)
            VALUES (new.id, new.session_id, new.role, new.content, COALESCE(new.reasoning_content, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS trg_messages_ad AFTER DELETE ON messages BEGIN
            DELETE FROM fts_messages WHERE message_id = old.id;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_messages_au AFTER UPDATE ON messages BEGIN
            DELETE FROM fts_messages WHERE message_id = old.id;
            INSERT INTO fts_messages(message_id, session_id, role, content, reasoning_content)
            VALUES (new.id, new.session_id, new.role, new.content, COALESCE(new.reasoning_content, ''));
        END;
    "#,
    },
    Migration {
        version: 2,
        sql: r#"
        -- Add full serialized SessionData JSON column for SQLite Single-Source-of-Truth
        ALTER TABLE sessions ADD COLUMN data TEXT;
    "#,
    },
    Migration {
        version: 3,
        sql: r#"
        -- Add indexed summary columns for sub-millisecond session listing
        ALTER TABLE sessions ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN last_user_prompt TEXT;
        ALTER TABLE sessions ADD COLUMN digest TEXT;
        CREATE INDEX IF NOT EXISTS idx_sessions_project_updated ON sessions(project_root, updated_at_ms DESC);
    "#,
    },
    Migration {
        version: 4,
        sql: r#"
        -- Unified prompt input history table (ADR-0168 / SSOT)
        CREATE TABLE IF NOT EXISTS input_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            session_id TEXT,
            workspace TEXT,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_input_history_text ON input_history(text);
        CREATE INDEX IF NOT EXISTS idx_input_history_created_at ON input_history(created_at_ms DESC);
     CREATE INDEX IF NOT EXISTS idx_input_history_session ON input_history(session_id, created_at_ms DESC);
     "#,
    },
    Migration {
        // Single-transcript persistence foundation. Facts live in
        // `entries` (position-free, immutable), positions in `entry_memberships`
        // (many-to-one so forks share facts), and projection decisions in
        // `projections`. The event ledger is renamed `events` to match the
        // single-source-of-truth vocabulary. The legacy `messages` table,
        // `sessions.data` JSON snapshot column, and their FTS structures are
        // retired together with the `SessionData` swap (same tranche).
        version: 5,
        sql: r#"
        ALTER TABLE session_events RENAME TO events;

        CREATE TABLE IF NOT EXISTS entries (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
            role          TEXT CHECK (role IN ('user','assistant','system','tool')),
            content       TEXT,
            origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
            hidden        INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            CHECK ( origin IS NULL OR hidden = 1 ),
            CHECK ( kind <> 'message' OR role IS NOT NULL )
        );

        CREATE TABLE IF NOT EXISTS entry_memberships (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            entry_id   TEXT NOT NULL REFERENCES entries(id),
            added_by   INTEGER NOT NULL,
            PRIMARY KEY (session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_memberships_entry ON entry_memberships(entry_id);

        CREATE TABLE IF NOT EXISTS projections (
            session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq           INTEGER NOT NULL,
            kind          TEXT NOT NULL CHECK (kind IN ('prune','compact','freeze')),
            up_to_seq     INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, seq)
        );

        CREATE TABLE IF NOT EXISTS session_blobs (
            hash TEXT PRIMARY KEY,
            size INTEGER NOT NULL,
            mime TEXT NOT NULL,
            data BLOB,
            path TEXT,
            CHECK ( (data IS NULL) <> (path IS NULL) )
        );

        -- Clean break (ADR-0186): retire the legacy snapshot payload and the
        -- messages materialization; sessions keep identity + working state.
        DROP TRIGGER IF EXISTS trg_messages_ai;
        DROP TRIGGER IF EXISTS trg_messages_ad;
        DROP TRIGGER IF EXISTS trg_messages_au;
        DROP TABLE IF EXISTS fts_messages;
        DROP TABLE IF EXISTS messages;
        -- fork_kind gains 'subagent' (ADR-0186 §6): SQLite cannot ALTER a
        -- CHECK, so the table is rebuilt. Legacy payload columns are dropped
        -- in the same rebuild (clean break); working state rides along.
        -- Legacy dependent rows are retired first so the parent drop passes
        -- the foreign-key check.
        DELETE FROM entry_memberships;
        DELETE FROM projections;
        DELETE FROM events;
        DELETE FROM commands;
        CREATE TABLE sessions_new (
            id                  TEXT PRIMARY KEY,
            parent_id           TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            fork_kind           TEXT NOT NULL DEFAULT 'trunk'
                                CHECK (fork_kind IN ('trunk','fork','aside','subagent')),
            title               TEXT,
            created_at_ms       INTEGER NOT NULL,
            updated_at_ms       INTEGER NOT NULL,
            project_root        TEXT NOT NULL,
            msg_count           INTEGER NOT NULL DEFAULT 0,
            last_user_prompt    TEXT,
            digest              TEXT,
            data                TEXT,
            title_manual        BOOLEAN NOT NULL DEFAULT 0
        );
        INSERT INTO sessions_new (id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest, data, title_manual)
            SELECT id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest, data, title_manual FROM sessions;
        DROP TABLE sessions;
        ALTER TABLE sessions_new RENAME TO sessions;
        ALTER TABLE sessions DROP COLUMN data;
        ALTER TABLE sessions DROP COLUMN title_manual;
        ALTER TABLE sessions ADD COLUMN provider_connection TEXT;
        ALTER TABLE sessions ADD COLUMN round_counter INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN unattended INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN disabled_tools TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN commands TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN round_interrupts TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN retry_pending TEXT;
        ALTER TABLE sessions ADD COLUMN request_usage_records TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN applied_seq INTEGER;
        ALTER TABLE sessions ADD COLUMN checksum INTEGER;
        ALTER TABLE sessions ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 13;

        -- Full-text search moves to the transcript entries.
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_entries USING fts5(
            entry_id UNINDEXED,
            session_id UNINDEXED,
            role UNINDEXED,
            content,
            tokenize = 'porter unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS trg_entries_ai AFTER INSERT ON entries BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.id, m.session_id, COALESCE(new.role, ''), COALESCE(new.content, '')
            FROM entry_memberships m WHERE m.entry_id = new.id;
        END;
        CREATE TRIGGER IF NOT EXISTS trg_entries_ad AFTER DELETE ON entries BEGIN
            DELETE FROM fts_entries WHERE entry_id = old.id;
        END;
    "#,
    },
    Migration {
        // Corrective migration: development builds of the tranche stamped
        // databases at version 5 with an interim `sessions` rebuild that carried
        // the legacy `scheduled_jobs` column and predated the final working-state
        // columns (`round_counter`, `unattended`, ... ). Because migration 5's SQL
        // was finalized after those stamps, such databases short-circuit the
        // migrator and break at runtime on the first INSERT. Migration 6 reconciles
        // every version-5 incarnation to the final schema. The DDL must be applied
        // conditionally (both `DROP COLUMN scheduled_jobs` and the `ADD COLUMN`
        // set are invalid on the "other" incarnation), so the work happens in
        // `repair_intermediate_sessions_schema` below — this entry only advances
        // `user_version`.
        version: 6,
        sql: "",
    },
    Migration {
        // Persistence v2: incremental append needs a transcript
        // generation id and a durable blob reference ledger; the schema must stop
        // promising what the runtime does not do (event ledger, session_blobs,
        // applied_seq, projections kind CHECK); timestamps and FTS become honest.
        // The DDL must be conditional (an interim version-5 database carries no
        // transcript tables at all), so the work happens in
        // `apply_persistence_v2_schema` below — this entry only advances
        // `user_version`.
        version: 7,
        sql: "",
    },
    Migration {
        // Usage ledger: per-attempt usage records move out of the
        // session row into their own key-addressed table. The row column
        // forced an O(records) serialization on every save; the table upserts
        // only the attempts a commit actually changed. DDL and the JSON
        // backfill are conditional (the column exists only on
        // databases), so the work happens in `apply_usage_ledger_schema`
        // below — this entry only advances `user_version`.
        version: 8,
        sql: "",
    },
    Migration {
        // Durable retry-resolution records: the success-side mirror of
        // `round_interrupts`. One JSON column on `sessions`, default `[]`.
        // The DDL is applied conditionally by the migration subagent (version
        // 9 arm) so a database that already carries the column — a dev
        // build's startup repair, or a partially-applied v9 — passes
        // through unchanged instead of failing on a duplicate column.
        version: 9,
        sql: "",
    },
    Migration {
        // Corrective migration (integrity fix): migration 5's
        // `entries` CHECK `origin IS NULL OR hidden = 1` coupled two orthogonal
        // concepts — `origin` (WHY a message exists) and `hidden` (whether it
        // is shown). That coupling was false: the durable transcript
        // legitimately carries *visible* harness injections (UserSteer /
        // SubagentSteer / SubagentTask, CommandEcho "/cmd" & "!cmd", ToolImage, and
        // SystemPrompt/SystemReminder), all of which ADR-0050 records
        // `.hidden = false` with an `origin`. The false constraint then turned
        // a lawful mid-round save (e.g. the steering fire-at-turn-boundary) into
        // `CHECK constraint failed: origin IS NULL OR hidden = 1`.
        //
        // The constraint is replaced by two honest, orthogonal ones that still
        // reject silent corruption:
        //   - a hidden message must explain WHY (`origin` present);
        //   - only a *visible user* envelope origin is outlawed — a checkpoint
        //     is never visible dialogue, and user-role visible steering/echo/
        //     image injections carry `origin = 'harness'`. (Role-level
        //     preferencing is intended: decompact/checkpoint entries are the
        //     only visible-non-user pathological case the old check guarded.)
        // Because a column-level CHECK cannot be altered in place, `entries`
        // is rebuilt (table rewrite) inside a transaction; the FK from
        // `entry_memberships` is deferred so the transient missing-table step
        // does not trip the referential guard.
        version: 10,
        sql: "",
    },
    Migration {
        // Durable request-projection archive: a key-addressed table
        // for forensic snapshots of assembled requests. It is deliberately a
        // table, not a `sessions` JSON column: the projection is written on the
        // request hot path, and a row column would force an O(records)
        // serialization on every session save (the same reason migration 8
        // moved the usage ledger into its own table). `CREATE TABLE IF NOT
        // EXISTS` is idempotent, so a database that already carries the table
        // passes through unchanged.
        version: 11,
        sql: r#"
        CREATE TABLE IF NOT EXISTS request_projections (
            session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            round         INTEGER NOT NULL,
            turn          INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            PRIMARY KEY (session_id, round, turn)
        );
        CREATE INDEX IF NOT EXISTS idx_request_projections_session_created
            ON request_projections(session_id, created_at_ms DESC);
        "#,
    },
    Migration {
        // Session scope: the mandatory `project_root` partition key
        // becomes a typed `SessionScope` (`scope_kind` + namespaced `scope_key`)
        // orthogonal to an optional `workspace_root` binding. The rebuild
        // enforces the NOT NULL constraints ALTER cannot add; every existing row
        // backfills losslessly as a workspace scope keyed by its old
        // project_root. The DDL is dispatched to `apply_session_scope_schema`.
        version: 12,
        sql: "",
    },
    Migration {
        // Grouping is derived: the stored `scope_kind`/`scope_key`
        // partition key is retired for the concrete `space` column; workspace
        // sessions keep grouping by `workspace_root`, non-workspace rows
        // migrate to the Personal grouping (`space IS NULL`).
        version: 13,
        sql: "",
    },
    Migration {
        // Workspace-only partition (revised): drop `space` (an unbound
        // session is simply `workspace_root IS NULL`) and add `persona`
        // metadata (the persona that staffed the session). Conditional DDL.
        version: 14,
        sql: "",
    },
    Migration {
        version: 15,
        sql: "",
    },
    Migration {
        version: 16,
        sql: "",
    },
    Migration {
        // Session IR clean break: initialize pure headless
        // `sessions_v2`, `session_policies`, `causal_nodes` schema and create
        // `session_list_view` read projection.
        version: 17,
        sql: "",
    },
    Migration {
        // Universal Asset Attestation Ledger: initialize
        // `asset_attestations` table for process and endpoint trust.
        version: 18,
        sql: "",
    },
    Migration {
        // Session Role Manifest Snapshotting:
        // add immutable role_manifest column to sessions table.
        version: 19,
        sql: "",
    },
    Migration {
        // Role-Anchored Session Isolation Index:
        // add covering index for workspace-free role session queries.
        version: 20,
        sql: "",
    },
    Migration {
        // Causal Nodes Composite Primary Key (session_id, id):
        // allows forked branches and asides to inherit and persist ancestral nodes without conflict.
        version: 21,
        sql: "",
    },
    Migration {
        // Canonical context-lifecycle substrate:
        // immutable facts, branch cursors, derived views/checkpoints, artifact
        // manifests + references, request manifests, tombstones, and the
        // idempotency ledger. The schema enforces fact immutability at the SQL
        // layer (INV-FACT-02) so a bug cannot rewrite history. DDL is idempotent.
        version: 22,
        sql: "",
    },
    Migration {
        // Retrieval leases, deletion jobs, and GC continuation:
        // read leases protect live reads from concurrent GC; deletion jobs make
        // purge observable and resumable across a crash.
        version: 23,
        sql: "",
    },
    Migration {
        // Canonical Thread persistence substrate, junk drawer elimination,
        // and zero-legacy clean break (ADR-0059):
        // establish threads as root SSOT, thread_policies with normalized columns,
        // thread_list_view, rename session_id -> thread_id on all child tables,
        // and purge legacy zombie tables and dead 'aside' concept.
        version: 24,
        sql: "",
    },
    Migration {
        // Thread lineage graph (ADR-0120): the fork point becomes a persisted,
        // typed fact on the child thread (`fork_round` / `fork_turn`), written by
        // the one fork primitive and read verbatim by every consumer. `fork_fact_id`
        // is dropped — no writer ever filled it, and a round/turn address is the
        // gesture `/fork` accepts and the browser can show. DDL is conditional
        // so a database already carrying the columns passes through.
        version: 25,
        sql: "",
    },
    Migration {
        // Workspace effect journal (ADR-0125): the durable record of what each
        // tool call did to state outside the conversation, with pre-image
        // content hashes for the calls whose effect is reversible. Immutable by
        // trigger for exactly the reason `facts` is: a journal a bug can
        // rewrite is not evidence, and `/undo` reads it as the sole authority
        // on "what was here before". DDL is idempotent.
        version: 26,
        sql: "",
    },
    Migration {
        // Backfill the causal fact graph for existing sessions.
        //
        // The transcript save path never mirrored `entries` into `causal_nodes`,
        // so every session created before that mirror existed has a complete
        // transcript and an empty graph. Compaction reads the graph, so those
        // sessions keep growing until the provider rejects the request — the
        // mirror only fills in on their *next* commit, and a session at the
        // context ceiling cannot reach a next commit.
        //
        // The DDL is a Rust step (`apply_causal_fact_backfill_schema`) rather than
        // inline `sql`, because it must run *after* migration 26 created the
        // tables verification requires: inline SQL is applied before the Rust
        // steps of the same batch, so a backfill there would run against a
        // pre-migration schema.
        version: 27,
        sql: "",
    },
    Migration {
        // Split the write path from the derived indexes.
        //
        // FTS indexing and the causal fact mirror used to run inside the
        // commit transaction (the trigger) or immediately after it (the live
        // mirror). Both are derived from `entries`/`entry_memberships`, so
        // neither belongs on the path a turn waits for. This migration drops
        // the FTS triggers, creates durable per-session cursors and the
        // pending queue, and seeds the cursors from the state the old paths
        // left behind: every existing FTS row is accounted for, and the causal
        // cursor resumes from the graph's newest fact (a legacy gap is
        // repaired on the first drain).
        version: 28,
        sql: "",
    },
    Migration {
        // Online derived-index scrub and main-line leaf reset.
        //
        // `causal_nodes` is a derive of the transcript. A graph a previous
        // binary left malformed, or one whose rows were lost, must heal without
        // a blocking startup scan and without a one-off corruption migration:
        // `derived_meta` carries a scrub cursor, and the writer probes a bounded
        // slice of sessions while idle, repairing any hit through the same
        // read-path routine (`flush_session`) that any reader would run. The
        // `active_leaf` reset drops a persisted branch position that the
        // transcript mirror never maintained for main-line sessions.
        version: 29,
        sql: "",
    },
    Migration {
        // Role-scoped thread history (ADR-0170). Every thread now carries an
        // explicit role; the legacy `general` placeholder is the default
        // developer role, not a role of its own. The covering index is on the
        // exact composite key every list/resume/purge query uses.
        version: 30,
        sql: "",
    },
    Migration {
        // Remove the FTS write triggers migration 28 intended to drop.
        //
        // Migration 24 re-created the membership pair as
        // `trg_entries_membership_ai`/`trg_entries_membership_ad`, names
        // migration 28 did not know. They survived, keeping FTS indexing on the
        // commit path (`[INV-DERIVED-01]`) and making a full rewrite's
        // `DELETE ... WHERE session_id` scan the whole FTS content table.
        version: 31,
        sql: "",
    },
    Migration {
        // A thread's role and workspace are its durable identity, not a
        // mutable preference. `/role` switches to a thread of the target role;
        // it never rebinds the current one. This trigger enforces the rule for
        // every writer, including a future path that bypasses the CLI.
        version: 32,
        sql: "",
    },
    Migration {
        // The workspace-effect journal is append-only evidence and must outlive
        // the thread it describes. The original table declared
        // `thread_id REFERENCES threads(id) ON DELETE CASCADE`, which made every
        // thread delete with journaled mutations fail on the no-delete trigger.
        // Rebuild the table without the reference; the journal keeps its
        // thread_id as evidence.
        version: 33,
        sql: "",
    },
    Migration {
        // Thread/Branch/Fact runtime substrate (RFC-0003 / ADR-0173):
        // branch cursors, a monotonic thread revision, per-fact execution
        // context, and a dedicated thread-policy JSON column.
        version: 34,
        sql: "",
    },
    Migration {
        // Durable command receipts, writer leases, and the blob reference
        // ledger (RFC-0003 §6.18–6.19).
        version: 35,
        sql: "",
    },
    Migration {
        // Thread facts become the conversation SSOT; transcript tables are
        // converted and dropped (RFC-0003 / ADR-0173).
        version: 36,
        sql: "",
    },
    Migration {
        // Working-state mirror (todos) on the thread policy row.
        version: 37,
        sql: "",
    },
    Migration {
        // Thread nomenclature and dead-table cleanup: origin, one fact table,
        // one revision table, no derived queue.
        version: 38,
        sql: "",
    },
];
