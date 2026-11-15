---
id: ADR-0198
title: "Context Checkpoints Preserve Active Work State"
status: accepted
date: 2026-11-15
scope: engine/compaction, persistence/thread, context
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0198. Context Checkpoints Preserve Active Work State

- Status: Accepted
- Date: 2026-11-15
- Amends: [ADR-0182](0182-context-policy-is-configuration-and-phase-a-substrate-is-deleted.md)

## Context and Problem Statement

The todo list lives in a durable `State` entry, deliberately excluded from
the model-facing projection. That is correct for the live window (the todo
tool emits its own result), but it meant a compaction fold could erase the
active plan: the summary prompt had no todo section, and the deterministic
fallback only kept user prompts and a file list. A model resuming after
compaction could not see what it had already finished or what remained.

Two smaller defects sat on the same path. Mid-turn compaction passed `None`
as the provider, so the only relief available to a long subagent round always
used the low-fidelity deterministic excerpt. And the deterministic excerpt
sliced `&message.content[..300]`, which panics when a multi-byte character
straddles byte 300.

## Architectural Decisions

### 1. The todo list is an explicit checkpoint input

`compact_thread` receives the rendered todo list, appends it to the
summarization context as `<active_todo_list>`, and passes it to the
deterministic fallback. The summary prompt requires an `## Active Task State`
section that preserves each item's status and the next concrete action.
The caller reads the list through `ThreadSession::todos()` (the same durable
`State` mirror the UI uses), not through a second scan.

### 2. Mid-turn compaction is model-assisted when a provider exists

`PruningProjectionGate` carries the live provider and passes it to
`compact_round_history_with_mode`. The deterministic fallback remains for
embeddings and tests that intentionally have no model, but the production
gate no longer chooses it by construction.

### 3. Byte truncation is char-boundary safe

The deterministic excerpt walks back to the nearest `is_char_boundary` before
cutting. A checkpoint can no longer panic on CJK text.

### 4. The durable transcript remains the source of truth

The todo block is summary text, not a replacement for the `State` entry. The
entry stays in the durable transcript and is still the UI's source; the
checkpoint is the model-visible projection of it.

## Consequences

- **Positive**: a folded session retains its plan; long subagent rounds get a
  real summary; CJK prompts no longer crash compaction.
- **Negative & Incurred Costs**: every compaction pays the todo render cost
  (small) and the summary token budget must leave room for the task state.
  The summary prompt is longer.
- **Neutral**: manual `/compact` and automatic compaction share the same
  checkpoint format.

## Discarded Alternatives (Negative Knowledge)

- **Keep the todo state as a model-visible fact**: leaks a harness bookkeeping
  entry into every request and invites the model to edit it directly.
- **Re-inject the todo tool result after compaction**: only works if the
  latest result survived the cut; the fold may be older than the result.
- **Leave mid-turn deterministic**: cheapest in tokens, but it discards the
  reasoning and tool results that make a long round resumable.

## Links

- [ADR-0175](0175-session-ir-is-deleted-and-the-transcript-is-a-projection.md)
- [ADR-0182](0182-context-policy-is-configuration-and-phase-a-substrate-is-deleted.md)
