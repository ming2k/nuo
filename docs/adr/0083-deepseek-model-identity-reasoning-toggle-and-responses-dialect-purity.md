---
id: ADR-0083
title: "DeepSeek Model Identity, Reasoning-Toggle Fidelity, and Responses Dialect Purity"
status: accepted
date: 2026-11-02
scope: providers/nuo-provider-deepseek, providers/nuo-provider-openai, providers/nuo-provider-catalog, providers/nuo-provider-opencode-plan, providers/nuo-provider-opencode-zen, providers/nuo-provider-qianwen, provider/nuo-provider, codec/nuo-model-codec, server/nuo-server
superseded_by: null
negative_knowledge: true
---

# 0083. DeepSeek Model Identity, Reasoning-Toggle Fidelity, and Responses Dialect Purity

- Status: Accepted
- Date: 2026-11-02
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Model Catalog, Agent Runtime
- Amends: [ADR-0080](0080-time-varying-rate-cards-and-off-peak-rating.md) §5 (the `deepseek-v4.1-flash` row was a relay id, not a platform id)

---

## Context and Problem Statement

DeepSeek is invoked by every Nuo agent turn, so any deviation between Nuo's
model of the DeepSeek surface and the platform's actual contract is a
correctness defect, not a cosmetic one. A live re-audit of the official
documentation (`api-docs.deepseek.com`: Thinking Mode guide, Responses API
reference, Chat Completions reference, Models & Pricing, Change Log — 2026-10
revision) found five defects in Nuo's implementation:

1. **Wrong model identity.** `nuo-provider-deepseek` seeded
   `deepseek-v4.1-flash`, which is a **Novita/OpenRouter relay id** (first seen
   in ADR-0014's live GOAT probe as `deepseek-v4.1-flash@novita`). The platform
   serves exactly two wire ids: `deepseek-flash` (DeepSeek-V4.1-Flash-1017) and
   `deepseek-v4-pro` (DeepSeek-V4-Pro-0813). `deepseek-flash` was absent from
   the seeds, `deepseek-v4.1-flash` never existed upstream (a first turn would
   fail with `model not exist`), and the two retired V4 Flash aliases were
   modelled as a text-only model even though upstream now serves them from the
   multimodal V4.1 Flash. ADR-0080 had already recorded the canonical id in its
   Context section while its §5 decision and the shipped code contradicted it.
2. **Unreachable "thinking off".** Thinking is on by default at effort `high`,
   and `none` — the off switch on both protocols — was absent from the compiled
   ladder. Worse, `clamp_to_levels` snaps a request below the ladder's floor
   *up*, so an explicit `none` was served as `low`: "do not think" became
   "think lightly".
3. **A silent downgrade for alias tiers.** The ladder named only the three
   distinct rungs (`low`/`high`/`max`) while the platform also accepts
   `minimal`/`medium`/`xhigh`/`ultra` and maps them itself (`medium`→`high`).
   The clamp therefore turned an explicit `medium` into `low` — *shallower* than
   the `high` upstream would have served.
4. **Foreign fields on the Responses wire.** The shared Responses builder
   unconditionally emitted OpenAI's `reasoning.summary: "detailed"` and, because
   the DeepSeek dialect forces `store: false`, `include:
   ["reasoning.encrypted_content"]` on every request. Neither field exists in
   DeepSeek's Responses schema (its `reasoning` object is `{effort}`; it returns
   plaintext `reasoning_text` items, so there is nothing to decrypt).
5. **Dropped CoT replay and unhandled terminals.** On Chat Completions the
   builder replayed an assistant turn's reasoning only for OpenRouter, so
   DeepSeek's documented requirement (with `tools` present, every prior
   `reasoning_content` must be passed back and is concatenated into context) was
   silently unmet. The stream parser treated only `response.completed` as
   terminal, so `response.incomplete` (output cap reached — the turn's tokens
   were billed) and `response.failed` (in-band upstream error) ended the turn
   with no usage and no reason.

The catalog layer compounded (2) and (3): a DeepSeek connection's live `/models`
refresh advertises `effort.supported_levels` — the rungs the model
*distinguishes*, which deliberately excludes `none` — and that advertisement
replaces the compiled ladder, so even a corrected static ladder would decay back
to `low`/`high`/`max` on first sync.

## Decision Drivers

- Model identity is a wire fact, not a naming preference: a seeded id that the
  platform does not serve is a first-turn failure.
- "Reasoning off" must be expressible wherever the provider offers it; a clamp
  that silently *raises* a request inverts its meaning.
- A provider dialect must never emit a field its backend does not define.
- Losing the replayable CoT degrades every tool round trip on a reasoning model,
  and losing terminal usage corrupts cost accounting.
- The compiled baseline and the discovered advertisement must agree, or the
  model menu changes shape (and persisted choices change meaning) after a sync.

