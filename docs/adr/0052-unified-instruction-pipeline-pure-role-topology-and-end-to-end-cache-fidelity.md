---
id: ADR-0052
title: "Unified Instruction Pipeline, Pure Role Topology, and End-to-End Cache Fidelity"
status: accepted
date: 2026-10-26
scope: protocol/wire, model-codec/instructions, harness/system_prompt, providers/cache
superseded_by: null
negative_knowledge: true
---

# 0052. Unified Instruction Pipeline, Pure Role Topology, and End-to-End Cache Fidelity

- Status: Accepted
- Date: 2026-10-26
- Deciders: Nuo Architecture Working Group
- Consulted: Core Infrastructure, Model Transport, Agent Runtime, Documentation Governance
- Informed: System Architects, Platform Engineering
- Amends: [ADR-0006](0006-wire-contract-consolidation.md)
- Complements: [ADR-0009](0009-single-agent-runtime.md), [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)

---

## Context and Problem Statement

Nuo introduced `InstructionBundle` and `InstructionTier` to move beyond legacy monolithic system prompt strings toward structured, lifecycle-aware prompt composition. However, architectural inspection of real-world sessions and provider adapters revealed serious structural fractures, dual-track split-brain behavior, and cognitive latency:

1. **Dual-Track Transmission Split-Brain**:
   System-level directives simultaneously traveled through two disjoint channels: the structured `ModelRequest.instructions` (`InstructionBundle`) and residual `Role::System` messages inside `ModelRequest.messages`. Every provider transport adapter was forced to implement ad-hoc glue code extracting the bundle, flattening it, scanning the message vector for `Role::System`, and concatenating them with `\n\n`. This violated Single Source of Truth (SSOT).

2. **Split Compilation Authorities**:
   Two disjoint modules assembled `InstructionBundle` independently with differing taxonomy:
   - Dynamic agent turns assembled slices via `nuo-harness::SystemPromptRegistry` using registered `SystemPromptSection` entities.
   - Offline / replay analysis in `nuo-wire::session_ir::compiler::pass3_cache_boundary_analysis` manually pushed bespoke `InstructionSlice` items from `ir.policy.rules`, creating divergent naming conventions (`base_persona` vs `system.identity_preamble`).

