-- Role + optional workspace permission rules (ADR-0212).
--
-- Replaces the legacy per-project `permissions.json` file with a single
-- relational source of truth inside `nuo.db`.
--
-- Scope key:
-- - `scope_kind = 'workspace'`, `workspace_key = <normalized workspace path>`
--   for workspace-bound roles (`developer`, workspace custom roles).
-- - `scope_kind = 'global'`, `workspace_key = ''`
--   for workspace-free roles (`ops`, `philosophist`, global custom roles)
--   or explicit cross-workspace grants for a role.
CREATE TABLE permission_rules (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    role_id       TEXT NOT NULL,
    scope_kind    TEXT NOT NULL CHECK (scope_kind IN ('workspace', 'global')),
    workspace_key TEXT NOT NULL DEFAULT '',
    tool          TEXT NOT NULL,
    scope         TEXT NOT NULL,
    effect        TEXT NOT NULL CHECK (effect IN ('allow', 'revoked')),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    last_used_ms  INTEGER CHECK (last_used_ms IS NULL OR last_used_ms >= 0),
    CHECK (
        (scope_kind = 'global' AND workspace_key = '')
        OR (scope_kind = 'workspace' AND workspace_key <> '')
    ),
    UNIQUE (role_id, scope_kind, workspace_key, tool, scope)
) STRICT;

CREATE INDEX idx_permission_rules_lookup
    ON permission_rules(role_id, scope_kind, workspace_key);
