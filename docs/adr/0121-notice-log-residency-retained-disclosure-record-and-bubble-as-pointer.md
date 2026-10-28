---
id: ADR-0121
title: "Notice Log Residency: The Retained Disclosure Record, Its Viewer, and the Bubble as a Pointer"
status: accepted
date: 2026-12-11
scope: presentation/nuo-view, presentation/nuo-ui-primitives, host/nuo, tui/event-loop, tui/overlays, tui/chrome
superseded_by: null
negative_knowledge: true
---

# 0121. Notice Log Residency: The Retained Disclosure Record, Its Viewer, and the Bubble as a Pointer

- Status: Accepted
- Date: 2026-12-11
- Deciders: Nuo Architecture Working Group
- Consulted: Interface Architecture, Presentation Substrate
- Amends: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — the `L2` receipt-log tier gains its
  second viewer and the thread scene stops declaring `MINIMAL`

---

## Context and Problem Statement

ADR-0116 created the `L2` surface-log tier and stated the correct destination for
every piece of content that is neither dialogue nor an acknowledgement. It then
recorded, honestly, that the tier had **exactly one viewer** — the dashboard's
cockpit console — and that the thread scene therefore ran
`SurfaceCapabilities::MINIMAL`, which degraded diagnostics to the toast layer.

The degradation was documented as a compromise. In practice it was the original
defect, unchanged:

1. **The bubble has one slot.** `notice_toast_until` / `notice_toast_message` are
   single fields, and a newer disclosure *replaces* an in-flight one. A second
   fault arriving 400 ms after the first erases it. The user never sees either
   in full, and there is no trace that the first one happened.
2. **The bubble lives ~2.6 s and dies on any keypress.** `tick_toast_timers`
   clears it on a wall-clock deadline, and `process_one_event` clears it on the
   first `Key`/`Paste` event. A user who looks down at the keyboard — which is
   where their hands are in a TUI — loses the message.
3. **What was retained was a side channel, not a record.** A private
   `ReceiptLog` kept the last eight *warning/error* disclosures as
   `(severity, String)` with no timestamp and no topic. It was reachable only by
   a palette verb that *copied* it; nothing on screen ever said it existed, so a
   user who missed a bubble had no way to discover that a record of it was
   sitting there. Info-level disclosures were dropped at the door entirely.
4. **The retained record and the rendered message could differ.** The receipt
   path and the bubble path each formatted the same disclosure independently, so
   "what I saw" and "what I copied" were two different strings.

The user-facing statement of the defect is the one that matters: *the message
that vanishes before it is read is the message the user needed to copy.*

## Decision Drivers

- **A transient channel cannot be the only channel.** Retention must not depend
  on the user having been present at the moment of delivery.
- **Retention must be unconditional; interruption must not be.** Every
  disclosure is worth keeping — "what did that command say?" is asked a minute
  later — but only a fault justifies taking the screen.
- **A record nobody can find is not a record.** The log needs a persistent,
  always-visible entry point and an unread signal, or it is the old side channel
  with a new name.
- **One registry** ([ADR-0116](0116-content-residency-and-surface-layer-topology.md) `[INV-LAYER-01]`). The notice log
  must be a `SurfaceLog` in `App::logs`, not a second store with a second cap and
  a second residency contract.
- **Zero legacy burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)) — no
  `#[deprecated]` alias for the retired receipt path.

## Architectural Decisions

### 1. The log is the home; the bubble is a pointer to it

One funnel, `App::record_notice`, decides residency for every producer:

```text
disclosure ──► append to SurfaceLog(NOTICES)   ← unconditional
                    │
                    ├─ status ∈ {Warn, Failed} ──► bubble + hint row ("Ctrl-o · all notices")
                    └─ status ∈ {Info, Ok}     ──► bubble only (no hint, shorter)
```

`raise_notice_bubble` runs *after* the append, so a bubble can never outlive its
record and a producer cannot express "bubble without record". The bubble's hint
row is what makes the interruption a pointer rather than a dead end; it is
reserved **before** the message is wrapped (`components::toast`), so a long
message yields rows to the hint instead of truncating the escape hatch away.

