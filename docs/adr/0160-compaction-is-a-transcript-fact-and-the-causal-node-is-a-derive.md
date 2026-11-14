---
id: ADR-0160
title: "Compaction Is a Transcript Fact; the Causal Node Is a Derive"
status: accepted
date: 2026-11-10
scope: [workspace]
superseded_by: null
negative_knowledge: true
---

# ADR-0160: Compaction Is a Transcript Fact; the Causal Node Is a Derive

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [RFC-0001](../rfc/0001-compaction-as-a-transcript-fact.md), [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md), [ADR-0158](0158-derived-indexes-are-built-off-the-commit-path-from-durable-cursors.md), [ADR-0159](0159-causal-lineage-is-write-once-and-derived-views-repair-on-read.md)
- **Supersedes in part:** ADR-0159 §INV-IR-04 (the two-write commit) and the "compaction node is written by `commit_session_ir`" half of ADR-0158

## Context

ADR-0159 made compaction visible to the transcript projection but left two
durable representations of one fact: the checkpoint entry/directive and the IR
`Compaction` node, written by two separate writer transactions. It also left the
compactor walking `linear_path`, which ignores `state.compaction_horizon`; the
kept tail's head deliberately keeps its parent pointing into the folded chain,
so the next fold re-read the archived prefix and re-summarized it twice. A
verbose summary (the IR path had no clamp) could then push the window back over
the gate, and 34 graphs in the live database still had a rewritten parent edge
(a cycle) that only healed when a reader touched them.

The RFC in `docs/rfc/0001` lays out the full analysis; this ADR records the
decision that was implemented.

## Decision

### [INV-CMP-01] A checkpoint is a typed transcript fact

`EntryPayload::Compaction(CompactionMeta)` carries the structure the causal
graph needs: `first_kept_node_id`, `split_parent_node_id`,
`folded_through_seq`, `tokens_before`, and the file manifest. The entry keeps
`kind = Message`, `role = User`, `origin = Checkpoint`, `hidden = true`; the
rendered summary is `entry.content` and `to_message` applies the shared
`CHECKPOINT_HEADER`. `EntryKind` does not grow a third variant: the SQLite CHECK
constraint only admits `message|state`, and rebuilding a 3.2 GB `entries` table
would buy nothing but a longer migration.

### [INV-CMP-02] The graph is derived from the transcript

`ir_mirror::plan_nodes` maps a checkpoint entry to exactly one `Compaction`
node. Its parent is `CompactionMeta.split_parent_node_id` (the last folded
node), **not** the previous row in sequence order, and the plan does not advance
its dialogue cursor through it. The checkpoint entry is appended at the end of
the transcript, but semantically it sits before the kept tail; chaining through
it would make the horizon an ancestor of the tail and hide the tail from
`linear_path`. The horizon walk (`linear_path_with_horizon`) injects the node
when it reaches `first_kept_node_id`, preserving the `INV-CHK-01` shape.

A checkpoint written by the intermediate ADR-0159 format (a plain message with a
checkpoint origin) has no structure to derive a horizon from and is skipped by
the plan. No production database contains one; the format is a clean break, not
a compatibility lane.

### [INV-CMP-03] Compaction is one writer transaction

`SessionStore::commit_compaction` appends the checkpoint entry and the `compact`
directive in one `save_session_inner` transaction and writes no graph row.
`commit_session_ir` is no longer on the compaction path. A crash after the
commit is recovered by the next flush: the entry is the only fact and the node is
rebuilt from it. This removes residual A (the two-step commit) by construction,
not by ordering the steps more carefully.

The entry id is the in-memory horizon id: the compactor generates it, the
session adopts it, and the derived node uses it. A reload therefore names the
same node.

### [INV-CMP-04] The compactor honors the horizon

`CausalCompactor::compact_session_ir` walks `resolve_active_branch()` — the same
walk every other consumer uses — instead of `linear_path`. It seeds
`previous_summary` and the file manifest from the horizon node, so a fold that
starts at the horizon still carries the prior summary. A second fold examines
only the unfolded tail; a target that fits the active path produces no cut.

### [INV-CMP-05] A fold has a post-condition

The tail budget is
`gate - request_overhead - summary_budget` (floored to avoid a degenerate
target); the summary is clamped to 2 000–24 000 tokens after either summarizer;
`find_cut_point_nodes` keeps **at most** the tail budget (it previously kept at
least the target) and folds a tool call/result pair together. The outcome
reports `satisfied = window_after + overhead <= gate`. An unsatisfied fold is
kept (it is still smaller) but logged and surfaced as an `AgentNotice`; it is
never reported as a successful shrink.

### [INV-CMP-06] Derived corruption heals without a one-off migration

`flush_session` probes structure (self-edge, orphan parent, non-increasing seq)
and coverage (graph max seq < last mirrorable entry) whenever the cursor claims
coverage, then rebuilds from the transcript in one transaction. A bounded online
scrub (32 sessions every 5 s, only while the writer has no queued command, cursor
in `derived_meta`) runs the same routine, so the 34 damaged graphs and any
future damage heal in the background. Schema **v29** adds `derived_meta` and
clears `threads.active_leaf` for `fork_kind = 'trunk'`: a main-line lineage
position is the newest dialogue fact, not a value the old IR-commit path
persisted.

## Consequences

- One durable owner per compaction; the graph can be deleted and rebuilt without
  changing the window or the horizon (tested).
- A crash between the transcript commit and the drain loses no fact: the drain
  reconstructs the node.
- A second compaction no longer re-reads the archived prefix; the summary and
  its file manifest carry forward from the horizon.
- The cut is bounded by the gate and the summary is bounded; a pathological
  single node is reported as `CompactionInsufficient` rather than silently
  re-triggering.
- The 34 existing corrupt graphs repair in the background; the writer remains
  responsive (bounded slice, idle-only, resumable cursor).
- Schema v29 is a hard break for older binaries (the version gate refuses it).
  There is no dual-format reader and no converter: the live database has zero
  compaction entries/nodes, so nothing needs migrating.
- IR-only execution facts (observations) remain a second source written by the
  kernel's `FactSink`. This ADR does not claim the IR is fully derived; it
  removes the compaction duplication only and records that boundary explicitly.
  The live census (2026-11-08) is 393 614 causal nodes, all `dialogue`.

## Alternatives rejected

See RFC-0001 §10. In short: a combined writer command would preserve two
representations; a message-payload checkpoint would be stringly typed; a
one-off migration would freeze repair logic; making `model_window` read the IR
would drop prune elisions and the factual view used by non-agent surfaces; and a
third `EntryKind` would force a full table rebuild for no reader benefit.
