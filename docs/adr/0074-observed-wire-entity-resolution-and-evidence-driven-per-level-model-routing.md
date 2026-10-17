---
id: ADR-0074
title: "Observed Wire-Entity Resolution: Evidence-Driven Per-Level Model Routing"
status: accepted
date: 2026-10-18
scope: codec/nuo-model-codec, provider/nuo-provider, provider/nuo-provider-google-antigravity, provider/nuo-provider-catalog, server/nuo-server
superseded_by: null
negative_knowledge: true
---

# 0074. Observed Wire-Entity Resolution: Evidence-Driven Per-Level Model Routing

- Status: Accepted
- Date: 2026-10-18
- Deciders: Nuo Architecture Working Group
- Consulted: Model Codec, Provider Integration, and Catalog Teams
- Amends: [ADR-0068](0068-canonical-model-catalog-normalization-and-semantic-lifting.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)

---

## Context and Problem Statement

Google Antigravity's CloudCode inference (`v1internal:streamGenerateContent`, `daily-cloudcode-pa.googleapis.com`) answers real user turns with:

```text
HTTP 404 Not Found — {"code":404, "message":"Requested entity was not found.", "status":"NOT_FOUND"}
```

when asked for `claude-opus-5-5` / `claude-sonnet-5-5` — ids the *catalog* advertises and Nuo's picker displays. Root cause, verified against three independent evidence sources (the shipped `agy` 1.4.0 binary's static tables, the paid-pro `/v1internal:fetchAvailableModels` snapshot, and upstream gateway reports):

1. **The bare 5-5 ids are catalog-side fold targets, not registered `PredictionService` entities.** Only the *per-level* entity ids `claude-{opus,sonnet}-5-5-{low,medium,high,max}` answer inference. This is the same "entity-suffix-is-effort" topology already known for `gemini-4-argon` (ADR-0068-era probe, live 2026-10-23).
2. **Nuo's request builder had no general mechanism to express this fact.** The interim fix hard-coded family-prefix string matches (`starts_with("claude-opus-5-5")`, `starts_with("gemini-4-argon")`) inside the antigravity provider — exactly the "continual generational blacklisting" shape ADR-0068 rejected for catalog parsing, now reborn one layer deeper.
3. **The catalog parser discarded the upstream's own entity evidence.** The fold observed `observed_effort_suffixes` and `observed_bare_id`, then *inferred* effort ladders from family prefixes instead of recording which concrete wire ids the account actually registered.

The architectural defect is a missing domain concept: **(canonical model id, requested effort) → wire entity id resolution is a typed, evidence-carrying domain fact**, not a per-provider string branch.

## Decision Drivers

- **Evidence over inference**: the upstream account surface is the only authoritative entity register; family-prefix guessing is a regression to ADR-0068's rejected alternative.
- **Mechanism over exhortation**: the resolution must live as typed data in the codec so a new tiered provider costs data rows, not new `match` arms in request builders.
- **Offline floor parity**: a session whose live catalog never ran must resolve identically to one that did — never degrade into a bare-id 404.
- **Pinned-depth passthrough**: an id that *is* itself a registered entity (`claude-opus-5-5-low`) is the user's explicit depth choice and routes verbatim, un-re-resolved.

## Architectural Decisions

### Subsystem Boundaries & Structural Changes

1. **Codec (`nuo_model_codec`) — typed decision mechanism.**
   - `DiscoveredModel::wire_entities: Option<Vec<String>>`: live-catalog **observation** of the concrete per-level entities the upstream registered, in shallow→deep advertisement order. Evidence, not policy — the parser records, it never infers a prefix (ADR-0068).
   - `RemoteModelMetadata::wire_entities`: persistence round-trip of the same evidence (serde, `skip_serializing_if` absent — no migration).
   - `ModelCapabilities::effort_entities: Vec<String>`: channel merge of baseline + remote layers.
   - [`ModelCapabilities::wire_entity_resolution(model_id) -> WireEntityResolution`]: the typed decision:
     - `Verbatim` — no entity evidence, or the channel id *is* a registered entity; stamp the canonical id as-is.
     - `PerLevel { base, entities }` — ranked entity list; `resolve_into(entities, effort)` stamps the id.
