---
id: ADR-0180
title: "Single Ownership of Shared Vocabulary: Retire Compatibility Facades"
status: accepted
date: 2026-11-14
scope: protocol, tool, provider, server, engine, persistence, workspace
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0180. Single Ownership of Shared Vocabulary: Retire Compatibility Facades

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0008](0008-single-tool-contract.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)

## Context and Problem Statement

The workspace has two vocabularies that every crate needs: the tool-facing
vocabulary (`Tool`, `ToolOutput`, `Message`, `Role`, …) and the provider/wire
vocabulary (`ModelRequest`, `ToolSpec`, `Model`, `Effort`, `Provider`, …).
After several crate splits, both vocabularies acquired **two import paths**:

- `nuo-tool` defines `ToolOutput` / `Message` / `Role`; `nuo-protocol`
  re-exports them, and 124 / 62 / 141 call sites import them through
  `nuo-protocol`.
- `nuo-protocol` defines `Effort` / `Model` / `ToolSpec`; `nuo-provider`
  re-exports them, and 275 / 22 / 16 call sites import them through
  `nuo-provider`.
- 29 modules in the workspace contain nothing but a re-export.
- `ToolContext` exists twice with different meanings (call-time context in
  `nuo-tool`, factory-time service map in `nuo-protocol`); `ToolScope` exists
  twice (effect domain in `nuo-tool`, capability selection in `nuo-protocol`).
- `ToolDescriptor` is documented as the canonical tool metadata, but no tool
  implements it and no non-test consumer reads it. The live model-request shape
  is `ToolSpec`.

The facade was introduced for source compatibility during the splits. The
workspace is closed and lockstep-versioned, so compatibility has no consumer;
it only hides the real dependency direction.

## Architectural Decisions

### 1. One canonical owner per public type

| Vocabulary | Owner | Examples |
| :--- | :--- | :--- |
| Tool contract and tool-facing data | `nuo-tool` | `Tool`, `ToolContext`, `ToolInvocation`, `ToolOutput`, `Message`, `Role`, `ToolCall`, `ToolAccess`, `PreviewResult`, `BuiltinTool`, `ScopeTarget`, `ToolScope` (effect domain) |
| Wire protocol and provider-neutral domain | `nuo-protocol` | `Wire`, `AgentRequest`, `AgentResponse`, `ModelRequest`, `ToolSpec`, `Provider`, `Model`, `Effort`, `Pricing`, `ContextPolicy`, thread/SSOT types |
| Tool assembly registry | `nuo-protocol` | `ToolSet`, `ToolFactory`, `ToolFactoryContext`, `CapabilityScope`, `ToolSelection`, `ToolDeclaration` |
| Provider transport, credentials, catalog, concrete specs | `nuo-provider` and `providers/*` | `ProviderPipeline`, `CredentialStore`, `ModelProviderSpec`, OAuth flows |

### 2. No cross-crate re-export modules

Every module whose body is only `pub use other_crate::...` is deleted. A crate
root may expose its **own** items; it may not re-export another crate's module
wholesale. The eight glob re-exports in `nuo-protocol` and fourteen in
`nuo-provider` are removed.

### 3. Two names are made unambiguous

- `nuo-protocol::ToolContext` becomes `ToolFactoryContext`;
  `ToolContextBuilder` becomes `ToolFactoryContextBuilder`.
- `nuo-protocol::ToolScope` becomes `CapabilityScope`. `nuo-tool::ToolScope`
  keeps its name (the tool's effect domain).

### 4. `ToolDescriptor` is deleted

The `Tool` trait's accessors remain the canonical metadata surface. `ToolSpec`
is the provider-facing projection built from them. The speculative descriptor
and its builder are removed, and `[INV-TOOL-09]` is superseded by
`[INV-VOCAB-01]`.

### 5. Invariants

- **`[INV-VOCAB-01]` Single Definition**: No public type may be defined in two
  crates. A test scans public type names across the workspace and fails on a
  duplicate.
- **`[INV-VOCAB-02]` Canonical Import**: No crate may `pub use` another crate's
  public module wholesale. A test rejects `pub use <crate>::*` across crate
  boundaries.
- **`[INV-VOCAB-03]` No Compatibility Facade**: A module whose only code is a
  re-export of another crate is a defect, not a staging state.

## Consequences & Trade-offs

- **Positive**: One place to change a type; import paths reveal the dependency
  graph; the two `ToolContext`/`ToolScope` meanings can no longer be confused;
  the workspace loses ~30 files of indirection.
- **Negative & Incurred Costs**: A large mechanical call-site sweep. The
  compiler catches every missed path; no behavior changes are permitted in the
  same change. This ADR deliberately accepts a wide diff.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep facades and mark them deprecated
- **Why considered**: Minimal call-site churn.
- **Why rejected**: A closed lockstep workspace has no external compatibility
  consumer. Deprecation preserves the wrong mental model and lets new code keep
  using the facade.

### Alternative B — Move the tool vocabulary into `nuo-protocol`
- **Why considered**: `nuo-protocol` is already the dominant import path.
- **Why rejected**: It creates a cycle (`nuo-protocol` needs `Tool` for its
  assembly registry) and destroys the zero-I/O tool leaf.

### Alternative C — Add a `nuo-vocabulary` crate
- **Why considered**: A neutral home for both vocabularies.
- **Why rejected**: It recreates the catch-all `contracts`/`common` crate that
  ADR-0001 bans and gives every layer a reason to depend on every type.

## Links

- [ADR-0008](0008-single-tool-contract.md)
- [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md)
- [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)
- [ADR-0055](0055-hierarchical-workspace-topology-and-component-namespace.md)
