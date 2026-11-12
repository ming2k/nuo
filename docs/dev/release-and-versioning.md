# Crate Versioning & Release Operations Guide

This document defines the operational procedures, versioning invariants, Git tagging conventions, and release playbooks for all crates within the **Nuo** repository.

It translates the architectural mandates of **[ADR-0004](../adr/0004-federated-cluster-semver-and-release-topology.md)** (*Federated Cluster SemVer and Release Topology*) and **[ADR-0109](../adr/0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)** (*Homogeneous Tiered Workspace Topology*) into daily development rules and release engineering runbooks.

---

## 1. Principles & The Federated SemVer Model

In a multi-crate repository spanning different technical layers, two common extremes lead to architectural decay:

1. **The Monolithic Lockstep Anti-Pattern**: Forcing domain-free graphics engines (`nuotc`) or universal open standards to bump their versions whenever an upper application bug in `nuo` is patched. This pollutes external consumers' dependency trees and misleads the open-source ecosystem regarding stability.
2. **The Anarchic Independent Anti-Pattern**: Allowing every in-house crate (`nuo-host`, `nuo-persistence`, `nuo-engine`) to drift on completely independent SemVer numbers. This causes severe maintenance friction, manual changelog sprawl, and diamond dependency resolution failures during rapid host development.

To solve this, the Nuo repository operates under **Federated Cluster SemVer**: autonomous substrate engines maintain independent versioning and release cycles, while the core host application suite evolves in lockstep.

---

## 2. Crate Classification & Versioning Matrix

Every crate in the repository belongs strictly to one of two operational versioning clusters:

```text
┌────────────────────────────────────────────────────────┐
│  Cluster A: Autonomous Substrate Engine                │
│  • nuotc (Retained-mode 2D character canvas & diff)    │
│  ➜ Source: External repository (github.com/ming2k/nuotc) │
│  ➜ Versioning: Autonomous (pinned tag / semver)        │
│  ➜ Manifest: Declares explicit registry requirement    │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster B: Application & Subsystem Host Suite         │
│  • crates/nuo (unified public binary & coordinator)    │
│  • crates/nuo-server (headless session container)      │
│  • crates/nuo-protocol (canonical East-West IPC wire)  │
│  • crates/nuo-ui-primitives (generic presentation UI)  │
│  • crates/nuo-view (agent domain views)                │
│  • crates/nuo-host (host supervision & sandboxing)     │
│  • crates/nuo-acp (Agent Coordination Protocol)        │
│  • crates/nuo-tool, nuo-tool-derive (tool contracts)   │
│  • crates/nuo-inference-wire (model wire substrate)    │
│  • crates/nuo-persistence (SQLite store & migrations)  │
│  • crates/nuo-engine (cognition & orchestration)       │
│  • crates/nuo-guardrails (execution policy & approval) │
│  • crates/nuo-provider (provider specs & auth)         │
│  • crates/nuo-model-routing (catalog pipeline)         │
│  • crates/nuo-mcp (Model Context Protocol transport)   │
│  • providers/* (model provider channel adapters)       │
│  • tools/* (decoupled capability tools)                │
│  ➜ Versioning: Unified Workspace Lockstep              │
│  ➜ Manifest: Declares `version.workspace = true`       │
└────────────────────────────────────────────────────────┘
```

### Detailed Crate Matrix

