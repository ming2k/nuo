---
id: ADR-0081
title: "Unidirectional Catalog Pipeline, Structural Display Normalization, and Relay Noise Hygiene"
status: accepted
date: 2026-10-19
scope: codec/nuo-model-codec, server/nuo-server, provider/nuo-provider-catalog, providers/nuo-provider-opencode-plan, providers/nuo-provider-opencode-zen
superseded_by: null
negative_knowledge: true
---

# 0081. Unidirectional Catalog Pipeline, Structural Display Normalization, and Relay Noise Hygiene

- Status: Accepted
- Date: 2026-10-19
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Agent Runtime, Presentation/TUI, Model Infrastructure
- Informed: Core Engineering
- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0068](0068-canonical-model-catalog-normalization-and-semantic-lifting.md), [ADR-0079](0079-opencode-provider-channel-bifurcation-and-legacy-container-retirement.md)
- Complements: [ADR-0074](0074-observed-wire-entity-resolution-and-evidence-driven-per-level-model-routing.md), [ADR-0080](0080-time-varying-rate-cards-and-off-peak-rating.md)

---

## Context and Problem Statement

Nuo supports both native direct providers (OpenAI, Anthropic, DeepSeek) and multi-model subscription relays (OpenCode Plan, OpenCode Zen, OpenRouter, CommandCode). In multi-model relays, the remote catalog endpoint (`/models`) operates as a bare identifier dictionary:

1. **Information-Deficient Remote Catalogs**:
   Endpoints such as OpenCode Go (`/zen/go/v1/models`) and Zen (`/zen/v1/models`) publish raw IDs without human-readable `name`, context window, or reasoning effort metadata.
2. **Presentation Degradation & Hyphen Pollution**:
   Historically, an absent remote name fell back directly to the raw wire ID (e.g. `deepseek-v4.1-flash`), littering terminal pickers with machine tokens rather than polished typography (`DeepSeek V4.1 Flash`).
3. **The Anti-Pattern of Per-Model Hardcoded Dictionaries**:
   Attempts to map IDs via brittle manual dictionaries (e.g. `{ "deepseek-v4.1-flash": "DeepSeek V4.1 Flash" }`) create continuous maintenance debt: every upstream model release requires an emergency release/patch to keep the client from degrading.
4. **Relay Ephemeral Noise Intrusion**:
   Relays conflate production models with ephemeral promotional freebies (`*-free`), partner-sponsored contributor nodes (`*-contributor`), and unstable staging tags (`*-preview`, `alpha-*`). Iterating blindly over the raw endpoint floods user selection surfaces with noisy, short-lived models that lack capability baselines.
5. **Dangers of Speculative Heuristics**:
   Attempting to guess invocation capabilities (such as blind-injecting `reasoning_effort` or assuming 1M context based on string prefixes) introduces high failure rates, causing upstream HTTP 400 Bad Request errors on unverified model variants.

---

## Decision Drivers

1. **Source-Agnostic Ingestion (`[INV-CATALOG-04]`)**: Catalog ingestion must treat input origins (remote HTTP, static compile-time baseline, curated seeds) as pure data providers without coupling ingestion logic to transport mechanics.
2. **Rule-Based Structural Normalization (`[INV-CATALOG-05]`)**: Replace brittle per-model dictionaries with structural tokenization and lexicon normalization. The normalization pipeline must deterministically lift raw IDs (`deepseek-v4.1-flash`) into polished names (`DeepSeek V4.1 Flash`) with zero per-model code additions.
3. **Relay Noise Hygiene (`[INV-CATALOG-06]`)**: Ephemeral promotional models and contributor variants must be structurally filtered from primary selection surfaces by default, preserving a clean professional catalog.
4. **Sovereign User Overrides Primacy**: The user's explicit configuration (`connection.models.overrides`, `connection.models.include`) must remain the supreme, uncompromised authority across all stages.
5. **Clean Epistemic Lineage**: Purge legacy external ADR references (`ADR-0199`) from code comments and documentation.

---

## Architectural Decisions

### 1. [INV-CATALOG-04] Three-Stage Unidirectional Catalog Pipeline
The model catalog ingestion and resolution pipeline is formally codified into three strictly decoupled, unidirectional stages:

```text
┌────────────────────────────────────────────────────────┐
│  Stage 1: Source (Source-Agnostic Raw Ingestion)       │
│  - Remote Endpoint, Static Baseline, or Seed List      │
│  - Emits immutable collection of raw identifiers       │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼
┌────────────────────────────────────────────────────────┐
│  Stage 2: Mapping & Normalization (Canonical Lifting)  │
│  - Structural display name normalization (hyphen-free) │
│  - Evidence-backed baseline binding (verified models)  │
│  - Safe conservative fallback floor (unverified models)│
│  - Structural relay noise hygiene filtering            │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼
┌────────────────────────────────────────────────────────┐
│  Stage 3: Overwrite (Sovereign User Overrides)         │
│  - Connection models.overrides & models.include apply  │
│  - User declarations unconditionally beat stages 1 & 2 │
└────────────────────────────────────────────────────────┘
```

### 2. [INV-CATALOG-05] Rule-Based Structural Display Normalization
Abolish manual per-model name dictionaries. In their place, `nuo-model-codec::catalog` provides deterministic structural normalization, and each provider spec declares its `DisplayPolicy` (ADR-0081) so the *surface*, never the id string shape, decides which normalization applies:
- `DisplayPolicy::Native` — a curated platform publishes real names; a published label is trusted verbatim (still stripped of `(Thinking)` annotations), and an absent label leaves the surfaces on the wire id.
- `DisplayPolicy::Structural` — an aggregation relay publishes bare ids, so every label is lifted structurally.

Structural normalization consists of:
- **Tokenization**: Tokenizes hyphenated identifiers into constituent semantic tokens.
- **Lexicon Casing**: Matches tokens against canonical brand dictionaries (`DeepSeek`, `GLM`, `Qwen`, `Claude`, `GPT`, `MiMo`, `MiniMax`, `Kimi`, `Gemini`, `Grok`, `Mistral`, `Llama`) and tier keywords (`Flash`, `Pro`, `Max`, `Plus`, `Lite`, `Turbo`, `Vision`, `Code`, `Chat`, `Reasoner`, `Haiku`, `Sonnet`, `Opus`).
- **Version Preserving**: Identifies version patterns (`V4.1`, `V4`, `K3`, `M3`) and formats adjacent numeric version tokens (e.g., `5-5` ➔ `5.5`, `4-6` ➔ `4.6`).
- **Separator Elevation**: Folds inter-token hyphens to natural spaces (`deepseek-v4.1-flash` ➔ `DeepSeek V4.1 Flash`).

The mapping is applied exactly once, inside the catalog derivation boundary (`nuo-server`/`nuo-harness` `catalog::derive_channel`), and written onto the route's resolved label. Server projections and clients consume that resolved label and never re-derive it, so a relay's typography can never diverge between surfaces.

### 3. [INV-CATALOG-06] Relay Noise Hygiene
Multi-model relay catalogs filter ephemeral, unstable, and promotional identifiers through the `NamedFilterPolicy::Curated` pipe valve, applied by default to surfaces that declare `DisplayPolicy::Structural`. Identifiers satisfying any of the following structural predicates are pruned from default display surfaces:
- Ending with `-free` (ephemeral promotional tier);
- Ending with `-preview` (unstable staging snapshot);
- Ending with `-contributor` (third-party sponsored relay node);
- Starting with `alpha-` or ending with `-alpha`;
- Literally `test`.

The predicate is a small set of **structural markers**, never a per-id blacklist
of specific products. A vendor's featured production model with a published quota
(`space-bunny`) is NOT noise: an ad-hoc exception list is exactly the brittle
generational blacklist ADR-0068 abolished, and it would silently hide a real
model the day the relay ships one.

**Sovereign Bypass**: The connection's `models.include` list is injected after the filter and therefore bypasses it unconditionally, so an operator who wants a promotional or preview model keeps full control.

---

## Consequences

### Positive Consequences
- **Zero Maintenance Lag**: New models released on upstream relays (e.g. `deepseek-v5-pro` or `qwen3.9-flash`) immediately render with formatted display names (`DeepSeek V5 Pro`, `Qwen 3.9 Flash`) without requiring Nuo code changes.
- **Noise-Free Pickers**: Users of `opencode-plan` and `opencode-zen` see only production-grade, capability-backed engineering models.
- **Deterministic Reliability**: No speculative parameter guessing; unverified models default safely without risking upstream HTTP 400 rejections.
- **Sovereign Escape Hatch**: Advanced operators can immediately activate and override capabilities for any new or unverified model via local configuration.

### Negative Knowledge & Prohibitions
- **Prohibition on Speculative Capability Synthesis**: Request builders and normalizers MUST NOT guess execution parameters (such as `reasoning_effort` or vision support) based purely on model name substrings when unverified.
- **Prohibition on Hardcoded Model Dictionaries**: New models MUST NOT be integrated by hardcoding ad-hoc display-name lookup tables.
- **Legacy Comment Clean Break**: References to legacy `ADR-0199` are eradicated from the codebase.