`NoticeSeverity::is_fault` is the single predicate for "does this interrupt",
read by the bubble's shape, duration and hint together, so the three cannot
drift. `LineStatus::from_notice_severity` is total, so a new severity cannot
compile until its residency is chosen — the same enforcement `layer_for` applies
to a new `NoticeKind`.

### 2. Retention is not severity-filtered

The retired `ReceiptLog` dropped `Info` disclosures. The notice log keeps them:
an acknowledgement is a *record* of an answer the user asked for, and the viewer
is where they go when they have forgotten it. What `Info` does **not** do is
raise the fault badge — a badge that lights up for "copied to clipboard" trains
the user to ignore the one that matters.

### 3. The viewer is a client-global surface, not a dialog

`NoticeViewer` state lives on `App` and the panel is painted above every overlay
(`UiKey::Notices`, barrier + modal). It is deliberately **not** a `DialogKind`:
`DialogKind::scope()` would make it `Session`- or `Scene`-scoped, and the
session-switch unwinder would discard the log at exactly the moment the user
switched tabs to go looking for it. A clipboard failure is a fact about this
client process, not about the conversation on screen.

The consequence is that the router must be told about the viewer explicitly
(`Dispatch::notice_viewer_open`), or its arrow keys would scroll the transcript
behind it. That field is the price of not abusing `OverlaySurface`, and it is
cheaper than the lifetime bug the abuse would cause.

### 4. The thread scene declares the log instead of degrading

```rust
// nuo_view::model::layer
pub const SHELL: SurfaceCapabilities = SurfaceCapabilities {
    toast: true,
    banner: false,
    log: Some(SurfaceId::NOTICES),
};
```

`layer_for` now takes only `(notice, caps)`. The retired `ambient` parameter let
a client be told to write into a log its capabilities had *not* declared — which
is precisely how content reaches a log nobody can open. The declaration is the
single authority.

### 5. The unread badge is the log's persistent affordance

`SurfaceLog` owns its own `unread` watermark (`push` increments, `mark_read`
clears), so the count survives a scene switch, a re-layout, and a viewer that has
never been constructed. It cannot be reset by an unrelated UI-tree rebuild — the
bug class that makes "you missed 3 errors" unreliable.

The badge is painted on the model bar — the one chrome row present whenever a
session is — as `!N notices` plus the chord that clears it, and it is a click
target. `has_unread_fault` gates it: unread **faults** only.

### 6. Dwell time is derived from reading load, inside published bounds

A single magic number cannot be right for both `link down` and a three-clause
backend error, so the bubble's lifetime is computed:

```text
ack:        4 000 ms                              (Material snackbar default)
fault:      clamp(6 000 + 400·words, 6 000, 10 000)
```

The bounds are external, not taste:

- **5 s** is WCAG 2.2.1's perception threshold. Content that appears
  automatically and disappears before five seconds never gave a user who was
  looking elsewhere a fair chance, so *every* fault dwell clears it.
- **10 s** is Material's snackbar maximum: past it an unattended interruption
  stops being feedback and becomes an obstruction over the transcript.

An acknowledgement sits deliberately *below* the floor (4 s) because it is not
information the user still needs — they caused it a moment ago and can repeat it.
That asymmetry is the whole reason the two shapes exist separately.

### 7. A fault bubble yields only to an `Esc` that has no other job

The retired behaviour cleared *any* bubble on *any* key or paste. That silently
defeated every dwell constant above: a fault then lived only as long as the user
happened to sit still, which is exactly the "it vanished before I looked up"
complaint restated. So:

- **acknowledgement** → any key dismisses it (it must never linger over the
  composer the user is returning to);
- **fault** → dies on its timer, or on `Esc` **when `Esc` has no other
  meaning** — not with the viewer open (that press closes the panel), not with an
  overlay or sheet open (that press backs out of it). One keystroke must never
  destroy both the interruption and the place it points at.

### 8. The viewer owns its own viewport arithmetic

The cursor is an index into the *notices*, while the body's rows are not
one-per-notice: an expanded notice contributes a wrapped block. The generic list
page's `follow` mechanism scrolls to a row index, so reusing it left the cursor
able to scroll out of the panel once `Enter` grew the selected row. The viewer
therefore measures its own viewport and applies the minimum scroll that keeps the
selected *row* visible, which also makes a single `↓` at the bottom edge move the
view by one row instead of recentering.

## Normative Invariants Established

