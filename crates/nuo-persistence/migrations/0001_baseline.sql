-- Nuo database baseline, schema version 1 (ADR-0189).
--
-- This file is immutable. A change to the durable schema is a new numbered
-- migration beside it, never an edit here. Existing databases are adopted at
-- this baseline exactly once; databases stamped by the retired 38-step
-- migration chain are refused with a clean-break error (ADR-0189).
--
-- The `schema_migrations` ledger is infrastructure created by the runner, not
-- by this file. The baseline covers only domain schema.
--
-- Every table is STRICT so SQLite enforces declared types instead of silently
-- storing whatever affinity happens to accept (ADR-0190). JSON columns carry
-- json_valid checks for the same reason: a malformed payload is a write bug,
-- not a value to discover at read time.

-- ---------------------------------------------------------------------------
-- Thread aggregate
-- ---------------------------------------------------------------------------

CREATE TABLE threads (
    id                    TEXT PRIMARY KEY,
    title                 TEXT NOT NULL DEFAULT '',
    status                TEXT NOT NULL DEFAULT 'idle'
                          CHECK (status IN ('idle','running','suspended','archived','terminated')),
    origin                TEXT NOT NULL DEFAULT 'trunk'
                          CHECK (origin IN ('trunk','fork','subagent')),
    parent_thread_id      TEXT REFERENCES threads(id) ON DELETE SET NULL,
    active_branch_id      TEXT NOT NULL DEFAULT 'main',
    active_leaf           TEXT,
    round_counter         INTEGER NOT NULL DEFAULT 0 CHECK (round_counter >= 0),
    msg_count             INTEGER NOT NULL DEFAULT 0 CHECK (msg_count >= 0),
    last_user_prompt      TEXT,
    transcript_generation TEXT,
    digest                TEXT CHECK (digest IS NULL OR json_valid(digest)),
    digest_anchor         INTEGER CHECK (digest_anchor IS NULL OR digest_anchor >= 0),
    checksum              INTEGER,
    revision              INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    spawned_from_fact_id  TEXT,
    created_at_ms         INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms         INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;

CREATE INDEX idx_threads_parent ON threads(parent_thread_id);
CREATE INDEX idx_threads_updated ON threads(updated_at_ms DESC);

CREATE TABLE thread_policies (
    thread_id              TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    workspace_root         TEXT NOT NULL,
    system_role            TEXT NOT NULL DEFAULT 'general',
    unattended             INTEGER NOT NULL DEFAULT 0 CHECK (unattended IN (0, 1)),
    provider_pin           TEXT CHECK (provider_pin IS NULL OR json_valid(provider_pin)),
    disabled_tools_json    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(disabled_tools_json)),
    additional_roots_json  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(additional_roots_json)),
    commands_json          TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(commands_json)),
    round_interrupts_json  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(round_interrupts_json)),
    retry_pending          TEXT CHECK (retry_pending IS NULL OR json_valid(retry_pending)),
    role_manifest          TEXT CHECK (role_manifest IS NULL OR json_valid(role_manifest)),
    thread_policy_json     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(thread_policy_json)),
    todos_json             TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(todos_json)),
    updated_at_ms          INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;

CREATE INDEX idx_thread_policies_workspace ON thread_policies(workspace_root);
CREATE INDEX idx_thread_policies_role_scope
    ON thread_policies(system_role, workspace_root, updated_at_ms DESC);

-- The role/workspace pair is the thread's durable identity, not a preference
-- (ADR-0170). `/role` starts or resumes a thread; it never rebinds this one.
CREATE TRIGGER trg_thread_policy_scope_immutable
BEFORE UPDATE OF system_role, workspace_root ON thread_policies
WHEN NEW.system_role <> OLD.system_role
  OR NEW.workspace_root <> OLD.workspace_root
BEGIN
    SELECT RAISE(ABORT, 'thread role/workspace is immutable');
END;

