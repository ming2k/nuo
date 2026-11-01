---
id: ADR-0138
title: "Notice Catalog: A Typed Registry of Every Client Disclosure, Abolishing Scattered String-Literal Producers"
status: accepted
date: 2026-12-17
scope: host/nuo (tui/event-loop, tui/app, tui/overlays), presentation/nuo-view
superseded_by: null
negative_knowledge: true
---

# 0138. Notice Catalog: A Typed Registry of Every Client Disclosure

- Status: Accepted
- Date: 2026-12-17
- Deciders: Nuo Architecture Working Group
- Amends: [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md) (the notice funnel gains a typed catalog on the producer side), [ADR-0134](0134-two-track-notice-architecture-sovereign-notice-center-dialog-and-footer-dock-eradication.md) (card content now originates in the catalog, never as ad-hoc literals)

---

## Context and Problem Statement

ADR-0121 and ADR-0134 centralized the *transport* of client disclosures: every
notice passes through `App::record_notice`, lands unconditionally in the
`SurfaceLog(NOTICES)`, and only a fault additionally raises a bubble. That
centralization was real and it works.

What it did **not** centralize is the *content* of those disclosures. The
producer side remains scattered string literals:

- `crates/nuo/src/shell/app/link.rs` formats `"Lost the connection to the session server: …"` inline.
- `crates/nuo/src/shell/event_loop/apply.rs` formats `"The session could not be prepared: {message}"` inline.
- `crates/nuo/src/shell/event_loop/actions/commands.rs` formats `"Session is still preparing; your message will be sent when it is ready."` inline.
- `crates/nuo/src/shell/event_loop/sync.rs` and `crates/nuo/src/shell/mod.rs` thread free-form topic tags (`"attach"`, `"client"`, `"link"`) that are **not** part of the closed wire `NoticeKind` vocabulary, so the notice-center header falls back to the generic `notification` topic for every local notice.

Three concrete defects follow:

1. **Two vocabularies.** The wire has a closed `NoticeKind` enum with a
   one-to-one topic map (`notice_topic_label`); local producers invent free-form
   tags. The dialog's topic pill and the transcript entry head therefore render
   `notification` — the generic fallback — for the client's own most important
   faults (link loss, session-prep failure).
2. **Words drift per call site.** The same class of failure can be phrased
   differently in each file, and there is no single place that answers "what
   does the client say when the link dies?".
3. **Timestamps are faked.** Several producers pass `at_ms: 0` to the funnel,
   so the notice log can carry rows whose wall-clock stamp is nonsense.

This is the classic smell the user identified: a *transport* registry without a
*message* registry. The fix is a **notice catalog**: one typed, exhaustive
registry of every disclosure the client can produce, with fixed severity, topic,
payload verbs, and default wording.

## Decision Drivers

- **One registry** ([ADR-0116](0116-content-residency-and-surface-layer-topology.md) `[INV-LAYER-01]`): the catalog is the single source of *content*,
  exactly as `SurfaceLog(NOTICES)` is the single source of *residency*.
- **Closed topic vocabulary**: every local producer contributes to a topic
  label that is part of the same predictable vocabulary the wire already uses,
  so the generic `notification` fallback disappears from the client's own rows.
- **Zero-legacy clean break** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)): the raw `record_notice(severity, topic,
  text, at_ms)` entry point is *removed*, not deprecated. A producer cannot
  express a disclosure that bypasses the catalog.
- **Time is real**: the catalog stamps `now_epoch_ms()` itself; no producer
  passes a fake `0`.

## Architectural Decisions

### 1. `NoticeCatalog` — one typed enum, one renderer

A new module `crates/nuo/src/shell/notices/catalog.rs` (host-side, presentation
never sees it) defines:

```rust
pub enum ClientNotice {
    LinkLost { reason: String },
    LinkSendFailed { reason: String },
    SessionPrepFailed { detail: String },
    QueuedWhilePreparing,
    AttachRetired { reason: String },
    ClipboardFailed,           // routed by the toast path, not the log
    // … every current producer, exhaustively
}
```

Each variant declares, via a single `impl ClientNotice` rendering pass:

```rust
pub struct RenderedNotice {
    pub severity: NoticeSeverity,
    pub topic: &'static str,     // from the notice_topic_label vocabulary
    pub title: String,           // one line, human-readable
    pub detail: Option<String>,  // optional structured body
}
```

`App::record_catalog_notice(kind: ClientNotice)` is the **only** producer
entry point (replacing `record_notice` / `record_local_fault` /
`record_notice_with_detail`). It renders the kind through the catalog, derives
the timestamp itself, and forwards the rendered payload into the existing
funnel — which is unchanged: log first, bubble for faults.

### 2. Topic vocabulary is closed and shared

`notice_topic_label` in `nuo-view` already owns the wire's kind→topic map. The
catalog reuses the **same labels** for the client's own rows (`link`, `attach`,
`client`, … are pinned as `&'static str` constants in the catalog), so a topic
pill never falls back to `notification` for a catalog-produced notice.

### 3. Time is the catalog's, never the producer's

`App::record_catalog_notice` calls `event_loop::now_epoch_ms()` once and passes
it down. The `at_ms` parameters on the retired entry points are gone; the
`at_ms: 0` call sites cease to exist by construction.

