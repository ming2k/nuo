---
id: ADR-0170
title: "Role-Scoped Thread History: The Role Owns the Workspace Policy"
status: accepted
date: 2026-11-12
scope: host/workspace, protocol/thread-scope, persistence/session-store, cli/thread, server/registry
superseded_by: null
negative_knowledge: true
---

# 0170. Role-Scoped Thread History: The Role Owns the Workspace Policy

- Status: Accepted
- Date: 2026-11-12
- Deciders: Nuo Architecture Working Group
- Related: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md), [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md), [ADR-0120](0120-thread-lineage-graph-persisted-fork-provenance-and-browser-grade-history-topology.md), [ADR-0128](0128-thread-browser-order-newest-creation-first-with-cursor-stability.md), [ADR-0171](0171-thread-removal-is-durable-and-server-owned.md)

## Context

Thread history had two incompatible scopes.

The persistence layer stored both `thread_policies.workspace_root` and
`thread_policies.system_role`, but `SessionPartition` collapsed them into an
either/or:

```rust
(Some(workspace), _) => SessionPartition::Workspace(workspace),
(None, Some(role))  => SessionPartition::Role(role),
```

A workspace-bound role therefore lost its role identity. `developer` and
`coder` threads in the same repository shared one picker partition and one
`--resume` target.

At the same time, `nuo thread ls` was not a history browser at all. It consumed
the live monitor snapshot, which contains only resident drivers. A suspended
thread, a thread from a previous server process, or any durable history with no
driver was invisible. The CLI could not enumerate what `nuo thread rm --all`
was about to remove, and the docs promised persisted history that the
implementation never returned.

The role system already carried the missing fact. Built-in roles expose
`requires_workspace()` (`developer` requires one; `philosophist` and `ops` do
not), and custom roles carry `RoleWorkspace::{Unbound, Inherit, Fixed(path)}`.
The role policy is the authority: it decides whether a thread is anchored to a
workspace at all.

## Decision

### 1. The scope is `(role, workspace binding)`

The durable identity of a thread is a `ThreadScope`:

```rust
pub struct ThreadScope {
    pub role: String,
    pub workspace: Option<PathBuf>,
}
```

The role is always present. `workspace = None` is the unbound set **for that
role**, not a wildcard. A role with `workspace = "inherit"` is bound to the
caller's project; `workspace = "none"` is unbound; a fixed path binds exactly
that path. `developer` is the default role and requires a workspace.

`SessionPartition` is deleted. There is no workspace-first fallback and no
role-less partition. `ThreadFilter` carries the query form:

```rust
pub struct ThreadFilter {
    pub role: Option<String>,          // None = every role (explicit admin)
    pub workspace: WorkspaceFilter,    // Any / Path / Unbound
}
```

### 2. Every thread operation is role-first

`nuo thread` no longer has an implicit global mode:

```text
nuo thread ls                                  # developer + current workspace
nuo thread ls --role philosophist              # philosophist + unbound
nuo thread ls --role coder --workspace /repo-a # coder + /repo-a (inherit)
nuo thread ls --global                         # every role, every workspace
nuo thread ls --all-roles                      # every role, current workspace
nuo thread ls --global --role developer         # developer, every workspace
```

- `--role` defaults to `developer`.
- A workspace-requiring role without a project context is an error.
- A workspace-free role with `--project` or `--workspace` is an error, not a
  silent ignore.
- An unknown role is an error. It never falls back to `developer`.
- Built-in aliases (`dev`, `sre`, `philosopher`, …) canonicalize before they
  become a scope key. Custom role ids are preserved verbatim.
- `--workspace` is the second stage and requires an explicit `--role`: it
  selects another binding for an `inherit` role and confirms (or is omitted
  for) a `fixed` one. It is never a free-form rebind and never implied by the
  default role.
- `--all-roles` expands the role dimension inside one workspace.
- `--global` is the explicit every-role × every-binding admin scope; it cannot
  be combined with `--workspace` or `--all-roles`.

### 3. `thread ls` reads history, not the live monitor

`nuo thread ls` is the CLI peer of the TUI thread picker and `--resume`:

- durable rows come from `thread_list_view` through the single-writer reader;
- resident drivers are merged by id so live status overlays the durable row;
- durable rows with no driver are `ThreadHosting::Dormant`;
- subagent runs are excluded from the user-facing list;
- `--hosted` is the explicit fleet view (only resident drivers);
- the table carries `ROLE` and `WORKSPACE` columns.

