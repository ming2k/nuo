---
id: ADR-0116
title: "Content Residency and Surface Layer Topology: One Home Per Content and Layer Ownership by Surface"
status: accepted
date: 2026-12-09
scope: presentation/nuo-view, presentation/nuo-ui-primitives, protocol/nuo-protocol, protocol/nuo-tool, host/nuo, tui/surfaces, tui/overlays
superseded_by: null
negative_knowledge: true
---

# 0116. Content Residency and Surface Layer Topology: One Home Per Content and Layer Ownership by Surface

- Status: Accepted
- Date: 2026-12-09
- Deciders: Nuo Architecture Working Group
- Consulted: Interface Architecture, Presentation Substrate, Server Core
- Amends: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md) — extends domain scoping from the *x/y* plane (which dialog owns which state) to the *z* plane (which layer owns which content), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) — the durable-versus-ephemeral boundary for command invocations is preserved, but the *reply* is re-homed to a layer instead of defaulting to the transcript, [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md) — the receipt log is a new Tier A presenter over a Tier B container

---

## Context and Problem Statement

The presentation stack models surfaces (`Scene` / `Dialog` / `Sheet`) along a spatial axis and
`NoticeSurface::{Inline, Toast, Banner}` along a delivery-hint axis. The two axes have never been
reconciled, and the delivery axis is carried **on the content** rather than on the surface. Three
consequences compound into a single systemic defect: the thread transcript became the default
destination for content that is not dialogue.

1. **The receipt/diagnostic tier does not exist.** The toast contract
   (`nuo-ui-primitives/src/components/toast.rs`) reserves toasts for exactly three shapes — immediate
   action acknowledgements, two-step confirmation gates, and `CommandAck` — and then states in the same
   breath that diagnostic errors, missing workspace assets, and initialization failures **MUST** route to
   `NoticeSurface::Inline`, i.e. into the transcript timeline. The prohibition is correct; the forced
   destination is the defect. With only two content layers, every piece of content that is neither
   dialogue nor a toast has nowhere to live but the time axis.

2. **The layer is a producer-chosen field, and the default is the transcript.**
   `AgentNotice::new` initialises `surface: NoticeSurface::Inline`. A producer that omits
   `.with_surface(..)` therefore *selects the transcript by accident*. The frontend applies the field
   verbatim (`nuo/src/shell/mod.rs`, the `RoundEvent::Notice` arm: `Toast` → toast surface, everything
   else → `TranscriptEdit::Append`). New notice kinds acquire transcript residency without anyone
   deciding that they should.

3. **`Inline` conflates two different questions.** "Which layer shows this?" and "what does this content
   *mean*?" are orthogonal, but `NoticeSurface` collapses them into one enum that lives on the payload.
   The same content type can legitimately need different layers in different contexts (a trust change is a
   retained condition in-session and a full-screen gate at attach time), which is exactly why a
   producer-side hint cannot be authoritative.

The result is the observed pathology: dialogs, configuration operations, connection failures, and
retrieval receipts all surface inside the thread's scrollback, where they cannot be displayed
effectively — the transcript is linear, streaming, position-stable only in time, non-addressable, and
participates in compaction, projection, search, and export. A dialog conversation is none of those
things. It has a cursor, an internal viewport, a reopenable identity, and a lifetime measured in the
open modal.

## Decision Drivers

- **Content must live where it can be displayed** — a layer that scrolls away and cannot be re-opened is
  an unacceptable home for addressable content.
- **Admission must be a decision, not a default** — the *absence* of a routing decision must never
  produce transcript residency.
- **The layer belongs to the surface, not the message** — the same content in a different context has a
  different correct destination; the producer cannot know the context.
- **`[INV-SURFACE-01]` restated on the z axis** — a dialog owning its cursor and scroll implies a dialog
  owning its receipts. Sharing the transcript as a receipt sink re-introduces the cross-dialog
  contamination ADR-0035 abolished on the x/y plane.
- **Zero legacy burden** ([ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md)) — no
  compatibility shim, no `#[deprecated]` alias, no dual-path tolerance.

## Architectural Decisions

### 1. A seven-tier z-model with disjoint residency contracts

```text
L0  thread transcript   time axis.  Dialogue facts + turn anchors.  The only source of `model_window`.
L1  chrome              status/activity bar, round + token counters.  Never content.
L2  surface log         per-surface receipt log: addressable, scrollable, capped, ephemeral.
L3  dialog              spatial overlay; owns its own cursor/scroll/input ([INV-SURFACE-01]).
L4  toast               transient acknowledgement.  No history, no scrollback.
L5  banner              retained condition until the condition clears or is superseded.
L6  sheet               request/response interaction (permission, question, stdin).
```

