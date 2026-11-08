# RFC-0001: Compaction Is a Transcript Fact; Derived Views Are Caches

- **Status:** Accepted — implemented by [ADR-0160](../adr/0160-compaction-is-a-transcript-fact-and-the-causal-node-is-a-derive.md)
- **Date:** 2026-11-08
- **Related:** ADR-0155 (O(delta) persistence), ADR-0158 (derived indexes off the
  commit path), ADR-0159 (write-once lineage, read-time repair)
- **Supersedes in part:** ADR-0159 §INV-IR-04 (the two-write compaction commit),
  and the "compaction node is written by `commit_session_ir`" half of ADR-0158

## 0. Recommendation (TL;DR)

Make the **transcript the only durable owner of a compaction**, and derive the
causal `Compaction` node from the transcript entry during the normal derived
drain.

Concretely:

1. Add a typed `EntryPayload::Compaction(CompactionMeta)` to the checkpoint
   entry. The entry already holds the rendered summary; the payload carries the
   structural facts (first kept node, split parent, folded boundary, files,
   tokens).
2. `ir_mirror::plan_nodes` emits exactly one `Compaction` node for that entry,
   with the **split parent** recorded in the payload — not the previous row in
   sequence order.
3. `CausalCompactor` becomes horizon-aware (`resolve_active_branch`, not
   `linear_path`) and seeds `previous_summary` from the horizon node.
4. The harness performs **one** write: the transcript entry + `compact`
   directive, in one transaction. `commit_session_ir` is no longer used for
   compaction. This deletes residual **A** by construction.
5. A bounded, online **derived-index scrub** runs on the writer when idle and
   repairs any corrupt graph through the existing read-time repair. This closes
   residual **C** without a one-off migration and without a blocking startup
   sweep.
6. The cut algorithm gets a **post-condition** (`window_after + overhead <=
   gate`) and a bounded summary. A fold that cannot meet it returns an explicit
   `CompactionInsufficient`; it never silently claims success. This closes
   residual **D**.

Do this **before the first release of the current hotfix**. The hotfix's
checkpoint format (a plain message payload + directive) should not ship: the
RFC's typed payload replaces it, and no production database contains a
compaction entry or node (verified: 0 `compact` directives, 0 `compaction`
nodes), so there is nothing to migrate.

## 1. Motivation

### 1.1 Residual A — compaction is two writer transactions

`commit_compaction_checkpoint` (transcript) and `commit_session_ir` (graph) are
separate commands. The ordering is chosen so a crash leaves an unfolded graph
over a compacted window (benign), but the split still exists and is a permanent
consistency hazard. Two durable representations of one fact ("this range is
folded into this summary") invite drift the moment any code path updates only
one of them.

### 1.2 Residual B — the transcript checkpoint is not a graph fact

`plan_nodes` skips entries whose origin is `CompactionCheckpoint`. A full
transcript rewrite (`translate_projection` fallback, `replace_messages`) clears
the graph and rebuilds it from the transcript; the checkpoint entry is skipped,
so the rebuilt graph has **no** horizon. The window keeps the summary, the IR
loses it, and `previous_summary` cannot carry it into the next fold.

### 1.3 Residual C — 34 corrupt graphs are repaired only on read

The read-time probe is correct, but a session that is never hydrated keeps its
damaged graph. It is inert (all walkers are cycle-safe), yet "inert until
touched" is not an integrity story.

### 1.4 Residual D — a fold can leave the window above the gate

`find_cut_point_nodes` keeps **at least** `target` tokens in the tail, and the
IR summarizer's output is unbounded (`compact_causal_nodes` appends the model's
text plus a file manifest with no token cap). Post-fold size is therefore not
bounded by the gate; a huge single turn or a verbose summary can re-trigger
compaction on the next send.

### 1.5 P0 finding — the compaction walk ignores the horizon

`CausalCompactor::compact_session_ir` calls `ir.history.linear_path(&leaf_id)`,
which walks parent edges to the root. The kept tail's head deliberately keeps
its parent pointing at the **folded** chain (`INV-CHK-01`); the horizon node is
a sibling under the split parent, not an ancestor. So after a fold, the next
compaction sees the folded prefix again and re-summarizes it. `previous_summary`
is not found either, because it only scans `nodes_to_fold` and the horizon is not
in that set.

`SessionIR::resolve_active_branch` already does the right thing
(`linear_path_with_horizon`); the compactor simply does not use it. This is a
correctness bug in its own right and must be fixed before the rest of the RFC
means anything.

## 2. Goals and non-goals

**Goals**

