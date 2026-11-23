---
id: ADR-0244
title: "The Catalog Materializes a Fact-Free Thread Summary"
status: accepted
date: 2026-11-22
scope: persistence/db, persistence/thread, migrations
superseded_by: null
amends:
  - "[ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (the list view derived `msg_count` / `last_user_prompt` from the fact log; the catalog plane re-materialises them as a bounded read model)"
negative_knowledge: true
---

# 0244. The Catalog Materializes a Fact-Free Thread Summary

- **Status**: Accepted
- **Date**: 2026-11-22
- **Amends**: ADR-0059 (the list view derived count/prompt from the fact log),
  migration `0002_drop_derived_thread_columns.sql` (the stored columns were
  dropped while the view and the facts shared one database)
- **Related**: [ADR-0242](0242-the-derived-index-is-split-into-a-catalog-plane-and-rebuildable-content-shards.md)

---

## Context and Problem Statement

ADR-0059 made `thread_list_view` derive `msg_count` and `last_user_prompt` from
`causal_nodes`, and migration `0002_drop_derived_thread_columns.sql` removed the
stored columns. That was correct while the list view and the facts shared one
database.

The catalog/content split (ADR-0242) removes facts from the catalog. The list
view can no longer join `causal_nodes`; a picker would show every session with
`message_count = 0` and no prompt. The summary is not a second source of truth;
it is a **bounded derived field** of the catalog read model.

## Architectural Decisions

### 1. `[INV-CATALOGSUMMARY-01]` The summary is derived, never authored

`threads.msg_count` and `threads.last_user_prompt` are computed from the
session state:

- on a full rewrite, from the full transcript;
- on a delta/metadata frame, from the values
  `clone_metadata_without_history` computed against the full in-memory state
  before it stripped the transcript;
- on a legacy load, from the loaded transcript.

No caller sets them by hand.

### 2. `[INV-CATALOGSUMMARY-02]` The transcript remains the authority

The summary is a display projection. It never substitutes for the transcript
in a request, a resume, or a search. A wrong summary is a picker cosmetic
defect; a wrong transcript is data loss. The journal replay remains the
authority for both.

### 3. `[INV-CATALOGSUMMARY-03]` A metadata-only write preserves the summary

`SaveMode::MetadataOnly` inserts the computed values for a new row but does not
update them on conflict. A title/posture/digest change must not reset the
count/prompt to zero.

### 4. Migration 6 backfills once, then the projection owns it

While facts and the catalog still share a database, migration 6 adds the
columns and backfills from `causal_nodes`. From then on every catalog write
carries the summary, and the content shard can move without touching the list
view.

## Consequences

- The picker/list is independent of content-shard availability and lag.
- One O(transcript) scan per turn computes the summary; the transcript is
  already in memory and the scan is a filter/count, not a clone.
- `thread_list_view` reads columns instead of running two correlated
  subqueries per row; list queries get cheaper.
- The summary is not part of the checksum: it is derived and recomputed, so a
  mismatch is not a corruption signal.

## Alternatives Considered

| Alternative | Why rejected |
|---|---|
| Keep the fact join in the catalog | The catalog would still need facts, defeating the split |
| Compute the summary on read from the content shard | The list becomes coupled to every shard and to projection lag |
| Incremental counters maintained by every append path | More state to keep consistent; the O(n) scan is bounded and simpler |
| Leave the picker count/prompt empty after the split | A user-visible regression with no upside |

## Negative Knowledge

- **Do not treat the summary as durable conversation state.** It is
  recomputable; never reconcile against it.
- **Do not update it on metadata-only writes.** That silently zeroes the
  picker.
- **Do not let the content shard write the catalog summary.** The catalog owns
  its read model; the projector writes both from the same frame.