`L2` is the tier that did not exist. It is the correct destination for every content class that the
toast contract forbids and that dialogue cannot host: diagnostics, connection/asset failures, retrieval
receipts, configuration confirmations, and every dialog's own question/answer pairs.

### 2. Single-home residency, cross-layer references are pointers

`[INV-LAYER-01]` — **One home per content.** A content record is admitted to exactly one layer. Any
other layer that needs to acknowledge it stores a *pointer* (an id plus a one-line summary), never a
copy. This is what makes "is this polluting?" a question with a single answer, instead of a per-layer
judgement call. The thread transcript uses anchors (`TranscriptRow::Anchor`) for structural events it must
position in time while the body lives in the ledger or a surface log.

### 3. The layer is derived from the surface contract, in one policy function

`NoticeSurface` is deleted. `AgentNotice` carries meaning only — `kind`, `severity`, `source`, `title`,
`body` — and the layer is computed by a single exhaustive policy function in the presentation substrate:

```rust
// nuo-view::model::layer
pub enum ContentLayer {
    Toast,
    Banner,
    SurfaceLog(SurfaceId),
    /// Nothing to display: a dedicated disclosure already owns this fact.
    Absorbed,
}

pub fn layer_for(notice: &AgentNotice, ambient: SurfaceId) -> ContentLayer;
```

`[INV-LAYER-04]` — **A notice cannot reach the thread transcript.** `ContentLayer` has *no transcript
variant*. The prohibition is therefore a property of the return type rather than of a test or a comment:
`layer_for` is physically unable to select L0. Anything that legitimately belongs in L0 (turn outcomes,
compaction checkpoints) is not a notice and is not routed through this function.

Exhaustiveness is the enforcement mechanism: the match has no wildcard, so a new `NoticeKind` fails the
build until its layer is chosen deliberately.

```
NoticeKind          source         layer
──────────────────────────────────────────────────────────────────────
ProviderRetry       any            Absorbed        (RetryScheduled owns the disclosure)
CommandAck          any            Toast           (ephemeral reply; invocation stays durable)
TrustChanged        any            Banner          (retained condition; gate owns attach-time)
TrustFailure        any            SurfaceLog      (a lost decision is a receipt; a fault bubbles)
NudgeInjected       any            SurfaceLog      (the injected fact is already an L0 entry)
ReviewAlert         any            SurfaceLog
ImageInputWithheld  any            SurfaceLog
(any future kind)   any            SurfaceLog(ambient)   ← default is the log tier, never L0
```

`TrustChanged` and `TrustFailure` are separate rows because they are opposite
facts (ADR-0127). A *condition* waits for a human decision and must stay on
screen; a *failure* is a past event with a cause worth re-reading, and its
`Error` severity is what makes it raise a bubble naming the log. While they
shared one kind, the mounted trust gate had to intercept every `TrustChanged`
into its error slot — which, once the gate mounts on changed content, would
render the workspace's own state as an error.

### 4. `SurfaceLog`: the addressable receipt log tier

```rust
// nuo-view::model::log
pub struct SurfaceId(pub &'static str);          // "thread", "dashboard", "dialog:models", …
pub struct SurfaceLog { /* lines: Vec<SurfaceLine>, cap */ }
pub struct SurfaceLine { at_ms: u64, head: SurfaceHead, status: LineStatus, body: Option<String> }
pub enum SurfaceHead { Query, Reply, Note }      // asked / answered / observed
pub enum LineStatus { Ok, Failed, Info }
```

The log is a capped ring owned per `SurfaceId` and held in one registry on the app shell. It is
**never** persisted, never enters `model_window`, never participates in compaction, search, or export,
and is discarded with its owner's lifetime. A dialog reaches its log through the surface contract
(`DialogView::log()`), so a dialog that needs to remember its own answers does so without borrowing the
thread.

The dashboard's cockpit console is the *proof* that the tier works: it was already an addressable,
ephemeral, per-surface receipt log. It keeps its own typed row vocabulary (`ConsoleLine`) in this
decision and is not migrated onto `SurfaceLine`: it is the only surface with a log viewer today, so
unifying the two would add a translation layer with no second consumer. The registry is installed and
the notice router writes through it, which is what makes a second viewer additive — when one exists, it
subscribes to `SurfaceLogs` instead of inventing storage.

### 4a. Capability-driven degradation, never a fallback to L0

A surface declares the layers it can host:

```rust
pub struct SurfaceCapabilities { pub toast: bool, pub banner: bool, pub log: Option<SurfaceId> }
pub fn layer_for(notice: &AgentNotice, caps: SurfaceCapabilities, ambient: SurfaceId) -> ContentLayer;
```

