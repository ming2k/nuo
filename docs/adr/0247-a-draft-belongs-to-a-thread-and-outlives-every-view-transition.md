---
id: ADR-0247
title: "A Draft Belongs to a Thread, Not to the View That Is Showing It"
status: accepted
date: 2026-12-28
scope: tui/nuo, architecture/composer, interaction/tabs, persistence/client-state
superseded_by: null
negative_knowledge: true
---

# 0247. A Draft Belongs to a Thread, Not to the View That Is Showing It

- Status: Accepted
- Date: 2026-12-28
- Amends: [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md)

---

## Context and Problem Statement

ADR-0091 gave every `ClientTab` a `TabContextState` and required that switching
tabs preserve "focus, scroll, **input**, cursor position, input scroll". The
intent was right — a lateral move must not lose the operator's work — but it
located the unsent draft on the wrong entity. Three independent losses followed,
and all three were reported:

1. **A cross-thread tab switch blanked the composer.**
   The server hosts one session per client, so switching to a *different* thread
   tab tears the TUI down and re-attaches (`TuiOutcome.switch_to_target`). The
   departing tab struct rode `TuiOutcome.tabs` into a **new** `App`; the boot
   path installed the preserved tabs and restored the arriving context. Then the
   first per-frame `sync_transcripts_and_session` noticed that
   `current_session_id` still held the boot placeholder, ran
   `on_viewed_session_changed`, and that hook cleared the composer. The draft
   survived the hand-off and died one frame later — and every thread→thread tab
   switch was a thread→thread re-attach, so *every* such switch lost it.

2. **A tab replaced in place inherited an empty draft.**
   Picking a thread in `/threads` (or `/host`) while the current thread was empty
   replaces the tab rather than opening a second one. The replacement carries
   `TabContextState::default()`, so `restore_active_tab_context` copied an empty
   string over whatever the user had typed.

3. **Nothing was durable.**
   `TabContextState` is in-memory only. A crash, a `kill`, a lost link, or the
   re-attach that follows either lost the draft outright.

Three further defects came from the same misplaced owner:

- The per-tab copy was **lossy**: it carried `input`, `cursor_position`, and
  `input_scroll` but dropped `pending_images` and `pending_text_pastes`, so a
  switch silently discarded staged screenshots and large pastes while keeping
  the `[Image #1]` chip text that referred to them.
- `DraftCapsule::restore_into` re-derived the caret as `input.chars().count()` —
  a **char** count assigned to a **byte** offset — and ignored the captured
  position, so every restore put the caret after the wrong character of any
  non-ASCII draft.
- `is_current_thread_empty` ignored the composer entirely, so a thread whose only
  content was typed-but-unsent text was "empty": `/new` recycled it, and picking
  another thread `DeleteThread`-ed it.

The concept had three encodings (`TabContextState`'s triple, `view::DraftCapsule`,
and `ComposerState::injection_stashed_input`) and two of them were lossy.

## Architectural Decisions

### 1. One value type for unsent input

`view::composer_draft::ComposerDraft` is the only representation: text, caret
(**byte** offset, clamped to a `char` boundary on install), staged images, staged
pastes, and the composer view offset. `DraftCapsule` is deleted; its two
remaining roles are named rather than typed twice:

- a **stash** (`App::composer.stashed_draft`) that lives as long as a sub-mode
  which borrows the composer line — history recall, history search, the
  input-injection sheet;
- a **parked draft** that lives as long as the thread, in the ledger below.

`injection_stashed_input` is deleted: the input-injection sheet now parks into
the one stash, which is why its staged attachments survive too.

### 2. The ledger is keyed by thread, one file per thread

```text
$XDG_STATE_HOME/nuo/drafts/<readable-id>-<sha256(id)[..8]>.json
    { "thread_id": "<id>", "draft": { text, cursor_position,
                                      pending_images, pending_text_pastes,
                                      input_scroll } }
```

- **Keyed by thread id**, never by tab index or by "the active tab": the tab is a
  view, and a view can be closed, replaced, reordered, or re-created by a
  re-attach while the thread lives on.
- **One file per thread.** One shared JSON map would make two `nuo` instances in
  different projects rewrite each other, make deletion a merge rule, and make one
  truncated file cost every draft. Per-thread files make deletion an `unlink`.
- **Under `state`, not `data`.** A draft is unsent working state for one
  installation; it is not thread history (which the server owns in the session
  journal) and must not follow the user to another machine. Under `state` rather
  than `cache` because it is not regenerable.
