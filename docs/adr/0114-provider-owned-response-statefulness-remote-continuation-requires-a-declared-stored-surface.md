---
id: ADR-0114
title: "Provider-Owned Response Statefulness: Remote Continuation Requires a Declared Stored Surface"
status: accepted
date: 2026-12-08
scope: provider/nuo-provider, routing/nuo-model-routing, server/nuo-server, providers/nuo-provider-commandcode-plan, providers/nuo-provider-openai
superseded_by: null
negative_knowledge: true
---

# 0114. Provider-Owned Response Statefulness: Remote Continuation Requires a Declared Stored Surface

- Status: Accepted
- Date: 2026-12-08
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Model Routing, Server Core
- Amends: [ADR-0113](0113-provider-owned-wire-dialects-route-derivation-never-infers-a-dialect-from-a-model-id.md) — extends "the provider owns its wire behavior" from the request *shape* to the request *sequence*

---

## Context and Problem Statement

Once ADR-0113 gave the `commandcode-plan` lane the Standard Responses dialect,
every turn that performed a tool call failed on its follow-up request:

```text
OpenAI Responses HTTP 400 Bad Request: Messages with role 'tool' must be a
response to a preceding message with 'tool_calls'
```

The label is `OpenAI Responses`, i.e. the Standard dialect — the ADR-0113 shape
defect was gone and a different one was exposed.

`OpenAiResponsesProvider::continuation_mode()` answers `RemoteStored` whenever
`store` is true, and `store` defaults to true for the Standard dialect. A
remote-stored route does not resend the conversation: after a completed
response it parks a `ContinuationCursor` on the assistant node, and the next
turn sends `previous_response_id` plus only the new suffix
(`RequestDelivery::RemoteContinuation`). With a tool call in flight, that suffix
is a single `function_call_output`.

CommandCode's `/provider/v1/responses` accepts `store: true` and answers with an
id (`gen_…`), but it keeps no state of its own: it translates each request into
a fresh chat-completions call against the upstream vendor. A delta-only turn
therefore reached DeepSeek as one `role:"tool"` message with no preceding
assistant `tool_calls`, which is exactly what it rejected.

The discriminating evidence, taken against the live gateway with the same model
and key:

| Turn | Result |
| --- | --- |
| tool-call request, no continuation | `200`, `id = gen_01M5Y49ATJCTBJPK4R3YEC2Z5R` |
| same turn continued with `previous_response_id` + only `function_call_output` | `400 Messages with role 'tool' must be a response to a preceding message with 'tool_calls'` |
| same content replayed in full, no `previous_response_id` | `200` |

`store` support is not storage. An endpoint can validate the field, echo an id,
and still forget everything — so accepting `store: true` can never be the
authority for "this endpoint remembers my turn". Before ADR-0113 the lane
carried `ProviderDialect::DeepSeek`, whose `supports_store()` is false, so the
route replayed in full by accident of the dialect; ADR-0113 removed the
accident, and the assumption underneath it surfaced.

## Decision Drivers

- Correctness on relays: a delta-only turn is invalid unless the endpoint truly
  holds the previous response.
- Fail-safe defaults: an undeclared surface must not be able to assume storage.
- Single source of truth: like the dialect, statefulness belongs to the
  endpoint's declaration, not to a model id, a vendor name, or a field the
  endpoint merely tolerates.
- No behavior change for surfaces that genuinely persist.

## Architectural Decisions

Response statefulness becomes an explicit, provider-declared property carried by
the route:

- `nuo_provider::ResponsesState { Stored, Stateless }`, defaulting to
  `Stateless`.
- `ModelProviderSpec::responses_state` declares it per surface. User-declared
  (BYOK) providers declare `Stateless`: an operator declares an endpoint, not a
  storage guarantee.
- `Transport::OpenAiResponses` carries the resolved `state`, so the composition
  root needs no spec lookup.