2. **Parser (`nuo-provider-google-antigravity::catalog`) — observation recorder.**
   - Records `observed_bare_id` (bare canonical row registered) and `observed_effort_suffixes` (suffixed rows).
   - Emits `wire_entities` = rank-ordered reconstruction `base+"-{suffix}"` of the rows it *saw* (`entity_rank`: `-tiered`/`-minimal` lead at in-band rank 0, then `-low < -medium < -high < -max`), with the bare id leading when the upstream registered it bare (in-band depth, `thinkingConfig`).
   - Zero observed variants ⇒ `wire_entities: None` — verbatim routing for legacy-tier rows that genuinely have no per-level entities.
   - The vendor ladder constants remain the *display* ladder (picker semantics); `wire_entities` is the *wire* truth. They may legitimately differ (argon displays four rungs, registers three entities).
3. **Provider request builder (`nuo-provider-google-antigravity`) — reads the decision, never re-derives.**
   - `effective_antigravity_wire_model(capabilities, model, effort)` resolves in strict priority: live observation → compiled baseline table → structural single-entity remaps → verbatim.
   - `BASELINE_WIRE_ENTITIES: &[(&str base, &str default_entity, &[&str ranked_entities])]` — the *offline floor*: `(base, default, ranked)` triples citing live-probed provenance; the table is **data**, not inference.
   - In-band-depth families (`gemini-3.8-flash`, whose first ranked entity *is* the default `-tiered`) broadcast the default for every effort — depth rides `thinkingConfig`.
   - Suffix-depth families rank `effort` across the per-level entities, clamped shallow.
   - Single-entity remaps (`claude-opus-4-6` → `-thinking`, `gpt-oss-120b` → `-medium`) are structural rows in the fn body, not ranked ladders.
4. **Composition root (`nuo-server::provider_registry`) — unchanged.** The typed carry is pure field plumbing through `Channel`/`RemoteModelMetadata`; no provider branches added.

### Normative Invariants Established

- `[INV-ENTITY-01] Evidence over inference` (mechanism: `catalog/tests::observes_wire_entities_from_live_rows` + `provider lib tests::antigravity_*`): the wire-entity evidence a request builder consumes must originate from an upstream observation (live catalog or a provenance-cited compiled table row). Family-prefix `starts_with`/`contains` matching that *fabricates* a wire id in a request builder is prohibited. Structural single-entity remaps are data rows, exempt.
- `[INV-ENTITY-02] Pinned entity passthrough` (mechanism: `WireEntityResolution::Verbatim` arm + tested): an id that is itself a registered wire entity routes verbatim for every effort. Resolution never rewrites an explicit per-level choice.

## Consequences

- **Positive**: the live failure mode (bare-id 404) is eliminated for the 5-5, argon, and any future tiered family; a new tiered provider is a parser that records + a table row / remote snapshot — zero Rust branches in request builders; the offline floor is behaviorally identical to the live surface; 63+ tests green across the codec/provider/catalog/server surface.
- **Negative & trade-offs**: `RemoteModelMetadata` and `DiscoveredModel` grow two optional fields (serde-transparent, absent-skip; no persistence migration); the compiled baseline table duplicates facts the live snapshot would otherwise supply — mitigated by provenance-cited rows and live-observation primacy.

## Rejected Alternatives & Negative Knowledge

### Alternative A (Rejected): Interim per-provider string-match ladder
- **Why considered**: smallest diff to stop the user-visible 404.
- **Why rejected**: re-binds the exact ADR-0068 "continual generational blacklisting" anti-pattern one layer deeper into the request path; every unmapped tiered family re-fails with the same 404; test expectations encoded the mismatch instead of the wire truth.

### Alternative B (Rejected): Effort-only in-band depth for all families
- **Why considered**: `thinkingConfig.thinkingLevel` would let the bare id route everywhere with depth in-band (Claude `thinking` params via Vertex envelopes).
- **Why rejected**: contradicts live wire truth — argon and 5-5 404 on the bare base id regardless of body shape; the effort surface for those families *is* the entity suffix. Gemini 3.8 is the family where in-band applies, and it is expressed as data (broadcast family) not as a universal rule.

### Alternative C (Rejected): Catalog emits per-level rows as separate pickable models
- **Why considered**: zero request-builder change — users pick `claude-opus-5-5-high` directly.
- **Why rejected**: pollutes the model picker with effort duplication (ADR-0068 [INV-CATALOG-02] explicitly folds parameter variants out of identity); breaks the orthogonal effort selector's mental model; curated agent surfaces would leak `-low`..`-max` rows as if they were distinct models.

## Links

- Amends: [ADR-0068](0068-canonical-model-catalog-normalization-and-semantic-lifting.md) (catalog folding: this ADR extends fold semantics with entity evidence carry)
- Related: [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md) (single-source provider definition), ADR-0065 (open effort ladders), Wire truth reference: `docs/reference/antigravity.yml` (StreamGenerateContentEnvelope.model)
