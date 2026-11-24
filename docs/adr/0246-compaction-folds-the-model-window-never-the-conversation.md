---
id: ADR-0246
title: "Compaction Folds the Model Window, Never the Conversation"
status: accepted
date: 2026-06-09
scope: [protocol/thread, protocol/transcript, protocol/thread_view, persistence/thread, engine/compaction, nuo/shell]
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0246. Compaction Folds the Model Window, Never the Conversation

- Status: Accepted
- Date: 2026-06-09
- Related: [ADR-0160](0160-compaction-is-a-transcript-fact-and-the-causal-node-is-a-derive.md),
  [ADR-0173](0173-thread-is-the-durable-ssot-branch-is-a-cursor.md),
  [ADR-0175](0175-session-ir-is-deleted-and-the-transcript-is-a-projection.md),
  [ADR-0239](0239-the-journal-carries-the-branch-graph-and-sqlite-branch-rows-are-derived.md),
  [RFC-0003](../rfc/0003-thread-branch-fact-and-request-assembly.md)

## Context and Problem Statement

A resumed thread's transcript could not be scrolled back past a particular turn
(observed: "turn 12"), with no marker explaining why the history began there —
and, in some sessions, history that had been visible a moment earlier vanished
after a reconnect.

The cause is a category error in the fact-graph API, not a rendering defect.

`Thread::path(branch)` walked from the branch head and **stopped at
`branch.compaction_horizon`**. That one walker answered two incompatible
questions at once:

1. *what may the model see in this request?* — correctly bounded by the
   compaction horizon;
2. *what did the humans and the model actually say in this thread?* — bounded by
   nothing.

Four consumers asked question (2) and received answer (1):

- `transcript_from_thread` — the UI/durable transcript projection — read
  `path()`, so the instant a fold landed, the folded rounds disappeared from the
  document;
- `full_transcript()` — the hydration payload of `ThreadSnapshot` and
  `ConversationReplaced` — inherited that projection, so every attach, every
  `/resume`, and every server "client lagged, re-anchoring from snapshot" path
  replaced the visible history with the horizon-bounded tail;
- `fork_fact_for_target` and `path_contains` refused to fork into folded
  history;
- `live_facts()`/`gc_eligible_facts()` treated folded facts as unreachable, so
  the same horizon that hid them also marked them for deletion.

Three aggravating defects made the result unreadable rather than merely short:

- **The checkpoint's watermark was derived from an id.** `transcript_from_thread`
  computed `CompactionMeta::folded_through_seq` by parsing the *fact id*
  (`fact_<uuid>`) as an integer, which always yields `0`. The `compact` directive
  therefore hid nothing, and the mirror only looked correct because `path()` had
  already truncated the entries.
- **The checkpoint was positioned by accident.** Because the mirror was
  truncated, only the checkpoint and the kept tail survived, so the actor
  (the *reason* the thread looked short) appeared both first and last depending
  on which projection a reader used: `Transcript::project` emits it first for
  the model, while the human-facing restore emitted it at the tail.
- **The cut is turn-agnostic.** Automatic compaction runs in
  `CompactionCutMode::TargetTokens` mode and only avoids splitting a tool call
  from its observation. The first kept fact is wherever the token budget landed
  — frequently mid-turn — which is why the wall was reported at a turn boundary
  the user could name but not explain.

## Decision

### [INV-CMP-07] A branch has a conversation and a request window; they are different walks

`Thread` exposes two walks with disjoint meanings:

| method | meaning | callers |
|--------|---------|---------|
| `path(branch)` | **the conversation**: head → root, every fact the branch reaches | transcript mirror, hydration, export, Archivist reads, fork targets, GC roots |
| `live_path(branch)` | **the request window**: head → `compaction_horizon` inclusive | `ThreadWindow`/`RequestView`, the compactor's fold candidates, `latest_compaction` |

The horizon is *not* a visibility filter. It is the boundary of the context the
next request assembles. `compaction_horizon` keeps one meaning and one reader
family. A new caller must choose which question it is asking by name, not by
passing a flag.

### [INV-CMP-08] A fold never removes a row from the conversation

Compaction is context relief. It may summarise, it may bound the request, and it
may cost the model detail — it may not rewrite what the thread said:

- the transcript mirror, hydration snapshots, export, Archivist reads, and fork
  targets read `path()`;
- `Transcript::project()` — the model-facing derive — is the only consumer that
  applies a `compact` watermark;