- One durable owner per fact; every agent-facing projection is a derive.
- One writer transaction per compaction; no multi-step fact commit.
- A fold survives reopen, transcript rebuild, fork, and `/retry` with exactly
  one horizon node.
- A fold is either provably below the gate or explicitly reported insufficient.
- The 34 damaged graphs (and any future ones) are repaired without manual
  intervention and without blocking startup.

**Non-goals**

- Changing the wire protocol. The transcript is not a wire artifact; no frame
  or protocol version changes.
- Changing the public compaction UX (`/compact`, checkpoint cards).
- Replacing the IR as the execution kernel's fact store. See §5.8: IR-only
  facts remain a second source for now; this RFC removes the **compaction**
  duplication only.

## 3. Principles

1. **The transcript is the fact log.** A checkpoint is a fact: it happened, it
   stands for a range, and it must be reconstructible from the transcript alone.
2. **Graph nodes are a derive.** `causal_nodes` is an index; losing it must
   never lose a fact or change the window.
3. **One write per fact.** A transaction that commits a checkpoint commits
   everything a reader needs to observe it consistently.
4. **Sequences are positions, not semantics.** Do not infer the archived range
   from message counts or from a node's ordinal; carry the boundary explicitly.
5. **No compatibility lane.** The intermediate checkpoint format is replaced,
   not versioned alongside. No production data exists in it.

## 4. Invariants

- **[INV-CMP-01]** A checkpoint entry is the sole durable record of a fold. Its
  payload is `EntryPayload::Compaction`; the rendered message is derived from it.
- **[INV-CMP-02]** The active causal path contains **at most one** visible
  horizon; `resolve_active_branch` yields `[horizon, kept tail…]`.
- **[INV-CMP-03]** The horizon node's `parent_id` is the payload's
  `split_parent_node_id` (the last folded node), never the previous row in
  sequence order. The kept tail's head parent is never rewritten.
- **[INV-CMP-04]** A compaction commits in one writer transaction. There is no
  state in which a durable checkpoint lacks its directive, or vice versa.
- **[INV-CMP-05]** `tokens_after + request_overhead <= compaction_threshold`
  after a fold, or the fold reports `CompactionInsufficient`.
- **[INV-CMP-06]** `linear_path` and the compactor honor
  `state.compaction_horizon`; a fold never re-reads its own archived prefix.
- **[INV-CMP-07]** Losing `causal_nodes` and re-deriving it from the transcript
  reproduces the same active window and horizon.

## 5. Design

### 5.1 The transcript fact

Keep `TranscriptEntry.kind = EntryKind::Message` (the SQLite CHECK constraint
only admits `message|state`; a third kind would force a full rebuild of a 3.2 GB
`entries` table for no semantic gain). The **payload** is the typed contract:

```rust
// crates/nuo-protocol/src/transcript.rs

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntryPayload {
    #[serde(rename = "message")]
    Message(Box<MessagePayload>),
    #[serde(rename = "state")]
    State(StatePayload),
    /// A durable compaction fact: the checkpoint that replaced
    /// `seq <= folded_through_seq`, keeping the tail from `first_kept_node_id`.
    #[serde(rename = "compaction")]
    Compaction(Box<CompactionMeta>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionMeta {
    /// ID of the first causal node preserved after the fold.
    pub first_kept_node_id: String,
    /// ID of the last folded node; the horizon node's parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_parent_node_id: Option<String>,
    /// Membership seq of the last folded entry. The directive's `up_to_seq`.
    pub folded_through_seq: u64,
    /// Token weight of the folded prefix, for the checkpoint card.
    pub tokens_before: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,
}
```

- The checkpoint text lives in `entry.content` (the summary; already token-bounded
  per §5.6). `to_message` renders it with the existing `CHECKPOINT_HEADER` and
  `InjectionKind::CompactionCheckpoint`, exactly as `checkpoint_message` does
  today. No consumer needs a new rendering path.
- The entry keeps `role = User`, `origin = EntryOrigin::Checkpoint`,
  `hidden = true` — the existing envelope rules and SQL CHECK constraints hold.
- `plan_nodes` matches the **payload**, not the origin. This is what makes the
  node derivable from the transcript.
- `load_dialogue_range` already loads `kind='message'` rows and decodes the
  payload; no query change. Its `kind` decode stays `Message`.
- FTS keeps indexing `content`, so checkpoint summaries remain searchable.

### 5.2 Graph derivation

`ir_mirror::plan_nodes` gains one rule and one state variable:

