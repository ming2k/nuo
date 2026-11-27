-- Migration 9: Pure catalog plane and truly nullable workspace_root (ADR-0255, ADR-0256).
--
-- 1. Drop retired content-plane tables that moved to versioned content shards
--    (`shards/content-XX.db`, ADR-0242, ADR-0255).
-- 2. Drop dead `SqliteThreadStore` tables and columns (ADR-0256).
-- 3. Rebuild `thread_policies` so `workspace_root` is genuinely nullable
--    (`NULL` = unbound, never `''`) and drop dead `thread_policy_json`.

DROP VIEW thread_list_view;

-- Retired content-plane tables (authoritative copies live in session journals
-- and project into `shards/content-XX.db`).
DROP TABLE fts_thread_facts;
DROP TABLE command_acts;
DROP TABLE workspace_effects;
DROP TABLE thread_blob_refs;
DROP TABLE causal_nodes;
DROP TABLE thread_branches;

-- Retired `SqliteThreadStore` tables (ADR-0256).
DROP TABLE thread_commands;
DROP TABLE thread_leases;
DROP TABLE thread_lease_counters;
DROP TABLE thread_blobs;

-- Retired `SqliteThreadStore` / zombie columns on `threads`.
-- Live thread activity is ephemeral runtime state owned by `MonitorTracker`
-- (`ThreadActivity`), never persisted in SQLite.
ALTER TABLE threads DROP COLUMN spawned_from_fact_id;
ALTER TABLE threads DROP COLUMN status;

-- Rebuild `thread_policies` with nullable `workspace_root` (backfilling
-- legacy `''` sentinel values to `NULL`) and without dead `thread_policy_json`.
CREATE TABLE thread_policies_v9 (
    thread_id              TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    workspace_root         TEXT CHECK (workspace_root IS NULL OR workspace_root <> ''),
    system_role            TEXT NOT NULL DEFAULT 'general',
    unattended             INTEGER NOT NULL DEFAULT 0 CHECK (unattended IN (0, 1)),
    provider_pin           TEXT CHECK (provider_pin IS NULL OR json_valid(provider_pin)),
    disabled_tools_json    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(disabled_tools_json)),
    additional_roots_json  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(additional_roots_json)),
    round_interrupts_json  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(round_interrupts_json)),
    retry_pending          TEXT CHECK (retry_pending IS NULL OR json_valid(retry_pending)),
    role_manifest          TEXT CHECK (role_manifest IS NULL OR json_valid(role_manifest)),
    todos_json             TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(todos_json)),
    updated_at_ms          INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;

INSERT INTO thread_policies_v9 (
    thread_id,
    workspace_root,
    system_role,
    unattended,
    provider_pin,
    disabled_tools_json,
    additional_roots_json,
    round_interrupts_json,
    retry_pending,
    role_manifest,
    todos_json,
    updated_at_ms
)
SELECT
    thread_id,
    NULLIF(workspace_root, ''),
    system_role,
    unattended,
    provider_pin,
    disabled_tools_json,
    additional_roots_json,
    round_interrupts_json,
    retry_pending,
    role_manifest,
    todos_json,
    updated_at_ms
FROM thread_policies;

DROP TABLE thread_policies;
ALTER TABLE thread_policies_v9 RENAME TO thread_policies;

CREATE INDEX idx_thread_policies_workspace ON thread_policies(workspace_root);
CREATE INDEX idx_thread_policies_role_scope
    ON thread_policies(system_role, workspace_root, updated_at_ms DESC);
CREATE INDEX idx_thread_policies_role
    ON thread_policies(system_role, updated_at_ms DESC);

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
    t.msg_count,
    t.last_user_prompt,
    t.digest,
    t.round_counter
FROM threads t
LEFT JOIN thread_policies p ON p.thread_id = t.id;
