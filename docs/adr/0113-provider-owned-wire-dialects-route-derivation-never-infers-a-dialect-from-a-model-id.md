---
id: ADR-0113
title: "Provider-Owned Wire Dialects: Route Derivation Never Infers a Dialect From a Model Id"
status: accepted
date: 2026-12-07
scope: routing/nuo-model-routing, provider/nuo-provider, providers/nuo-provider-openai, providers/nuo-provider-deepseek, providers/nuo-provider-commandcode-plan
superseded_by: null
negative_knowledge: true
---

# 0113. Provider-Owned Wire Dialects: Route Derivation Never Infers a Dialect From a Model Id

- Status: Accepted
- Date: 2026-12-07
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Model Routing, Server Core
- Related: [ADR-0083](0083-deepseek-model-identity-reasoning-toggle-and-responses-dialect-purity.md) §4/§5 — this ADR applies its dialect-purity principle to route derivation; it does not amend it.

---

## Context and Problem Statement

A turn on the `commandcode-plan` lane with a DeepSeek model failed before the
first streamed token:

```text
DeepSeek HTTP 400 Bad Request: Invalid input: expected string, received undefined (type: invalid_request_error)
```

The label `DeepSeek` is emitted by `OpenAiResponsesProvider::label()` only when
the provider was built with `OpenAiResponsesDialect::DeepSeek`. Nothing in the
`commandcode-plan` spec asks for that dialect — it declares
`ProviderDialect::Standard`, `protocol: OpenAiResponses`, and
`root_url: https://api.commandcode.ai/provider/v1`. The dialect was instead
forced by the route-derivation layer:

```rust
let responses_dialect = if model.contains("deepseek") {
    nuo_wire::OpenAiResponsesDialect::DeepSeek
} else {
    dialect.openai_responses()
};
```

That heuristic made the *model id* — a string the relay is free to rewrite
server-side (ADR-0014) — the authority on the endpoint's wire schema. It
overrode the only place that actually knows the endpoint: the provider spec.

The override is not cosmetic. `OpenAiResponsesDialect::DeepSeek` changes the
serialized body: it emits DeepSeek's **nested** tool definitions
(`{type:"function", function:{name, description, parameters}}`), and omits
`store`, `tool_choice`, `parallel_tool_calls`, `reasoning.summary`, and
`include`. CommandCode's `/provider/v1` is an OpenAI-compatible gateway whose
`/responses` surface is Zod-validated; its schema expects the **flattened**
OpenAI tool shape and a string at the tool's top-level `name`. The nested shape
leaves that field undefined, and the gateway rejects the turn with the message
above.

The same defect reaches every multi-vendor relay that serves a DeepSeek id:
opencode-plan, opencode-zen and qianwen happen to escape it only because they
serve DeepSeek over Chat Completions, where dialect selection was never
model-name-derived.

## Decision Drivers

- **The dialect describes the endpoint, not the model.** One wire id can be
  served by DeepSeek's native platform and by an OpenAI-compatible relay with
  different accepted fields; only the endpoint knows which body it validates.
- **ADR-0083 §4** already states the principle: "A shared builder must not
  guess a backend's schema." A model-name heuristic is exactly such a guess.
- **Provider ownership.** `base_route` documents that "dialect and credentials
  remain provider-owned"; `ProviderDialect::openai_responses()` is the single
  mapping from the spec to the Responses surface.
- **One selection site.** Dialect must be decided in exactly one place, with no
  per-provider special cases layered on top.

## Architectural Decisions

### 1. The Responses dialect is a pure function of the provider spec

`derive_channel` now selects the Responses dialect as
`dialect.openai_responses()`. The `model.contains("deepseek")` branch is deleted
outright — no fallback, no compatibility shim, no provider-id allowlist. The
native `deepseek` provider keeps the DeepSeek dialect because *it* declares
`ProviderDialect::DeepSeek`; the `commandcode-plan` gateway keeps the Standard
dialect because *it* declares `ProviderDialect::Standard`.

### 2. A per-model dialect is not expressible, and must not be

`ModelProviderSpec` carries one `dialect` per service surface. That is
sufficient and deliberate: a surface is a single endpoint with a single
validated schema. A route that genuinely needs DeepSeek's native field set is
the DeepSeek surface; a relay that serves DeepSeek ids is an OpenAI-shaped
surface. Folding both into one spec would reintroduce the ambiguity this ADR
removes.

### 3. Enforcement

This is a **Domain Contract** (a behavioral rule inside `nuo-model-routing`),
not a system invariant, so it mints no `INV-*` tag (see
`docs/dev/standards/invariants.md` §2). It is enforced by unit tests:

- `nuo_model_routing::tests::responses_dialect_is_provider_owned_not_model_derived`
  asserts both directions: a namespaced DeepSeek id on the OpenAI-compatible
  relay derives `Standard`, and `deepseek-flash` on the native platform derives
  `DeepSeek`.
- `nuo_model_routing::tests::commandcode_connection_derives_routes` pins the
  CommandCode route to `Standard` at the gateway endpoint.

## Consequences

- **Positive**: CommandCode DeepSeek turns travel in the OpenAI shape the
  gateway validates; the same wire id can be served by a native platform and a
  relay without a name heuristic; ADR-0083's dialect-purity principle now holds
  at the routing layer as well as in the request builder.
- **Negative & Trade-offs**: a *user-declared* provider that points at
  `api.deepseek.com` with DeepSeek ids and omits `dialect` now resolves to
  `Standard` instead of silently receiving the DeepSeek dialect. The operator's
  `model_providers.toml` declaration is authoritative — `dialect = "deepseek"`
  expresses the native surface. Silent name-based inference is the failure mode
  being removed.

## Rejected Alternatives & Negative Knowledge

### 1. Keep the heuristic and special-case `commandcode-plan`
- **Why considered**: Smallest possible diff; one provider stops failing.
- **Why rejected**: It preserves string sniffing as the dialect authority and
  leaves the next relay to discover the same bug. ADR-0027 already rejects
  provider-id/base-URL string sniffing as a routing mechanism.

### 2. Derive the dialect from the model's *family*
- **Why considered**: `family == "deepseek"` looks more principled than an id
  substring.
- **Why rejected**: Same category error. The family names the upstream model
  author, not the endpoint's request schema; the relay may route the same
  family through a vendor with a different accepted field set.

### 3. Declare `ProviderDialect::DeepSeek` on the `commandcode-plan` spec
- **Why considered**: Would make the existing heuristic "spec-owned".
- **Why rejected**: That surface also serves Claude and GPT over one relay, and
  its `/provider/v1` endpoints are OpenAI-shaped. A spec-level dialect of
  DeepSeek would mis-shape every other model on the lane to fix one.

### 4. Keep emitting DeepSeek's nested tools and ask the gateway to accept them
- **Why considered**: The nested shape is valid on the native platform.
- **Why rejected**: The client does not get to widen a server's accepted
  schema. `/provider/v1/models` advertises the standard `/responses` surface
  (see `providers/nuo-provider-catalog/src/list_models.rs`), and the gateway's
  validation is authoritative.

## Links

- Related ADRs: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) (server-side model rewriting), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md) (provider-owned channels), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md) (no string sniffing), [ADR-0083](0083-deepseek-model-identity-reasoning-toggle-and-responses-dialect-purity.md) (Responses dialect purity)
- Wire reference: `docs/reference/openapi/commandcode-plan.yml`
