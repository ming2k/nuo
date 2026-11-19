---
id: ADR-0217
title: "Context Projection Is Inspectable and Prefix Drift Is Reported"
status: accepted
date: 2026-12-19
scope: engine/cognition, server/handlers, persistence/thread, cli
superseded_by: null
supersedes:
  - "the unread `RequestProjection::prefix_fingerprint`"
negative_knowledge: true
---

# 0217. Context Projection Is Inspectable and Prefix Drift Is Reported

- **Status**: Accepted
- **Date**: 2026-12-19
- **Derives from**: [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)

## Context and Problem Statement

The repository already stores the evidence for context-cache loss and never reads
it. `RequestProjection` records a `prefix_fingerprint` per `(session, round,
turn)` — a deterministic identity of the cacheable prefix `S | H | I` — and is
persisted on every request. No code compares two consecutive fingerprints, so a
prefix that changes for a reason nobody intended (a workspace edit, a reordered
section, a tool activation) costs a full re-bill and produces no signal.

Nor is the prompt inspectable. An operator can see total token usage and a
compaction card, but not *which block* costs what, which blocks are inactive and
why, or what a projection dropped. `[INV-CONTEXT-04]` requires a lossy projection
to keep its escape hatch, and there is no surface that shows whether it did.

The consequence is that every quality question about the prompt ("is the catalog
worth 318 tokens?", "did my AGENTS.md change invalidate the cache?", "why did the
prompt shrink by 300 tokens?") is answered by reading source.

## Architectural Decisions

### 1. `ContextPlan` is a first-class value

`Policy::plan(&ContextInputs)` returns a plan, not just text:

```rust
pub struct ContextPlan {
    pub blocks: Vec<PlannedBlock>,
    pub fixed: Vec<Block>,
    pub transient: Vec<Block>,
    pub fixed_tokens: usize,
    pub transient_tokens: usize,
    pub notices: Vec<ContextNotice>,
    pub drift: PrefixDrift,
}

pub struct PlannedBlock {
    pub id: String,
    pub topic: Topic,
    pub rank: Rank,
    pub position: Position,
    pub stability: Stability,
    pub tokens: usize,
    pub elided: Option<Elision>,   // what was cut and how to get it back
    pub active: bool,
    pub inactive_reason: Option<&'static str>,
}
```

`inactive` entries are retained. A contributor asking "why is this block not in my
prompt" is the most common debug question, and an absent row cannot answer it.

### 2. Prefix drift is computed from the stored fingerprint

At each request the assembler compares the new `prefix_fingerprint` with the
previous one recorded for the same thread:

| Condition | `PrefixDrift` | Meaning |
| :--- | :--- | :--- |
| equal | `None` | cache hit on the prefix |
| changed, only `Workspace`/`Turn` blocks differ | `Suffix { blocks }` | expected; the tail moved |
| a `Frame` or `Frozen` block changed with no route change | `Head { blocks }` | a defect: this should be impossible |
| route changed | `Route` | expected; a new prefix is a new conversation context |

`Head` drift is logged at `warn` with the block ids. It is the only cheap
detector for an impure source, an unstable ordering, or a workspace value that
was classified too stably — the three ways this design can rot.

### 3. The plan summary rides the existing request-projection record

`RequestProjection` (one row per `(thread, round, turn)`, `payload` is JSON)
gains `blocks: Vec<PlannedBlockRecord>` and `drift: PrefixDrift`. Both are
`#[serde(default, skip_serializing_if = ...)]`, so records written before the plan
existed still decode and an empty vector means "not recorded" rather than "no
blocks".

No schema migration is needed: the row is a JSON document, and this is the record
that already carries `prefix_fingerprint` — the value drift is computed from, so
the evidence and the classification sit together.

`ContextProjectionCheckpoint` is **not** the home for this. It is reconstructed
from the transcript's directive history rather than stored, so extending it would
have meant inventing a table for derived state.

The record carries the **SHA-256** prefix, never an in-process memo hash: a
`DefaultHasher` value is not stable across builds and must never be persisted.

### 4. One rendered table, three entry points

| Entry point | Audience | Source |
| :--- | :--- | :--- |
| `/context` | interactive session | live `ContextPlan` |
| `nuo context [--json] [--diff ROUND]` | operator, CI | persisted records |
| `nuo context --json` in tests | CI | golden snapshot of the default policy |

All three render the same `PlannedBlock` set, so a discrepancy between what the
model saw and what the operator sees is a rendering bug, not a second code path.

### 5. The golden snapshot is the anti-regression mechanism

`nuo context --json` over a fixture workspace is pinned. Any prompt change becomes
a reviewed diff with a token delta per block. This is the mechanism that makes
duplication visible: two blocks carrying the same sentence render as two rows with
a suspiciously equal cost, and the reviewer sees it. `[INV-CTXSRC-01]` catches it
structurally at compile time; the golden catches paragraph-level duplication that
owns different topics.

## Normative Invariants

- **`[INV-CTXPLAN-01]` Plan/Request Agreement** — The `fixed` and `transient`
  strings in a `ContextPlan` are byte-identical to the `InstructionText` in the
  request assembled from that plan. Asserted in the assembler's tests, so an
  inspection surface can never show a prompt the model did not receive.
- **`[INV-CTXPLAN-02]` Drift Is Reported** — A `Head` drift is logged as a defect,
  never swallowed.
- **`[INV-CTXPLAN-03]` Persisted Identity Is Stable** — Records carry the SHA-256
  prefix only; no in-process hash is ever persisted.

## Consequences & Trade-offs

- **Positive**: prefix drift becomes a signal instead of an unexplained bill;
  every prompt-quality question is answerable without reading source; a prompt
  change is a reviewable diff; the plan is the single source of truth for the
  renderer *and* the inspector.
- **Negative & Incurred Costs**: the projection row grows with the block count
  (bounded by the source count and the retention cap that already prunes the
  archive); the `--json` golden must be updated deliberately on every prompt
  change, which is friction by design. `PlannedBlock` duplicates a little of
  `Block` (id, tokens) — accepted, because `Block` is the wire fragment and
  `PlannedBlock` is the accounting record.
- **Neutral**: no new crate and no new process. `/context` and `nuo context`
  read the same `Policy`.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Log block costs at `debug` and read the logs

- **Why considered**: Zero new surface.
- **Why rejected**: The question is asked during review, not during a session, and
  a log line cannot be diffed against a previous build.

### Alternative B — Derive drift from token totals

- **Why considered**: No fingerprint comparison needed.
- **Why rejected**: A total is invariant under a swap of two equal-cost blocks, and
  a reorder is exactly the drift that must be caught.

### Alternative C — A separate `nuo context show` binary

- **Why considered**: Keeps the CLI surface small.
- **Why rejected**: `nuo` already owns the CLI; a second binary would need its own
  instance discovery and config loading for no benefit.

### Alternative D — Persist the whole rendered prompt per request

- **Why considered**: Maximum forensic value.
- **Why rejected**: Duplicates the transcript in durable storage and grows the
  database without bound. The plan summary plus the fingerprint answers the
  operational questions; `RequestProjection` already stores the request-local tail
  verbatim, which is the only part not reproducible from the thread.

## Compliance and Verification

- `crates/engine/src/cognition/context/tests.rs` — plan/request byte agreement,
  drift classification (none / suffix / head / route).
- `tests/integration/` — `nuo context --json` golden; persisted record round-trip.
- `tests/integration/governance/` — the golden is scanned for paragraph-level
  duplication across blocks.

## Links

- [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)
- [ADR-0153](0153-effective-input-ceiling-tool-output-retention-and-ingest-image-normalization.md)
- [ADR-0198](0198-context-checkpoints-preserve-active-work-state.md)
- [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)
