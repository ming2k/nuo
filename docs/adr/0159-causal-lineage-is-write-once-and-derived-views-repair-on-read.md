---
id: ADR-0159
title: "Causal Lineage Is Write-Once; Derived Views Repair on Read"
status: accepted
date: 2026-11-10
scope: [workspace]
superseded_by: null
negative_knowledge: true
---

# ADR-0159: Causal Lineage Is Write-Once; Derived Views Repair on Read

- **Status:** Accepted; §INV-IR-04 amended by [ADR-0160](0160-compaction-is-a-transcript-fact-and-the-causal-node-is-a-derive.md)
- **Date:** 2026-11-08
- **Supersedes:** none
- **Related:** [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md), [ADR-0158](0158-derived-indexes-are-built-off-the-commit-path-from-durable-cursors.md), [ADR-0160](0160-compaction-is-a-transcript-fact-and-the-causal-node-is-a-derive.md)

## Context

On 2026-11-08 the local server was killed twice by the kernel OOM killer
(`anon-rss` 17.3 GB and 22.9 GB) while a single session was open. The log stopped
mid-turn before the kill: the process was no longer reaching its event loop.
The user-visible symptoms were `● saving request` sitting for tens of seconds,
then `● compacting context` never finishing, and the TUI dying when the link
dropped.

The session's transcript holds 926 entries / 1.6 MB, so the memory was not
conversation data. The derived causal graph held 926 nodes, but walking
`parent_id` from the newest node never terminated: node `seq 0` had
`parent_id = <node seq 922>`, and `seq 922` descended back through `seq 0`.
`CausalGraph::linear_path` followed parent edges in a `while` loop and pushed
every node it saw. The cycle therefore produced an unbounded `Vec` — the
22 GB — while holding the compaction task.

### How the cycle was written

Two independent defects combined:

1. **A whole-transcript rebuild advanced the cursor by one batch.**
   `derived::advance_causal` rebuilt the *entire* transcript when it detected a
   missing or incomplete graph, but `advance_session` then set `causal_seq` to
   `cursor + budget` (128 entries). The next drain batch believed entries it had
   just rebuilt were still unmigrated and mirrored them again.
2. **The mirror rewrote existing parent edges.**
   `mirror_transcript`'s upsert was
   `ON CONFLICT(session_id, id) DO UPDATE SET parent_id = excluded.parent_id`.
   Its purpose was to repair legacy nodes whose `parent_id` was `NULL`
   (migration 17), but it also rewrote *valid* edges: the first re-mirrored
   entry was linked to `newest_position` — a node later in the chain — closing
   the loop.

34 sessions in the live database carry the same signature (one forward
dialogue edge each). None carried a compaction node, so no folded summary was
at risk.

### A second, independent defect: folds did not reach the window

Compaction produced a durable IR compaction node via `commit_session_ir`, but
`model_window()` — the seed of the next round — projects the **transcript**, and
that projection carried no `Compact` directive. Every new send therefore
reloaded the full (375 k token) history and re-ran the summarizer over the same
prefix. `projections` contained zero `compact` rows on a database that had been
compacted many times.

## Decision

**[INV-PERSIST-05] Causal lineage is write-once.** A mirror may fill a `NULL`
parent (legacy repair), never overwrite an existing one. Re-mirroring is
replay, not reparenting.

**[INV-PERSIST-06] A whole-transcript rebuild advances the cursor to the
rebuilt end.** Repair is not a batch: the drain records that the *entire*
transcript is covered in the same transaction. The batch itself is one
`BEGIN IMMEDIATE`/`COMMIT`: graph rows, cursor, FTS rows, and the pending row
describe a single position, so committing them one statement at a time both
paid an fsync per row on a `synchronous=FULL` connection and could leave the
cursor disagreeing with what was actually mirrored after a crash. A guard rolls
the transaction back if the work unwinds.

**[INV-PERSIST-07] Derived views repair from the durable transcript on read.**
A flush probes for a lineage edge that is not append-ordered
(`parent.seq >= child.seq`, or a self-edge — a cycle necessarily contains one).
If found, the session's graph is rebuilt from the transcript and the cursor
moved to the membership end — even when the cursors claim full coverage. No
one-time schema migration: the durable transcript is the only authority, and a
derived view must remain self-healing for *any* future defect, not only this
incident's. Zero compaction nodes existed across the affected sessions, so the
rebuilds cannot lose a folded summary.

**[INV-IR-03] A lineage walk terminates.** `linear_path` tracks visited node
ids and stops on the first revisit. A read is never allowed to convert malformed
stored data into unbounded memory; the path it returns is the longest acyclic
prefix.

**[INV-IR-04] A fold is durable only when the window it produced is durable.**
`commit_compaction_checkpoint` appends a checkpoint entry and a `Compact`
directive to the transcript, then `commit_session_ir` records the graph. The
archived entries remain durable and auditable; the directive hides them from
the projection. A reopen, a new turn, and a `/retry` all seed from the compacted
window, so the summarizer runs once per fold rather than once per send.

The directive's boundary is the **last folded node that exists as a transcript
entry**, resolved from the folded node ids. A message count or a raw sequence
maximum is wrong when the folded prefix contains IR-only nodes (observations,
prior compaction anchors) or the transcript contains hidden `state` entries:
both shift positions without shifting what is actually archived.

The projection is committed **before** the graph. They are two writer
transactions, and only the projection governs the next request. If the process
dies between them, the failure mode is an unfolded graph over an
already-compacted window: the next send is still small, and a later fold
re-reads the full lineage and rewrites the projection. The opposite order would
leave the graph folded over a full window, and the next fold — anchored at the
horizon — could no longer see the archived head it must summarize.

## Consequences

- Existing corrupted graphs heal lazily: the first `session_ir()` / search flush
  that touches a session rebuilds it from the transcript (milliseconds for a
  1.6 MB session).
- Until then the graph is inert, not dangerous: every walker is cycle-safe.
- Compaction now costs one summarization call per fold. A session whose window
  remains above the threshold after a fold (a tail larger than the target) will
  still report that it folded but may not shrink below the trigger; that is a
  budget-policy question, no longer a persistence one.
- The drain pays one indexed `EXISTS` probe per batch
  (`causal_nodes` PK `(session_id, id)`), and `flush_session` pays it once per
  read-your-writes boundary.
- A rebuild clears `threads.active_leaf`: after deleting the facts it named, a
  dangling leaf made `load_session_ir` resolve an empty active branch instead of
  falling back to the rebuilt tail.
- Compaction is committed as **two** writer transactions (projection, then
  graph). There is no cross-store transaction that would cover both without
  duplicating the transcript-save machinery, and the ordering above makes the
  split benign and self-correcting. This is the one deliberate residual: it is
  a performance-consistency window, never a data-loss window — the transcript
  and the archived entries are untouched either way.

## Alternatives rejected

- **A one-time migration that deletes corrupt graphs.** It repairs this
  incident but not the next one, duplicates the read-path logic, and would
  freeze repair behaviour into a version number.
- **Ignoring forward edges and relying on the cycle guard alone.** The guard
  prevents the OOM, but compaction would keep reading a graph whose head is
  linked into its own tail; the correctness bug would remain.
- **Making `model_window` read from the IR.** The transcript projection also
  carries prune elisions and remains the factual view used by non-agent
  surfaces. Recording the fold as a projection keeps one window authority and
  preserves the archived entries.
