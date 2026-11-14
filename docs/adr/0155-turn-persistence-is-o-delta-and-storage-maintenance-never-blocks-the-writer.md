---
id: ADR-0155
title: "Turn Persistence Is O(Delta) and Storage Maintenance Never Blocks the Writer"
status: accepted
date: 2026-11-07
scope: persistence/session-store, persistence/ir-mirror, persistence/wal, harness/orchestration, presentation/phase
superseded_by: null
negative_knowledge: true
---

# 0155. Turn Persistence Is O(Delta) and Storage Maintenance Never Blocks the Writer

- Status: Accepted
- Date: 2026-11-07
- Deciders: Nuo Architecture Working Group
- Consulted: Storage Engine, Agent Runtime, Presentation Substrate, Performance Engineering
- Amends: [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md) (save-point *cadence* stands; the cost model of a save is corrected here)
- Complements: [ADR-0019](0019-token-pressure-measurement-and-budget-accounting.md), [ADR-0090](0090-autonomous-persistence-resilience-transparent-lease-handoff-and-zero-plaintext-credentials.md)

---

## Context and Problem Statement

On 2026-11-07 the interactive bar sat on `preparing context [32s]` after an
ordinary send. The investigation found three independent defects that had
compounded into a write-path latency cliff:

1. **The turn hot path still had two O(history) scans, and the one that was
   supposed to be O(delta) was untested.**
   - `commit_turn` deep-compared the caller's **whole window** against the
     durable projection on every commit to infer append-vs-rebuild
     (`wire_eq`). That scan ran at round admission and at every ReAct turn
     boundary, on the caller's thread.
   - A delta save persists only the appended entries, so the causal mirror was
     already delta-shaped; but nothing proved the **linkage** across saves. The
     append path linked each new segment to the newest node without checking
     that the graph actually covered the transcript, and a full rewrite
     re-serialized and re-published every oversized body. (An earlier revision
     of this ADR claimed the mirror re-serialized the whole transcript on every
     save; that was wrong — the save command carries a delta-only
     `SessionData`. The corrected statement is what the decision below
     implements.)

2. **Storage maintenance ran on the writer thread and could not fail fast.**
   `maintain_storage_pressure` was invoked from the writer loop every five
   seconds. It chose `PRAGMA wal_checkpoint(TRUNCATE)` whenever
   `active_readers == 0`, on a connection with `busy_timeout = 5000`. A single
   read snapshot that the pool did not count — the logs showed `busy=1` with
   `active_readers=0` — made every attempt park the writer for the full five
   seconds. The warnings fired exactly every 5.0 s, i.e. the writer spent
   essentially all of its time inside a checkpoint that could never complete:

   ```
   WAL checkpoint could not complete wal_bytes=35732792 busy=1
       log_frames=8673 checkpointed=135 active_readers=0
   ... 1 094 occurrences ...
   WAL checkpoint could not complete wal_bytes=862089432 busy=1
       log_frames=209245 checkpointed=135 active_readers=0
   ```

   Session saves queued behind that loop and took 20–55 s
   (`slow local session persistence ... duration_ms=54347`). The request for the
   affected round was dispatched 24 ms after the blocked save completed: the
   "context preparation" the bar claimed was a SQLite write.

3. **The phase vocabulary hid the defect.** `"starting request"`,
   `"saving request"` and `"preparing context"` all folded into one
   `Phase::Preparing`, so a storage stall was indistinguishable from token
   estimation. The only signal that could have named the problem was erased by
   the fold.

The database is 3.27 GB with `entries` 730 MB, `causal_nodes` 456 MB and
`fts_entries` 182 MB, so the O(history) work was not a small constant either.

## Decision

### 1. A save is O(delta), by contract

- **The caller states the commit basis.** `CommitTurn` carries
  `CommitBasis::{Appended, Reprojected}`. `Appended` means "the committed
  prefix is untouched; this window is that prefix plus a tail". The store
  verifies the claim with **one tail comparison**, not a whole-history scan,
  and appends the tail. A failed claim degrades to `Reprojected`, which runs the
  comparison and rebuilds if needed. `Reprojected` is set by the one place that
  knows a projection rewrote committed messages: the agent's
  `project_context_if_needed`, whose marker the next persist consumes.
- **The mirror's cost is the slice it is handed.** A delta save carries only the
  appended entries, so `mirror_transcript` plans exactly those and links the
  first to the graph's newest fact. A rewrite hands it the whole transcript and
  replaces the graph.
- **Coverage is proved, not assumed.** Before linking a delta, the save probes
  for an unmirrored dialogue entry in the range the graph claims to cover
  (one indexed `EXISTS`). If the graph is empty, truncated, or rewound, the
  save rebuilds it from the durable transcript **in the same transaction**
  instead of creating an unreachable segment. A one-time repair, never a
  per-turn cost.
- **The blob ledger consumes the hashes the write already computed.**
  `upsert_entry` returns them; `record_blob_refs` records exactly those. No
  second base64 decode or hash per image, and a failed publish never records a
  reference for bytes that did not land.

`mirror_transcript` returns the number of planned nodes — the direct measure of
the save's work, pinned by regression tests.

The same "never re-tokenize immutable bytes" discipline now holds on the request
path: `project_historical_tool_outputs` takes the shared content-addressed
weights cache instead of re-estimating every historical tool body on each of the
three or four request builds per round, and the mid-turn projection gate measures
with `estimate_tokens_weighted` rather than the uncached `estimate_tokens`.

### 2. Storage maintenance has its own thread, its own connection, and a bounded policy