When the semantically preferred layer is unhostable, content degrades **towards the most transient layer
it can host** (`log → banner → toast → absorbed`). It never degrades onto the time axis: a client with no
log viewer shows a bubble rather than writing into the conversation. The thread scene therefore runs
`SurfaceCapabilities::MINIMAL` today, which is exactly why diagnostics that used to be appended inline
now surface as toasts.

### 5. The toast contract is inverted and restated

The toast component doc previously *pointed* misplaced content at the transcript. It now points at
`L2`. Toasts remain strictly reserved for the three shapes they always were; the difference is that the
forbidden content now has a correct home instead of a wrong one.

## Normative Invariants Established

- `[INV-LAYER-01]` **Single-home residency** — one content record, one layer; cross-layer references are
  pointers. Enforced by: the transcript writer accepting only fact/anchor types
  ([ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md)), and the operation rows being
  derived from the ledger rather than copied.
- `[INV-LAYER-02]` **Layer ownership by surface** — no content payload carries a presentation-layer field.
  Enforced by: `NoticeSurface` and `AgentNotice::surface` not existing (compile-time).
- `[INV-LAYER-03]` **Log tier containment** — a surface log is capped, ephemeral, and unreachable from
  persistence and the model window. Enforced by: `SurfaceLog` living in `nuo-view` (zero-IO substrate) and no
  `FactSink`/`SessionStore` handle crossing into the log registry.
- `[INV-LAYER-04]` **Notices cannot reach L0** — `ContentLayer` has no transcript variant, and
  `layer_for` is exhaustive over `NoticeKind`. Enforced by: the type definition plus a match with no
  wildcard arm.

## Consequences

- **Positive**: the "which layer?" question is answered once, by one function, from the surface's
  identity. New notice kinds cannot silently acquire transcript residency. Dialog receipts, diagnostics,
  and configuration confirmations become displayable in a layer that has a viewport, a cursor model, and
  a reopen path. The toast contract stops being an instruction to misuse the transcript.
- **Positive, structural**: `nuo-view` gains the receipt-log tier as a Tier A presenter over a Tier B
  container, matching [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md);
  no new I/O, no new dependency edge, no cycle.
- **Negative & trade-offs**: a diagnostic that used to be permanently greppable in the export is now
  ephemeral. Mitigation: diagnostics that are *durable session facts* (trust decisions, withheld images,
  compaction checkpoints) remain session-store records and are re-projected on resume; only their
  *presentation* moves. The rule is "if it must survive, it is a fact first and a presentation second".
- **Negative & trade-offs**: `ContentLayer::Absorbed` makes a notice invisible by policy. Mitigation: the
  absorbing disclosure is a first-class, self-refreshing surface (the provider-retry card), and the policy
  table above is the auditable list of what is absorbed.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): add `NoticeSurface::Log` and keep the field

- **Why considered**: One more enum variant would have been a two-line change, and producers already know
  how to set a surface.
- **Why rejected**: It preserves the defect. The producer still chooses the layer, the default is still
  transcript residency (or, after flipping the default, still an accident), and `Inline` survives as a
  legitimate-looking destination. A field on the content cannot express "the same content needs a
  different layer in a different context".

### Alternative B (Rejected): keep rendering dialog receipts in the transcript as collapsed rows

- **Why considered**: Users want to see what a dialog did; a collapsed, expandable row would be the
  cheapest way to show it in place.
- **Why rejected**: It reproduces the live/resume split-brain. The transcript is the authority consulted
  on attach, so a receipt rendered as a row is replayed to every future client as though it were dialogue,
  and it enters every consumer that walks the transcript (export, search, summarisation, review). It also
  destroys the round/turn sequence, which is a semantic property of conversation, not of operations.

### Alternative C (Rejected): keep the dashboard's private `ConsoleLine` and add a second log type

- **Why considered**: The cockpit console already worked; a shared type is a refactor with no user-visible
  change.
- **Why rejected**: Two log types means two vocabularies for the same tier, and the second one grows
  variants the first cannot express. The dashboard is the *proof* that the tier is viable, so it is the
  right thing to generalise rather than duplicate.

## Links

- Related ADRs: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md) (encapsulated dialog entities), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) (durable invocation, ephemeral reply), [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md) (dual-tier presentation), [ADR-0059](0059-canonical-thread-persistence-schema-and-zero-legacy-clean-break.md) (zero legacy burden)
- Successors: [ADR-0117](0117-transcript-admission-purity-dialogue-only-thread-documents.md) (what L0 may contain), [ADR-0118](0118-dialog-conversation-sovereignty-ephemeral-scratch-and-explicit-promotion.md) (where dialog conversations live)
- Code: `crates/nuo-view/src/model/log.rs`, `crates/nuo-view/src/model/layer.rs`, `crates/nuo-ui-primitives/src/components/toast.rs`
