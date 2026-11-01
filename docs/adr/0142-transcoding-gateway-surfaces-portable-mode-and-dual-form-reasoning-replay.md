---
id: ADR-0142
title: "Transcoding Gateway Surfaces: Portable Mode Is a Declared Dialect, and Its Replay Shape Satisfies Every Upstream Behind One URL"
status: accepted
date: 2026-11-01
scope: provider/nuo-provider, providers/nuo-provider-openai, providers/nuo-provider-opencode-go, server/nuo-server, routing/nuo-model-routing, docs/reference/openapi
superseded_by: null
negative_knowledge: true
---

# 0142. Transcoding Gateway Surfaces: Portable Mode Is a Declared Dialect, and Its Replay Shape Satisfies Every Upstream Behind One URL

- Status: Accepted
- Date: 2026-11-01
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Model Routing, Server Core
- Amends: [ADR-0113](0113-provider-owned-wire-dialects-route-derivation-never-infers-a-dialect-from-a-model-id.md) — the dialect vocabulary gains a variant for a surface that is neither Standard nor any vendor's native shape. Applies, without amending, [ADR-0114](0114-provider-owned-response-statefulness-remote-continuation-requires-a-declared-stored-surface.md) §"Subsystem Boundaries" to a new surface.

---

## Context and Problem Statement

The `opencode-go` channel was driven over `chat/completions`. The vendor's own
Console documentation says that is not the only way in:

> Each model is listed with its native API, but **every text model also works
> through the other APIs, which Console translates.**

Probing `POST https://opencode.ai/zen/go/v1/responses` with a real Go key
confirms it: 23 of the 28 ids the live catalog advertises answer `200` with a
real `response` object, tool calls included. The Responses surface is real and
covers the whole fleet, not just the four models whose *native* API it is
(`grok-4.7`, `gpt-5.6-luna`, `gpt-6-luna`, the two `muse-spark-*` rows).

Moving the channel onto it is not a URL swap, because the gateway is a
**translator**, and its behaviors differ from both the Standard Responses
dialect and DeepSeek's native one. Each was established by probe, and each fails
a turn outright:

| Probe | Result |
| --- | --- |
| `store: true` — **stateless row** (`deepseek-v4.1-flash`, and every converted model) | `400 store: stored responses require a native Responses target; use store=false and send prior output items in input` |
| `store: true` — **native row** (`grok-4.7`, `gpt-6-luna`, `gpt-5.6-luna`, `deepseek-v4-pro` passthrough) | `200`, and real state: a continuation with only the new input recalls the prior turn |
| `previous_response_id` — stateless row | `400 … requires a native Responses target; portable routing requires the full conversation in input` |
| nested tool definition | `400 tools[0].function: this tool feature requires a native Responses target` |

Statefulness is therefore **per-model behind one URL**, and the protocol surface
is stateless because it is declared once for the whole surface. See §6 — this is
the closest call in the decision, and it deliberately trades a capability away.

The fourth discovery is the one that decides the design. Reasoning replay is
**not uniform across the upstreams behind the single URL**:

| Replayed `reasoning` item | `deepseek-v4.1-flash` (converted) | `deepseek-v4-pro` (native passthrough) | `gpt-6-luna` (native) |
| --- | --- | --- | --- |
| `summary` only | `200` | `400` "The `reasoning_text` in the thinking mode must be passed back" | `200` |
| `content` only | `400` `input[N].summary: expected an array` | `200` | `400` `Missing required parameter: 'input[N].summary'` |
| **both in one item** | **`200`** | **`200`** | **`200`** |

No single-form item is accepted by every upstream. Two of the three shapes fail
depending on which model the user picked, and the failure lands on the *second*
turn of a tool round — after the first turn has already been paid for and after
the model has already committed to a call. Nothing in the client warned about
this; the old chat-completions lane had never needed to replay reasoning items
in Responses form at all.

