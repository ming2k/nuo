---
id: ADR-0130
title: "Model Guidance: Dynamic Prompt Engineering, Pattern-Based Behavioral Demarcation, and Provider Purity"
status: accepted
date: 2026-10-30
scope: persistence/nuo-persistence, harness/nuo-harness, server/nuo-server, protocol/nuo-protocol
superseded_by: null
negative_knowledge: true
---

# 0130. Model Guidance: Dynamic Prompt Engineering, Pattern-Based Behavioral Demarcation, and Provider Purity

- Status: Accepted
- Date: 2026-10-30
- Deciders: Core Engineering, Architecture Working Group
- Amends: [ADR-0052](0052-unified-instruction-pipeline-pure-role-topology-and-end-to-end-cache-fidelity.md) §1 (Canonical Slice Taxonomy: `system.model_guidance`), [ADR-0126](0126-provider-owned-model-tunables-closed-shape-vocabulary-over-open-knob-namespace.md) (Carrier of per-route settings)

---

## Context and Problem Statement

Language models exhibit divergent dialectal habits, verbosity tendencies, and alignment quirks. For example, DeepSeek models frequently lean into bureaucratic Chinese cliches ("综上所述", "深入剖析", "极致优雅") and may generate unprompted pseudo-tool markers in conversational text unless explicitly nudged with prompt engineering.

Historically, Nuo reserved a stub field on its static compiled `Model` metadata struct:
```rust
pub struct Model {
    ...
    pub model_guidance: &'static str,
}
```
In real-world multi-provider operations, this legacy design suffered from three critical architectural failures:

1. **The Global Canonical Model Fallacy**:
   There is no single global registry for model identifiers. Upstream API platforms, relay gateways, and local backends use wildly inconsistent identifier strings for the same model family:
   - DeepSeek Official: `deepseek-flash`, `deepseek-v4-pro`
   - OpenRouter: `deepseek/deepseek-chat`, `deepseek/deepseek-r1:free`
   - SiliconFlow: `deepseek-ai/DeepSeek-V3`
   - Local Ollama: `deepseek-r1:14b`, `my-deepseek-q4`
   A hardcoded, static `&'static str` on compiled `Model` entries cannot address relay prefixes, user-injected models, or local deployments.

2. **Cross-Model Prompt Pollution on Provider Instances**:
   Binding model guidance to a Provider instance (e.g. an OpenRouter or private proxy connection) contaminates heterogeneous endpoints. An instruction written to suppress DeepSeek cliches would be sent unconditionally to Claude 3.7 or Llama 3.3 when routed through the same gateway, wasting tokens and degrading performance on models that do not share DeepSeek's quirks.

3. **Dead Compiled Scaffolding Without User Interfaces**:
   Every baseline model in `providers/*` compiled `model_guidance: ""` into the binary. Users had no native mechanism to configure, override, or tune behavioral prompts for specific models or families without recompiling the framework. This ADR cleanly **purges** the `Model.model_guidance: &'static str` static carrier entirely — a hard break with no migration shim.

---

## Decision Drivers

- **Pure Cognitive Demarcation (`[INV-GUIDE-01]`)**: Behavioral prompt adjustments belong to the model cognition domain, not provider transport. Model guidance MUST NOT be bound to Provider connection instances.
- **Hierarchical Resolution Cascade (`[INV-GUIDE-02]`)**: Guidance resolution must resolve gracefully without brittle global model assumptions, cascading through:
  `Route Override > Exact Model ID > Glob Pattern > Model Family > Baseline Default`.
- **Cache Fidelity & Base Tier Placement (`[INV-GUIDE-03]`)**: System guidance must reside in `InstructionTier::Base` directly following identity tone, maximizing prefix cache hit ratios on platforms supporting prompt caching (e.g. DeepSeek, Anthropic, Gemini).
- **Subagent Guidance Inheritance (`[INV-GUIDE-04]`)**: Subagents spawned by an agent on the same model must inherit the parent's resolved guidance handle without separate reconfiguration.

---

## Considered Options

- **Option 1: Hardcoded per-model strings in provider crates**. Keep `Model.model_guidance: &'static str` and write bespoke guidance strings into Rust code. Rejected: inflexible, impossible for users to customize, and breaks on relays/OpenRouter.
- **Option 2: Provider-instance level prompt binding**. Add `system_guidance` to connection configuration. Rejected: contaminates multi-model gateways (OpenRouter, OneAPI) and causes configuration duplication across duplicate providers.
- **Option 3: User-Configured Model Guidance with Glob Cascading (Chosen)**. Declarative `[model_guidance]` configuration in `config.toml` supporting glob patterns (e.g. `"*deepseek*"`) with exact-id and route-level overrides.

---

## Decision Outcome

Chosen Option: **Option 3: User-Configured Model Guidance with Glob Cascading**.

### 1. Configuration Specification

Users declare model guidance in `config.toml` (or `agent.toml`):

```toml
[model_guidance]
# Glob pattern matching all variants and relays of deepseek
"*deepseek*" = """
- 请使用自然、平实、接地气的现代技术中文交流，严禁使用公文腔和陈词滥调。
- 直奔主题，给出结论和核心代码，杜绝冗长空洞的修饰。
"""

# Exact model override
"deepseek-flash" = """
- 严格遵循平实语言风格。
- 采用极简回复模式。
"""

# Family fallback
"glm" = """
- 保持紧凑代码输出。
"""
```

### 2. Resolution Precedence

When preparing a request turn for model `M` with family `F` over route `R`:
1. **Route Explicit Override**: `RouteSettings.model_guidance` (per-instance, per-model custom setting).
2. **Exact Model ID Match**: `config.model_guidance.get(M)`.
3. **Glob Pattern Match on Model ID**: Patterns like `*deepseek*` matching against lowercased `M`.
4. **Exact Family Match**: `config.model_guidance.get(F)`.
5. **Glob Pattern Match on Family**: Patterns matching against lowercased `F`.
6. **Empty**: No `Model::model_guidance` static carrier exists (ADR-0130 purged it); an unmatched rule degrades to an empty string and the `system.model_guidance` slice folds out of the rendered prompt.

### 3. Runtime Integration

- `nuo-harness`: `SystemPromptContext.model_guidance` becomes an owned `String`. `Agent` stores an `Arc<RwLock<String>>` guidance handle.
- `nuo-server`: `agent_setup::reseed_model_guidance` evaluates the cascade at startup and on every connection/model switch, updating the agent's guidance in-place.
- `nuo-harness::subagent_tool`: Propagates `parent_model_guidance` handle to spawned subagents.

---

## Invariants

- **`[INV-GUIDE-01]` Pure Cognitive Demarcation**: Model guidance is keyed on model identities and family traits, never on provider transport instances.
- **`[INV-GUIDE-02]` Cascading Fallback**: Any unresolved pattern degrades cleanly to an empty string without failing request generation.
- **`[INV-GUIDE-03]` Base-Tier Cache Invariance**: Model guidance remains in `InstructionTier::Base`, ensuring identical prefix caching across conversational turns.
- **`[INV-GUIDE-04]` Delegation Inheritance**: Subagents inherit the active model guidance of their parent agent.