```
previous_dialogue: Option<NodeId>   // never a compaction node

for entry in entries (seq ascending):
    match entry.payload:
        Compaction(meta):
            parent = meta.split_parent_node_id (if that node exists in the
                     graph or in this plan) else previous_dialogue
            emit CausalNode {
                id: entry.id,            // identity is the entry's identity
                parent_id: parent,
                seq: entry.seq,
                kind: Compaction,
                payload: NodePayload::Compaction {
                    summary: entry.content,
                    first_kept_node_id: meta.first_kept_node_id,
                    tokens_before: meta.tokens_before,
                    read_files, modified_files,
                    belief_state: None,
                },
            }
            // Deliberately do NOT advance previous_dialogue.
        Message(..):
            emit Dialogue with parent = previous_dialogue
            previous_dialogue = Some(entry.id)
        State(..):
            skip
```

Why the special parent and the non-advancing cursor:

- The checkpoint entry is appended at the **end** of the transcript (its seq is
  the largest). Semantically it sits before the kept tail. Sequence-ordered
  chaining would parent it to the tail's last node, and would parent messages
  arriving after the fold to the checkpoint — making the horizon an ancestor of
  the tail and hiding the tail from `linear_path`.
- Parenting the horizon to the split parent (the existing `INV-CHK-01` shape)
  and leaving the tail chain untouched makes the horizon a **sibling anchor**.
  `linear_path_with_horizon` injects it when it reaches `first_kept_node_id`.
- New messages after the fold link to `previous_dialogue` (the tail's last
  dialogue), so the next horizon walk sees the tail and stops at the old
  horizon — not at a checkpoint that swallowed the tail.

Full rebuilds (`replace = true`) use the same plan. A graph reset therefore
reconstructs the horizon from the transcript, closing residual **B**.

### 5.3 Compactor changes

1. **Horizon-aware walk.**
   ```rust
   let active_nodes = ir.resolve_active_branch();
   ```
   (Equivalent to `linear_path_with_horizon(leaf, ir.state.compaction_horizon)`.)
   A second compaction now examines only the unfolded tail.

2. **Seed the previous summary from the horizon**, not only from folded nodes.
   - `previous_summary` = the horizon node's `NodePayload::Compaction.summary`
     if present, else the last compaction payload among `nodes_to_fold`.
   - Seed `FileOperations` with the horizon's `read_files` / `modified_files`.
   - This preserves summary continuity even when the old horizon is the first
     kept node and is not part of `nodes_to_fold`.

3. **Emit a `CompactionMeta`**, not an IR-only node:
   - `first_kept_node_id = cut_point.first_kept_entry_id`
   - `split_parent_node_id = nodes_to_fold.last().map(|n| n.id)`
   - `folded_through_seq = nodes_to_fold.last().map(|n| n.seq)`
   - `tokens_before`, `read_files`, `modified_files` from the tracker.

4. **Update the in-memory IR with the durable entry's id.** The session method
   generates the entry id and returns it; `ir.append_compaction(entry_id, …)`
   then uses that id, so `state.compaction_horizon` names the durable node.
   The current `format!("compact_{uuid}")` id disappears (no back-compat).

### 5.4 Single write

Replace `commit_compaction_checkpoint` + `commit_session_ir` with one session
method:

```rust
pub async fn commit_compaction(&self, meta: CompactionMeta) -> Result<CommittedCompaction, String>
```

In one `SessionData` delta transaction (`save_session_inner`, delta path; the
generation does not change):

1. Append the checkpoint entry (`entries.push`, id = UUID, seq = next_seq,
   payload = `Compaction(meta)`, content = bounded summary, origin = Checkpoint,
   hidden = true).
2. Append the `Compact` directive: `up_to_seq = meta.folded_through_seq`
   (snapped to the greatest existing entry seq `<=` it), `checkpoint_seq = the
   new entry's seq`.
3. Set `last_projection`, invalidate the projection cache, save.
4. Return the entry id (the future node id) and seq.

The derived drain then observes the new entry and produces the horizon node
(§5.2). If the process dies after the commit, the next flush reconstructs the
node from the entry — there is no second fact to lose. Residual **A** is gone:
the checkpoint fact and its directive are in the same transaction, and the graph
is an index.

`commit_session_ir` remains for IR-only execution facts (§5.8) but is removed
from the compaction path. A layering test pins that.

### 5.5 Online derived-index scrub (residual C)

Add a bounded scrub to the writer's idle loop, next to `derived::advance_batch`:

- Persist a scan cursor in a new `derived_meta(key TEXT PRIMARY KEY, value TEXT)`
  table (`schema v29`; see §8).
- Each idle tick, probe at most `SCRUB_BATCH` sessions (e.g. 32) after the
  cursor with the existing `has_corrupt_lineage` query.