## Decision Drivers

- **The endpoint's contract is the endpoint's to declare.** ADR-0113 established
  that a dialect belongs to the provider spec, never to a model id. Here the
  dialect must describe a surface that is *not* any single upstream's shape.
- **One URL, many upstreams.** A client cannot select a replay shape per model,
  because the model is chosen at runtime from a catalog the relay controls and
  rewrites server-side (ADR-0014). The shape must be safe for all of them at once.
- **Fail-safe statefulness.** ADR-0114 already requires "storage" to be declared,
  not inferred. This surface declares itself stateless in its own error text
  ("portable routing requires the full conversation in input") — the vendor names
  the mode, so the client vocabulary should carry that name.
- **No silent capability loss.** Moving surfaces must not trade away tool calling,
  streaming, effort control, or session affinity, all of which work today.

## Architectural Decisions

### 1. `ProviderDialect::Portable`, one variant, four predicates

`nuo_provider::ProviderDialect` gains `Portable`, mapping to
`OpenAiResponsesDialect::Portable`. It is a *gateway* dialect: a surface that
accepts one protocol and translates it onto heterogeneous upstreams.

The variant answers the existing dialect predicates, so the request builder keeps
exactly one shape decision:

- `nested_tool_definitions()` → `false` (flattened OpenAI tools)
- `supports_store()` → `false` (`store` is never emitted)
- `supports_tool_choice()` / `supports_parallel_tool_calls()` / `supports_developer_role()` → `true`
- `encrypted_reasoning()` / `reasoning_summary()` → `true` (a translator understands both grants)

### 2. Dual-form reasoning replay is a first-class dialect predicate

A new predicate names the requirement rather than hiding it in the builder:

```rust
pub const fn dual_form_reasoning_replay(self) -> bool {
    matches!(self, OpenAiResponsesDialect::Portable)
}
```

When true, every reconstructed `reasoning` item carries **both** part families:
`summary:[{type:"summary_text",text}]` and `content:[{type:"reasoning_text",text}]`.
This is the only shape the table above shows every upstream accepting.

The pruning rule composes with it: an older historical turn emits
`summary: []` and **no** `content` key. An empty `reasoning_text` is itself
rejected ("must be passed back"), while an empty `summary` array is accepted —
so the pruned case keeps the array and omits the parts. Pruning older chains is
the existing ADR-0083 hygiene (replaying every historical chain saturates the
context and collapses attention), now applied to a surface that needs it too.

### 3. The `opencode-go` surface declares itself

`nuo_provider_opencode_go::MODEL_PROVIDER_SPEC` becomes:

- `protocol: OpenAiResponses`, `root_url` unchanged (the `/responses` suffix comes
  from the ADR-0015 endpoint algebra, so no URL is hard-coded)
- `dialect: ProviderDialect::Portable`
- `responses_state: ResponsesState::Stateless` (unchanged — it was already this)
- every baseline row's `protocol` moves from `OpenAiChatCompletions` to
  `OpenAiResponses`

`protocol_roots` keeps its two Anthropic and Google entries: those models
genuinely speak their own protocols, and the gateway mounts dedicated routes for
them.

### 4. Normative invariants established

- `[INV-PROV-RESPONSES-REPLAY-01]`: a Responses route whose spec declares a
  dual-form dialect emits both reasoning part families in one item, and emits a
  pruned older turn as an empty `summary` array with no `content` key. Enforced by
  `nuo_provider_openai::responses::request::tests::portable_dialect_replays_reasoning_in_both_forms`
  and `…::portable_dialect_prunes_older_reasoning_to_an_empty_summary`.
- `[INV-PROV-RESPONSES-REPLAY-02]`: such a route never emits `store` and never
  emits a nested tool definition. Enforced by
  `…::portable_dialect_emits_flat_tools_and_never_store`.