- Checkpointing leaves the writer thread entirely. Each writer generation spawns
  `nuo-storage-maintenance`, which opens its own connection with a **100 ms**
  busy timeout. Dropping the writer generation signals and joins it, so no
  maintenance connection survives into the next generation.
- SQLite's automatic checkpoint is disabled (`wal_autocheckpoint = 0`). An
  automatic checkpoint runs inside the writer's commit path; against a pinned
  WAL it walks the whole wal-index on every commit.
- The maintenance policy attempts **PASSIVE only**. `TRUNCATE` is attempted
  solely after a PASSIVE pass has fully backfilled the log *and* no tracked
  reader exists — i.e. only when it has nothing to wait for.
- A checkpoint that cannot complete enters an exponential backoff ladder
  (5 s → 300 s) instead of retrying every tick.
- Crossing `WAL_HARD_CAP_BYTES` (512 MB) is an error-level signal: it can only
  happen when a snapshot has been pinned for minutes, and it must be visible.

### 3. A storage phase is not a context phase

`Phase::Admitting`, `Phase::Saving` and `Phase::Preparing` are distinct
vocabularies with distinct labels (`starting request`, `saving request`,
`preparing context`). The bar never claims context work while the process is
writing to disk. The round-lifecycle fallback arms `Admitting`, not `Preparing`.

### 4. The wire shape is allowed to break

This ADR changes no wire type, but it does not preserve compatibility with any
storage artifact of the old shape: the mirror is recomputed from the transcript
(the transcript is the SSOT), so there is nothing to migrate. The derived graph
is rebuildable by construction:

> `causal_nodes` is a projection of `entries`. A missing or stale node is a
> cache miss, never a correctness loss.

## Invariants

1. **[INV-PERSIST-01] Delta cost by contract.** A save's serialization, row
   work, and mirror work are proportional to the entries it appends. The store
   never infers append-vs-rebuild by scanning the session's history: the caller
   declares the basis, and the store verifies the claim in O(1) messages.
2. **[INV-PERSIST-02] No maintenance on the writer.** The writer loop executes
   commands; it never performs a checkpoint, a metrics scan, or a retry ladder.
3. **[INV-PERSIST-03] A checkpoint never waits on a reader.** The maintenance
   connection's busy timeout is bounded and failures back off.
4. **[INV-PERSIST-04] Derived views are rebuildable — and repaired.** `causal_nodes`
   is derived from the transcript. A delta links onto the graph's newest fact
   only after a coverage probe proves no dialogue entry in the claimed range is
   unmirrored; an empty, truncated, or rewound graph is rebuilt from the durable
   transcript inside the same transaction. Mirroring a tail onto a graph with a
   hole would build a segment `linear_path` cannot reach from the root, which
   would make compaction read a conversation starting mid-history.
5. **[INV-PERSIST-05] No duplicate publication work.** A blob hash computed by
   the row write is the hash the ledger records; content is decoded once.
6. **[INV-PHASE-01] A storage phase is labeled as storage.**
   `"saving request"` folds to `Phase::Saving`, never to `Phase::Preparing`.

## Consequences

- Turn persistence is now bounded by the new entries of the turn. The regression
  test `causal_mirror_plans_only_the_delta` asserts a fully mirrored window
  plans zero nodes and an appended entry plans exactly one.
- A pinned read snapshot can still grow the WAL, but it can no longer delay a
  turn: the writer never waits for it. The condition is reported at error level
  once it crosses the hard cap.
- Removing the automatic checkpoint means a process that opens the database
  without a writer generation (a short-lived tool) relies on the retire
  checkpoint. That path already runs `PRAGMA wal_checkpoint(FULL)` before the
  engine closes.
- The phase vocabulary is wider. `nuo-view::phase`'s closure test pins every
  label; a new storage phase now requires an explicit decision rather than
  silently reusing `Preparing`.

## Negative Knowledge

- **Do not** re-mirror the whole transcript "because the upsert is idempotent".
  Idempotence bounds *correctness*, not cost; the serialize walks are the cost.
- **Do not** infer "no reader" from `reader_ages.active()`. The incident's
  checkpoints returned `busy=1` while the pool reported zero readers. A PASSIVE
  checkpoint that cannot advance is the evidence; `TRUNCATE` is not a probe.
- **Do not** run a "cheap PRAGMA" on the writer thread. A checkpoint waits on a
  busy handler in a way that is indistinguishable from a hung write.
- **Do not** fold a storage phase into a context phase for UI convenience. It
  hid a 35-second write behind a label that blamed context assembly.

## Verification

- `db::tests::{a_delta_mirror_plans_only_its_slice,
  a_delta_chain_links_across_saves,
  a_missing_graph_is_backfilled_instead_of_linked_onto_nothing}`
- `session::tests::{a_broken_appended_claim_falls_back_to_a_rebuild,
  reprojected_basis_rewrites_an_equally_sized_window}`
- `nuo-harness` integration: `compaction_engages_on_a_session_built_from_plain_rounds`
  (a broken chain makes `linear_path` see only the newest segment, so this test
  fails when linkage regresses)
- `db::actor::tests::{maintenance_is_safe_in_every_wal_state,
  a_pinned_snapshot_never_makes_maintenance_wait,
  a_completed_pass_enters_backoff_before_retrying, a_small_wal_is_left_alone}`
- `nuo-view::phase::tests::{vocabulary_closure,
  only_the_transport_wait_phase_hosts_a_setback_clause}`
- Operational: `slow local session persistence` absent under load; WAL peak
  under 64 MB in ordinary use; checkpoint failures report a retry delay and do
  not repeat inside it.
