---
id: ADR-0086
title: "Dual-Lane Inference Wire Topology (Generative and Discriminative) and `nuo-decision` Retirement"
status: accepted
date: 2026-11-23
scope: codec/nuo-inference-wire, protocol/nuo-protocol, crate/nuo-decision
superseded_by: null
negative_knowledge: true
---

# 0086. Dual-Lane Inference Wire Topology (Generative and Discriminative) and `nuo-decision` Retirement

- Status: Accepted
- Date: 2026-11-23
- Deciders: Nuo Architecture Working Group
- Consulted: Provider Integration, Agent Runtime, Protocol Governance
- Amends: [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md) (modifies Stage S2 dedicated crate allocation into in-crate dual-lane isolation)
- Amends: [ADR-0066](0066-canonical-protocol-nomenclature-and-codec-purity.md) (reinforces East-West protocol decoupling from North-South discriminative engines)

---

## Context and Problem Statement

During the execution of [ADR-0084](0084-model-aperture-shrink-to-pure-codec-and-rename-to-nuo-inference-wire.md), the monolithic `nuo-model-codec` was purged and renamed to `nuo-inference-wire`. In Stage S2 of that ADR, the non-generative System One evaluation engine (TypeSafe Jev) was moved out into a dedicated crate `crates/nuo-decision` to preserve the wire codec aperture `[INV-AP-01]`.

Subsequent architectural auditing revealed critical structural anomalies and epistemic debt:

1. **Hypernym Hijacking & Semantic Leakage**:
   The name `nuo-inference-wire` usurped the overarching category "Inference" (which mathematically and operationally encompasses both generative completions and discriminative probability scoring), but the crate solely housed generative chat/streaming protocols (OpenAI, Anthropic, Google). Non-generative models like TypeSafe Jev—which are undeniably model inference—were exiled into `nuo-decision`, creating a conceptual category mismatch and cognitive friction.

2. **Severe Nomenclature Ambiguity ("Decision" Overload)**:
   The identifier `decision` was overloaded across four disjoint contexts:
   - Human permission decisions (`nuo-harness::PermissionDecision` / `HookDecision`);
   - Architectural Decision Records (`docs/adr/`);
   - Agent cognitive action planning (`nuo-agent::CognitiveLoop`);
   - Proprietary vendor probability scoring (`TypeSafe Jev`).
   The crate `nuo-decision` did not make decisions; it evaluated contextual states and emitted calibrated probabilities (`Noul`), categorical distributions (`Choice`), and ordinal rubric rankings (`Score`). The name was an unvetted vendor marketing term rather than a technical mechanism.

3. **Protocol Passthrough Pollution**:
   `crates/nuo-protocol` (the East-West IPC contract) depended on and re-exported `nuo-decision` (`pub mod decision; pub use decision::*;`). This directly violated the architectural red-line against intermediate passthrough re-export conduits ([ADR-0066](0066-canonical-protocol-nomenclature-and-codec-purity.md), `architecture.md` §2.3) by linking a North-South HTTP client and external network types into the IPC layer.

4. **Zero Production Consumers**:
   Neither `nuo-agent`, `nuo-harness`, `nuo-server`, `nuo-client`, `nuo-tui`, nor any of the 15+ provider crates consumed `nuo-decision`. It was an orphaned, untracked crate compiling dead code.

---

## Decision Drivers

- **Driver 1: Semantic Integrity**: Model inference includes both generative text/thinking completion and discriminative state/probability evaluation. Both belong inside `nuo-inference-wire`.
- **Driver 2: Zero Blast Radius with Symmetrical Lanes**: Generative and discriminative modalities must be physically separated into explicit submodules (`src/generative/` and `src/discriminative/`), sharing the common transport substrate (`WireClient`, `Endpoint`, `CredentialsProvider`, `WireError`) without compromising either modality's type system or breaking existing call sites.
- **Driver 3: Uncompromising Clean Break from Legacy Burden**: Eliminate `crates/nuo-decision`, purge all passthrough shims in `nuo-protocol`, and upgrade all types from ambiguous `Decision*` to domain-accurate `Discriminative*` / `Question` / `Answer`.

---

## Architectural Decisions

### 1. Dual-Lane Modality Separation in `nuo-inference-wire`

`crates/nuo-inference-wire` is structured into two non-intersecting operational lanes over a shared transport substrate:

```text
crates/nuo-inference-wire/src/
├── generative/                <-- System Two: Generative Chat, Messages, Thinking & Streams
│   ├── mod.rs                 (exports generative types, protocols, stream, and execute helper)
│   ├── types.rs               (WireRequest, WireResponse, ContentBlock, WireRole...)
│   ├── stream.rs              (SseDecoder, WireChunk, parse_vendor_chunk...)
│   ├── protocol/              (openai_chat_completions, openai_responses, anthropic_messages,
│                               google_generate_content, google_interactions)
│   ├── cache.rs               (PromptCacheMode, CacheRetention, cache_plan)
│   ├── effort.rs              (EffortLevel, Effort, COMMON_LADDER)
│   ├── instructions.rs        (InstructionBundle, InstructionSlice, InstructionTier)
│   ├── json.rs                (find_balanced_object streaming tool argument balancer)
│   ├── loop_detector.rs       (StreamLoopDetector, DegeneratePattern)
│   ├── reasoning.rs           (ReasoningMode, ReasoningSupport)
│   ├── token_classification.rs(TokenClass, TokenBreakdown)
│   ├── wire_protocol.rs       (WireProtocol enum of generative chat dialects)
│   └── wire_surface.rs        (AgentChatSpec, ModelBinding, ModelCarrier)
│
├── discriminative/            <-- System One: Discriminative States, Probabilities & Rubrics
│   ├── mod.rs                 (execute helper over shared WireClient)
│   ├── endpoint.rs            (DiscriminativeEndpoint, DiscriminativeProtocol)
│   ├── types.rs               (DiscriminativeRequest, DiscriminativeResponse, Question, Answer,
│                               NoulQuestion, ChoiceQuestion, ScoreQuestion)
│   └── protocol/
│       ├── mod.rs             (build_request, parse_response dispatch)
│       └── typesafe.rs        (TypeSafe Jev System One wire protocol)
│
├── client.rs                  <-- Shared pooled transport: WireClient (executes both lanes)
├── credentials.rs             <-- Shared authentication sources (StaticApiKey, DynamicCredentials)
├── endpoint.rs                <-- Shared physical endpoint metadata (telemetry, timings, vendor)
├── error.rs                   <-- Unified WireError
├── model.rs                   <-- Catalog model metadata and baseline resolution
├── model_providers.rs         <-- Built-in baseline model registries
├── auth.rs / connection_*     <-- Connection & authentication descriptors
└── lib.rs                     <-- Dual-lane module exposure and root facade
```

### 2. Ergonomic Root Façade for Zero Blast Radius

To prevent mass churn across 180+ downstream call sites in providers and runtime crates, `crates/nuo-inference-wire/src/lib.rs`:
- Declares both modalities: `pub mod generative;` and `pub mod discriminative;`.
- Re-exports the primary generative types and protocol namespaces at the crate root (`pub use generative::{...}; pub use generative::*;`).
- Existing imports (e.g. `use nuo_inference_wire::WireRequest`) continue to compile without disruption, while discriminative callers import explicitly via `use nuo_inference_wire::discriminative::*`.

### 3. Complete Retirement of `crates/nuo-decision` and Protocol Decoupling

- `crates/nuo-decision/` is excised from the workspace and deleted.
- `Cargo.toml` workspace member and dependency entries for `nuo-decision` are removed.
- `crates/nuo-protocol` removes the `nuo-decision` dependency, deletes `src/decision.rs`, and purges `pub mod decision;` from `src/lib.rs`. East-West IPC remains strictly decoupled from North-South inference engines.

### 4. Nomenclature Refinement: Abolishing "Decision" and Zero Hedging Aliases

Types are migrated to unambiguous domain nomenclature without keeping hedging legacy aliases:
- `DecisionRequest` $\to$ `DiscriminativeRequest`
- `DecisionResponse` $\to$ `DiscriminativeResponse`
- `DecisionQuestion` $\to$ `Question` (no `Probabilistic*` or `DiscriminativeQuestion` aliases)
- `DecisionAnswer` $\to$ `Answer` (no `Probabilistic*` or `DiscriminativeAnswer` aliases)
- `DecisionUsage` $\to$ `DiscriminativeUsage`
- `DecisionEndpoint` $\to$ `DiscriminativeEndpoint`
- `DecisionProtocol` $\to$ `DiscriminativeProtocol`
- Execution: `WireClient` provides explicit symmetric APIs `execute_generative` / `execute_discriminative`, alongside lane-level functions `generative::execute` and `discriminative::execute`.

---

## Normative Invariants Established

- **`[INV-WIRE-01] Dual-Lane Modality Separation`**: `nuo-inference-wire` models generative (Chat/Streaming) and discriminative (Probabilistic/Scoring) as structurally independent, non-intersecting lanes sharing only transport substrate (connection pooling, credentials, endpoint telemetry). Neither lane may import or compromise the type domain of the other.
- **`[INV-WIRE-02] Protocol Purity`**: `nuo-protocol` (East-West IPC) shall contain zero dependencies on or re-exports of non-generative or discriminative external model engines.

---

## Negative Knowledge & Retrospective Record

- **Anti-Pattern: Dedicated Crate Inflation for Sub-500-Line Channel Dialects**:
  ADR-0084 S2 attempted to solve the monolithic drift of `nuo-model-codec` by creating a standalone crate `nuo-decision`. This was an anti-pattern: creating a separate crate for a single 300-line vendor dialect introduced workspace friction, required redundant forwarding facades in `nuo-protocol`, and fragmented the inference concept. Hosting distinct modalities within a single crate via explicit submodule lanes (`generative` / `discriminative`) achieves isolation without crate inflation.

- **Anti-Pattern: Marketing-Driven Domain Naming**:
  Naming a protocol crate after a third-party vendor's marketing tagline ("System One Decision Engine") created severe conceptual collisions with user permission gating and cognitive loop planning. Domain types must describe their technical computation (probability classification and score regression), not vendor branding.

---

## Verification Record

1. **Workspace Compilation**: `cargo check --workspace` passes clean with zero errors across all 40+ workspace crates.
2. **Unit & Protocol Regression**:
   - `cargo test -p nuo-inference-wire`: 98 tests pass (77 unit tests, 11 protocol tests, 3 transport tests, 7 discriminative fidelity tests).
   - `cargo test -p nuo-protocol`: 229 tests pass with zero legacy decision shims.
3. **Decoupling Assertion**: `grep -rn "nuo_decision" crates/` returns zero matches. `crates/nuo-protocol` has zero external network/client dependencies.
