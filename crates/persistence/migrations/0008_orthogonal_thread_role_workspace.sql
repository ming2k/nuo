-- Orthogonal Thread, Role, and Workspace primitives (ADR-0254).
--
-- Thread identity is `thread_id`. `system_role` and `workspace_root` on
-- `thread_policies` are mutable attributes so `/role` and workspace
-- attachment can update an existing thread in place.

DROP TRIGGER IF EXISTS trg_thread_policy_scope_immutable;

CREATE INDEX IF NOT EXISTS idx_thread_policies_role
    ON thread_policies(system_role, updated_at_ms DESC);
