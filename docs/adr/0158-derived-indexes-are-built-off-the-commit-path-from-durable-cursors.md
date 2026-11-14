---
id: ADR-0158
title: "Derived Indexes Are Built Off the Commit Path from Durable Cursors"
status: accepted
date: 2026-11-07
scope: persistence/derived-index, persistence/session-store, persistence/migrations, harness/session-ir, server/history
superseded_by: null
negative_knowledge: true
---

# 0158. Derived Indexes Are Built Off the Commit Path from Durable Cursors

- Status: Accepted
- Date: 2026-11-07
- Deciders: Nuo Architecture Working Group
- Consulted: Storage Engine, Agent Runtime, Retrieval, Performance Engineering
- Amends: [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md) (save-point cadence stands; what a save writes is narrowed here)
- Complements: [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md), [ADR-0157](0157-provider-calls-borrow-the-assembled-request.md)

---

## Context and Problem Statement

Two indexes are derived from the durable transcript `entries` /
`entry_memberships`:

- **FTS** (`fts_entries`), populated by membership triggers inside the commit
  transaction;
- the **causal fact graph** (`causal_nodes`), populated by the live mirror in
  the same save (and by IR folds).

Neither is a fact the turn depends on. Both were nevertheless on the path a
turn waited for: a large tool result was indexed twice (row + FTS) before the
commit acknowledged, and every save mirrored its delta before returning. A slow
index or a write amplification spike therefore became turn latency — the same
class of failure as [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md),
one layer up.

The obvious response is a physical hot/cold database split. It is the wrong one:
two databases cannot be committed atomically without a cross-store transaction,
so a crash between them produces exactly the divergence the split was meant to
avoid. The requirement is not "two files"; it is **no derived write may delay a
command**.

## Decision

### 1. A commit writes the transcript and one outbox row

`save_session_inner` no longer mirrors a node or indexes a row. It writes the
thread row, the entry/membership delta, the revision and — when it appended
anything — one upsert into `derived_pending(session_id, through_seq)`. A full
rewrite (a rebuilt transcript: compaction fallback, resume, replace) instead
resets both derived views and enqueues a full rebuild, because entry identities
and sequences restart.

### 2. Durable cursors, bounded idle drain

`derived_cursors(session_id, causal_seq, fts_seq)` records how far each view
has consumed the transcript. `causal_seq` is "the graph represents the
transcript through this sequence" — after a fold it is advanced to the durable
end because the folded graph covers that range, not because every node still
exists. `fts_seq` is the highest membership sequence written into FTS, keeping
`fts_entries.rowid == entry_memberships.rowid` (the join `snippet`/`bm25`
depend on).

The writer thread advances the queue **only while no command is waiting**,
bounded by `IDLE_BATCH` entries per pass, with the same discipline as the usage
projection. There is no second thread, no second connection, no second writer:
the one-door rule holds.

### 3. Read-your-writes is explicit, not hoped for

- `SessionStore::session_ir()` flushes its own session before hydrating. A
  flush failure is logged; a stale IR is never presented as current.
- Transcript search (`handlers_history`, the Archivist) flushes the **whole**
  pending queue first, because recall spans sessions.
- An IR fold (`commit_session_ir` / fork) advances the causal cursor to the
  durable end in the same transaction that writes its nodes, so the drain can
  never re-mirror — and therefore relink — facts the fold just committed.

The idle drain is an optimization; correctness never depends on it having run.

### 4. Lifecycle owns derived rows

- Session deletion purges `fts_entries`, `causal_nodes`, the cursor and the
  pending row **before** deleting the thread (none of them has a usable FK to
  `threads`; the old membership trigger was the only reclaimer of FTS rows).
- A transcript replacement resets both views and their cursors.
- The migration drops the membership triggers, creates the two tables, and
  seeds cursors from the state the old paths left behind — no re-index of the
  whole database, no lost rows.

### 5. A physical hot/cold split is rejected, deliberately

The goal is separation of *work*, not of *files*. A single database with an
outbox and durable cursors gives:

- one crash-consistent commit (transcript + outbox in the same transaction);
- derived work that runs between commands, never inside one;
- a resumable, idempotent indexer whose only failure mode is lag;
- no cross-database consistency window, no second writer, no second WAL.

A split would weaken all four to move the same rows. If a future requirement
demands separate storage (e.g., remote search), it must arrive with a
cross-store transaction story, not as a preprocessing step.

## Invariants