### 4. The transcript bridge stays where it is

`TranscriptMessage::notice_from_core` (the wire→transcript projection) is
**not** moved: it keeps its `NoticeParts` split, and the catalog's rendered
`title`/`detail` map onto that same split when a routed notice also lands in the
transcript. The catalog and the wire bridge produce the same structured shape,
so the presentation tier has exactly one renderer for both.

## Subsystem Boundaries & Structural Changes

- **New**: `crates/nuo/src/shell/notices/catalog.rs` — the enum, the renderer,
  exhaustive tests locking wording/topic/severity per kind.
- **Modified**: `App::record_notice*` removed; `App::record_catalog_notice`
  added; call sites in `link.rs`, `apply.rs`, `commands.rs`, `sync.rs`,
  `mod.rs` migrate to catalog kinds.
- **Unchanged**: `SurfaceLog(NOTICES)`, `raise_notice_bubble`, the dialog, the
  toast, `LineStatus::from_notice_severity`, `NoticeSeverity::is_fault`.

## Normative Invariants Established

- `[INV-NOTICE-08]` **Catalog-Only Producers**: Every client disclosure MUST be
  produced through the catalog — `App::record_client_notice(ClientNotice::…)`
  for structural kinds, `App::record_local_fault(ClientTopic, …)` for the
  bounded dynamic path. The free-form
  `record_notice(severity: NoticeSeverity, topic: impl Into<String>, text,
  at_ms: u64)` entry point MUST NOT exist. The funnel itself
  (`App::record_notice(RenderedNotice)`) accepts only the typed payload, whose
  fields are private and whose constructors are the closed enum and the closed
  wire projection.
  Enforcement: the removed entry point is a compile error; `RenderedNotice` has
  no public fields (a struct-literal is a compile error outside the defining
  module); a test enumerates every `ClientNotice` variant and asserts a
  non-empty, single-line title and a topic that is never the generic fallback.

## Scope of "every client disclosure"

The catalog governs disclosures the **client** surfaces into the notice log. It
does **not** govern the transcript's turn-outcome anchors (a round failure, a
background-job tick), which are harness turn structure appended on the thread's
time axis (ADR-0117) and never pass through the notice funnel.

## Consequences

- **Positive**:
  - One place answers "what does the client say when the link dies?".
  - Local *notice-log* topics enter the closed vocabulary; the `notification`
    fallback disappears from the client's own retained disclosures. (Transcript
    turn-outcome anchors — a round failure, a background-job tick — are not
    notice-log disclosures and still render the generic topic; see the scope
    note below.)
  - Real timestamps by construction; fake `0` stamps are unrepresentable.
  - The catalog is the natural home for wording lint (one-line titles, no
    boilerplate), matching the notice-center's truncation budget.

### Scope boundary: turn-outcome anchors are not disclosures

`RoundEvent`/`AgentResponse` failures and background-job events append a
`MessageKind::Notice` to the **transcript** as a turn-outcome anchor (ADR-0117:
the thread's time axis). Those are not client disclosures, never pass through
the notice funnel, and legitimately carry no `NoticeParts` — so they render the
generic `notification` topic by design. Closing *their* topic vocabulary is a
separate, future projection (a new `SystemNoticeTopic` variant per anchor); the
catalog does not and should not govern them, because it owns the client's own
disclosures, not the harness's turn structure.
- **Negative & Trade-offs**:
  - Every new disclosure requires a catalog variant (a small, deliberate cost:
    wording is a contract, and the invariant test makes forgetting it loud).
  - Server-side notices remain on the wire `NoticeKind`; the catalog governs
    the client's own rows only. The two already speak the same topic vocabulary,
    so the seam is invisible to the presentation tier.

## Rejected Alternatives & Negative Knowledge

### Alternative A: A free-form `record_notice` with a validation lint (Rejected)

- **Why considered**: Least churn; keep the existing signature, add a test that
  walks call sites.
- **Why rejected**: A lint cannot enumerate what *cannot* be expressed. The
  free-form signature is exactly what allows a future producer to invent a new
  topic string or fake a timestamp; removing the signature is the only
  mechanism that makes those futures unrepresentable ([ADR-0059]).

### Alternative B: Move the catalog into `nuo-view` (Rejected)

- **Why considered**: The presentation layer is where the dialog/transcript
  live, and the catalog is "content".
- **Why rejected**: `nuo-view` must stay a pure presentation substrate with no
  knowledge of client event sources or timestamps. The catalog is host policy
  (what the client says, when) and belongs in the host binary, exactly where
  the event loop already lives.

### Alternative C: Keep `at_ms` as a parameter "for tests" (Rejected)

- **Why considered**: Existing tests pass synthetic timestamps.
- **Why rejected**: Tests can equally freeze time by passing a fixed epoch to
  the catalog's own clock hook; a public `at_ms` parameter is an open door for
  the fake-`0` defect to return. One clock, owned by the catalog.

## Links

- Amends: [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md),
  [ADR-0134](0134-two-track-notice-architecture-sovereign-notice-center-dialog-and-footer-dock-eradication.md)
- Related: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) (one registry),
  [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (zero legacy burden)