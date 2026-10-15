---
id: ADR-0059
title: "Canonical Thread Persistence Schema, Zero-Legacy Clean Break, and Junk Drawer Elimination"
status: accepted
date: 2026-10-30
scope: persistence/sqlite, architecture/database, domain/thread, runtime/lifecycle
superseded_by: null
negative_knowledge: true
---

# 0059. Canonical Thread Persistence Schema, Zero-Legacy Clean Break, and Junk Drawer Elimination

- Status: Accepted
- Date: 2026-10-30
- Deciders: Nuo Architecture Working Group
- Consulted: Persistence, Runtime, Wire, Server, and Storage Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md), [ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md)

---

## Context and Problem Statement

Following the ratification of Thread Entity SSOT ([ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md)), Thread Domain Nomenclature Unification ([ADR-0043](0043-thread-domain-nomenclature-unification-compact-tabbar-affordance-and-c-x-namespace-reorganization.md)), and Granular Causal Session Forking with Aside Retirement ([ADR-0058](0058-granular-causal-session-forking-sovereign-tab-projection-and-aside-retirement.md)), the user-facing CLI, TUI TabBar, slash command grammar, and network protocols successfully unified under the `Thread` domain entity.

However, the underlying SQLite database schema in `nuo-persistence` accumulated 23 sequential versions of historical compromises, resulting in severe architectural anomalies:

1. **Persistent Nomenclature Divergence**:
   While the application domain standardized on `Thread`, the physical persistence schema retained `sessions` and `sessions_v2`, forcing all child relational tables (`facts`, `branches`, `checkpoints`, `context_views`, `artifact_manifests`, `read_leases`, `deletion_jobs`) to reference `session_id` rather than `thread_id`.
2. **Junk Drawer Anti-Patterns**:
   The schema degraded into pseudo-relational JSON dumpsters:
   - `sessions` stored entire causal DAGs and state arrays inside opaque JSON strings (`tree`, `disabled_tools`, `commands`, `round_interrupts`, `retry_resolutions`).
   - `session_policies` stored vital lookup keys (such as `workspace_root` and `system_persona`) inside unindexed JSON blobs (`rules_json`), forcing `session_list_view` to execute expensive runtime `json_extract()` operations on every listing pass.
   - Dual-representation redundancy: `facts.artifact_refs` serialized an array of IDs as a JSON text column despite the concurrent existence of the normalized `artifact_refs` join table.
3. **Temporal Incoherence (Chaotic Measurement Units)**:
   Timestamp columns across tables lacked standardized units, oscillating unpredictably between seconds (`created_at_s`, `updated_at_s`), milliseconds (`created_at_ms`, `timestamp_ms`), dimensionless integers (`updated_at`), and abbreviated monikers (`at_ms`).
4. **Zombie Table and Dead Concept Residue**:
   Past architectural generations left orphaned tables (`entries`, `entry_memberships`, `projections`, `request_projections`, `session_blobs`, `causal_nodes_v2`) permanently lingering in database files. Furthermore, table check constraints continued to permit retired concepts like `'aside'`, violating ADR-0058.

We require an uncompromising, forward-facing, zero-legacy clean break that purges historical debt, establishes `threads` as the single physical root entity, eliminates junk drawers, standardizes time units, and guarantees absolute domain consistency.

---

## Decision Drivers

- **Domain Consistency SSOT (`[INV-SCHEMA-01]`)**: The persistence layer MUST establish `threads` as the authoritative root table and `thread_id` as the uniform foreign key across all child tables. All physical tables and columns bearing legacy `session` or `sessions_v2` nomenclature MUST be decommissioned.
- **Junk Drawer Elimination (`[INV-SCHEMA-02]`)**: High-frequency query columns (such as `workspace_root` and `system_role`) MUST exist as normalized, typed SQL columns. Unindexed opaque JSON blobs for primary entities are prohibited.
- **Monotonic Timestamp Uniformity (`[INV-SCHEMA-03]`)**: All temporal columns across all tables MUST be normalized to millisecond precision integers with the explicit `_ms` suffix (`created_at_ms`, `updated_at_ms`).
- **Zero Legacy Burden (`[INV-SCHEMA-04]`)**: Schema migration 24 executes an uncompromising clean break. Obsolete zombie tables (`entries`, `entry_memberships`, `projections`, `request_projections`, `session_blobs`, `causal_nodes`) and deprecated enum states (`fork_kind = 'aside'`) are permanently purged without defensive shims or dual-write overhead.

---

## Considered Options

### Option 1: Incremental Column Aliases and Backward-Compatible Views
- Retain `sessions_v2` and create SQLite `VIEW` aliases (`CREATE VIEW threads AS SELECT ... FROM sessions_v2`).
- *Assessment*: Rejected per **[INV-AGENT-01]**. View shims conceal incomplete migrations, prevent the Rust compiler and CI test suites from uncovering lingering legacy call sites, and perpetuate cognitive dissonance between documentation and physical schema.

### Option 2: Dual-Writing and Phased Deprecation
- Maintain both `sessions` and `threads` tables concurrently, writing to both in the single-writer actor for multiple release cycles.
- *Assessment*: Rejected per **[INV-AGENT-01]**. Dual-writing doubles write latency, introduces synchronization race conditions in the writer actor, and risks split-brain state during unexpected process crashes.

