---
id: ADR-0220
title: "The Context Plan Is a Wire Surface and Its Dialog Is a Projection of It"
status: accepted
date: 2026-12-20
scope: protocol/context, engine/cognition, server/handlers, cli
superseded_by: null
supersedes:
  - "the client-local `/context` text receipt"
  - "`inspect::render_json`'s body-less `PlannedBlockRecord` projection"
negative_knowledge: true
---

# 0220. The Context Plan Is a Wire Surface and Its Dialog Is a Projection of It

- **Status**: Accepted
- **Date**: 2026-12-20
- **Derives from**: [ADR-0217](0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md),
  [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)
- **Amended by**: [ADR-0231](0231-one-instruction-region-and-no-ghost-extension-subsystem.md) — `ContextPlanView::transient_tokens` and the "Transient tail" row are deleted; the plan carries one instruction region.

## Context and Problem Statement

ADR-0217 built the measurement and named three entry points for it. One of them
does not exist and the other two stop short of the question the plan was built to
answer.

1. **`nuo context` was never built.** `nuo`'s top level has no `context` verb and
   a test actively asserts `parse(&["context"]).is_err()`. The design claimed an
   operator/CI entry point that a CI job cannot call.
2. **`/context` reaches the terminal app only as prose in a notice.** The command
   is declared `Reading`, so its reply is filed as a `CommandReceipt` disclosure
   (`translators/response/command.rs`). An operator who wants to compare two
   blocks reads a wall of text inside the notice centre.
3. **The terminal app cannot see a plan at all.** The TUI is a pure attach
   client: `App` never holds an `Agent`, and `DialogRenderCtx` borrows only
   `&App`. Every dialog that shows server state does so through an
   `AgentRequest`/`AgentResponse` round-trip. No such pair exists for the plan,
   so "add a dialog" was never a presentation task — it is a wire task.
4. **The block *content* is not addressable anywhere.** `PlannedBlock` carries
   cost and provenance; the text lives in `ContextPlan::fixed`/`transient`. No
   surface joins the two, and `render_json` deliberately excludes bodies (its own
   test asserts the absence). "Which bytes does `project.rules` actually
   contribute, and where did it get cut?" is answerable only by reading source.
5. **Tool-schema cost is invisible.** `LayeredRequestWeights` measures
   `instructions_tokens` and `tool_schema_tokens` separately, then
   `estimate_model_request` collapses them into one `overhead_tokens`. A nine-
   thousand-token tool surface reads as "overhead" beside a two-thousand-token
   system prompt, so the only number an operator can act on is the sum.

The consequence is that the most common prompt question — *"what is in my
context, how big is each part, and what does it say"* — is still answered by
reading source, which is exactly the failure ADR-0217 set out to remove.

## Decision Drivers

- **One measurement, many readers.** The plan the model received and the plan an
  operator reads must be the same value, byte for byte
  (`[INV-CTXPLAN-01]`). A dialog that re-measured would be a second truth.
- **The client is not the engine.** A surface reachable by an attached frontend
  must be reachable over the wire, with no `Agent` in the process.
- **Bodies are live, never durable.** ADR-0217 discarded prompt persistence for
  cost reasons. Showing content must not reintroduce it.
- **No second vocabulary.** A breakdown type beside `RequestTokenEstimate` would
  be two spellings of one accounting.
- **Exhaustiveness over convention.** Adding a surface must fail to compile
  everywhere it needs a decision, not silently fall through.

## Architectural Decisions

### 1. The plan's operator view is a protocol type

`protocol::context::ContextPlanView` is the one shape a client, the CLI, and the
golden snapshot all read:

```rust
pub struct ContextPlanView {
    pub blocks: Vec<PlannedBlockView>,
    /// Sum of the per-block costs; deliberately *not* the whole-region cost.
    pub fixed_tokens: usize,
    pub transient_tokens: usize,
    pub notices: Vec<ContextNotice>,
    pub drift: PrefixDrift,
    pub window_tokens: usize,
    pub instructions_ceiling_tokens: usize,
    /// The request's token shape, measured once by the engine.
    pub estimate: Option<RequestTokenEstimate>,
}

pub struct PlannedBlockView {
    pub id: String,
    pub topic: Topic,
    pub rank: Rank,
    pub position: Position,
    pub stability: Stability,
    pub tokens: usize,
    pub elision: Option<Elision>,
    pub active: bool,
    pub inactive_reason: Option<String>,
    /// The exact rendered text this block contributes, envelope and title
    /// included. `None` for an inactive block.
    pub body: Option<String>,
}
```

`body` is `render_blocks(&[block])` — the same call `registry::measure` uses to
produce `tokens`. So a client can re-tokenize `body` and must get `tokens` back;
that equality is the invariant, not a comment.