| Crate | Cluster | Responsibility | Version Declaration | Release Artifact |
| :--- | :--- | :--- | :--- | :--- |
| **`nuotc`** | **Cluster A** | Domain-free 2D terminal canvas & diffing engine | Pinned tag in `Cargo.toml` | External Git / crates.io |
| **`nuo`** | **Cluster B** | Unified public executable (server, CLI, TUI runner) | `version.workspace = true` | Pre-compiled binary |
| **`nuo-server`** | **Cluster B** | Headless server container runtime & session host | `version.workspace = true` | crates.io package |
| **`nuo-protocol`** | **Cluster B** | Canonical IPC wire, session entities, discovery DTOs | `version.workspace = true` | crates.io package |
| **`nuo-ui-primitives`**| **Cluster B** | Zero-IO generic presentation widgets & layout tokens | `version.workspace = true` | crates.io package |
| **`nuo-view`** | **Cluster B** | Zero-IO agent domain views, turn headers & cards | `version.workspace = true` | crates.io package |
| **`nuo-host`** | **Cluster B** | Host environment, process supervision & sandboxing | `version.workspace = true` | crates.io package |
| **`nuo-acp`** | **Cluster B** | Canonical inter-agent communication & channels | `version.workspace = true` | crates.io package |
| **`nuo-tool`** | **Cluster B** | Zero-agent-runtime tool specification & token estimator | `version.workspace = true` | crates.io package |
| **`nuo-tool-derive`** | **Cluster B** | Compile-time JSON schema derive macro | Direct path companion to `nuo-tool` | crates.io package |
| **`nuo-inference-wire`**| **Cluster B** | Model wire substrate, JSON framing & loop detection | `version.workspace = true` | crates.io package |
| **`nuo-persistence`**| **Cluster B** | SQLite session store, migrations & memory ledger | `version.workspace = true` | crates.io package |
| **`nuo-engine`** | **Cluster B** | Cognitive engine, ReAct orchestration & model requests | `version.workspace = true` | crates.io package |
| **`nuo-guardrails`** | **Cluster B** | Execution policy, approvals & trajectory supervision | `version.workspace = true` | crates.io package |
| **`nuo-provider`** | **Cluster B** | Model provider specification, OAuth & transport | `version.workspace = true` | crates.io package |
| **`nuo-model-routing`** | **Cluster B** | Single model-catalog pipeline (derive, picker, sync) ([ADR-0082](../adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md)) | `version.workspace = true` | crates.io package |
| **`nuo-mcp`** | **Cluster B** | Model Context Protocol client & server transport | `version.workspace = true` | crates.io package |
| **`providers/*`** | **Cluster B** | Provider channel adapters (Anthropic, OpenAI, etc.) | `version.workspace = true` | crates.io package |
| **`tools/*`** | **Cluster B** | Capability tools (`fs`, `exec`, `web`, `ast`) | `version.workspace = true` | crates.io package |

---

## 3. Dual-Resolving Dependencies Invariant

All internal inter-crate dependencies declared in the root `Cargo.toml` under `[workspace.dependencies]` **must declare both `version` and `path`** (`[INV-VER-02]`):

```toml
[workspace.dependencies]
# In-house Nuo crates (Cluster B lockstep suite)
nuo-protocol = { version = "0.0.84", path = "crates/nuo-protocol" }
nuo-host = { version = "0.0.84", path = "crates/nuo-host" }
nuo-tool = { version = "0.0.84", path = "crates/nuo-tool" }
nuo-inference-wire = { version = "0.0.84", path = "crates/nuo-inference-wire", default-features = false }
nuo-persistence = { version = "0.0.84", path = "crates/nuo-persistence" }
nuo-engine = { version = "0.0.84", path = "crates/nuo-engine" }
nuo-guardrails = { version = "0.0.84", path = "crates/nuo-guardrails" }
nuo-provider = { version = "0.0.84", path = "crates/nuo-provider" }
nuo-model-routing = { version = "0.0.84", path = "crates/nuo-model-routing" }
nuo-ui-primitives = { version = "0.0.84", path = "crates/nuo-ui-primitives" }
nuo-view = { version = "0.0.84", path = "crates/nuo-view" }
```

### Why Both Fields Are Non-Negotiable:
- **Local Development Velocity (`path`)**: Cargo resolves the local filesystem path directly. Any code changes across crates are immediately reflected across the entire workspace without requiring intermediate local releases or cache invalidations.
- **Publish-Ready Distribution (`version`)**: When running `cargo publish`, Cargo automatically removes the local `path` field and emits a `.crate` tarball specifying only the semver requirement. If `version` is missing, `cargo publish` fails immediately.

### Local-Only Override for the External `nuotc` Substrate

`nuotc` is developed in its own repository and consumed here as a pinned dependency:

```toml
[workspace.dependencies]
nuotc = { git = "https://github.com/ming2k/nuotc.git", tag = "v0.0.6" }
```

For day-to-day development against a local sibling checkout, a developer may keep an **untracked** `.cargo/config.toml` (git-ignored via `/.cargo`) that overrides the Git source with a local path patch:

```toml
# .cargo/config.toml  (local development only, never committed)
[patch."https://github.com/ming2k/nuotc.git"]
nuotc = { path = "../nuotc" }
```

---

## 4. Git Tagging & Release Playbooks

### Playbook 1: Bumping the External `nuotc` Substrate

When a new release of `nuotc` is published upstream:

1. Update the `tag = "vX.Y.Z"` requirement in the root `Cargo.toml`.
2. Run full workspace compilation and UI snapshot tests:
   ```bash
   cargo check --workspace
   cargo test -p nuo-view
   cargo test -p nuo --test focus_plane_invariants
   ```
3. Commit the dependency update:
   ```bash
   git commit -am "chore(deps): bump nuotc to vX.Y.Z"
   ```

### Playbook 2: Releasing the Unified Host Suite (`vX.Y.Z`)

When releasing a new milestone of the Nuo product and workspace:

1. **Run Full Verification**:
   ```bash
   cargo check --workspace
   cargo test --workspace
   cargo test -p nuo --test architecture_firewall
   ```
2. **Bump Workspace Version**:
   - Update `version = "X.Y.Z"` under `[workspace.package]` in root `Cargo.toml`.
   - Update all in-house entries in `[workspace.dependencies]` to `"X.Y.Z"`.
   - Re-bless wire census if protocol numbers moved: `cargo test -p nuo --test architecture_firewall -- --ignored bless_wire_census`.
   - Update `CHANGELOG.md` with release highlights and migration notes.
3. **Commit & Tag**:
   ```bash
   git commit -am "chore(release): bump version to X.Y.Z"
   git tag vX.Y.Z
   git push origin main --tags
   ```
4. **Distribution Pipeline**:
   - CI runner builds release binaries for `nuo` across Linux (x86_64, aarch64), macOS (universal), and Windows.
   - Publishes library crates in topological dependency order.

---

## 5. Architectural Redlines & Invariants

### 1. In-Tree Crates Are Self-Contained (`[INV-WS-01]`)
- In-house crates within `crates/*`, `providers/*`, and `tools/*` must never reference relative paths outside the workspace repository root (`../`).
- A clean checkout must compile and test in a sandboxed CI runner without external sibling directories.

### 2. Pure Tiered Workspace Topology (`[INV-ARCH-TIER-01]`, ADR-0109)
- All crates reside uniformly in one of exactly three flat tiers:
  - `crates/*` — Layers 1–4 core platform crates, including the host binary `crates/nuo`.
  - `providers/nuo-provider-*` — Provider channel adapters.
  - `tools/nuo-tool-*` — Tool plugins.
- Zero crates reside directly at the workspace root. Enforced mechanically by `cargo test -p nuo --test architecture_firewall`.

### 3. Unified Lockstep Invariant (`[INV-VER-01]`)
- Host suite crates (`nuo`, `nuo-server`, `nuo-engine`, `nuo-guardrails`, `nuo-protocol`, `nuo-ui-primitives`, `nuo-view`, etc.) must **never** declare independent version numbers; they inherit `version.workspace = true` to prevent diamond dependency resolution failures and version drift.

---

## 6. Pre-Release Validation Checklist

Before submitting a version bump PR or pushing a release tag, verify:

- [ ] `cargo check --workspace` compiles cleanly without warnings.
- [ ] `cargo test --workspace` passes all unit and integration tests.
- [ ] All architectural firewall tests pass: `cargo test -p nuo --test architecture_firewall`.
- [ ] Every internal crate in `[workspace.dependencies]` declares matching `version` and `path`.
- [ ] Wire surface census matches protocol version (`cargo test -p nuo --test architecture_firewall -- test_inv_wire_census_matches_protocol_version`).
- [ ] Changelog has been updated with user-facing and breaking API notes.