- On a hit, call `derived::flush_session` for that session (which rebuilds from
  the transcript and advances the cursor).
- Advance the cursor; wrap at the end. The scan is resumable, cheap (one indexed
  `EXISTS` per session), and never runs while a command is queued.

This repairs the 34 damaged graphs within the first passes, generalizes to any
future corruption, and keeps the repair logic in exactly one place (the read
path), with no frozen one-off migration.

### 5.6 Cut policy and post-condition (residual D)

The cut must guarantee, not hope:

```
gate    = budget.compaction_threshold_tokens
overhead = request.estimate.overhead_tokens          // instructions + tools + temp
summary_budget = clamp(gate - overhead - CRUISE_MARGIN, 2_000, 24_000)
tail_budget   = gate - overhead - summary_budget
```

- `find_cut_point_nodes` changes from "keep at least target" to "keep at most
  `min(target_tokens, tail_budget)`, and at least one complete user turn when
  possible". It walks from the newest and stops **before** adding a node that
  would exceed the budget; then it backs up to a tool-call-safe boundary.
- If the last complete turn alone exceeds `tail_budget`, it folds inside the
  turn at a tool-call-safe point (the existing `PreserveTailRounds` fallback is
  the precedent). If a single node exceeds the budget, the request projection
  must truncate it (or the fold reports insufficient).
- The summarizer output is clamped with `summary_token_budget_clamped` (the IR
  path currently does not clamp; the legacy path does).
- The outcome carries `tokens_before`, `tokens_after`, `gate`, and
  `satisfied: bool`. `satisfied == false` is logged and surfaced on the
  checkpoint card; the round proceeds on the closest achievable window. It is
  never silently reported as a successful shrink.
- Property tests: for arbitrary message shapes (including one huge tool result),
  `tokens_after + overhead <= gate` or `satisfied == false`.

### 5.7 Active leaf and newest position

- Main-line sessions leave `threads.active_leaf = NULL`. `newest_position`
  uses the highest-seq non-compaction node.
- `active_leaf` is an **explicit branch pin** (fork/aside) written only by the
  branch operations; the compaction path and the transcript mirror never write
  it.
- A stale pin (pointing at a node that is not the lineage tail) must not be
  silently preferred: `newest_position` validates that the pinned node belongs
  to the current branch and, for a main-line session, ignores it. The v29
  scrub clears `active_leaf` for sessions without a fork kind.

### 5.8 What remains a second fact source (explicit scope boundary)

The IR is currently also written directly by the kernel's `FactSink`
(`nuo-server/src/durability_sink.rs` → `save_session_delta`) for execution
facts such as observations. Those facts are **not** transcript entries. This RFC
does not remove that path; it only removes the compaction duplication.

Live census (2026-11-08): 393,614 causal nodes, **all** `dialogue`; zero
`observation`, `compaction`, `termination`, `system_notice`. No live session
depends on IR-only facts today, so the v29 reset is lossless for this database.

A follow-up RFC must decide between:

- route every durable execution fact through a transcript payload variant and
  make the IR a pure derive; or
- declare the IR the authority for non-dialogue facts and define a
  reconciliation/rebuild contract.

This RFC takes no position beyond "compaction is not a second source".

## 6. Work plan

No phase may ship in a partial state: each ends with tests, clippy, and a
migration/rollout note.

**Phase 1 — horizon correctness (P0)**
- `CausalCompactor`: `resolve_active_branch`, horizon summary/files seeding.
- Tests: a second fold sees only the tail; the summary carries forward; the
  window is monotone.

**Phase 2 — typed compaction fact + derivation (A, B)**
- `EntryPayload::Compaction` + `CompactionMeta`; `to_message`; serde.
- `plan_nodes` compaction rule; full-rebuild test.
- `SessionStore::commit_compaction`; remove the IR commit from the harness.
- Delete `commit_compaction_checkpoint` and the old two-step call.
- Tests: crash between commit and drain; reopen; rebuild; idempotent re-mirror;
  directive/node boundary equality.

**Phase 3 — cut policy and post-condition (D)**
- `find_cut_point_nodes` budget semantics; summary clamping; insufficiency
  outcome; property tests; diagnostics on the checkpoint card.

**Phase 4 — online scrub + schema v29 (C)**
- `derived_meta` + scan cursor; idle-loop scrub; v29 migration; clear
  `active_leaf` for non-fork sessions.
- Tests: seeded corrupt graphs are repaired within N idle ticks; migration
  resumes after an interruption; old-version gate refuses the DB.

**Phase 5 — docs and firewall**
- ADR: supersede ADR-0159 §INV-IR-04; new invariants.
- Layering test: the compaction path does not call `commit_session_ir` and does
  not construct a `Compaction` node by hand.