- The `thread_id` is repeated **inside** the payload, and a payload naming a
  different thread than its filename is ignored rather than adopted: the name is
  a sanitized derivative, and trusting it over the payload would graft one
  thread's draft onto another.

### 3. Ownership is explicit: `ComposerState::draft_thread`

The composer is one buffer; every thread has one draft; so exactly one identity
owns the buffer at any instant. `draft_thread: Option<String>` records which, and
`App::hand_composer_to_current_thread` is the **only** transition protocol:

1. park the buffer under `draft_thread` (the thread being left — read from the
   record, never re-derived from a session id that is mid-transition);
2. install the arriving thread's parked draft, or a clean buffer.

Arriving at the thread that already owns the buffer is a no-op, which is what
makes re-focusing the active tab (`Alt-1` on the tab you are on, a click on the
active pill) a non-event instead of a silent reset. Leaving the chat canvas for a
workspace scene **releases** ownership (`draft_thread = None`) after parking, so
the Dashboard's inline prompt — which borrows the same buffer — is never
mistaken for a thread's draft, and the dashboard directive is never resurrected
in the chat box.

### 4. Parking is a signal comparison, not a copy

The ledger takes a borrowed `LiveDraft` and compares a `DraftSignal`: hash and
length of the text, caret, view offset, and `(count, byte total)` per attachment
vector. Identical signals mean "no change" — no clone, no write, no dirty mark.
Attachment payloads are compared by shape rather than content because the
composer never mutates a staged payload in place (a chip is atomic; editing one
drops the payload), so the pair is a sound key and costs O(attachments) instead
of O(megabytes).

Transitions park **synchronously** (send, clear, tab switch, scene switch, exit
seal); a one-second autosave tick on the event loop covers only the "process died
mid-sentence" case. Residency is capped at 32 MiB with LRU eviction; an evicted
draft is still on disk and is re-read on demand, and the draft the user is
working in never evicts itself.

### 5. The identity that answers "is this thread real?" includes its draft

`App::is_current_thread_empty` is the predicate every recycling path consults
(`/new`, `Alt-t`, the TabBar `+`, `/threads`, `/host`). It now returns `false`
when the thread holds a parked or live draft: discarding a thread whose only
content is text the user can see is exactly the loss this ADR exists to prevent.

### 6. Failure policy: a draft is not durable history

A read that fails yields "no draft" plus a warning; a write that fails keeps the
entry dirty and retries after a 15-second backoff; an unparseable file is
**left in place**, because it may be the only copy of the text and deleting it to
tidy up is the irreversible action. None of these may block a frame or fail it.

### Normative Invariants Established

- **[INV-DRAFT-01] One Owner per Thread**: A thread's unsent draft has exactly one
  authoritative store — the parked-draft ledger, keyed by thread id. No view, tab,
  or scene may hold a second writable copy.
  *Verification*: `shell::tests::draft_persistence::a_tab_switch_parks_and_restores_the_draft_per_thread`,
  `...::replacing_a_tab_in_place_keeps_the_departing_threads_draft`.
- **[INV-DRAFT-02] Drafts Cross Every View Seam**: A cross-thread tab switch,
  scene switch, tab replacement, tab close, and process exit all preserve the
  departing thread's draft, and all install the arriving thread's.
  *Verification*: `...::the_first_frame_session_sync_installs_the_arriving_draft_instead_of_blanking_it`,
  `...::a_workspace_scene_parks_the_draft_and_releases_the_buffer`,
  `...::sealing_on_exit_makes_the_draft_survive_the_process`.
- **[INV-DRAFT-03] A Restored Draft Is Byte-Exact**: Text, caret, staged
  payloads, and view offset round-trip through the ledger unchanged; the caret is
  clamped to a `char` boundary rather than re-derived.
  *Verification*: `view::composer_draft::tests::*`,
  `shell::draft_store::tests::attachment_payloads_survive_the_round_trip`.
- **[INV-DRAFT-04] Unsent Text Is Content**: A thread holding a live or parked
  draft is not empty and is never recycled in place, never `DeleteThread`-ed, and
  its draft is deleted only with the thread.
  *Verification*: `...::a_thread_with_a_parked_draft_is_not_empty`,
  `...::sending_clears_the_parked_draft`, `...::a_parked_draft_can_be_dropped_with_its_thread`.