- `[INV-NOTICE-01]` **Retention precedes interruption.** Every disclosure is
  appended to the notice log before any bubble is painted; there is no mutation
  that shows a bubble without recording it. Enforced by: the single
  `NoticeDisclosure` mutation and the single `record_notice` funnel.
- `[INV-NOTICE-02]` **The viewer is reachable from every scene.** The notice log
  is client-global and is never discarded by a session or scene switch. Enforced
  by: `NoticeViewer` living on `App`, and the log addressed as `SurfaceId::NOTICES`
  in the registry rather than through `DialogKind`'s session-scoped unwinding.
- `[INV-NOTICE-03]` **The bubble never hides its own escape hatch.** A fault
  bubble's hint row is reserved before the message wraps. Enforced by:
  `message_row_budget` in `components::toast` and its regression test.
- `[INV-NOTICE-04]` **A keystroke cannot silently defeat a fault's dwell.** A
  fault bubble is dismissed by its timer or by an `Esc` that has no other
  meaning; only an acknowledgement yields to arbitrary input. Enforced by:
  `dismisses_notice_bubble` plus the viewer/overlay/sheet preconditions in
  `process_one_event`, and their tests.
- `[INV-NOTICE-05]` **The cursor stays on screen.** The viewer computes its own
  viewport offset from the selected *row*, because the body is not one row per
  notice. Enforced by: the pre-paint scroll clamp in `overlays::notices` and its
  expanded-row regression test.

## Consequences

- **Positive**: a missed fault is recoverable by construction, not by luck. The
  log is scrollable, time-stamped, topic-tagged, copyable as one diagnostic
  block, and reachable by chord, slash command, palette verb and badge click.
  The bubble got shorter (it no longer has to carry the whole message) because
  the record carries it. Diagnostics no longer degrade onto the time axis, which
  closes the last open item of ADR-0116.
- **Negative & trade-offs**: one more global overlay to keep out of the way of
  the armed-confirmation bubbles (it paints below them, above dialogs), and the
  router carries an explicit `notice_viewer_open` flag. The log is capped at 128
  lines — a very noisy client evicts older faults, and the rolling client log
  remains the full history (every copied diagnostic names its path).

## Rejected Alternatives & Negative Knowledge

### Alternative A: Make the bubble a stack (Rejected)

- **Why considered**: The direct reading of the complaint — let the bubbles pile
  up in the corner instead of overwriting each other.
- **Why rejected**: It moves the defect rather than fixing it. A stack of
  bubbles in the top-right corner occludes the transcript, has no scrollback, no
  cursor, no timestamps, and still dies on the first keypress. It is a worse
  viewport than a modal in every dimension except latency.

### Alternative B: Keep the private `ReceiptLog`, add a viewer over it (Rejected)

- **Why considered**: Smallest possible change; the receipt log already existed.
- **Why rejected**: It would be a second store with a second cap and a second
  residency contract, violating `[INV-LAYER-01]` and re-introducing the question
  "where does this content live?" with two answers. `SurfaceLog` already existed
  and needed one field (`unread`) to serve as the notice log.

### Alternative C: Persist the notice log to the state store (Rejected)

- **Why considered**: "What did the client tell me yesterday?" is a real
  question.
- **Why rejected**: It breaks `[INV-LAYER-03]` (log tier containment) and, worse,
  it makes a presentation artifact durable state that has to be migrated,
  compacted and garbage-collected. The rolling client log already answers the
  question with full fidelity, and it is a file the user can grep.

### Alternative D: Route the viewer through `DialogKind` (Rejected)

- **Why considered**: Every other full-panel surface is a dialog; the registry,
  the keymap scopes and `dialog_keys` all already work for dialogs.
- **Why rejected**: `DialogKind::scope()` would make the viewer session- or
  scene-scoped, and `Dialogs::switch_session` / `SurfaceRouter::switch_scene`
  would reset or archive it. Losing the notice log on a tab switch is losing it
  precisely when the user goes looking.

## Links

- Amends: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (content residency and the `L2` tier)
- Related: [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)
  (why a notice can never be dialogue), [ADR-0069](0069-execution-log-spilling-epistemic-context-paging-and-anti-recursion-topology.md)
  (the rolling client log, which holds the full history this log is a window onto),
  [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)
  (zero-legacy clean break)