`inactive_reason` is `String` on the wire and `&'static str` in the engine row.
This is not duplication for its own sake: every reason is a compile-time
literal in `registry.rs`, the in-process row must not allocate one per request,
and a wire type cannot carry a `'static` lifetime. `Resume::Tool` — the one
`&'static str` that had no such defence, because the tool name is a budget
argument — becomes `String`, so `Resume`, `Elision`, and `Overflow` are directly
serializable and need no parallel `*View` mirror.

### 2. `RequestTokenEstimate` names its layers

`overhead_tokens` conflated three costs. It gains `instructions_tokens` and
`tool_schema_tokens`, and a `measured` constructor derives `overhead` from the
measured total rather than summing the parts:

```rust
overhead = total - history          // not: instructions + tools + temporary
```

The layers explain what the overhead is *made of*; they are not its definition.
A system-role row that ever reached `messages` is history to nobody and belongs
to no layer, so summing the parts would understate the request by exactly that
row. Deriving from the measured total absorbs it and keeps
`history + overhead == total` exact by construction.

`ContextTokenSnapshot` gains both layers as `Option<usize>` with
`serde(default)`. The `/stats` overview now shows what `overhead` is made of
instead of one opaque row, and reads the same numbers the context dialog does.

### 3. The plan is queryable over the session duplex

```rust
AgentRequest::QueryContextPlan
AgentResponse::ContextPlan { thread_id: String, plan: Option<ContextPlanView> }
```

The plan belongs to one `Agent`, so the request carries no thread id (the driver
answers for its own session, exactly like `QueryThreadTree`); the **reply**
carries the id so a frontend can reject a reply that raced a thread switch, the
same contract `TokenUsageReport` already uses.

`plan: None` is a real answer, not an error: no request has been assembled yet.
It is why the dialog can say "send a message first" instead of showing an empty
table.

`QueryContextPlan` is a **reading over live process state**, so it does not go
through the slash-command funnel, has no ledger row, and never enters the
transcript.

### 4. The engine keeps one measurement, not two

`Agent`'s `last_context_plan: Mutex<Option<ContextPlan>>` becomes
`last_measurement: Mutex<Option<ContextMeasurement>>`:

```rust
pub struct ContextMeasurement {
    pub plan: ContextPlan,
    /// `None` until the attempt that measures the assembled request runs.
    pub estimate: Option<RequestTokenEstimate>,
}
```

`model_request` writes `{ plan, estimate: None }` — a fresh plan never carries
the previous request's token shape. `open_streaming_attempt` fills `estimate`
from the request it is about to send, which is the request that plan built. The
ordering contract is one line long and is pinned by a test.

### 5. One machine form, not two

`inspect::render_json` now serializes `ContextPlanView` instead of a body-less
`PlannedBlockRecord` projection. Two JSON shapes for one plan is precisely the
divergence ADR-0217 exists to prevent, and the body-less form answered strictly
fewer questions.

The golden snapshot therefore changes character: `default_prefix_json` becomes
the reviewed artifact for both bytes and accounting, and the separate rendered-
prefix snapshot is **removed**. The stronger invariant survives as an assertion —
the bodies joined from `blocks` must equal `render_blocks(plan.fixed)` — which is
what the snapshot was really pinning.

### 6. `DialogKind::ContextPlan` is a session-scoped projection

A new dialog, `DialogScope::Session` (a plan is per-thread and must not leak
across a tab switch), rendering one scrolling selectable body:

| Section | Content |
| :--- | :--- |
| Request | window, instruction ceiling, per-region tokens, and the residual between the per-block sum and the whole-region cost |
| Sources | one row per declared block, in **emission order**, band headers inline |
| Bodies | the selected block's rendered text, expanded in place |
| Notices | every elision/index/drop/ceiling decision |
| Drift | `PrefixDrift`, with `Head` in the error tone |

Emission order is read from `plan.blocks` verbatim rather than re-sorted. A view
that re-ordered its rows would be a second opinion about the prompt, and the
whole point of the surface is that it is not.

The **residual** (`fixed_tokens + transient_tokens - estimate.instructions_tokens`)
is displayed rather than hidden. Tokenizers are not additive, so a per-block sum
cannot equal a whole-region count; showing the difference is how a reader learns
that the per-block column is an attribution, not a partition.

### 7. `/context` opens the dialog; `--json` stays server-side

Bare `/context` is intercepted locally as `InputAction::OpenContextPlan`
(`/stats`, `/trace`, `/tools` … already work this way). `/context --json` still
travels to the server, because a non-TUI client has no dialog and the wire form
is the answer it asked for. The two forms are the same value, so a client that
sees one and an operator who sees the other cannot disagree.

`nuo context` remains **removed**. A CLI verb would need its own instance
discovery, a session to attach to, and a second renderer for a plan whose live
form only exists inside a running `Agent`. The question it was meant to answer —
"what does this thread's prompt cost, block by block" — is answered by the
dialog for a live thread and by the golden snapshot for CI. Reintroducing the
verb is a decision to make when a concrete workflow needs it, not a gap to fill
for symmetry.

## Normative Invariants

