---
id: ADR-0166
title: "A Tool Call Has One Identity and the Tool-Step Reducer Is Idempotent"
status: accepted
date: 2026-11-10
scope: [workspace]
superseded_by: null
negative_knowledge: true
---

# ADR-0166: A Tool Call Has One Identity and the Tool-Step Reducer Is Idempotent

- **Status:** Accepted
- **Date:** 2026-11-08
- **Related:** [ADR-0026](0026-streamed-tool-input-progress.md), [ADR-0046](0046-bifurcated-tool-execution-lifecycle-and-progressive-in-flight-tui-states.md), [ADR-0162](0162-stream-restarts-are-a-separate-budget-and-visible-attempts-are-discarded.md)
- **Supersedes in part:** ADR-0026 §"2026-11-07 Amendment: Announcement Identity and Retraction"

## Context

ADR-0162 stopped a retry storm from replaying a visible attempt. It did not
remove the reason the replay left residue: the pipeline modelled one tool call
under **three identities**.

| identity | minted where | used for |
|---|---|---|
| `ToolCall.id` (provider id) | provider adapter | the model's `tool_call_id`, history |
| `call_{uuid}` (dispatch id) | `dispatch_preflight` | every UI event after dispatch |
| `announcement: u64` | `next_announcement_id()`, a process-wide `AtomicU64` | every UI event before dispatch |

The UI therefore had no identity at all during the argument stream. It inferred
the state from an **empty `id` sentinel** (`tool_step("", …)` ⇒ `Ingesting`), and
the whole announce → collapse → result handshake was a chain of lookups with
different tie-break rules:

- collapse and progress resolve the **last** matching announced row (`rfind`);
- `ToolResult` and `ToolCancel` resolve the **first** matching dispatch row
  (forward scan + `break`);
- the announcement map is keyed by the **provider slot**, while dispatch looks
  its announcements up by **batch position** — two namespaces that only happen
  to coincide.

That asymmetry is what makes a duplicate fatal rather than harmless. A duplicate
`ToolCallStarted` was an unconditional `messages.push()`, so it created a second
row; the first row then absorbed the result and the second stayed `Ingesting`
forever — `Read file (receiving input)`, permanently. ADR-0026's own "Negative
Consequences" predicted exactly this ("a frontend that announces twice would
double-insert a step") and the retraction mechanism was the only thing standing
between the model and that outcome.

Retraction itself was live-only and fragile:

- `ToolAnnouncementsRetracted` is an ordinary wire event. It is lost if the
  client link is down when the harness emits it (the relay's `tx.send` result is
  discarded), and it is never durable;
- the client's boundary sweep was scoped by a translator-local
  `live_announcements_by_session` set, which is empty in the listener that a
  relink spawns, so a reconnected client could not sweep its own orphans;
- one abort path returned before the retraction call
  (`rounds.rs`, `cut_without_content && stream_loop_hard_stopped`);
- `announcement: u64` is process-local, so it is not a legitimate cross-process
  key at all: a restarted server restarts the counter.

None of this is a bug in one line. It is the shape of the model: **a provisional
row whose key does not exist yet, resolved by arrival order, in a reducer that is
not idempotent.**

## Decision

**A tool call has exactly one identity, established the first time the stream
names it, and the transcript reducer is idempotent and monotone in that
identity.**

### 1. One identity: the call id

`ToolCall.id` becomes the single identity of a call for *both* audiences — the
model-visible `tool_call_id` and every UI/wire event.

- Every provider lane already guarantees a non-empty id at the moment the call's
  name is known: Anthropic sends `id`+`name` at `content_block_start`, the OpenAI
  lanes send `id` with the first delta, and the Gemini lane synthesises
  `call_{uuid}` when the payload carries none.
- The harness mints `call_{uuid}` at the first named fragment **only if** the id
  is still empty, and freezes it: a later provider id fragment can never re-key a
  row that the frontend already holds.
  A provider that supplies its id *after* the name is therefore outside what the
  workspace models. The harness keeps the announced id and emits a `tracing::warn`
  naming both values — it does not silently drop the anomaly, and it does not
  re-key. This is a deliberate trade: re-keying would have to reach every
  consumer that already holds the old id, and every first-party lane (Anthropic,
  the OpenAI lanes, Gemini) supplies the id with the name.
- A provider that returns **two calls with the same id in one response** is
  refused as a protocol violation (`ProviderErrorKind::Protocol`), not merged.
  Merging would make one call silently wear the other's result; the attempt's
  announced rows are retracted before the error surfaces.
- `dispatch_preflight` no longer mints anything, and no longer carries an id
  array: it reads `call.id` at the site that needs it.
- The text-fallback path reuses the id `parse_text_tool_call` already mints.
- The restore path carries the call id as well. A persisted assistant
  `tool_calls[].id` and the answering tool message's `tool_call_id` pair by
  identity, so two same-named calls restore as two distinct rows and a reordered
  result stream still lands correctly. An id-less legacy entry gets a synthetic
  unique id; the tool name is never used as an id. The per-name FIFO is kept only
  as the fallback for a result whose `tool_call_id` is absent or empty.