## Architectural Decisions

### 1. The platform's ids are the only seeded ids

`providers/nuo-provider-deepseek` seeds `deepseek-flash` and `deepseek-v4-pro`.
The retired names `deepseek-v4-flash` and `deepseek-v4-flash-vision-exp` stay
registered as baselines (never seeded) because upstream still accepts them and
serves them from DeepSeek-V4.1-Flash-1017 — so their row is the *serving*
model's (1M context, `vision: true`). `deepseek-v4.1-flash` is removed entirely:
it is a relay spelling, documented with its relays.

`nuo_model_codec::model_providers::DEEPSEEK_BUILTIN_MODELS` is the single source
for the seeded list, and the provider spec asserts both facts in tests.

### 2. The ladder is the accepted set, and `none` is the off switch

`nuo_provider::effort_ladders::DEEPSEEK_NATIVE` declares the whole closed
vocabulary (`none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max`/`ultra`)
because that is what the platform accepts; the alias mapping
(`minimal`→`low`, `medium`/`xhigh`→`high`, `ultra`→`max`) is upstream's
business, not the clamp's. `none` ranks below every rung and is now *present*,
so the clamp keeps it instead of snapping up to `low`.

`LOW_HIGH_MAX` remains only where the accepted **strings** are unverified beyond
the three presets (Moonshot Kimi K3 and the CommandCode `provider/v1` lanes,
which additionally advertise an `off` token Nuo has not wire-verified).

### 3. Live discovery expands the advertisement to the accepted set

`OpenAiCatalogParser` maps DeepSeek's `effort.supported_levels` through
`deepseek_accepted_efforts`, which prepends `none` and adds a documented alias
only when its mapping target is advertised. Consequences:

- the discovery result equals the compiled `DEEPSEEK_NATIVE` ladder, so a sync
  never changes the menu (enforced by a test comparing the two);
- a future model that narrows its own set never inherits a rung above its
  ceiling — nothing is invented above the model's own `max`.

### 4. Dialect-declared field sets on the Responses wire

`OpenAiResponsesDialect` gains two capability predicates —
`reasoning_summary()` and `encrypted_reasoning()` — and `DeepSeek` returns false
for both. The request builder emits `reasoning.summary` and
`include: ["reasoning.encrypted_content"]` only where the dialect declares them,
and omits an empty `reasoning` object entirely so the documented server default
applies. A shared builder must not guess a backend's schema.

### 5. Reasoning replay is a dialect property

`OpenAiChatDialect` gains `DeepSeek` (mapped from `ProviderDialect::DeepSeek`)
with `replays_reasoning_content() -> true`: an assistant turn's CoT is stamped
back as `reasoning_content` — the field name DeepSeek documents — on that
dialect only. OpenRouter keeps its `reasoning_details` sidecar; standard
OpenAI-compatible surfaces keep receiving no CoT (they define none).

### 6. Terminal events are a closed set

`ResponsesStream` treats `response.completed` **and** `response.incomplete` as
the same terminal completion (output items, replay artifact, usage) and surfaces
`response.failed` as a `ProviderError` carrying the backend's message and code.
A truncated turn is a *completion whose output is short*: its tokens were billed
and its items stay replayable.

### 7. Shared wire ids carry one capability row across providers

The registry is keyed by model id, and an invariant test requires every table
that declares the same id to agree field-for-field. DeepSeek's ids therefore
carry the platform's contract in the OpenCode Go/Zen tables too (they forward
`reasoning_effort` to the same backend); consequently the V4 Flash family is
multimodal there as well. Qianwen's Token Plan keeps its stricter floor
(`none`/`minimal` rejected) documented in `DEEPSEEK_V4_PLAN`, but cannot enforce
it on a shared id — the pre-existing, explicitly accepted limitation of an
id-keyed capability union.

Capability amendments made after the baseline-registry migration are recorded in
one explicit list (`POST_MIGRATION_AMENDMENTS`) in the migration-fidelity proof
instead of being folded into its historical snapshot.

## Normative Invariants Established

- **`[INV-DS-01]` Official identity**: a provider preset seeds only ids the
  upstream platform serves. Enforced by
  `spec::tests::seeds_the_two_official_ids_only` and
  `relay_id_is_not_registered_as_an_official_model`.
- **`[INV-DS-02]` Off switch reachability**: a reasoning model whose provider
  accepts `none` declares it in its ladder; the clamp must never raise it into a
  thinking rung. Enforced by
  `spec::tests::reasoning_depth_declares_the_whole_upstream_vocabulary`.
- **`[INV-DS-03]` Discovery/baseline agreement**: an expanded live advertisement
  equals the compiled ladder. Enforced by
  `deepseek_discovery_expands_exactly_to_the_compiled_ladder`.
