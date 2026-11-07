---
id: ADR-0157
title: "Provider Calls Borrow the Assembled Request"
status: accepted
date: 2026-11-07
scope: provider/interface, provider/anthropic, provider/openai, provider/google, harness/agent-loop
superseded_by: null
negative_knowledge: true
---

# 0157. Provider Calls Borrow the Assembled Request

- Status: Accepted
- Date: 2026-11-07
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Adapters, Agent Runtime, Performance Engineering
- Complements: [ADR-0155](0155-turn-persistence-is-o-delta-and-storage-maintenance-never-blocks-the-writer.md)

---

## Context and Problem Statement

The harness owns one assembled `ModelRequest` per logical turn and re-dispatches
it on transport retries. The provider trait nevertheless took it **by value**:

```rust
async fn stream_chat_events(&self, request: ModelRequest) -> …
```

so every attempt paid `request.clone()` — a deep copy of the entire message
window — solely to stamp a fresh per-attempt `transport_telemetry` handle. On a
400k-token session that is a multi-megabyte memcpy and a full allocation burst
per attempt, on a path whose entire purpose is to hand bytes to the transport.
The same owned signature forced every provider builder to take
`Vec<Message>`, which made "build a wire body" look like "consume the
conversation" and hid the copy inside the adapter.

## Decision

### 1. The request is borrowed, telemetry is an attempt argument

```rust
async fn chat(&self, request: &ModelRequest) -> …
async fn stream_chat(&self, request: &ModelRequest, telemetry: TransportTelemetry) -> …
async fn stream_chat_events(&self, request: &ModelRequest, telemetry: TransportTelemetry) -> …
```

- `ModelRequest` no longer carries `transport_telemetry`. Telemetry is a
  runtime handle for one *network attempt*; the assembled request is a fact of
  the *logical turn*. Stamping attempt state onto the request was what forced
  the clone, and it was the only mutable field that differed between attempts.
- `chat` (titles, compaction summaries, diagnostics) uses a default handle: it
  is not the streaming telemetry path.
- The harness hands `request` directly and the attempt guard's handle
  separately (`crates/nuo-harness/src/cognition/agent/rounds.rs`). A retry
  reuses the same request and takes a new handle.

### 2. Builders borrow the window

- `ModelRequest::wire_messages()` yields conversation + request-local tail;
  `tool_specs()` yields the declarations.
- `project_images_for_route` returns `Cow<[Message]>`: borrowed unless the
  route rejects images the window actually carries.
- Provider-specific cleanup copies **only the messages it changes**:
  - Anthropic filters unanswered tool calls and empty turns at serialization
    (`message_obj(&Message, &answered) -> Option<Value>`), mutating nothing.
  - OpenAI chat strips orphan tool results, unanswered calls, and historical
    reasoning into a `Vec<Cow<Message>>`; a message that needs no change stays
    borrowed.
  - OpenAI Responses projects into wire items from borrowed messages.
- Google's builder still reconciles into an owned list internally; that is the
  remaining borrow candidate and is called out in Consequences rather than
  hidden.

### 3. Wire output is unchanged

This is an interface and copy-model change only. Provider golden/unit suites
(Anthropic, OpenAI chat + Responses, Google) pin the JSON shapes; they pass
unchanged.

## Invariants

1. **[INV-PROVIDER-01] No per-attempt window copy.** A provider call receives a
   borrow; nothing on the dispatch path clones the message list.
2. **[INV-PROVIDER-02] Telemetry is attempt-scoped.** It is passed to the call,
   never stored on the turn's request, so one attempt's timings cannot be
   attributed to another.
3. **[INV-PROVIDER-03] The caller's window is immutable.** A builder may copy a
   message to filter it; it never rewrites the request it was handed.
4. **[INV-PROVIDER-04] Wire bytes are stable.** Borrowing changes the cost
   model, not the protocol.

## Consequences

- A transport retry no longer copies the conversation; only the request
  projection (already built) is reused.
- Providers that need to mutate a message for wire validity pay for **that
  message**, not the window.
- The remaining copy is the `serde_json::Value` body every adapter builds
  before the HTTP layer serializes it. Removing it requires direct borrowed
  serialization per dialect; that is a separate, wire-risk-bearing change and
  is not claimed here.
- Google's request builder still materializes an owned list because its
  reconciliation mutates. It is the next candidate for the same treatment.

## Negative Knowledge

- **Do not** put per-attempt state back on `ModelRequest`. That is the clone.
- **Do not** take ownership "to simplify a filter". Filters that need to change
  a message copy that message; `Cow` is the shape that makes the cost visible.
- **Do not** mutate the caller's window to satisfy a provider contract. The
  request is borrowed and read-only; a wire cleanup is a projection.
- **Do not** treat the borrowed interface as permission to serialize lazily
  across an `await` boundary; the body is still built before dispatch.

## Verification

- `nuo-provider-anthropic` (40 tests), `nuo-provider-openai` (101),
  `nuo-provider-google-antigravity` (96): wire-shape suites unchanged.
- `nuo-harness` tests, including
  `each_attempt_carries_its_own_transport_telemetry_across_a_retry` (per-attempt
  handles survive a retry without a request copy).
- `cargo check --workspace --all-targets` and `cargo clippy --workspace
  --all-targets` clean.
