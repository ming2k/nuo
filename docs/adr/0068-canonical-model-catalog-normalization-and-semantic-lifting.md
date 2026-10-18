---
id: ADR-0068
title: "Canonical Model Catalog Ingestion, Semantic Lifting, and Orthogonal Capability Normalization Topology"
status: accepted
date: 2026-11-06
scope: provider/nuo-provider-catalog, provider/nuo-provider-google-antigravity, server/nuo-server, harness/nuo-harness
superseded_by: null
negative_knowledge: true
---

# 0068. Canonical Model Catalog Ingestion, Semantic Lifting, and Orthogonal Capability Normalization Topology

- Status: Accepted
- Date: 2026-11-06
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Server Runtime, TUI, and Model Codec Teams
- Informed: Core Engineering
- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md), [ADR-0065](0065-multi-factor-pricing-spec-dual-mode-connection-budget-and-spend-circuit-breaker.md)

---

## Context and Problem Statement

Remote model discovery endpoints across heterogeneous LLM providers (most notably Google Antigravity CloudCode `/v1internal:fetchAvailableModels`, OpenAI/compatible `/v1/models`, and multi-tenant aggregators) expose uncurated technical dictionaries. These endpoints frequently:

1. **Conflate Model Identity with Invocation Parameters**:
   Internal wire variants (e.g. `gemini-3.8-flash-high`, `gemini-3.8-flash-medium`, `gemini-3.8-flash-low`, `gemini-3.8-flash-minimal`, `gemini-3.8-flash-tiered`) are dumped into the model catalog as if they were independent base models.
2. **Conflate Dedicated Sub-Task Lanes with General Chat Models**:
   Endpoints expose internal command, commit-message, image-generation, and telemetry lane models (e.g. `gemini-3-flash`, `gemini-3.5-flash-lite`, `chat_*`, `tab_*`, `models/proactive-*`) alongside general-purpose SWE agent models.
3. **Ignore First-Class Upstream Curation Structures**:
   Upstream responses often explicitly delineate curated agent surfaces (e.g. `agentModelSorts` and `defaultAgentModelId`), but parsers historically bypassed these structures and iterated blindly over raw, uncurated definition dictionaries.
4. **Accumulate Brittle Blacklist Debt**:
   Historical implementations accumulated ad-hoc string-prefix blacklists (e.g. `starts_with("gemini-3.6-flash")`) to suppress older duplicates. When providers bumped generations (e.g. from 3.6 to 3.8), the hardcoded blacklist failed, causing all parameterized variations to leak into the UI and picker.

This created conceptual collision: Nuo's user interface and request builders already support orthogonal reasoning depth controls (`effort_levels`), yet the catalog forced users to choose between duplicate model identities representing those same parameter rungs.

---

## Decision Drivers

1. **Zero Legacy String Heuristics**: Abolish version-locked string prefixes (e.g. `starts_with("gemini-3.6-flash")`). Catalog ingestion must be generic, parameter-aware, and forward-compatible.
2. **Curated Agent Surface Primacy**: When an upstream response provides explicit agent curation structures, the parser must honor that curated ordering rather than flattening uncurated backend dictionaries.
3. **Orthogonal Capability Lifting**: Model identity and runtime execution parameters (such as reasoning effort / thinking budget) must be strictly separated. Parameterized wire IDs must be folded into canonical models, with implied parameters lifted to capability metadata.
4. **Concentric Boundary Cleanliness**: Normalization and semantic lifting belong strictly inside the provider catalog discovery boundary (`nuo-provider-catalog`). The server core and TUI must only observe clean, canonical models.

---

## Architectural Decisions

### 1. [INV-CATALOG-01] Curated Agent Surface Primacy
When an upstream catalog response provides explicit agent curation hierarchies (such as Google Antigravity's `agentModelSorts` and `defaultAgentModelId`), the catalog ingestion pipeline MUST prioritize and preserve this curated view. Dedicated lane models (`commandModelIds`, `commitMessageModelIds`, `imageGenerationModelIds`, `audioTranscriptionModelIds`) and internal helpers (`chat_*`, `tab_*`, `models/*`) MUST NOT be surfaced as standalone general agent models.

### 2. [INV-CATALOG-02] Orthogonal Capability Lifting & Parameter Suffix Folding
Model identity and runtime execution parameters MUST NOT be conflated into model names. Parsers MUST fold parameterized wire ID variations (`-high`, `-medium`, `-low`, `-minimal`, `-tiered`, `-thinking`) into canonical base model IDs, and strip execution mode / thinking annotations (`(Thinking)`, `(thinking)`) from human display names. For models exhibiting reasoning support, the parser MUST lift parameter rungs into `DiscoveredModel::effort_levels` (e.g., `[minimal, low, medium, high]` for Gemini 3.x/4.x, `[low, medium, high, max]` for Claude), allowing users to control reasoning depth orthogonally.

### 3. [INV-CATALOG-03] Abolition of Legacy Version Blacklists
Hardcoded generational blacklists (such as `starts_with("gemini-3.6-flash")`) are permanently abolished. Filtering and folding MUST operate structurally on:
- Formal deprecation maps (`deprecatedModelIds`);
- Upstream dedicated lane enumerations (`commandModelIds`, etc.);
- Generic parameter suffix patterns (`-high`, `-medium`, `-low`, `-minimal`, `-max`, `-tiered`, `-thinking`);
- Disabled account entitlement flags (`-disabled`).

### 4. [INV-CATALOG-04] Provider-Owned Dialect Ingestion & Zero-Central-Leakage
Per ADR-0027 §2, `nuo-provider-catalog` is a shape-keyed dispatch engine, not a vendor junk drawer. Provider-specific response dialect parsing (such as `CatalogShape::GoogleCloudCode` for Google Antigravity, alongside `OpencodeConsole` and `SceneMap`) MUST be owned by and encapsulated within the respective `providers/nuo-provider-*` crate (`nuo-provider-google-antigravity::catalog`). Inlining vendor-specific AST inspection into `list_models.rs` is strictly prohibited.

---

## Consequences

### Positive
- **Clean Mental Model**: Users select canonical models (e.g. `gemini-3.8-flash`, `gemini-4-argon`), and adjust reasoning depth via the orthogonal effort selector.
- **Forward Compatibility**: Unreleased or future model generations (e.g. `gemini-3.9-flash`, `gemini-4.5-pro`) will automatically have their parameter variants folded and effort ladders lifted without requiring manual code modifications.
- **Zero UI Pollution**: Internal lane helpers and dedicated specialized endpoints no longer clutter the model picker.

### Negative & Trade-offs
- Internal request builders must ensure that if an upstream provider requires parameterized wire routing, the mapping from `(canonical_model, effort_level) -> wire_lane` is handled transparently in the provider transport layer.

---

## Rejected Alternatives & Negative Knowledge

- **UI-Level Regex Deduplication**: Rejected. Filtering or deduplicating model names in `nuo-tui` violates Server Purity and concentric layering. Presentation layers must receive already-normalized models over `nuo-protocol`.
- **Continual Generational Blacklisting**: Updating `starts_with("gemini-3.6-flash")` to `starts_with("gemini-3.8-flash")` was categorically rejected. This represents technical debt smearing and guarantees future regression upon the next upstream release.