- GC roots are the union of **full** branch paths, so no fact becomes eligible
  because a fold covered it. A fact is eligible only when no branch reaches it
  at all (`[INV-CMP-02]` does not require the horizon to own reachability: the
  kept tail's parent edge crosses the folded range).

Retention is a user decision (`/thread rm`) or an explicit retention policy. It
is not a side effect of staying inside a token budget.

### [INV-CMP-09] A fold's seam is a reference, resolved by the reader

The checkpoint fact carries `first_kept_fact_id`: "everything before this fact
was folded". Its watermark is **derived** by whichever reader holds the order —
`Transcript::project` does it in entry space by locating the id, and the mirror
does it while building entries.

Two rules follow, and both are enforced by construction rather than by comment:

1. **No position is stored twice.** The fact does not carry a parallel
   `folded_through_seq`; a second coordinate that must agree with the first is
   how a stale watermark gets in.
2. **No identity is parsed as a position.** Deriving a sequence from an id is
   forbidden: the previous arm produced `0` for every UUID id, i.e. a directive
   that hid nothing.

`Thread::compact` validates `first_kept` against `live_path`, not `path`: a fold
may only keep something that is already inside the window it was asked to
relieve, otherwise a caller could move the horizon *backwards*.

### [INV-CMP-10] The checkpoint reads where it happened

The checkpoint's fact is appended at the branch head, at the moment of the fold:
after the tail it kept, before the messages that follow. That position is
already the truth a live client observes — `RoundEvent::Compacted` arrives as a
turn-seam event — so the restored transcript keeps it verbatim. No projection
rewrites the row's position, which is what makes live and resumed views agree
row for row.

The consequence is intentional: a fold no longer needs to explain a hole,
because the conversation has no hole. The card's job is to say *what the fold
did to the model's window*, and it says that where it did it.

### [INV-CMP-11] One count, one meaning

"Messages" is a dialogue count, not an entry count and not a window length.
`Transcript::message_count()` is the single definition; the stored
`msg_count` column, the picker row, the session detail, and the hydration
summary all read it. Before this decision `msg_count` counted `EntryPayload::
Message` entries while `ThreadSummary::message_count` and the session detail
counted `entries.len()` — including `state` entries and compaction checkpoints —
so the same session reported different sizes on different surfaces.

## Consequences

- **Resuming, re-attaching, and re-anchoring after a broadcast lag show the
  whole thread.** The truncated-tail symptom is gone without a wire change: the
  snapshot already carried `full_transcript()`, it was simply fed by the wrong
  walk.
- **Existing databases repair on read.** Folded facts were never deleted,
  because the horizon's reachability rule marked them GC-eligible but
  `ThreadStore::gc` is not on any production path. History that is invisible
  today reappears as soon as the projection reads `path()`.
- **Turn numbering becomes honest.** Restore reconstructs `(round, turn)` by
  counting visible driving-user and assistant messages, so counting over the
  full conversation reproduces the original numbering. Counting over a horizon
  tail was what made `rebase_transcript_rounds` necessary in the first place;
  the rebase remains as the guard for genuinely partial history (legacy sessions,
  forked prefixes), not as the normal path.
- **The UI gains no new scrolling mechanism.** `max_scroll` was always correct —
  `content_lines - view_height`, floored at zero — the rows simply did not exist.
  Virtualisation, the height cache, and pagination are untouched.
- **Snapshot size is honest.** A long thread's hydration is its conversation.
  Tool bodies are already bounded by the observation lifecycle
  (`ObservationLifecycle::ActiveTruncated` + CAS), which is where weight
  belongs — not in dropping rounds.
- **The compactor's candidate set is unchanged.** It folds `live_path()`, so a
  second fold still examines only the unfolded tail (`[INV-CMP-04]`).
- **The produced schema does not move.** The checkpoint variant's shape is
  unchanged (a new field would have been a `THREAD_SCHEMA_VERSION` break for a
  derived value); only its *reading* changes, which is why no migration is
  needed.

## Discarded Alternatives (Negative Knowledge)

- **Keep one walker and add a `truncate: bool` parameter.** The two callers
  disagree about the *meaning* of a branch, not about a flag. A boolean lets a
  new caller pick the wrong answer silently; two names make the choice explicit
  at every call site.
- **Store `folded_through_seq` on the fact.** Tried first and reverted: it
  duplicates a coordinate the reader can derive, and it forced every writer
  (compactor, mirror, engine) to keep two representations in agreement. A
  reference plus a derived seam has one owner.
- **Render the checkpoint at its semantic seam (before the kept fact).**
  Tried and reverted. The live client observes the fold as an event at the tail;
  moving it only in restored views makes the two readings disagree, and the move
  exists to explain a hole that `[INV-CMP-08]` removes.
- **Make the UI read `projected_out()` (the archived originals).** That keeps
  the fold as a *visibility* fact, so every reader would have to re-join two
  arrays and would still lack the rows GC marked. It re-introduces the second
  truth ADR-0175 deleted.
- **Page the transcript instead of un-hiding it.** `TranscriptView` paging exists
  and is correct, but it answers "the conversation is too large to hydrate at
  once". It cannot answer "the server decided to forget", which is what produced
  the report; paging over a horizon tail would have formalised the data loss.