There is exactly **one** identity. A provider-supplied id is adopted as that
identity, so the id in the model's tool-use payload and the id the UI keys the
row by are the same string; a provider that supplies none gets a harness-minted
`call_{uuid}`, used for both. `announcement: u64`, `next_announcement_id`, the
announcement→slot map, and the empty-id sentinel are deleted.

So is every **derivable parallel** of the call id, because a copy that can drift
is the same defect in a different shape: `PreparedDispatch::call_ids`,
`ToolResultRecord::call_id`, the `call_id` parameter threading through
`execute_tool` / `execute_tool_evented`, `schedule_tool_calls`' `call_ids`
slice, and the ignored `_call_id` argument of `format_tool_result_message`.
Every one of those sites now reads `ToolCall.id`.

The wire stops carrying correlation the client cannot need:

```text
ToolCallStarted    { id, name }                        // was { announcement, index, id: Option, name }
ToolInputProgress  { id, bytes, arguments? }           // was { announcement, index, id: Option, bytes, arguments? }
ToolCall           { id, name, arguments }             // was { announcement: Option<u64>, … }
ToolCallsRetracted { ids: [String] }                   // was ToolAnnouncementsRetracted { announcements: [u64] }
```

### 2. One reducer, idempotent and monotone

`TranscriptEdit::ToolStart`, `ToolAnnounce`, and `ToolCallCollapse` collapse into
one edit: `ToolStepUpsert { message }`. The applier resolves **by call id**
through one helper and:

- pushes when the id is absent;
- otherwise merges **monotonically**: `name`/`arguments` are adopted when the
  incoming fact carries them, and the lifecycle only advances
  (`Ingesting → Executing → terminal`). A duplicate, reordered, or replayed
  fact is a no-op by construction.

There is no first/last tie-break because there is nothing to tie-break: an id
identifies one row.

### 3. Retraction removes only provisional rows

`ToolCallsRetracted { ids }` removes a row **only while it is still
`Ingesting`**. A row that has dispatched is a real call and must survive a
retraction that crossed it in flight.

### 4. A settled attempt contains no `Ingesting` row

The client no longer tracks live announcements. It retracts **all** `Ingesting`
steps (`TranscriptEdit::RetractPendingToolSteps`) at the events that *mean* the
attempt is over:

- a round outcome — `RoundCompleted`, `RoundInterrupted`, or a terminal
  `RoundEvent::Error` (`RoundInterrupted` is deliberately not emitted for
  `reason == Error`, so the error event is its own boundary);
- a scheduled retry — the failed attempt is definitively over.

`HarnessState` is deliberately **not** used as the trigger. A snapshot is a
*state sample*, not an edge, and the server publishes one for control-plane
reasons too. An `Idle` sample that raced a live round would sweep rows that are
still streaming. The same audit found that the trust/reload handler broadcast
`Idle` unconditionally; it now publishes only when no round is live, so the
snapshot cannot lie about the round it is describing.

This is an invariant of the lifecycle, not bookkeeping: it needs no ids, so it
stays correct after a relink, where no translator-local set exists.

### 5. Every abort path retracts

Including the loop hard stop that previously returned before the retraction
call. The rule is: an attempt that announced and did not dispatch retracts
before it returns.

### 6. One display truth

`tool_step_summary` derives the `receiving input` clause from
`status == Ingesting` (plus the optional byte count), exactly as
`tool_step_semantic_summary` already did — not from a separate announce flag.

## Invariants

- `[INV-TOOL-ID-01]` **One identity.** A call is addressed by `ToolCall.id` from
  the first named fragment until its result. Announcement, progress, dispatch,
  stream, preview, and result events all carry that id. Within one response an id
  identifies exactly one call; a provider that reuses one is refused, not
  silently merged, and a late id never re-keys an announced call.
- `[INV-TOOL-ID-02]` **Idempotent.** Applying the same tool-step fact twice
  produces the same transcript as applying it once.
- `[INV-TOOL-ID-03]` **Monotone.** A tool step's lifecycle never regresses:
  `Ingesting → Executing → {Ok, Failed, Denied, Cancelled, Interrupted}`.
- `[INV-TOOL-ID-04]` **Retraction is provisional-only.** Retracting a call id
  removes a row only while it is `Ingesting`; a dispatched call is never
  removed by a retraction.
- `[INV-TOOL-ID-05]` **No orphan ingestion.** A round outcome — completion,
  interruption, or a terminal error — or a scheduled retry leaves no `Ingesting`
  row behind. A `HarnessState` sample is not a boundary and never triggers the
  sweep.