- Both are dialect-scoped, so `Standard`, `DeepSeek`, `ChatGpt`, and `Copilot`
  routes keep their existing single-form contracts byte-for-byte.

### 5. Subsystem boundaries touched

- `crates/nuo-provider`: `ProviderDialect::Portable`, `OpenAiResponsesDialect::Portable`,
  `dual_form_reasoning_replay()`.
- `providers/nuo-provider-openai`: `reasoning_item()` (the single builder for a
  replayable reasoning item) plus the two replay paths that use it.
- `providers/nuo-provider-opencode-go`: the spec and its 26 baseline rows.
- `docs/reference/openapi/opencode-go-responses.yml`: the wire contract, marked
  `probe`/`docs` per fact.

## Consequences

- **Positive**: the Go channel rides the protocol its vendor documents as the
  modern one; tool calling, streaming, effort, `include`, and session affinity
  all keep working (verified end-to-end over `deepseek-v4.1-flash` and
  `deepseek-v4-pro`, the strictest upstream); prompt-cache reads still report
  (`cache_read_tokens: 1792` on a replays turn); a future translating gateway can
  declare `Portable` instead of reinventing the shape.
- **Negative & Trade-offs**: the route replays the full context every turn — no
  `previous_response_id` continuation, so a long conversation resends its history.
  For the converted models that *is* the surface's own constraint. For the native
  rows it is a **capability the channel gives up**: `grok-4.7`, `gpt-6-luna`,
  `gpt-5.6-luna`, and the `deepseek-v4-pro` passthrough genuinely store responses
  and accept a delta-only tool continuation — probed, and verified by sending
  nothing but a `function_call_output` against a `previous_response_id`. They
  worked before this ADR only because the channel spoke chat-completions, which
  has no such continuation either; nothing was lost by the move, but nothing was
  gained either, and a per-model state declaration would be needed to gain it
  (§6). The escaped cost is cache reads. The vendor's converter also remains a
  translation layer: a failure surfaces as `Upstream request failed: …` with the
  upstream's message, so the gateway's own errors and the upstream's are
  indistinguishable except by text.
- **Deliberately not done**: the free-tier rows (`deepseek-v4.1-flash-free`,
  `glm-5.3-flash-free`) answer `Model is unavailable` over *both* surfaces for
  this account, so their capability rows stay registered (the catalog decides
  availability) and no routing change was inferred from an outage.

## 6. The Statefulness Trade-Off, Stated Plainly

This is the one place where the implementation is **less capable than the wire
allows**, and it deserves to be recorded as such rather than buried in a
consequence bullet.

Three shapes are possible for "which response state does this route preserve?":

| Option | Shape today | What it costs |
| --- | --- | --- |
| Declare `Stored` for the whole example | `ProviderDialect::Portable` + `ResponsesState::Stored` | **Breaks every converted model.** `store: true` is a hard `400` on `deepseek-v4.1-flash`, all GLM/Kimi/MiMo/Hy/LongCat/Step/Qwen rows, and MiniMax. A surface-wide declaration cannot be true for them. |
| Declare `Stateless` for the whole surface | **the status quo** | Native rows (`grok-4.7`, `gpt-6-*`, `deepseek-v4-pro`) replay context they could have anchored. Correct but leaves a feature unused. |
| Declare per model | not expressible | `ModelProviderSpec` carries one `dialect` and one `responses_state` per **surface**; ADR-0113 §2 forbids per-model dialects, and the same argument applies to state. |

The middle option is chosen because it is the only one that is *correct*. Making
it possible to choose the first for a subset of models requires a per-model
statefulness declaration on a surface that has, by construction, one wire schema
and one routing mode — which is the ambiguity ADR-0113 §2 already rejected for
dialects. That is a routing-layer design decision, not a provider-spec field, and
it is deliberately left open rather than silently approximated.