- **`[INV-CTXPLAN-04]` Body Cost Agreement** — For every active block,
  `tool::count_tokens(view.body) == view.tokens`. A body that does not cost its
  reported tokens is a lying inspector.
- **`[INV-CTXPLAN-05]` One Plan, One Order** — `view.blocks` is
  `plan.blocks` in declaration order, with no client-side re-sorting by cost.
  The dialog's order IS the request's order.
- **`[INV-CTXPLAN-06]` Live Only, Never Durable** — `PlannedBlockView.body` never
  reaches `request_projections`, a transcript row, or any file. It exists for the
  duration of one reply.
- **`[INV-CTXPLAN-07]` No Second Measurement** — the dialog and `--json` render
  the engine's stored `ContextMeasurement`; neither re-runs `Policy::plan` nor
  `estimate_model_request`.

## Consequences & Trade-offs

- **Positive**: "what is in my context, how big is each part, and what does it
  say" is answerable in one keystroke without reading source; tool-schema cost
  stops hiding inside `overhead`; `/stats` and the dialog agree because they read
  one estimate; the JSON golden is now a complete forensic artifact; an attached
  frontend gains a real context surface for the first time.
- **Negative & Incurred Costs**: one reply carries the whole rendered prompt, so
  opening the dialog on a 24k-token instruction region moves ~100 KiB over the
  local socket. Accepted: the transport is a local UDS in the supported
  postures, the reply is request-scoped and dropped on the next switch, and the
  alternative — a second, partial measurement — is the failure this ADR is
  written against.
- **Negative & Incurred Costs**: `PROTOCOL_VERSION` moves to 32 and the wire
  census is re-blessed. `Resume::Tool` changes from `&'static str` to `String`,
  which touches every budget constructor that names a tool.
- **Neutral**: no new crate. `ContextPlanView` lives beside `ContextPlan`, and
  the join is one function in `engine::cognition::context::inspect`.

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Let the dialog render the plain-text table the server already builds

- **Why considered**: Zero new wire surface; the `/context` table already exists.
- **Why rejected**: A terminal app cannot select, scroll, fold, or copy a string
  it receives as a notice body. The table's column widths were chosen for prose
  output, and the dialog's job is exactly the interaction the string form cannot
  support.

### Alternative B — Ship the plan only through the on-disk archive

- **Why considered**: `RequestProjection.blocks` is already persisted and needs no
  new request.
- **Why rejected**: The archive is capped at 64 rows per thread and, by
  ADR-0217's own decision, carries no bodies. It answers history questions; it
  cannot answer "what does this block say right now".

### Alternative C — Re-measure in the handler

- **Why considered**: No engine state to extend; the handler already holds the
  `Agent`.
- **Why rejected**: `Policy::plan` is impure — it advances the prefix-drift
  witness. Calling it to answer a question would make the *next* real request
  report drift against a measurement nobody sent. The one stored measurement is
  not an optimisation; it is the only correct source.

### Alternative D — Keep `render_json` body-less and add a second `--with-bodies`

- **Why considered**: Protects the existing golden's size.
- **Why rejected**: Two machine forms for one value is the divergence hazard this
  whole ADR lineage exists to remove. A consumer that needs less can ignore a
  field; a consumer that needs more cannot conjure one.

### Alternative E — Reintroduce `nuo context` as a CLI verb

- **Why considered**: ADR-0217 §4 promised it, and CI wants a stable entry point.
- **Why rejected**: A live plan only exists inside a running `Agent`, so the verb
  would have to attach to a session, drive a control round-trip, and render the
  result — duplicating the dialog's renderer in a second crate for a workflow
  that also has to choose *which* thread it means. CI's need is met by the golden
  snapshot, which is hermetic and needs no server.

## Compliance and Verification

- `crates/protocol/src/context/view.rs` — `ContextPlanView`/`PlannedBlockView`
  serde round-trip, including `Resume::Tool` as an owned name.
- `crates/engine/src/cognition/context/inspect.rs` — `[INV-CTXPLAN-04]` body/cost
  agreement; `[INV-CTXPLAN-05]` order preservation.
- `crates/engine/src/cognition/agent/state.rs` — a fresh plan clears the previous
  estimate.
- `crates/nuo/src/shell/overlays/context_plan.rs` — row construction: band
  headers, inactive rows, elision notes, drift tone, empty state.
- `tests/integration/engine/it/context_prefix.rs` — joined bodies equal the
  rendered prefix; the re-pinned JSON golden.
- `tests/integration/governance/context_engineering.rs` — no durable type names
  `PlannedBlockView`; no client calls `Policy::plan`.

## Links

- [ADR-0215](0215-context-sources-declare-moment-topic-rank-stability-and-budget.md)
- [ADR-0216](0216-the-prompt-branches-on-declared-tool-capability-not-tool-names.md)
- [ADR-0217](0217-context-projection-is-inspectable-and-prefix-drift-is-reported.md)
- [RFC-0004](../rfc/0004-context-sources-declaration-budget-and-ownership.md)