- `wire-census` unchanged (protocol stays 23).

## 7. Test plan

- **Unit (protocol):** serde round-trip of `Compaction` payload; `to_message`
  renders the checkpoint; `plan_nodes` parent/seq rules; `linear_path` horizon
  injection.
- **Unit (persistence):** single-transaction commit; directive + entry atomicity
  under an injected failure; full rebuild reconstructs exactly one horizon;
  boundary snap with `state` entries; re-mirror is a no-op.
- **Integration (harness):** second fold is tail-only; summary continuity;
  post-condition; `/retry` and fork keep the horizon; subagent sessions.
- **Crash injection:** kill between the transcript commit and the drain; reopen;
  flush rebuilds; window identical.
- **Migration:** seeded corrupt graph + v28→v29; scrub repairs; interrupted
  scrub resumes; old binary refuses v29.
- **Property:** arbitrary message sizes → `tokens_after + overhead <= gate` or
  `insufficient`; graph is a DAG; exactly one active horizon.

## 8. Rollout and versioning

- **Wire:** no change; `PROTOCOL_VERSION` stays 23.
- **DB:** schema **v29** adds `derived_meta` and invalidates nothing fact-wise.
  No `entries` table rebuild (the compaction payload is JSON inside the existing
  column; `kind` stays `message`).
- **Hard break:** the intermediate checkpoint format is replaced. The old
  binary must not open a v29 DB (the existing `current_version >
  CURRENT_DB_VERSION` gate handles this). No dual-format reader.
- **No data migration:** the live DB has 0 compaction entries/nodes; the 34
  corrupt graphs are repaired by the scrub. If a deployment ever exists with the
  intermediate entries, its checkpoints must be recomputed; this RFC does not
  define a converter.
- **Observability:** log/emit `tokens_before`, `tokens_after`, `gate`,
  `satisfied`, `nodes_folded`, `folded_through_seq`, scrub repairs; keep the
  storage metrics.

## 9. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| `plan_nodes` parent rule regresses the chain | Rebuild test that walks `linear_path_with_horizon` and asserts `[horizon, tail…]`; property test that the graph is a DAG. |
| Removing `commit_session_ir` from compaction loses the horizon on an in-flight round | `session_ir()` flushes before every hydrate; the durable commit is the transcript; test a mid-round `session_ir()`. |
| v29 scrub competes with turns | Batch of 32 probes per idle tick, writer-yield between batches, cursor persists; only when `rx.is_empty()`. |
| Summary clamping changes summary quality | Clamp to the existing 2k–24k budget, not a new cap; golden summary tests. |
| Horizon injection with an IR-only first kept node | `folded_through_seq` snap to the greatest existing entry; skip the projection if no folded entry exists; test both cases. |
| `active_leaf` semantic change breaks forks | Fork/aside tests; v29 clears only sessions without a fork kind. |

## 10. Alternatives rejected

- **A combined writer command for the two existing writes.** It would preserve
  two durable representations of one fact and require duplicating the
  transcript-save machinery inside the IR delta path. Deriving the node removes
  the second write instead of synchronizing it.
- **Keeping the checkpoint as a plain message payload and using the origin as
  the discriminator.** Stringly-typed; the structured facts (split parent,
  first kept) would have nowhere durable to live.
- **A one-off v29 migration that deletes the 34 corrupt graphs.** Repairs one
  incident, freezes repair logic, and does not cover the next corruption. The
  online scrub reuses the read-path repair.
- **Making `model_window` read the IR.** The transcript projection also carries
  prune elisions and is the factual view used by non-agent surfaces. The
  transcript stays the window authority; the IR derives from it.
- **Bumping `EntryKind` to add `Compaction`.** Forces a full rebuild of the
  3.2 GB `entries` table to relax a CHECK constraint, with no reader benefit.

## 11. Open questions

1. Should the checkpoint entry's `seq` remain the append (largest) position, or
   should the transcript gain a durable "logical position" for compact facts?
   The derivation above works with the append position; a logical position
   would be cleaner but touches every projection consumer.
2. Does the follow-up fact-authority RFC route observations through the
   transcript, or declare the IR authoritative for them? (Current live census:
   no such nodes.)
3. Should `CompactionInsufficient` abort the round or continue on the closest
   window with a visible warning? This RFC recommends continue + warn, so a
   pathological turn is still answerable.
4. Is the scrub cadence a fixed batch (32) or a byte/time budget? A time budget
   is more robust across database sizes but needs a monotonic clock injection
   for tests; a batch is deterministic. This RFC recommends batch first.