**The trap this avoids**: `deepseek-v4-pro` accepts `store: true`, answers with an
id, and **still remembers nothing** (continuation with only the question replies
"I don't know your favourite number"). Had the declaration been derived from
"does `store` return `200`", this row would have looked stateful and every
delta-only turn would have silently lost its context — exactly the failure
ADR-0114 was written to prevent. `store` support is not storage, and per-model it
is not even consistent.

**Not to be confused with CommandCode.** The `commandcode-goat` relay also speaks
Responses and also declares `Stateless`, but for a different reason: it accepts
`store: true` and returns an id, then rejects every continuation with
`InvalidParameter.PreviousResponseNotFound` — the gateway keeps no store at all.
Both are `Stateless`; only one of them is stateless because it says so.

## Rejected Alternatives & Negative Knowledge

### 1. Keep `chat/completions` and declare the rumor false
- **Why considered**: chat-completions works today; the change costs a full
  rewrite of the channel's default protocol.
- **Why rejected**: the Responses surface is real, covers the whole fleet, and is
  what the vendor documents as applicable to every text model. What the rumor got
  wrong is the *mechanism* (a stateless translation layer, not a native Responses
  backend) — which is a reason to model it precisely, not a reason to stay put.

### 2. Reuse `OpenAiResponsesDialect::DeepSeek` for the Go channel
- **Why considered**: DeepSeek's native surface is also stateless and also emits
  plaintext reasoning, so most predicates already agree.
- **Why rejected**: probed directly. The DeepSeek dialect emits nested tool
  definitions (the gateway answers `400`), omits `tool_choice` and
  `parallel_tool_calls` (both accepted and used here), and emits single-form
  `content`-only reasoning items (`400` on the converted DeepSeek rows and on
  every native Responses upstream). Bedrock of the ADR-0113 argument: the dialect
  describes the *endpoint*, and this endpoint is not DeepSeek's.

### 3. Pick a replay form per model — `summary` for native, `content` for converted
- **Why considered**: it is what the probe table literally shows each upstream
  accepting, and it would keep payloads minimal.
- **Why rejected**: a per-model rule is exactly the model-id-derived dialect
  ADR-0113 abolishes, and it is unimplementable here regardless: the catalog the
  ids come from is relay-controlled and rewritten server-side (ADR-0014), so the
  client would be guessing at runtime. The dual form is accepted by every upstream
  and costs a few tokens once per tool round.

### 4. Emit no reasoning item at all on replay
- **Why considered**: the fleet's most tolerant upstream accepts it, and it is the
  smallest possible request.
- **Why rejected**: probed — `deepseek-v4-pro` (native passthrough) 400s with
  ``The `reasoning_text` in the thinking mode must be passed back to the API``.
  Tolerance on one upstream is not a contract.

### 5. Send `store: false` explicitly to be safe
- **Why considered**: explicit is better than absent.
- **Why rejected**: the field is not part of this surface's accepted vocabulary
  for stateless routing; the spec's `Stateless` declaration already guarantees it
  is never emitted (ADR-0114), and `supports_store()` returning `false` is the one
  place that decides it.

## Links

- [ADR-0083](0083-deepseek-model-identity-reasoning-toggle-and-responses-dialect-purity.md) — dialect purity and the CoT-pruning hygiene this reuses
- [ADR-0113](0113-provider-owned-wire-dialects-route-derivation-never-infers-a-dialect-from-a-model-id.md) — the dialect belongs to the provider spec
- [ADR-0114](0114-provider-owned-response-statefulness-remote-continuation-requires-a-declared-stored-surface.md) — a surface must declare whether it stores responses
- [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md) — provider-owned channels and the endpoint suffix algebra
- [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) — relays rewrite model ids server-side
- Wire reference: [`docs/reference/openapi/opencode-go-responses.yml`](../reference/openapi/opencode-go-responses.yml)
- Vendor authority: `opencode.ai/v2/docs/console/go` (endpoint table, "Console translates", `x-opencode-session` requirement)