1. **[INV-DERIVED-01] No derived write on the commit path.** A commit writes no
   `causal_nodes` and no `fts_entries` row. The only extra row is the outbox.
2. **[INV-DERIVED-02] Coverage is durable.** A cursor plus the transcript is
   sufficient to resume indexing after a crash; the work is idempotent
   (`INSERT OR IGNORE`, node identity is the entry id).
3. **[INV-DERIVED-03] Read-your-writes.** Every reader of derived state flushes
   the relevant scope first: one session for the IR, all sessions for search.
4. **[INV-DERIVED-04] No derived row outlives its source.** Session deletion and
   transcript replacement purge the derived rows, cursors and queue entries.
5. **[INV-DERIVED-05] Cursors never move backwards.** A fold advances the causal
   cursor; a full rewrite resets it only together with the graph it describes.
6. **[INV-DERIVED-06] FTS rowid identity.** `fts_entries.rowid` always equals
   the owning `entry_memberships.rowid`.

## Consequences

- A turn's commit cost is now independent of both the FTS index and the causal
  graph. The largest remaining per-save work is the entry delta itself.
- Search and IR reads pay the catch-up cost, which the idle drain keeps small.
  An explicit flush after a long idle period is bounded by the backlog, not by
  session length in the steady state.
- `count_causal_nodes`-based observability lags the transcript; anything that
  reports derived facts must flush first or label itself eventual.
- The migration is one-way: the triggers are gone. Running an older binary
  against a v28 database is refused by the version gate, as designed.

## Negative Knowledge

- **Do not** reintroduce a membership trigger "for freshness". It puts the
  index back inside the commit.
- **Do not** write FTS or a causal node from `save_session_inner`. The outbox is
  the only derived write a commit may perform.
- **Do not** filter `fts_entries` by `session_id`. It is an `UNINDEXED` FTS5
  column, so the predicate scans the whole content table. Delete by the
  membership rowid (`[INV-DERIVED-06]`) while those rows still exist.

## Amendment (2026-11-12): the migration missed the live trigger names

Migration 28 dropped `trg_memberships_*` and `trg_entries_*`, but migration 24's
thread-canonical rebuild had re-created the same membership triggers as
`trg_entries_membership_ai`/`_ad`. They survived, so the invariant this ADR
declares was silently violated on every database: commits still wrote FTS
inline, and `reset_for_rebuild`'s `DELETE FROM fts_entries WHERE session_id = ?`
scanned the entire index. On the live database (≈400 k rows, ≈1 GB of FTS
state) that scan took 8–9 s on a new session's first save — the visible
`● saving request` stall.

Migration 31 drops every trigger on `entries`/`entry_memberships` that writes
`fts_entries`, including names this catalog did not anticipate. The FTS delete
paths (`reset_for_rebuild`, `purge_session`) now resolve rowids from
`entry_memberships` before the membership rewrite or thread delete removes
them, and `reset_for_rebuild` runs **before** those rows are replaced. A
570-message full rewrite measures 8.4 s → 0.2 s; a new session's first save
~150 ms → ~30 ms.
- **Do not** read `causal_nodes`/`fts_entries` on a path that did not flush and
  call the result current.
- **Do not** split the database without a transaction spanning both stores.
  Two independent commits are a divergence bug with extra steps.
- **Do not** advance the causal cursor from a delta whose graph was not hydrated
  after a flush. The cursor is a promise; a fold that skips the read breaks it.

## Verification

- `session::tests::session_ir_flushes_pending_derived_work` — the IR path
  drains before hydrating; without it the graph is empty.
- `db::tests::{a_save_enqueues_derived_work_and_a_flush_produces_it,
  a_delta_chain_links_across_saves,
  a_missing_graph_is_backfilled_instead_of_linked_onto_nothing,
  a_full_rewrite_replaces_the_mirrored_facts,
  derived_fts_rows_align_with_memberships_and_are_purged,
  search_history_finds_persisted_transcripts,
  relaxed_recall_widens_the_net_after_a_strict_miss}`
- `nuo-harness` compaction integration:
  `compaction_engages_on_a_session_built_from_plain_rounds` — proves the IR is
  flushed before a fold (an unflushed graph returns `Ok(None)` and the test
  fails).
- `nuo-persistence` integration `long_session_commit_turn_scales_with_delta_not_session_length`
  — a commit no longer pays any derived cost.
- Workspace: `cargo check/clippy --workspace --all-targets` clean; the full
  persistence/harness/server/client suites pass.
