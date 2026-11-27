-- Content shard baseline, schema version 1 (ADR-0242, ADR-0255).
--
-- This file is immutable. Every content shard (`shards/content-XX.db`) goes
-- through the same migration ledger, checksum validation, and `sqlite_master`
-- structural verification as `nuo.db` and `role_memory.db` (ADR-0192).
--
-- Content shards are 100% derived from the per-session journals: there are no
-- fake `threads` stub rows and no cross-plane foreign keys.

-- ---------------------------------------------------------------------------
-- Branch cursors and immutable causal fact graph
-- ---------------------------------------------------------------------------

CREATE TABLE thread_branches (
    thread_id                  TEXT NOT NULL,
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

CREATE TABLE causal_nodes (
    id           TEXT NOT NULL,
    thread_id    TEXT NOT NULL,
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
-- CAS blob reference ledger and normalized command act ledger
-- ---------------------------------------------------------------------------

CREATE TABLE thread_blob_refs (
    thread_id TEXT NOT NULL,
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

CREATE TABLE command_acts (
    thread_id    TEXT NOT NULL,
    seq          INTEGER NOT NULL CHECK (seq >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    PRIMARY KEY (thread_id, seq)
) STRICT;

CREATE INDEX idx_command_acts_thread ON command_acts(thread_id, seq ASC);

-- ---------------------------------------------------------------------------
-- Workspace effect journal projection (append-only evidence, ADR-0125/0249)
-- ---------------------------------------------------------------------------

CREATE TABLE workspace_effects (
    id                  INTEGER NOT NULL,
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
    created_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (thread_id, id)
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
-- Derived full-text index over facts and per-shard projection watermarks
-- ---------------------------------------------------------------------------

CREATE VIRTUAL TABLE fts_thread_facts USING fts5(
    fact_id UNINDEXED,
    thread_id UNINDEXED,
    role UNINDEXED,
    content,
    tokenize = 'porter unicode61'
);

CREATE TABLE projection_watermarks (
    thread_id     TEXT PRIMARY KEY,
    seq           INTEGER NOT NULL CHECK (seq >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;
