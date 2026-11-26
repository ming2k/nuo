---
id: ADR-0264
title: "Durable Append-Only Projection Directives for Tool Pruning and Media Offload"
status: accepted
date: 2026-11-29
scope: protocol/transcript, persistence/thread, engine/orchestration
superseded_by: null
amends:
  - "[ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md) (clarifies that model-context prune and media-offload decisions persist as append-only `ProjectionDirective` records over immutable entries rather than mutating facts or silently dropping on restart)"
negative_knowledge: true
---

# 0264. Durable Append-Only Projection Directives for Tool Pruning and Media Offload

- **Status**: Accepted
- **Date**: 2026-11-29
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md)
- **Related**: [ADR-0236](0236-the-session-journal-is-the-durable-source-of-truth-and-sqlite-is-a-derived-projection.md), [ADR-0246](0246-compaction-folds-the-model-window-never-the-conversation.md), [ADR-0255](0255-versioned-content-shards-pure-catalog-plane-and-durability-boundaries.md), [ADR-0256](0256-retire-parallel-thread-store-and-unify-on-journal-session.md)

---

## Context and Problem Statement

When `ThreadSession::commit_turn` became strictly append-only under [ADR-0256](0256-retire-parallel-thread-store-and-unify-on-journal-session.md) (retiring `CommitBasis::Reprojected` and `SaveMode::FullRewrite` from the turn path), `ThreadSession::commit_context_projection` was left as an in-memory stub that updated only `state.data.last_projection` without appending a `ProjectionDirective` to `Transcript::directives` or writing a journal delta.

Meanwhile, the durable transcript model (`protocol::Transcript`) already separates **immutable entries** (`entries`, surfaced verbatim by `conversation_messages()`) from **append-only projection directives** (`directives`, applied by `Transcript::project()` to derive `model_window()`). Leaving `commit_context_projection` unpersisted caused two concrete production regressions:

1. **Repeated media re-offload and invoice loss**: After the pre-turn payload gate persisted evicted screenshots to content-addressed spool paths and emitted `[Image Offloaded: ...]` invoices, `session.model_window()` still returned the raw multi-megabyte attachments on every subsequent round and after session reopen — re-running offload, rewriting spool artifacts, and re-emitting `PayloadRelief` notices on every turn.
2. **Lossy partial media offload representation**: `PrunedMediaOutput` previously cleared `message.images = None` unconditionally, which could not represent a single user message that offloaded its older screenshots while sparing its newest attachment (`PAYLOAD_KEEP_NEWEST_IMAGES`).

---

## Decision Drivers

- **Immutable Facts (`[INV-THREADSSOT-02]`, `[INV-CMP-08]`)**: Human-facing history (`full_transcript()` / `conversation_messages()`) must never lose an entry, strip an image, or mutate an observation body.
- **O(delta) Append-Only Durability**: Persisting a prune or media-offload projection must append one small `ProjectionDirective` in a `SaveMode::Delta` journal frame — never rewriting prior entries, clearing `ThreadData::graph`, or duplicating base64 image pixels.
- **Lossless Partial Attachment Offload**: A directive must be able to keep selected attachments inline on an entry while replacing the evicted attachments with their content-addressed artifact invoices.
- **Restart & Next-Round Convergence**: Reopening a session or starting the next round must observe the same projected `model_window()` that the provider actually received, avoiding redundant offloading and prompt-cache thrashing.

---

## Architectural Decisions

### 1. `[INV-PROJ-01]` Tool pruning and media offload persist as append-only `DirectivePayload::Prune` directives

- `PrunedMediaOutput` carries `retained_image_indices: Vec<usize>` (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`), indexing into the immutable entry's `images` vector:
  - Empty `retained_image_indices` clears all images on the projected message (`message.images = None`).
  - Non-empty `retained_image_indices` retains strictly those original attachments in order, with zero base64 payload duplicated inside the directive.
- `ThreadSession::commit_context_projection` diffs the projected `model_window` prefix against the current `transcript.project()` entries:
  - Tool-role entries whose content changed emit `PrunedToolOutput { tool_call_id, placeholder }`.
  - Entries whose attachments changed emit `PrunedMediaOutput { seq, placeholder, retained_image_indices }` with indices resolved against the immutable entry in `transcript.entries`.
  - When non-empty, it appends one `ProjectionDirective { kind: DirectiveKind::Prune, up_to_seq, payload: DirectivePayload::Prune { elided, pruned_media } }` and persists strictly that directive via `SaveMode::Delta` with `SessionUndo::appending` rollback protection.
- `ThreadSession::last_projection()` classifies directives whose payload contains only `pruned_media` as `ContextProjectionKind::Payload` and tool-output elisions as `ContextProjectionKind::Prune`.

---

## Consequences & Trade-offs

- **Positive**: `model_window()` converges immediately and across restarts without re-offloading images or re-pruning stale tool outputs on every turn.
- **Positive**: `full_transcript()` / `conversation_messages()` stays 100% immutable — every original user image and tool body remains intact in the append-only journal.
- **Positive**: Partial offload on multi-image prompts preserves newest inline screenshots while recording zero extra image bytes in the journal directive.
- **Negative & accepted**: `commit_context_projection` performs one lightweight `SaveMode::Delta` journal append when committed entries are pruned or offloaded (already amortized behind `SessionJournal` group commit).

---

## Discarded Alternatives (Negative Knowledge)

- **Keep `commit_context_projection` ephemeral (in-memory only, dropped across rounds/restarts).** Every subsequent round on a session with large screenshots would re-exceed the body ceiling, re-run media offload, re-emit `PayloadRelief` notices, and invalidate prompt prefixes.
- **Restore `CommitBasis::Reprojected` + `SaveMode::FullRewrite` on `commit_turn`.** Rewriting `transcript.entries` violates `[INV-THREADSSOT-02]` and `[INV-CMP-08]`, destroys original images from human history, clears `ThreadData::graph`, and turns an O(delta) commit into an O(history) rewrite.
- **Store retained base64 image copies inside `PrunedMediaOutput`.** Duplicating megabytes of base64 pixels inside a projection directive defeats the purpose of offloading and bloats the journal. Storing indices relative to the immutable entry achieves zero-copy retention.
