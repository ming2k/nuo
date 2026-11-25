---
id: ADR-0254
title: "Orthogonal Thread, Role, and Workspace Primitives"
status: accepted
date: 2026-11-25
scope: protocol/workspace, protocol/agent-role, persistence/roles, persistence/schema, engine/agent, server/registry, cli/thread
supersedes: ADR-0170
superseded_by: null
negative_knowledge: false
---

# 0254. Orthogonal Thread, Role, and Workspace Primitives

- Status: Accepted
- Date: 2026-11-25
- Deciders: Nuo Architecture Working Group
- Supersedes: [ADR-0170](0170-role-scoped-thread-history-and-workspace-binding.md)

## Context

[ADR-0170](0170-role-scoped-thread-history-and-workspace-binding.md) coupled three
distinct concepts into a single immutable composite key (`ThreadScope = (role,
workspace)`):

1. **Role as a capability profile** — prompt identity, admitted tools, MCP
   allowlist, and permission defaults.
2. **Role as a workspace policy** — `RoleWorkspace::{Unbound, Inherit, Fixed}`
   dictating whether and where a thread could bind a directory.
3. **Thread scope as a rigid history partition** — enforced by the SQLite
   trigger `trg_thread_policy_scope_immutable`, refusing in-place `/role`
   switches, forcing complex CLI flag matrices (`--role`, `--workspace`,
   `--all-roles`, `--global`), and creating a circular dependency between
   workspace-local `.nuo/roles.toml` loading and role-driven workspace
   resolution.

In practice, specialization through focused toolsets and clear role identity is
high-leverage, whereas coupling workspace lifecycle and thread partitioning into
the role definition multiplied complexity across every layer of the system.

## Decision

Separate **Thread**, **Role**, and **Workspace** into three orthogonal
primitives with a clean break (no compatibility shims):

### 1. Thread is the sole first-class conversation identity
- A thread is identified solely by `thread_id`.
- `role` and `workspace_root` are mutable attributes on `thread_policies`
  recording the thread's active role and optional workspace attachment.
- `ThreadScope` and `WorkspaceFilter` are deleted.
- `ThreadFilter` becomes a flat pair of independent optional filters:
  `{ role: Option<String>, workspace: Option<PathBuf> }`.

### 2. Role is a pure stateless capability profile
- A role declares identity/instructions, admitted tools, admitted MCP servers,
  runtime knobs, and default confinement.
- `RoleWorkspace` (`Unbound` / `Inherit` / `Fixed`), `MainAgentRole::requires_workspace()`,
  and `MainAgentRole::workspace_policy()` are deleted.
- Global `roles.toml` is the single source of truth for custom role definitions;
  `RolesConfig::load_for_workspace` is deleted, eliminating the circular
  dependency between role resolution and workspace discovery.
- `/role <id>` hot-swaps the active role profile on the current thread in place
  (updating tools, identity, permissions, and durable policy). `/role <id> --new`
  starts a fresh thread with that role.

### 3. Workspace is an optional environment attachment
- A thread may optionally carry a `WorkspaceBinding` (root + additional roots).
- Workspace-dependent tools and workspace trust gates check the thread's
  attached workspace directly rather than querying a role policy.
- Role selection never mutates or rejects a thread's workspace attachment.

### 4. Persistence & CLI simplification
- Migration `0008_orthogonal_thread_role_workspace.sql` drops
  `trg_thread_policy_scope_immutable` and adds an independent role index
  `idx_thread_policies_role` alongside `idx_thread_policies_workspace`.
- `nuo thread ls / stop / rm` accept `--role` and `--workspace` as independent,
  composable filters; `--all-roles` and `--global` are removed.

## Invariants

- **`[INV-ORTHO-01]` Role Owns Capability Only**: A role MUST NOT carry or
  enforce a filesystem workspace binding policy.
- **`[INV-ORTHO-02]` Thread Identity Is `thread_id`**: Thread history, lookup,
  and filtering MUST treat `role` and `workspace` as orthogonal, independently
  filterable attributes rather than a composite immutable partition key.
- **`[INV-ORTHO-03]` Global Role Registry**: Role canonicalization and loading
  MUST NOT depend on a workspace path.
- **`[INV-ORTHO-04]` In-Place Role Switch**: Switching roles on an active thread
  MUST update the thread's active capability profile without forcing a new
  thread unless explicitly requested (`--new`).