-- Branch cursors over the append-only fact log (ADR-0173).
CREATE TABLE thread_branches (
    thread_id                  TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    branch_id                  TEXT NOT NULL,
    name                       TEXT NOT NULL,
    head_fact_id               TEXT,
    forked_from_fact_id        TEXT,
    compaction_horizon_fact_id TEXT,
    policy_json                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(policy_json)),
    created_at_ms              INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms              INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (thread_id, branch_id)
) STRICT;

CREATE INDEX idx_thread_branches_thread ON thread_branches(thread_id);

-- Facts are the conversation SSOT. Ids and sequences are thread-scoped; the
-- payload is decoded by nuo-protocol, never rewritten in SQL (ADR-0173).
CREATE TABLE causal_nodes (
    id           TEXT NOT NULL,
    thread_id    TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    parent_id    TEXT,
    seq          INTEGER NOT NULL CHECK (seq > 0),
    kind         TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','system_notice','observation')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    timestamp_ms INTEGER NOT NULL CHECK (timestamp_ms >= 0),
    context_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(context_json)),
    PRIMARY KEY (thread_id, id),
    UNIQUE (thread_id, seq)
) STRICT;

CREATE INDEX idx_causal_nodes_thread_seq ON causal_nodes(thread_id, seq ASC);
CREATE INDEX idx_causal_nodes_parent ON causal_nodes(thread_id, parent_id);

-- Facts are immutable evidence. A bug may add a fact; it may not rewrite one.
CREATE TRIGGER trg_causal_nodes_immutable
BEFORE UPDATE ON causal_nodes
WHEN NEW.id <> OLD.id
  OR NEW.thread_id <> OLD.thread_id
  OR NEW.parent_id IS NOT OLD.parent_id
  OR NEW.seq <> OLD.seq
  OR NEW.kind <> OLD.kind
  OR NEW.payload_json <> OLD.payload_json
  OR NEW.timestamp_ms <> OLD.timestamp_ms
  OR NEW.context_json <> OLD.context_json
BEGIN
    SELECT RAISE(ABORT, 'thread facts are immutable');
END;

-- ---------------------------------------------------------------------------
-- Durable command door, leases, and blob ledger (ADR-0174)
-- ---------------------------------------------------------------------------

CREATE TABLE thread_commands (
    thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    command_id    TEXT NOT NULL,
    ack_json      TEXT NOT NULL CHECK (json_valid(ack_json)),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (thread_id, command_id)
) STRICT;

CREATE TABLE thread_leases (
    thread_id     TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    owner_id      TEXT NOT NULL,
    fencing_token INTEGER NOT NULL CHECK (fencing_token >= 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0)
) STRICT;

CREATE TABLE thread_lease_counters (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    last_token INTEGER NOT NULL CHECK (last_token >= 0)
) STRICT;

CREATE TABLE thread_blobs (
    hash          TEXT PRIMARY KEY,
    size          INTEGER NOT NULL CHECK (size >= 0),
    mime          TEXT NOT NULL,
    data          BLOB NOT NULL,
    preview       TEXT,
    trust         TEXT NOT NULL DEFAULT 'trusted'
                  CHECK (trust IN ('trusted','untrusted')),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0)
) STRICT;

CREATE TABLE thread_blob_refs (
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    fact_id   TEXT NOT NULL,
    hash      TEXT NOT NULL,
    size      INTEGER NOT NULL CHECK (size >= 0),
    mime      TEXT NOT NULL,
    preview   TEXT,
    trust     TEXT NOT NULL DEFAULT 'trusted'
              CHECK (trust IN ('trusted','untrusted')),
    PRIMARY KEY (thread_id, fact_id)
) STRICT;

CREATE INDEX idx_thread_blob_refs_hash ON thread_blob_refs(hash);

-- D3 commit idempotency. One retained receipt per thread: a later commit
-- subsumes an earlier one, and replay only needs the latest identity.
CREATE TABLE commit_receipts (
    operation_id TEXT PRIMARY KEY,
    thread_id    TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    revision     INTEGER NOT NULL CHECK (revision >= 0),
    payload_hash TEXT NOT NULL
) STRICT;

