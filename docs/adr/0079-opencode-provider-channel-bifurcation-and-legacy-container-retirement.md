---
id: ADR-0079
title: "OpenCode Provider Channel Bifurcation and Legacy Container Crate Retirement"
status: accepted
date: 2026-11-21
scope: providers/architecture, providers/nuo-provider-opencode-plan, providers/nuo-provider-opencode-zen, server/nuo-server
superseded_by: null
negative_knowledge: true
---

# 0079. OpenCode Provider Channel Bifurcation and Legacy Container Crate Retirement

- Status: Accepted
- Date: 2026-11-21
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Model Infrastructure, Server Platform Teams
- Informed: System Architects
- Amends: [ADR-0015](0015-dependency-inversion-architecture-for-model-providers.md), [ADR-0027](0027-provider-definition-single-source-and-adapters-retirement.md)
- Complements: [ADR-0014](0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)

---

## Context and Problem Statement

ADR-0015 established the Dependency Inversion Principle (DIP) for model providers and explicitly rejected the **Naive "One Crate per Corporate Brand" (Vendor-First Fallacy)**:

> "A vendor is a commercial company, not a technical service surface... Collapsing them into one vendor crate re-introduces internal branching and coupling. The classification unit is the concrete access channel/plan (`chatgpt-plan`, `commandcode-plan`, `opencode-plan`)."

Despite this architectural mandate, `providers/nuo-provider-opencode` remained as a legacy multi-surface container crate. Historically, this grouping was a pragmatic compromise:
1. OpenCode originally hosted an enterprise Console service surface (`console.rs`) requiring private `/console/api/config` parsing.
2. It required heavy OAuth2 device code flow implementation (`device.rs`, `oauth.rs`).
3. Plan and Zen surfaces shared client identity headers and nascent routing infrastructure.

However, recent clean-break refactorings dismantled these historical justifications:
- Commit `060956d` abolished OpenCode OAuth device authorization, deleting `device.rs` and `oauth.rs`.
- The enterprise Console provider was retired outright, deleting `console.rs`.

As a consequence:
- The remaining modules (`plan.rs` and `zen.rs`) shared **zero lines of code**: no shared data types, helper functions, or custom driver logic.
- `plan.rs` represented `opencode-plan` (OpenCode Go subscription, `/zen/go/v1` relay, session-injected headers, high-concurrency subscription pool).
- `zen.rs` represented `opencode-zen` (OpenCode Zen public relay, `/zen/v1`, bearer/public keyless models).
- The composition root in `crates/nuo-server/src/provider_registry.rs` was forced to introduce non-standard exports (`PLAN_SPEC`, `ZEN_SPEC`), breaking the uniform 1-to-1 symmetry where every provider crate exposes `pub const MODEL_PROVIDER_SPEC`.

Allowing this legacy umbrella crate to persist violated the core granularity and channel boundary directives of Nuo's concentric architecture.

---

## Decision Drivers

- **Channel as First-Class Unit (ADR-0015 Compliance)**: Integrations must be segregated by concrete access channel/plan rather than commercial vendor brands.
- **Architectural Granularity & Purity**: `opencode-plan` and `opencode-zen` possess distinct endpoints, authentication models, baseline registries, and lifecycle semantics.
- **Contract Uniformity**: Every provider channel crate must export a canonical `MODEL_PROVIDER_SPEC`, restoring symmetrical registration at the composition root.
- **Zero Legacy Burden**: Eliminate dead umbrella containers when their underlying shared implementations are eradicated.

---

## Architectural Decisions

### 1. Creation of Dedicated Channel Crates

We bifurcate `nuo-provider-opencode` into two focused, modern, leaf provider crates:

1. **`providers/nuo-provider-opencode-plan`**:
   - Owns the `opencode-plan` channel specification (`id: "opencode-plan"`).
   - Serves the OpenCode Go subscription relay surface (`https://opencode.ai/zen/go/v1`).
   - Maintains the compiled capability baseline and ladder definitions for subscription models.
   - Exports the canonical `pub const MODEL_PROVIDER_SPEC: ModelProviderSpec`.

2. **`providers/nuo-provider-opencode-zen`**:
   - Owns the `opencode-zen` channel specification (`id: "opencode-zen"`).
   - Serves the OpenCode Zen public/keyless relay surface (`https://opencode.ai/zen/v1`).
   - Maintains the curated baselines for Zen relay models (including promotional free-tier models).
   - Exports the canonical `pub const MODEL_PROVIDER_SPEC: ModelProviderSpec`.

### 2. Retirement and Purging of `nuo-provider-opencode`

The legacy crate `providers/nuo-provider-opencode` is deleted outright from the workspace and `Cargo.toml`. No forwarder, deprecated re-export, or transitional alias crate is preserved.

### 3. Restoration of Composition Root Symmetry

In `crates/nuo-server/src/provider_registry.rs`, non-standard spec accessors are replaced with canonical exports:

```rust
// Before (Asymmetrical):
nuo_provider_opencode::PLAN_SPEC,
nuo_provider_opencode::ZEN_SPEC,

// After (Symmetrical & Modern):
nuo_provider_opencode_plan::MODEL_PROVIDER_SPEC,
nuo_provider_opencode_zen::MODEL_PROVIDER_SPEC,
```

---

## Invariants & Behavioral Boundaries

- **`[INV-PROV-06] Canonical Provider Spec Export`**: Every channel crate in `providers/` MUST export its primary specification as `pub const MODEL_PROVIDER_SPEC: ModelProviderSpec`. Multi-spec container crates are strictly prohibited unless sharing a substantive in-crate runtime execution engine (e.g. `nuo-provider-google-antigravity` sharing `GoogleProvider`).
- **`[INV-PROV-07] Channel Boundary Segregation`**: Subscription access channels (designated with `-plan`) and public or BYOK relay channels MUST NOT be bundled into a single vendor container crate.

---

## Negative Knowledge & Rejected Alternatives

### 1. Retaining `nuo-provider-opencode` as an Umbrella Crate
- **Why considered**: Avoided adding another crate directory and `Cargo.toml` to the workspace.
- **Why rejected**: Perpetuated the "Vendor-First Fallacy". Because `plan.rs` and `zen.rs` shared zero lines of executable code or types, bundling them was cosmetic and misleading, confusing developers about channel boundaries.

### 2. Feature-Flagged Surfaces in a Single Crate
- **Why considered**: Enabling `plan` or `zen` via Cargo features on `nuo-provider-opencode`.
- **Why rejected**: Introduced artificial feature-flag complexity for static specifications. In Cargo workspaces, feature unification easily causes unintended cross-leakage, whereas separate crates enforce hermetic boundary firewalls at compile time.