- **[INV-DRAFT-05] The Ledger Never Fails a Frame**: A failed draft read or write
  degrades to a warning plus a retry; it may not block, panic, or abort a frame.
  *Verification*: `shell::draft_store::tests::an_unreadable_file_is_ignored_and_left_in_place`,
  `...::a_payload_naming_another_thread_is_never_adopted`.

### Amendment to ADR-0091

`TabContextState` now carries **view** state only (`focus`, `scroll`). The draft
fields it introduced (`input`, `cursor_position`, `input_scroll`) are deleted. The
`[INV-TAB-12]` rule is retained for what remains — focus mode and scroll offset
are per-tab — and the draft half of it is superseded by `[INV-DRAFT-01]`, which
puts the draft on the thread instead.

### Amendment to ADR-0051

`DraftCapsule` is deleted. `[INV-COMP-03]` (atomic stash and guaranteed rollback
when a transient sub-mode borrows the composer line) is unchanged and now uses
`ComposerDraft`; the same value type serves the parked-draft ledger, so the stash
and the park cannot drift apart.

## Consequences & Trade-offs

- **Positive**: Six reported losses close at their root rather than at their
  symptoms — cross-thread switch, in-place replacement, restart, staged-payload
  loss on switch, non-ASCII caret misplacement, and recycling of a thread the
  user had typed into. The "is this thread real?" predicate now answers a
  question that includes the user's work. One type, one owner, one transition
  protocol: a new transition path has one function to call instead of a rule to
  remember.
- **Negative & Incurred Costs**: The client now writes to the state directory
  during normal editing (one file per changed draft, on transitions and at most
  once a second), and reads it at boot and on demand. `TabContextState` loses
  three fields, so any future code that reached for `context.input` must go
  through the ledger. A draft is per installation: moving to another machine
  loses unsent text, which is the intended trade against copying prompts onto
  machines where they were never typed there.
- **Accepted residue**: a thread deleted by a path the client does not see — the
  `nuo thread rm` CLI, a peer machine — leaves its draft file behind. The client
  cannot enumerate the server's threads from here, so the alternative is either a
  TTL that would delete a draft the user still wants or a GC that depends on a
  scan this layer does not own. The residue is one small file per such thread,
  never restored (a deleted thread id never returns), and the in-app deletion
  paths (`/threads`, `/host`, `/new` recycling) do forget it.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep the draft on the tab and fix only the blanking hook

- **Why considered**: Smallest possible change; `on_viewed_session_changed` was
  the visible culprit.
- **Why rejected**: It fixes one of six defects. The in-place replacement still
  overwrites with a `Default` context, the per-tab copy is still lossy for staged
  payloads, the caret is still re-derived in the wrong unit, nothing is durable,
  and *every* future transition path still has to remember to copy the draft. The
  ownership was the defect, not any one code path.

### Alternative B — Persist one JSON map of every draft under a single key

- **Why considered**: One read and one write, no per-file bookkeeping.
- **Why rejected**: Two `nuo` instances in different projects would rewrite each
  other's entries; deleting a draft becomes a merge rule rather than an `unlink`;
  and a single truncated file costs every draft instead of one.

### Alternative C — Resume across machines by keeping drafts in the session store

- **Why considered**: `data/` already follows the user's thread history, so the
  draft would appear wherever the thread does.
- **Why rejected**: The draft is not thread history — it is what the user has not
  said yet, and it is not durable content the server owns. Copying it into the
  session journal would make unsent text a durable fact, put it in every
  transcript projection, and let a prompt the user never sent appear on another
  machine.

### Alternative D — Autosave on every keystroke, synchronously

- **Why considered**: The smallest possible loss window.
- **Why rejected**: An fsync per character makes typing latency a function of the
  disk, at exactly the moment latency is most visible. The one-second cadence plus
  synchronous parking on every transition keeps the worst case at "one second of
  typing", which is the right trade.

### Alternative E — A blank tombstone instead of deleting an emptied draft

- **Why considered**: Uniform "every thread has a record" storage.
- **Why rejected**: Sending a message and clearing the composer are precisely the
  moments the user made the draft stop existing. A blank record makes the next
  restore indistinguishable from a real empty buffer, and leaves files behind for
  every thread ever touched.

## Links

- Amends: [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md)
- Complements: [ADR-0018](0018-on-demand-session-review-diagnostics-and-multi-process-history-union.md), [ADR-0123](0123-attach-failure-typing-stale-tab-retirement-and-identity-addressed-session-rebasing.md), [ADR-0186](0186-state-is-partitioned-and-giants-are-prohibited.md)