3. **Downstream Cache Degradation (The False-Promise Abstraction)**:
   While `InstructionTier` defined four tiers (`Base`, `Session`, `Task`, `Ephemeral`) with clear volatility semantics, downstream provider transports uniformly called `instructions.render_combined()`. This collapsed all tier metadata into a flat string, throwing away structured cache breakpoints on providers natively supporting tiered blocks (e.g. Anthropic's block-level `cache_control`).

4. **Cold-Start Exploration Tax in Multi-Crate Workspaces**:
   In complex monorepos (such as Nuo itself with 15+ crates), absence of a deterministic workspace topology slice forced agents into 50–100 turns of trial-and-error text grepping across crate boundaries before making their first code edit.

We require a modernized, uncompromising, forward-looking instruction architecture that unifies the instruction pipeline into a single SSOT, enforces conversational role purity, delivers end-to-end cache fidelity across provider transports, and introduces deterministic workspace topology projection.

---

## Decision Drivers

- **Instruction Pipeline SSOT (`[INV-INST-01]`)**: System instructions must enter the model request exclusively through `InstructionBundle`. `Role::System` is prohibited within conversational message history at provider boundaries.
- **Canonical Compilation Authority (`[INV-INST-02]`)**: `InstructionBundle` assembly must follow unified semantic naming conventions across both runtime session dispatch and `SessionIR` compilation passes.
- **Provider Cache Fidelity (`[INV-INST-03]`)**: Providers supporting structured system instruction blocks (Anthropic block caching, OpenAI developer separation) must preserve tier boundaries on the wire rather than degrading blindly to flat string concatenation.
- **Deterministic Workspace Topology Projection (`[INV-INST-04]`)**: Multi-crate and monorepo sessions must project a compact, deterministic architecture summary into `InstructionTier::Session`, eliminating exploratory blind grepping.

---

## Considered Options

- **Option 1: Legacy Flat System String**: Abandon `InstructionBundle` and revert to a single flat `String` prompt.
- **Option 2: Implicit Dynamic Subgraph Injection (RAG-style Push)**: Dynamically inject code subgraphs into system prompt every turn based on keyword matching.
- **Option 3: Unified Pure Instruction Pipeline with Tier-Preserving Transports (Chosen)**: Enforce `InstructionBundle` as the sole authority, banish `Role::System` from conversation vectors, map tiers faithfully to provider cache protocols, and provide static workspace topology projection.

---

## Decision Outcome

Chosen Option: **Option 3: Unified Pure Instruction Pipeline with Tier-Preserving Transports**.

### 1. Invariant Ratification

#### `[INV-INST-01]` Pure Conversational Role Topology
- Inside `ModelRequest.messages`, permissible roles are strictly limited to conversational turns: `Role::User`, `Role::Assistant`, and `Role::Tool`.
- All operational guidance, personas, workspace rules, and boundaries reside solely within `ModelRequest.instructions: InstructionBundle`.
- Provider transports MUST reject or filter out `Role::System` inside the dialogue sequence, eliminating dual-track ambiguity.

#### `[INV-INST-02]` Canonical Slice Taxonomy
Instruction slices across both runtime registry and IR compilation share canonical IDs:
- `system.base_persona`: Core identity / persona (`InstructionTier::Base`)
- `system.project_rules`: Content-attested project directives (`InstructionTier::Session`)
- `system.workspace_topology`: Deterministic workspace crate/module DAG summary (`InstructionTier::Session`)
- `system.workspace_roots`: Permitted workspace roots (`InstructionTier::Session`)
- `system.task_directive`: Subagent task framing (`InstructionTier::Task`)
- `system.ephemeral_hint`: Transient turn-specific nudges (`InstructionTier::Ephemeral`)

#### `[INV-INST-03]` Tier-Preserving Provider Caching
- **Anthropic Provider**: Maps static tiers (`Base` and `Session`) to structured system blocks bearing `cache_control: {"type": "ephemeral"}`. Dynamic tiers (`Ephemeral`) are emitted as un-cached trailing blocks, maximizing KV-cache hit rate without cache thrashing.
- **OpenAI Responses Provider**: Distinguishes static instructions (`InstructionTier::Base` + `Session`) as top-level `instructions` and dynamic nudges as developer inputs.
- **Google / Generic Transports**: Groups static slices into stable prefixes to guarantee deterministic prefix hashing across requests.

#### `[INV-INST-04]` Workspace Topology Slice
A session initialized within a multi-member workspace automatically renders a compact, static topological summary of crate responsibilities and dependency DAG into `InstructionTier::Session`, providing immediate architectural orientation.

---

## Negative Knowledge & Rejected Alternatives

### 1. Why Not Legacy Flat System String?
- Reverting to a flat system string sacrifices KV-cache reuse across turns and sessions. Turn-dynamic nudges (such as ephemeral warnings) would invalidate multi-megabyte prompt caches on every turn, escalating latency and inference cost.

### 2. Why Not Implicit Dynamic Subgraph Injection (Push-RAG)?
- Injecting ad-hoc AST subgraphs into the system prompt per-turn creates severe non-determinism, blows up token budgets, and invalidates prefix caches continuously (cache thrashing).
- Context discovery must be pull-based (via explicit `code_query` / AST tools), anchored by static macro-level workspace topology (push-based).

### 3. Why Not Ad-Hoc Manual `AGENTS.md` Maintenance as the Sole Solution?
- Human-maintained architectural markdown suffers from drift, cognitive decay, and zero applicability to foreign codebases. Static analysis and crate metadata must supply structural facts deterministically.

---

## Architectural Consequences

### Positive
- **Elimination of Dual-Track Glue**: Provider adapters no longer parse or concatenate `Role::System` from conversation histories.
- **Predictable Prefix Caching**: Static tiers remain byte-identical across dialogue rounds, ensuring near-100% KV-cache reuse.
- **Radical Reduction in Cold-Start Exploration**: Agents orient themselves immediately within multi-crate monorepos via the workspace topology slice.

### Negative
- Existing tests that artificially planted `Message::new(Role::System, ...)` in conversation vectors must migrate to `InstructionBundle`.

---

## Compliance and Verification

- `cargo check --workspace` & `cargo test --workspace` pass without regressions.
