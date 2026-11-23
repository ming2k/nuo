# Crate Versioning & Release Operations Guide

This document defines the operational procedures, versioning invariants, Git tagging conventions, and release playbooks for all crates within the **Nuo** repository.

It translates the architectural mandates of **[ADR-0004](../adr/0004-federated-cluster-semver-and-release-topology.md)** (*Federated Cluster SemVer and Release Topology*) and **[ADR-0109](../adr/0109-host-binary-relocation-to-crates-and-complete-workspace-homogenization.md)** (*Homogeneous Tiered Workspace Topology*) into daily development rules and release engineering runbooks.

---

## 1. Principles & The Federated SemVer Model

In a multi-crate repository spanning different technical layers, two common extremes lead to architectural decay:

1. **The Monolithic Lockstep Anti-Pattern**: Forcing domain-free terminal engines (`graticule-canvas`, `graticule-tty`, `graticule`) or universal open standards to bump their versions whenever an upper application bug in `nuo` is patched. This pollutes external consumers' dependency trees and misleads the open-source ecosystem regarding stability.
2. **The Anarchic Independent Anti-Pattern**: Allowing every in-house crate (`host`, `persistence`, `engine`) to drift on completely independent SemVer numbers. This causes severe maintenance friction, manual changelog sprawl, and diamond dependency resolution failures during rapid host development.

To solve this, the Nuo repository operates under **Federated Cluster SemVer**: autonomous substrate engines maintain independent versioning and release cycles, while the core host application suite evolves in lockstep.

---

## 2. Crate Classification & Versioning Matrix

Every crate in the repository belongs strictly to one of two operational versioning clusters:

```text
┌────────────────────────────────────────────────────────┐
│  Cluster A: Autonomous Substrate Engine                │
│  • graticule (graticule-canvas + graticule-tty)        │
│  ➜ Source: External repository (github.com/ming2k/graticule)│
│  ➜ Versioning: Autonomous (pinned tag / semver)        │
│  ➜ Manifest: Declares explicit registry requirement    │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster B: Application & Subsystem Host Suite         │
│  • crates/nuo (unified public binary & coordinator)    │
│  • crates/server (headless session container)          │
│  • crates/protocol (canonical East-West IPC wire)      │
│  • crates/ui-primitives (generic presentation UI)      │
│  • crates/view (agent domain views)                    │
│  • crates/host (host supervision & sandboxing)         │
│  • crates/acp (Agent Coordination Protocol)            │
│  • crates/tool, tool-derive (tool contracts)           │
│  • crates/inference-wire (model wire substrate)        │
│  • crates/persistence (SQLite store & migrations)      │
│  • crates/engine (cognition & orchestration)           │
│  • crates/guardrails (execution policy & approval)     │
│  • crates/provider (provider specs & auth)             │
│  • crates/model-routing (catalog pipeline)             │
│  • crates/provider-* (model provider adapters)         │
│  • crates/tool-* (decoupled capability tools)          │
│  ➜ Versioning: Unified Workspace Lockstep              │
│  ➜ Manifest: Declares `version.workspace = true`       │
└────────────────────────────────────────────────────────┘
```

### Detailed Crate Matrix