### Option 3: Uncompromising Clean Break to Canonical Thread Schema (Chosen)
- Formally ratify Migration 24 as a clean-break DDL transition:
  1. Purge all legacy zombie tables and deprecated views.
  2. Instantiate the pure canonical `threads` and `thread_policies` schema.
  3. Reorganize child tables to reference `thread_id` with strict `CHECK` constraints.
  4. Standardize all timestamps to `_ms`.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Retaining `rules_json` in `thread_policies`**
   - *Reason*: Storing `workspace_root` in a JSON column forces full table scans and `json_extract` expressions on every thread filter operation. Elevating it to a native SQL column enables B-tree indexing and referential integrity.
2. **Rejected: Preserving Retired Concept `'aside'` in Check Constraints**
   - *Reason*: Preserving `'aside'` in SQLite `CHECK(fork_kind IN ('trunk', 'fork', 'aside'))` directly undermines ADR-0058. Relaxed legacy constraints encourage downstream bugs and false-positive code branches.
3. **Rejected: Hybrid Millisecond and Second Timestamps**
   - *Reason*: Mixing `created_at_s` with `created_at_ms` causes arithmetic errors during sorting, window calculations, and TTL evaluations across different storage engines.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Canonical Physical Schema Topology (`[INV-SCHEMA-01]`)

```text
┌────────────────────────────────────────────────────────────────────────┐
│                          threads (Root SSOT)                           │
│  - id: TEXT PRIMARY KEY                                                │
│  - title: TEXT                                                         │
│  - status: idle | running | suspended | archived | terminated          │
│  - fork_kind: trunk | fork | subagent (no 'aside')                     │
│  - parent_thread_id: TEXT REFERENCES threads(id)                       │
│  - active_branch_id: TEXT NOT NULL DEFAULT 'main'                      │
│  - round_counter: INTEGER NOT NULL DEFAULT 0                           │
│  - created_at_ms / updated_at_ms: INTEGER NOT NULL                     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ 1:1
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   thread_policies (Normalized Governance)              │
│  - thread_id: TEXT PRIMARY KEY REFERENCES threads(id)                  │
│  - workspace_root: TEXT NOT NULL (First-class normalized column)       │
│  - system_role: TEXT NOT NULL (Normalized role identifier)             │
│  - unattended: INTEGER (0 or 1)                                        │
│  - max_context_tokens / compaction_trigger_tokens: INTEGER             │
│  - updated_at_ms: INTEGER NOT NULL                                     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ 1:N
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                          Relational Children                           │
│  - facts (thread_id, id, seq, kind, payload_json, created_at_ms...)    │
│  - branches (thread_id, branch_id, head_fact_id, revision...)          │
│  - checkpoints (thread_id, checkpoint_id, branch_id, summary...)       │
│  - context_views (thread_id, view_id, branch_id, basis_revision...)    │
│  - artifact_manifests (thread_id, artifact_id, content_hash...)        │
│  - artifact_refs (thread_id, artifact_id, fact_id)                     │
│  - request_manifests (thread_id, request_id, prompt_blocks_json...)    │
│  - read_leases / deletion_jobs / gc_continuations (thread_id...)       │
└────────────────────────────────────────────────────────────────────────┘
```

### 2. Standardized CQRS Projection View

```sql
CREATE VIEW thread_list_view AS
SELECT
    t.id,
    t.parent_thread_id,
    t.fork_kind,
    t.title,
    t.status,
    p.workspace_root,
    p.system_role,
    t.round_counter,
    (SELECT COUNT(*) FROM facts f WHERE f.thread_id = t.id AND f.kind = 'dialogue') AS msg_count,
    (SELECT json_extract(f.payload_json, '$.message.content')
     FROM facts f
     WHERE f.thread_id = t.id AND f.kind = 'dialogue' AND json_extract(f.payload_json, '$.message.role') = 'user'
     ORDER BY f.seq DESC LIMIT 1) AS last_user_prompt,
    t.created_at_ms,
    t.updated_at_ms
FROM threads t
LEFT JOIN thread_policies p ON p.thread_id = t.id;
```

---

## Invariants & Behavioral Boundaries

- **[INV-SCHEMA-01] Thread Entity SSOT**: All durable agent dialogue workloads MUST persist under `threads` table with `thread_id` foreign keys.
- **[INV-SCHEMA-02] Junk Drawer Elimination**: High-frequency operational fields MUST be modeled as typed SQL columns rather than serialized inside JSON strings.
- **[INV-SCHEMA-03] Monotonic Timestamp Uniformity**: All database time fields MUST be integers representing Unix epoch milliseconds named `*_ms`.
- **[INV-SCHEMA-04] Zero Legacy Burden**: No backward-compatibility shims, legacy view aliases, or zombie tables shall be preserved after Migration 24.

---

## Positive Consequences

- Eliminates semantic drift between UI/CLI (`Thread`) and SQLite schema (`sessions`).
- Reclaims disk space and reduces metadata bloat by removing zombie tables and triggers.
- Accelerates thread listing queries by eliminating JSON extraction overhead for workspace paths and roles.
- Standardizes all duration and timestamp logic across the entire persistence crate.