The monitor remains the live fleet plane (`nuo status`, dashboard). It is not
the history plane.

### 4. Persistence

`thread_policies` already carries both dimensions. The work is query and
identity, not a new table:

- `list_session_summaries`, `latest_session`, `list_switch_candidates`, and
  `resolve_session_prefix` filter `(persona, workspace_root)` together.
- `SessionStore::scope()` is the exact role + binding; `SessionStore::list`
  and prefix resolution use it.
- The `/role` switch, `--resume`, the TUI picker, and the delete handler all
  use the same scope.
- Migration 30 folds the legacy `general` placeholder into `developer` and
  adds the `(system_role, workspace_root, updated_at_ms)` covering index.
- New session writes canonicalize the role id at claim time.

The role manifest snapshots the workspace policy (`workspace`) at birth, so a
later `roles.toml` edit cannot reinterpret an existing thread's binding.

## Invariants

- **`[INV-SCOPE-01]` Role-First Scope**: Every thread list/resume/stop/delete
  operation MUST resolve a role (default `developer`) before it resolves a
  workspace.
- **`[INV-SCOPE-02]` Workspace Policy Owns Binding**: A role's
  `RoleWorkspace` policy, not cwd alone, decides whether a workspace is
  required, unbound, or fixed.
- **`[INV-SCOPE-03]` No Silent Global**: A global scope is reachable only via
  an explicit `--global` (or the equivalent admin selector). `--all` and
  `--all-roles` alone are never global.
- **`[INV-SCOPE-04]` Unknown Role Refused**: Scoped operations MUST reject an
  unknown role instead of falling back to `developer`.
- **`[INV-SCOPE-05]` Workspace-First Is Retired**: `SessionPartition` and any
  query that drops the role when a workspace exists MUST NOT exist.
- **`[INV-SCOPE-06]` Thread Scope Is Write-Once**: A thread's role and resolved
  workspace are immutable for its lifetime. `/role` resumes or starts a thread
  owned by the target role; it never rebinds the current transcript. The
  durable rule is enforced by `trg_thread_policy_scope_immutable`.

## Consequences

- `developer` and `coder` histories in one repository are isolated, and
  `--resume` cannot cross roles.
- A workspace-free role is addressable without inventing a fake workspace.
- `nuo thread ls` finally shows suspended and post-restart history.
- Scripts that relied on the old hosted-only `thread ls` use `--hosted`.
- Existing `general` role rows are folded into `developer`; other historical
  alias ids remain their own keys (custom role names can shadow aliases, so a
  blind alias rewrite would corrupt legitimate custom roles).
- Every scoped operation must resolve the role registry, which is a local file
  read; this is bounded and stays off the server hot path.

## Validation

- Unit coverage for scope resolution (built-in, alias, custom none/inherit/fixed,
  unknown, missing workspace).
- Persistence coverage for role + workspace isolation in summaries, switch
  candidates, and `latest_session`.
- CLI integration coverage proves `thread ls` and `rm --all` keep a coder
  thread when the default developer scope is purged, and delete it only under
  `--role coder`.
- `cargo check --workspace --all-targets` is clean.

## Amendment (2026-11-12): explicit policy, explicit global, write-once scope

- `RoleWorkspace::None` is renamed `Unbound` (serde keeps `"none"`; `"unbound"`
  is accepted) and its doc no longer says "run anywhere". `CustomRole` carries a
  plain `RoleWorkspace` with `inherit` as the serde default, removing the two
  `None`s-with-opposite-meaning encoding. Built-in roles expose
  `workspace_policy()` and derive `requires_workspace()` from it.
- `Fixed` paths must be absolute. (Resolving a relative path against the
  process cwd made the binding depend on where the command happened to run.)
- The CLI no longer exposes the workspace dimension as a boolean expansion.
  `--workspace <path>` selects another binding for an `inherit` role and is
  refused for `unbound`/`fixed`; `--all-roles` expands roles inside one
  workspace; `--global` is the explicit every-role × every-binding scope and
  may be narrowed by `--role`. `--all-workspaces` is retired.
- `[INV-SCOPE-06]` makes the thread scope write-once in the schema, not only in
  the command path. `/role` still switches threads; it never rebinds the
  current one.