| Crate | Cluster | Responsibility | Version Declaration | Release Artifact |
| :--- | :--- | :--- | :--- | :--- |
| **`graticule`** (`graticule-canvas`, `graticule-tty`) | **Cluster A** | Domain-free 2D terminal canvas, diffing engine, and TTY host transport | crates.io semver (or an untracked local `[patch.crates-io]` override) | crates.io |
| **`nuo`** | **Cluster B** | Unified public executable (server, CLI, TUI runner) | `version.workspace = true` | Pre-compiled binary |
| **`server`** | **Cluster B** | Headless server container runtime & session host | `version.workspace = true` | crates.io package |
| **`protocol`** | **Cluster B** | Canonical IPC wire, session entities, discovery DTOs | `version.workspace = true` | crates.io package |
| **`ui-primitives`**| **Cluster B** | Zero-IO generic presentation widgets & layout tokens | `version.workspace = true` | crates.io package |
| **`view`** | **Cluster B** | Zero-IO agent domain views, turn headers & cards | `version.workspace = true` | crates.io package |
| **`host`** | **Cluster B** | Host environment, process supervision & sandboxing | `version.workspace = true` | crates.io package |
| **`acp`** | **Cluster B** | Canonical inter-agent communication & channels | `version.workspace = true` | crates.io package |
| **`tool`** | **Cluster B** | Zero-agent-runtime tool specification & token estimator | `version.workspace = true` | crates.io package |
| **`tool-derive`** | **Cluster B** | Compile-time JSON schema derive macro | Direct path companion to `tool` | crates.io package |
| **`inference-wire`**| **Cluster B** | Model wire substrate, JSON framing & loop detection | `version.workspace = true` | crates.io package |
| **`persistence`**| **Cluster B** | SQLite session store, migrations & memory ledger | `version.workspace = true` | crates.io package |
| **`engine`** | **Cluster B** | Cognitive engine, ReAct orchestration & model requests | `version.workspace = true` | crates.io package |
| **`guardrails`** | **Cluster B** | Execution policy, approvals & trajectory supervision | `version.workspace = true` | crates.io package |
| **`provider`** | **Cluster B** | Model provider specification, OAuth & transport | `version.workspace = true` | crates.io package |
| **`model-routing`** | **Cluster B** | Single model-catalog pipeline (derive, picker, sync) ([ADR-0082](../adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md)) | `version.workspace = true` | crates.io package |
| **`crates/provider-*`** | **Cluster B** | Provider channel adapters (Anthropic, OpenAI, etc.) | `version.workspace = true` | crates.io package |
| **`crates/tool-*`** | **Cluster B** | Capability tools (`fs`, `exec`, `web`, `ast`) | `version.workspace = true` | crates.io package |

MCP is not a crate: the client-facing DTOs live in `protocol`, the one client lives in `server::runtime::mcp`, and Nuo does not export itself as an MCP server (ADR-0230, ADR-0234).

---

## 3. Dual-Resolving Dependencies Invariant

All internal inter-crate dependencies declared in the root `Cargo.toml` under `[workspace.dependencies]` **must declare both `version` and `path`** (`[INV-VER-02]`):

```toml
[workspace.dependencies]
# In-house Nuo crates (Cluster B lockstep suite)
protocol = { version = "0.0.98", path = "crates/protocol" }
host = { version = "0.0.98", path = "crates/host" }
tool = { version = "0.0.98", path = "crates/tool" }
inference-wire = { version = "0.0.98", path = "crates/inference-wire", default-features = false }
persistence = { version = "0.0.98", path = "crates/persistence" }
engine = { version = "0.0.98", path = "crates/engine" }
guardrails = { version = "0.0.98", path = "crates/guardrails" }
provider = { version = "0.0.98", path = "crates/provider" }
model-routing = { version = "0.0.98", path = "crates/model-routing" }
ui-primitives = { version = "0.0.98", path = "crates/ui-primitives" }
view = { version = "0.0.98", path = "crates/view" }
```

### Why Both Fields Are Non-Negotiable:
- **Local Development Velocity (`path`)**: Cargo resolves the local filesystem path directly. Any code changes across crates are immediately reflected across the entire workspace without requiring intermediate local releases or cache invalidations.
- **Publish-Ready Distribution (`version`)**: When running `cargo publish`, Cargo automatically removes the local `path` field and emits a `.crate` tarball specifying only the semver requirement. If `version` is missing, `cargo publish` fails immediately.

### Local-Only Override for the External `graticule` Substrate

`graticule` (`graticule-canvas` + `graticule-tty`) is developed in its own repository and consumed here as a pinned dependency:

```toml
[workspace.dependencies]
graticule = "0.0.2"
graticule-canvas = "0.0.2"
```

For day-to-day development against a local sibling checkout, keep an untracked `.cargo/config.toml` (git-ignored via `/.cargo`) overriding crates.io:

```toml
# .cargo/config.toml  (local development only, never committed)
[patch.crates-io]
graticule = { path = "../graticule/crates/graticule" }
graticule-canvas = { path = "../graticule/crates/graticule-canvas" }
graticule-tty = { path = "../graticule/crates/graticule-tty" }
```

---

## 4. Git Tagging & Release Playbooks

### Playbook 1: Bumping the External `graticule` Substrate

When a new release of `graticule` is published upstream:

1. Update the `graticule` / `graticule-canvas` semver requirement in the root `Cargo.toml`
   (they are registry dependencies, not git tags).
2. Run full workspace compilation and UI snapshot tests:
   ```bash
   cargo check --workspace
   cargo test -p view --lib
   cargo test -p nuo-tests --test integration governance::focus_plane_invariants
   ```
3. Commit the dependency update:
   ```bash
   git commit -am "chore(deps): bump graticule to vX.Y.Z"
   ```

### Playbook 2: Releasing the Unified Host Suite (`vX.Y.Z`)

When releasing a new milestone of the Nuo product and workspace:

1. **Run Full Verification**:
   ```bash
   cargo check --workspace
   cargo nextest run --workspace --profile unit
   cargo nextest run --workspace --profile integration
   cargo nextest run -p nuo-tests --profile e2e
   ```
2. **Bump Workspace Version**:
   - Update `version = "X.Y.Z"` under `[workspace.package]` in root `Cargo.toml`.
   - Update all in-house entries in `[workspace.dependencies]` to `"X.Y.Z"`.
   - Re-bless wire census if protocol numbers moved: `cargo nextest run -p nuo-tests --profile integration -E 'test(governance::architecture_firewall::bless_wire_census)' --run-ignored ignored-only`.
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
- In-house crates within `crates/*` must never reference relative paths outside the workspace repository root (`../`).
- A clean checkout must compile and test in a sandboxed CI runner without external sibling directories.

### 2. Flat `crates/*` Workspace Topology (`[INV-ARCH-TIER-01]`)
- All production crates reside uniformly under `crates/*`:
  - `crates/nuo` — Host binary.
  - `crates/*` — Core platform crates, provider adapters (`provider-*`), and tool plugins (`tool-*`).
- No production crate resides directly at the workspace root. The sole root package is the dev-only `nuo-tests` test host; all production crates reside in `crates/*`. Enforced mechanically by the nextest `governance::architecture_firewall` suite.

### 3. Unified Lockstep Invariant (`[INV-VER-01]`)
- Host suite crates (`nuo`, `server`, `engine`, `guardrails`, `protocol`, `ui-primitives`, `view`, etc.) must **never** declare independent version numbers; they inherit `version.workspace = true` to prevent diamond dependency resolution failures and version drift.

---

## 6. Pre-Release Validation Checklist

Before submitting a version bump PR or pushing a release tag, verify:

- [ ] `cargo check --workspace` compiles cleanly without warnings.
- [ ] All tiers pass: `cargo nextest run --workspace --profile unit`, `cargo nextest run --workspace --profile integration`, and `cargo nextest run -p nuo-tests --profile e2e` (see [Testing](testing.md)).
- [ ] All architectural firewall tests pass: `cargo nextest run -p nuo-tests --profile integration -E 'test(governance::architecture_firewall::)'`.
- [ ] Every internal crate in `[workspace.dependencies]` declares matching `version` and `path`.
- [ ] Wire surface census matches protocol version (`cargo nextest run -p nuo-tests --profile integration -E 'test(governance::architecture_firewall::test_inv_wire_census_matches_protocol_version)'`).
- [ ] Changelog has been updated with user-facing and breaking API notes.
