---
id: ADR-0182
title: "Context Policy Is Configuration; the Phase-A Context Substrate Is Deleted"
status: accepted
date: 2026-11-14
scope: protocol/context, persistence/config, engine/agent, engine/projection
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0182. Context Policy Is Configuration; the Phase-A Context Substrate Is Deleted

- Status: Accepted
- Date: 2026-11-14
- Amends: [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
- Supersedes: the Phase-A context lifecycle substrate described in the retired
  `nuo-protocol::context_lifecycle` module.

## Context and Problem Statement

`nuo-protocol::context_lifecycle` was introduced as "Phase A" of a future
context architecture. It declared typed fact/view/checkpoint/artifact shapes,
execution groups, retrieval handles, and a budget algebra beside the then-live
`SessionIR` model. Its own documentation promised that "the cutover that removes
the legacy representation is Phase F".

That cutover has happened differently: [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md),
[ADR-0174](0174-thread-command-receipts-fencing-leases-and-blob-liveness-share-one-durable-door.md),
and [ADR-0175](0175-session-ir-is-deleted-and-the-transcript-is-a-projection.md)
made the thread fact log the durable SSOT; the engine-side `context_lifecycle`
module was deleted. The protocol copy remained: 2,387 lines, with the obsolete
`SessionIR` narrative and almost no consumers. Most exported types
(`FactNode`, `RequestBlock`, `RequestManifest`, `TaskState`, `ExecutionRecord`,
`ContextView`, `ArtifactManifest`, `SourceManifest`, the typed IDs, execution
groups, retrieval handles) have zero references outside the module.

The live surface is a compact configuration policy: watermarks, effective input
ceiling, retention bands, and the budget derivation used by compaction.

## Architectural Decisions

### 1. The Phase-A substrate is deleted

`nuo-protocol/src/context_lifecycle/` and every type that exists only to
describe the retired phase are removed. No alias module, no deprecated
re-export, and no dead "future shape" remain in the shipped crate.

### 2. The live policy is a compact `context_policy` module

`nuo-protocol/src/context_policy.rs` owns exactly what the runtime uses:

- `ContextPolicy` with only live fields: `schema_version`, `watermarks`,
  `preferred_recent_rounds`, `checkpoint_enabled`,
  `lightweight_degradation_enabled`, `fallback_window_tokens`,
  `max_input_tokens`, `recent_observation_protect_tokens`, and
  `tool_output_retention_tokens`.
- `WatermarkPolicy` and its validation.
- `PolicyError` and the constants required by the live policy.
- `ContextPolicy::resolve_budget(window) -> ContextBudget`.

The dead fields (`capture_*`, `checkpoint_max_*`, `checkpoint_*_timeout_ms`,
`gc_*`, `inspect_page_tokens`, `session_artifact_quota_bytes`,
`attempt_diagnostic_ttl_ms`) are removed. They configured a checkpoint/artifact
engine that no longer exists.

### 3. Legacy-key rejection is deleted

`LegacyKeyRejected` and `reject_legacy_runtime_key` have no call site. The
runtime configuration parser already rejects unknown keys through
`serde(deny_unknown_fields)`; a second, unused rejection helper is dead code.

### 4. Persistence and engine import the new path

`nuo-persistence::config`, `nuo-persistence::db::migration_policy`, and
`nuo-engine` import `nuo_protocol::context_policy`. No source file references
`context_lifecycle`.

### 5. Invariant

- **`[INV-CONTEXT-06]` No Dead Context Substrate**: The runtime context model
  is the compact policy plus the live `ContextBudget`/`CompactionPolicy`.
  A type that does not participate in request assembly, compaction, or
  configuration is not part of the context module.

## Consequences & Trade-offs

- **Positive**: The protocol crate stops advertising a parallel, unwired
  context architecture; readers see one policy and one budget path; the
  obsolete SessionIR narrative is gone.
- **Negative & Incurred Costs**: Existing `context.toml` documents that
  contain the dead fields will fail `deny_unknown_fields` and must be trimmed.
  This is a deliberate clean break, consistent with the project's no-legacy
  posture.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep the module and mark it `#[doc(hidden)]`
- **Why considered**: Preserves a future design sketch.
- **Why rejected**: A hidden 2k-line module is still compiled, exported, and
  maintained. Design sketches belong in `docs/rfc/`, not in a shipped crate.

### Alternative B — Keep the dead fields for config compatibility
- **Why considered**: Avoids rewriting existing config files.
- **Why rejected**: There is no consumer for those fields. Accepting them
  silently would be a compatibility lane for a feature that never shipped.

## Links

- [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
- [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md)
- [ADR-0175](0175-session-ir-is-deleted-and-the-transcript-is-a-projection.md)