CREATE INDEX idx_commit_receipts_thread ON commit_receipts(thread_id, revision DESC);

-- ---------------------------------------------------------------------------
-- Working state that is not a conversation fact
-- ---------------------------------------------------------------------------

CREATE TABLE request_projections (
    thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    round         INTEGER NOT NULL CHECK (round >= 0),
    turn          INTEGER NOT NULL CHECK (turn >= 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    payload       TEXT NOT NULL CHECK (json_valid(payload)),
    PRIMARY KEY (thread_id, round, turn)
) STRICT;

CREATE INDEX idx_request_projections_recency
    ON request_projections(thread_id, created_at_ms DESC);

CREATE TABLE input_history (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    text          TEXT NOT NULL,
    session_id    TEXT,
    workspace     TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0)
) STRICT;

CREATE INDEX idx_input_history_text ON input_history(text);
CREATE INDEX idx_input_history_created_at ON input_history(created_at_ms DESC);
CREATE INDEX idx_input_history_session ON input_history(session_id, created_at_ms DESC);

CREATE TABLE kv_store (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- Usage ledger (analytics; survives thread deletion by design)
-- ---------------------------------------------------------------------------

CREATE TABLE usage_records (
    session_id     TEXT NOT NULL,
    actor_id       TEXT NOT NULL,
    round          INTEGER NOT NULL,
    turn           INTEGER NOT NULL,
    attempt        INTEGER NOT NULL,
    payload        TEXT NOT NULL CHECK (json_valid(payload)),
    day            TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    project        TEXT NOT NULL,
    revision       INTEGER NOT NULL,
    PRIMARY KEY (session_id, actor_id, round, turn, attempt)
) STRICT;

CREATE INDEX usage_records_day ON usage_records(day, recorded_at_ms);

CREATE TABLE usage_clock (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL
) STRICT;

INSERT INTO usage_clock(id, revision) VALUES (1, 0);

CREATE TABLE usage_dirty (
    session_id TEXT NOT NULL,
    actor_id   TEXT NOT NULL,
    round      INTEGER NOT NULL,
    turn       INTEGER NOT NULL,
    attempt    INTEGER NOT NULL,
    revision   INTEGER NOT NULL,
    PRIMARY KEY (session_id, actor_id, round, turn, attempt),
    FOREIGN KEY (session_id, actor_id, round, turn, attempt)
        REFERENCES usage_records ON DELETE CASCADE
) STRICT;

CREATE INDEX usage_dirty_revision ON usage_dirty(revision);

CREATE TABLE usage_contributions (
    session_id         TEXT NOT NULL,
    actor_id           TEXT NOT NULL,
    round              INTEGER NOT NULL,
    turn               INTEGER NOT NULL,
    attempt            INTEGER NOT NULL,
    revision           INTEGER NOT NULL,
    day                TEXT NOT NULL,
    provider           TEXT NOT NULL,
    model              TEXT NOT NULL,
    requests           INTEGER NOT NULL,
    completed          INTEGER NOT NULL,
    prompt_tokens      INTEGER NOT NULL,
    completion_tokens  INTEGER NOT NULL,
    total_tokens       INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    cache_read_tokens  INTEGER NOT NULL,
    estimated_tokens   INTEGER NOT NULL,
    PRIMARY KEY (session_id, actor_id, round, turn, attempt),
    FOREIGN KEY (session_id, actor_id, round, turn, attempt)
        REFERENCES usage_records ON DELETE CASCADE
) STRICT;

CREATE INDEX usage_contributions_day ON usage_contributions(day, provider, model);

-- ---------------------------------------------------------------------------
-- Workspace effect journal (append-only evidence; deliberately no thread FK)
-- ---------------------------------------------------------------------------

CREATE TABLE workspace_effects (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id           TEXT NOT NULL,
    round_id            TEXT NOT NULL,
    turn_id             TEXT NOT NULL,
    call_id             TEXT NOT NULL,
    seq                 INTEGER NOT NULL,
    tool_name           TEXT NOT NULL,
    path                TEXT,
    op                  TEXT NOT NULL CHECK (op IN ('create','edit','delete')),
    effect_class        TEXT NOT NULL
                        CHECK (effect_class IN ('none','file_bytes','external_effect','process_effect','opaque')),
    before_hash         TEXT,
    after_hash          TEXT,
    irreversible_reason TEXT,
    completed_at_ms     INTEGER,
    reversed_at_ms      INTEGER,
    reversal_note       TEXT,
    expired_at_ms       INTEGER,
    created_at_ms       INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_workspace_effects_thread_seq ON workspace_effects(thread_id, seq);
CREATE INDEX idx_workspace_effects_path ON workspace_effects(thread_id, path);
CREATE INDEX idx_workspace_effects_undoable
    ON workspace_effects(thread_id, id DESC)
    WHERE effect_class = 'file_bytes' AND reversed_at_ms IS NULL;

CREATE TRIGGER trg_workspace_effects_no_delete
BEFORE DELETE ON workspace_effects
BEGIN
    SELECT RAISE(ABORT, 'workspace journal rows are append-only; prune by retention policy, not delete (INV-EFFECT-01)');
END;

CREATE TRIGGER trg_workspace_effects_immutable
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

CREATE TRIGGER trg_workspace_effects_after_hash_monotonic
BEFORE UPDATE OF after_hash ON workspace_effects
WHEN OLD.after_hash IS NOT NULL
  AND NEW.after_hash IS NOT OLD.after_hash
BEGIN
    SELECT RAISE(ABORT, 'workspace journal post-image is write-once (INV-EFFECT-03)');
END;

CREATE TRIGGER trg_workspace_effects_completion_monotonic
BEFORE UPDATE OF completed_at_ms ON workspace_effects
WHEN OLD.completed_at_ms IS NOT NULL
  AND NEW.completed_at_ms IS NOT OLD.completed_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal completion is one-shot (INV-EFFECT-03)');
END;

CREATE TRIGGER trg_workspace_effects_expiry_monotonic
BEFORE UPDATE OF expired_at_ms ON workspace_effects
WHEN OLD.expired_at_ms IS NOT NULL
  AND NEW.expired_at_ms IS NOT OLD.expired_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal expiry is one-way (INV-EFFECT-05)');
END;

CREATE TRIGGER trg_workspace_effects_no_early_reversal
BEFORE UPDATE OF reversed_at_ms ON workspace_effects
WHEN NEW.reversed_at_ms IS NOT NULL
  AND NEW.completed_at_ms IS NULL
BEGIN
    SELECT RAISE(ABORT, 'workspace journal refuses to reverse an unconfirmed effect (INV-EFFECT-04)');
END;

CREATE TRIGGER trg_workspace_effects_reversal_monotonic
BEFORE UPDATE OF reversed_at_ms ON workspace_effects
WHEN OLD.reversed_at_ms IS NOT NULL
  AND NEW.reversed_at_ms IS NOT OLD.reversed_at_ms
BEGIN
    SELECT RAISE(ABORT, 'workspace journal reversal is one-shot (INV-EFFECT-02)');
END;

-- ---------------------------------------------------------------------------
-- Derived full-text index over facts (rebuilt from the fact log)
-- ---------------------------------------------------------------------------

CREATE VIRTUAL TABLE fts_thread_facts USING fts5(
    fact_id UNINDEXED,
    thread_id UNINDEXED,
    role UNINDEXED,
    content,
    tokenize = 'porter unicode61'
);

-- ---------------------------------------------------------------------------
-- Thread list projection
-- ---------------------------------------------------------------------------

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
          WHERE cn.thread_id = t.id AND cn.kind IN ('dialogue','observation')),
        t.msg_count
    ) AS msg_count,
    COALESCE(
        (SELECT json_extract(cn.payload_json, '$.message.content')
           FROM causal_nodes cn
          WHERE cn.thread_id = t.id
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