- `[INV-TOOL-ID-06]` **One session per buffer.** A `Round` frame is applied only
  when its session is not one this client has switched away from
  (`ConversationCleared` / `ConversationReplaced` supersede the session they
  leave; a `ThreadSnapshot` un-supersedes the session it hydrates). A superseded
  round's trailing frames are dropped, so no id-less sweep and no fallback insert
  can act on the wrong session's rows.

## Consequences

- The `receiving input` wall cannot be reproduced by retries, duplicate
  delivery, a relink, or a server restart, because none of those can create a
  second row for one call, and none can leave a provisional row past the round.
- The client no longer needs `live_announcements_by_session`; the translator
  keeps no tool-call bookkeeping at all.
- `PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION` move to 24. This is a clean
  break, not a compatibility layer: a v23 peer cannot deserialize the new
  variants, and the version gate refuses it before any session work.
- A provider that supplies a tool id *after* the call's name would have its id
  replaced by the harness's minted one. No lane in the workspace does this; the
  alternative (re-keying a row the frontend already holds) is strictly worse and
  is the class of bug this ADR removes.
- The renderer gains nothing new to draw: `Ingesting` and its byte clause already
  existed. What changes is that the state is now owned by the model instead of
  being inferred from a sentinel.
- The dead parallel representation is deleted rather than left beside the new
  one: `nuo_view::model::document::ToolInvocation`, `ToolInvocationStatus`, the
  `*_invocation_*` alias methods, `as_tool_invocation`, and
  `ToolStepStatus::default()` (whose `Ingesting` default could manufacture a
  provisional row). One model, one set of names.
- The id-less boundary sweep is safe because the translator **enforces** that
  the buffer holds exactly one session. A connection subscribes to one session's
  event channel; `ConversationCleared` / `ConversationReplaced` replace the
  buffer and mark the session they leave **superseded**; a `ThreadSnapshot`
  un-supersedes the session it hydrates. A round frame whose session is
  superseded is dropped before any edit. This is *not* an equality check against
  the live id — a hydration path that repoints the buffer must not be starved —
  and it is needed because a switch does **not** join the round it supersedes
  (`cancel_current` only signals the token), so that round really can emit
  trailing frames afterwards. Without the gate, a late result with no matching
  row would be inserted into the new transcript as a fallback, and a late
  control-plane snapshot could tear down a live round's state. A switch also
  clears the translator's per-round state (retry progress, the reasoning clock,
  the per-session position and turn-time entries), because the terminal frames
  that would have cleared them are exactly the frames the gate now drops.
- The retraction sweep is driven by round *outcomes*, never by a `HarnessState`
  sample, and the server no longer publishes `Idle` while a round is live —
  including from the trust/reload handler, which used to broadcast it
  unconditionally. A state sample that every frontend treats as a boundary must
  not lie about the round it describes.

## Validation

- `a_duplicate_announcement_does_not_insert_a_second_step` — applying the same
  `ToolStepUpsert` twice yields one row.
- `a_duplicate_announcement_after_dispatch_cannot_regress_the_step` — a late
  `Ingesting` fact leaves an `Executing`/terminal row untouched, including its
  full name (the announcement's name may be a fragment prefix).
- `a_replayed_partial_announcement_cannot_shorten_the_dispatched_name` — the
  same guard at the model boundary.
- `progress_after_dispatch_cannot_rewrite_the_arguments` — a late progress tick
  is a no-op once the call dispatched; a partial fragment never replaces the
  complete argument object.
- `only_an_ingesting_step_is_retractable` and
  `retraction_after_dispatch_leaves_the_dispatched_step` — a retraction that
  crosses a dispatch keeps the dispatched row.
- `a_settled_round_retracts_every_ingesting_step` — the boundary sweep leaves no
  `Ingesting` row.
- `restored_tool_steps_carry_the_call_id_and_pair_by_identity` — a restored step
  is keyed by the call id, and a reordered result stream still pairs correctly.
- `a_switch_supersedes_the_session_it_leaves`,
  `a_no_op_switch_does_not_supersede_the_current_session`,
  `hydration_un_supersedes_the_session_it_rebuilds`, and
  `switching_back_and_forth_tracks_only_the_current_session` — the
  `SessionGate` state machine that scopes the buffer.
- `streamed_tool_input_is_announced_then_progressed_before_completion` (harness)
  — `ToolCallStarted.id` equals the later `ToolCall.id` for the same call on a
  fragmented-argument stream, and the call is announced exactly once.
- `a_provider_that_reuses_a_call_id_is_refused_not_merged` (harness) — two calls
  under one id fail the attempt as a protocol error; their rows are retracted and
  nothing is dispatched.
- `a_late_provider_id_cannot_re_key_an_announced_call` (harness) — a late id
  never replaces the announced one; the announced id is the call's identity for
  its whole life.
- `an_aborted_stream_retracts_its_announced_calls_rather_than_cancelling_them`
  (harness) — retraction names the same call id the announcement carried.
- `a_tool_argument_loop_cut_retracts_the_call_it_announced` (harness) — the loop
  cut retracts *before* it drops the fragments the retraction names.