- **`[INV-DS-04]` Dialect purity**: the shared Responses builder emits only the
  fields the request's dialect declares. Enforced at the builder level by
  `responses::request::tests::deepseek_dialect_emits_only_the_fields_its_schema_defines`
  (plus its standard-dialect counterpart), and end to end by
  `it::wire::deepseek_factory_body_carries_only_its_own_reasoning_surface`, which
  matches the complete request body a factory-built DeepSeek provider
  dispatches — the absence of `reasoning.summary` / `include` is part of the
  assertion.

## Consequences

- **Positive**: DeepSeek routes name real models; "thinking off" and the alias
  tiers reach the wire exactly as documented; no OpenAI-only field is sent to a
  backend that does not define it; tool-call turns keep their CoT; truncated and
  failed streams are billed and diagnosed instead of vanishing; a live catalog
  refresh is menu-stable.
- **Negative & Trade-offs**: the DeepSeek ladder now offers eight rungs where
  three are distinct — the editor shows the same shape as GPT-6 models, and the
  alias mapping is upstream's (documented in `docs/reference/effort.md`).
  Shared ids cannot carry per-route floors, so a Qianwen Token Plan Pro route
  still offers `none`, which that endpoint rejects; the alternative (a narrow
  row shared by every provider) is what produced this ADR's defects.
- **Migration**: a config saved with `deepseek-v4.1-flash` never worked upstream;
  it must be switched to `deepseek-flash` (the id is no longer registered).

## Rejected Alternatives & Negative Knowledge

### 1. Keep `deepseek-v4.1-flash` as an alias of `deepseek-flash`
- **Why considered**: zero-touch migration for configs written against the
  previous release.
- **Why rejected**: it would require the provider to rewrite the model id on the
  wire — a routing alias for an id upstream never accepted, hiding a naming
  error instead of correcting it, and diverging from `Model::id` ("the id is the
  identity everywhere").

### 2. Model "thinking off" as a separate `ReasoningMode` on OpenAI transports
- **Why considered**: symmetry with Anthropic, where `thinking` and
  `output_config.effort` are genuinely two knobs.
- **Why rejected**: DeepSeek's OpenAI-format surface already expresses the off
  switch as `reasoning.effort: "none"` / `reasoning_effort: "none"`. A second
  knob would mean two fields that can disagree, and `ReasoningMode::Off` is
  already the "omit the field" default for every OpenAI route — reinterpreting
  it as "send none" would change behaviour for every GPT/Kimi/GLM channel.

### 3. Narrow the ladder to the advertised `supported_levels`
- **Why considered**: `/models` is authoritative and advertises three rungs.
- **Why rejected**: the advertisement describes the rungs a model *distinguishes
  with thinking on* and explicitly excludes `none`. Taking it literally makes
  the off switch unreachable and degrades `medium` to `low`.

### 4. Send `reasoning.summary` and `include` to every Responses backend
- **Why considered**: one builder, one shape.
- **Why rejected**: a foreign field is not a capability negotiation. DeepSeek's
  schema defines `reasoning` as `{effort}` and returns plaintext reasoning, so
  the fields are at best ignored noise and at worst a 400.

### 5. Stamp the harness completion budget onto the wire as `max_tokens`
- **Why considered**: the audit flagged that OpenAI-family requests never carry
  `max_output_tokens` / `max_tokens`, so a user-set output bound is not enforced
  and the vendor's documented per-mode defaults (8K non-thinking / 64K thinking
  / 128K at `max`) are what apply.
- **Why rejected**: the only value available to stamp today is
  `session.budget.max_completion_tokens`, a *local accounting budget* whose
  default is 4096. Sending it as the API cap would truncate a DeepSeek thinking
  turn at 4K — a severe regression against a vendor default of 64K. Enforcing an
  output bound needs a decision about budget-vs-cap semantics (and a per-route
  cap source) that is independent of DeepSeek; until then, omitting the field is
  the behaviour that matches the vendor's documented contract. Tracked as a
  cross-provider follow-up, not silently wired here.

### 6. Treat `response.incomplete` as an error
- **Why considered**: "the answer did not finish, so the turn failed".
- **Why rejected**: the output exists, the tokens are billed, and the items are
  replayable. Discarding them loses the user's paid work and the usage record.
  Surfacing the incomplete reason as a hard error also gives the harness no way
  to continue from a truncated-but-valid turn.

## Links

- [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md) — the live probe that surfaced the relay id `deepseek-v4.1-flash@novita`
- [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md) — effort ladders and the discovery precedence chain
- [ADR-0080](0080-time-varying-rate-cards-and-off-peak-rating.md) — DeepSeek pricing/off-peak support; §5's model row is amended here
- `docs/reference/effort.md` — the prose companion for the depth vocabulary and ladders
- `docs/reference/openapi/deepseek.yml` — the authoritative wire reference updated by this decision
