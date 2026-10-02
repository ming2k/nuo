---
id: ADR-0008
title: "Federated Cluster SemVer and Release Topology"
status: accepted
date: 2026-10-02
scope: workspace/versioning, release/topology, semver/governance
superseded_by: null
negative_knowledge: true
---

# 0008. Federated Cluster SemVer and Release Topology

- Status: Accepted
- Date: 2026-10-02
- Deciders: Nuo Architecture Working Group
- Consulted: Core Contributors, Release Engineering
- Informed: System Architects

---

## Context and Problem Statement

Following the ingestion of `nuotc` (ADR-0007) and substrate crates (`acp`, `nuo-tool`, `nuo-model-wire`, etc., ADR-0006), the Nuo monorepo encompasses 16 crates spanning distinct technical domains:
- Low-level, domain-free terminal rendering (`nuotc`).
- Open inter-agent communication protocols (`acp`).
- Application host daemons, semantic terminals, and agent execution engines (`nuo`, `nuox`, `nuo-contracts`, `nuo-harness`, etc.).

Historically, all workspace members simply inherited `version.workspace = true` (`0.1.0`). As the repository matures toward production distribution and independent crate reusability on `crates.io`, two anti-patterns emerge:
1. **The Monolithic Lockstep Anti-Pattern**: Forcing every crate—including domain-free graphics engines like `nuotc` and protocol specs like `acp`—to bump their versions whenever an upper application bug in `nuo` is patched. This pollutes external consumers' dependency trees and misleads the open-source ecosystem regarding stability.
2. **The Anarchic Independent Anti-Pattern**: Allowing every single in-house crate (`nuo-client`, `nuo-contracts`, `nuo-persistence`, `nuo-harness`, `nuo-providers`, etc.) to drift on completely independent SemVer numbers. This causes severe maintenance friction, manual changelog sprawl, and diamond dependency resolution failures during rapid host development.

We require a principled, long-term release and versioning topology that delivers both domain independence and cohesive product synchronization without developer friction.

---

## Decision Drivers

- **Domain Decoupling**: Substrates with zero AI vocabulary (`nuotc`) must progress according to their own API surface and graphic capabilities.
- **Protocol Stability**: Standard specifications (`acp`) must version according to protocol RFC revisions and cross-runtime wire backward compatibility.
- **Frictionless Application Co-Evolution**: The core Nuo host, terminal, and agent execution subsystems must evolve synchronously with zero version-matrix friction.
- **Publish-Ready Manifests**: All internal `[workspace.dependencies]` must declare explicit version constraints alongside filesystem paths so `cargo package` and `cargo publish` succeed without modification.

---

## Decision Outcome

We establish the **Federated Cluster SemVer** topology, partitioning the workspace into three distinct versioning clusters:

```text
┌────────────────────────────────────────────────────────┐
│  Cluster A: Independent Substrate Engine               │
│  • nuotc (Retained-mode 2D character canvas & diff)    │
│  ➜ SemVer: Autonomous (e.g. 0.0.1 -> 0.0.2 -> 0.1.0)   │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster B: Canonical Open Protocol Standard           │
│  • acp (Agent Coordination Protocol envelopes & state) │
│  ➜ SemVer: Protocol Specification Baseline             │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster C: Application & Subsystem Host Suite         │
│  • nuo, nuo-client, nuo-contracts, nuo-agent           │
│  • nuo-harness, nuo-persistence, nuo-providers         │
│  • nuo-host, nuo-tool, nuo-tool-derive, nuo-mcp        │
│  • nuo-model-codec                                     │
│  ➜ SemVer: Unified Lockstep (workspace.package.version)│
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster D: Autonomous Interactive Terminal Client     │
│  • nuox (Semantic terminal UI & headless runner)       │
│  ➜ SemVer: Autonomous Client Lifecycle (e.g. 0.0.1)   │
└────────────────────────────────────────────────────────┘
```

### 1. Concrete Manifest Implementations
- **`nuotc/Cargo.toml`**: Decoupled from `version.workspace`. Declares its own `version = "0.0.1"`, autonomous categories, keywords, and release metadata.
- **`acp/Cargo.toml`**: Decoupled from `version.workspace`. Declares its own `version = "0.0.1"` anchored to the ACP specification.
- **`nuox/Cargo.toml`**: Decoupled from `version.workspace`. Declares its own `version = "0.0.1"`, allowing the presentation client to evolve independently across protocol windows.
- **Cluster C Crates**: Retain `version.workspace = true`, driven synchronously by `[workspace.package].version` in root `Cargo.toml`.

### 2. Dual-Resolving Workspace Dependency Specifications
In root `Cargo.toml`, all internal dependencies are declared with **both** `version` and `path`:
```toml
[workspace.dependencies]
acp = { version = "0.0.1", path = "acp" }
nuotc = { version = "0.0.1", path = "nuotc" }
nuox = { version = "0.0.1", path = "nuox" }
nuo-contracts = { version = "0.0.1", path = "nuo-contracts" }
...
```
During local development, Cargo uses `path`. During `cargo publish`, Cargo automatically embeds the exact `version` constraint required for public consumption.

---

## Invariants & Validation Contracts

- **`[INV-VER-01] Cluster Boundary Integrity`**:
  - `nuotc`, `acp`, and `nuox` must never depend on `version.workspace`.
  - Cluster C member crates must always inherit `version.workspace = true` to preserve lockstep coherence.
- **`[INV-VER-02] Publish-Ready Dependency Declarations`**:
  - All workspace path dependencies declared in `[workspace.dependencies]` must include an explicit `version` field matching the target crate's declared version.

---

## Positive Consequences

- `nuotc` can be published to `crates.io` and utilized by external Rust projects without bearing `nuo` application version bumps.
- `acp` stands as a pristine, independent protocol crate suitable for external polyglot agent runtimes.
- Developers working on `nuo`, `nuox`, and internal agent pipelines continue to experience zero version-management overhead: updating the application version requires changing exactly one number in root `Cargo.toml`.
- Crates are immediately publishable without CI packager scripts rewriting manifests on the fly.

---

## Negative Knowledge & Rejected Alternatives

### 1. Monolithic Single-Version Lockstep Across All 16 Crates
- **Why considered**: Simplest configuration (`version.workspace = true` on all 16 crates).
- **Why rejected**: Destroys the credibility of `nuotc` as a general-purpose TUI library. External users will not adopt a canvas library whose version increments from `0.1` to `0.8` due to LLM provider API changes.

### 2. Fully Fragmented Independent SemVer for All 16 Crates
- **Why considered**: Maximum theoretical SemVer granularity.
- **Why rejected**: Catastrophic maintenance tax during 0.x iteration. Patching a contract in `nuo-contracts` would require manual version bumps and commit coordination across 10+ dependent in-house crates, causing developer fatigue and dependency drift.