- `build_provider_for_channel` derives `store` from **both** axes:
  `state.is_stored() && dialect.supports_store()`. The dialect veto is
  preserved; the surface veto is new. Anything less than both yields
  `ContinuationMode::OpaqueReplay`, where the full local context (preferring
  provider-owned output artifacts) is replayed and `previous_response_id` is
  never emitted.

### Subsystem Boundaries & Structural Changes

- `crates/nuo-provider`: `ResponsesState`, `Transport::OpenAiResponses::state`,
  `ModelProviderSpec::responses_state`, and the `Stateless` default for
  user-declared surfaces.
- `crates/nuo-model-routing`: the route derivation forwards the spec's declared
  state, exactly as it forwards the declared dialect.
- `crates/nuo-server`: the factory intersects the declaration with the dialect's
  `store` support.
- `providers/*`: every surface states its position. `openai` (its own Responses
  API persists) declares `Stored`; `commandcode-plan`, `deepseek`,
  `chatgpt-plan` and every other surface declare `Stateless`.

### Normative Invariants Established

- `[INV-PROV-RESPONSES-STATE-01]`: a Responses route may request remote
  continuation only when the provider spec declares `ResponsesState::Stored`
  **and** the dialect defines `store`. Enforced by
  `provider_registry::tests::unstated_responses_storage_replays_the_full_context`
  (declaration → `ContinuationMode`) and
  `responses_state_is_provider_declared_not_assumed` (spec → derived route).
- `[INV-PROV-RESPONSES-STATE-02]`: an undeclared surface is `Stateless`. The
  enum's default, the user-declared builder, and the routing tests all fail
  closed.

## Consequences

- **Positive**: relay routes work across tool rounds; a new relay cannot
  reintroduce the defect by omitting a declaration; stored platforms keep their
  exact behavior.
- **Negative & Trade-offs**: `Stateless` routes resend the full context on every
  turn, which costs prefix tokens those relays cannot reuse server-side anyway
  (they hold no state). A user-declared BYOK surface pointed at a platform that
  *does* persist (e.g. `api.openai.com`) also replays in full until the operator
  can declare otherwise; correctness is preserved because `store: false` plus
  `include: ["reasoning.encrypted_content"]` is the supported stateless
  continuation on that platform.
- The route fingerprint already distinguishes `stored` from `local`, so a
  policy change invalidates parked cursors instead of mixing the two modes.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): treat `store` acceptance as proof of storage

- **Why considered**: The endpoint validated the field and returned an id; that
  looks like a stored response.
- **Why rejected**: Disproved directly — the same gateway accepts `store: true`,
  returns `gen_…`, and then rejects a `previous_response_id` follow-up. Protocol
  toleration is not a capability.

### Alternative B (Rejected): retry the failed turn as a full replay

- **Why considered**: A transparent fallback would keep remote continuation on
  good endpoints.
- **Why rejected**: It converts a deterministic routing fact into a per-turn
  failure path: every tool round would burn a rejected request, and the retry
  cannot be distinguished from a genuine malformed-context error without
  string-matching the upstream message.

### Alternative C (Rejected): infer statefulness from the response id shape

- **Why considered**: `gen_…` is not `resp_…`; sniffing the prefix would
  separate relays from the real platform.
- **Why rejected**: Same category error ADR-0113 removed for dialects — the
  client would be guessing a declared property from a string, and the first
  relay that papers over its ids to look like OpenAI would silently break.

### Alternative D (Rejected): keep the DeepSeek dialect on the lane

- **Why considered**: `supports_store() == false` would restore full replay for
  free.
- **Why rejected**: It reintroduces the nested tool shape that ADR-0113 fixed.
  Wire shape and statefulness are separate axes and are now declared separately.

## Links

- Related ADRs: [ADR-0113](0113-provider-owned-wire-dialects-route-derivation-never-infers-a-dialect-from-a-model-id.md) (provider-owned wire shape), [ADR-0083](0083-deepseek-model-identity-reasoning-toggle-and-responses-dialect-purity.md) (stateless DeepSeek Responses surface), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md) (provider-owned channels), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md) (no string sniffing)
- Wire reference: `docs/reference/openapi/commandcode-plan.yml`
