# Crate Versioning & Release Operations Guide

This document defines the operational procedures, versioning invariants, Git tagging conventions, and release playbooks for all crates within the **Nuo** repository.

It translates the architectural mandate of **[ADR-0004](../adr/0004-federated-cluster-semver-and-release-topology.md)** (*Federated Cluster SemVer and Release Topology*) into daily development rules and release engineering runbooks.

---

## 1. Principles & The Federated SemVer Model

In a multi-crate repository spanning different technical layers, two common extremes lead to architectural decay:

1. **The Monolithic Lockstep Anti-Pattern**: Forcing domain-free graphics engines (`nuotc`) or universal open standards (`acp`) to bump their versions whenever an upper application bug in `nuo` is patched. This pollutes external consumers' dependency trees and misleads the open-source ecosystem regarding stability.
2. **The Anarchic Independent Anti-Pattern**: Allowing every in-house crate (`nuo-host`, `nuo-persistence`, `nuo-harness`) to drift on completely independent SemVer numbers. This causes severe maintenance friction, manual changelog sprawl, and diamond dependency resolution failures during rapid host development.

To solve this, the Nuo repository operates under **Federated Cluster SemVer**: autonomous protocol and substrate crates maintain independent versioning and release cycles, while the core host application suite evolves in lockstep.

---

## 2. Crate Classification & Versioning Matrix

Every crate in the repository belongs strictly to one of three versioning clusters:

```text
┌────────────────────────────────────────────────────────┐
│  Cluster A: Autonomous Substrate Engine                │
│  • nuotc (Retained-mode 2D character canvas & diff)    │
│  ➜ Versioning: Autonomous (e.g. 0.1.0 -> 0.1.1)        │
│  ➜ Manifest: Declares explicit `version = "0.1.0"`     │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster B: Canonical Open Protocol Standard           │
│  • acp (Agent Coordination Protocol specifications)   │
│  ➜ Versioning: Protocol RFC Baseline                   │
│  ➜ Manifest: Declares explicit `version = "0.1.0"`     │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│  Cluster C: Application & Subsystem Host Suite         │
│  • nuo (server & unified CLI coordinator)              │
│  • nuo-server (headless server container runtime)      │
│  • nuo-tui (terminal presentation view library)        │
│  • nuo-host (host environment)                         │
│  • nuo-protocol (IPC wire)                             │
│  • nuo-tool, nuo-tool-derive (tool specifications)     │
│  • nuo-inference-wire (dialect translation & streaming)   │
│  • nuo-harness (orchestrator), nuo-persistence (store) │
│  • nuo-providers (catalog & auth), nuo-mcp (MCP)       │
│  • tools/* (decoupled tool implementations)            │
│  ➜ Versioning: Unified Lockstep                        │
│  ➜ Manifest: Declares `version.workspace = true`       │
└────────────────────────────────────────────────────────┘
```

### Detailed Crate Matrix

| Crate | Cluster | Responsibility | Version Declaration | Release Artifact |
| :--- | :--- | :--- | :--- | :--- |
| **`acp`** | **Cluster B** | Universal inter-agent communication & channels | Explicit `version = "0.0.1"` | Independent crates.io package |
| **`nuotc`** | **Cluster A** | Domain-free 2D terminal canvas & diffing | Explicit `version = "0.0.1"` | Independent crates.io package |
| **`nuo`** | **Cluster C** | Unified public executable (server, CLI, TUI runner) | `version.workspace = true` | Binary distribution |
| **`nuo-server`** | **Cluster C** | Headless server container runtime & session host | `version.workspace = true` | crates.io package |
| **`nuo-tui`** | **Cluster C** | Semantic terminal interactive presentation view library | `version.workspace = true` | crates.io package |
| **`nuo-protocol`** | **Cluster C** | IPC envelopes, session entities, discovery & shared domain contracts | `version.workspace = true` | crates.io package (`nuo-wire` alias) |
| **`nuo-host`** | **Cluster C** | Host environment, sandboxing & native tools | `version.workspace = true` | crates.io package |
| **`nuo-tool`** | **Cluster C** | Zero-agent-runtime tool specification & schemas | `version.workspace = true` | crates.io package |
| **`nuo-tool-derive`** | **Cluster C** | Compile-time JSON schema derive macro | Direct path companion to `nuo-tool` | crates.io package |
| **`nuo-inference-wire`**| **Cluster C** | LLM dialect translation & SSE streaming codec | `version.workspace = true` | crates.io package |
| **`nuo-persistence`**| **Cluster C** | SQLite session store, migrations & memory | `version.workspace = true` | crates.io package |
| **`nuo-harness`** | **Cluster C** | Host execution harness, approvals & policy | `version.workspace = true` | crates.io package |
| **`nuo-model-routing`** | **Cluster C** | Single model-catalog pipeline (derive, picker, sync) shared by server & harness ([ADR-0082](../adr/0082-single-catalog-pipeline-definition-in-a-dedicated-layer2-crate.md)) | `version.workspace = true` | crates.io package |
| **`nuo-providers`** | **Cluster C** | Multi-vendor model catalog & OAuth engine | `version.workspace = true` | crates.io package |
| **`nuo-mcp`** | **Cluster C** | Model Context Protocol client & server transport | `version.workspace = true` | crates.io package |
| **`nuo-tool-*`** | **Cluster C** | Decoupled tool capability crates (`fs`, `exec`, `web`, `ast`)| `version.workspace = true` | crates.io package |

---

## 3. Dual-Resolving Dependencies Invariant

All internal inter-crate dependencies declared in the root `Cargo.toml` under `[workspace.dependencies]` **must declare both `version` and `path`** (`[INV-VER-02]`):

```toml
[workspace.dependencies]
# Autonomous protocol and substrate crates
# (versions below are illustrative — `[workspace.package].version` is the
# single source of truth for the Cluster C lockstep suite)
acp = { version = "0.0.51", package = "nuo-acp", path = "crates/nuo-acp" }
nuotc = { version = "0.0.3" }

# Component libraries
nuo-host = { version = "0.0.51", path = "crates/nuo-host" }
nuo-tool = { version = "0.0.51", path = "crates/nuo-tool" }
nuo-inference-wire = { version = "0.0.51", path = "crates/nuo-inference-wire" }
nuo-persistence = { version = "0.0.51", path = "crates/nuo-persistence" }
nuo-harness = { version = "0.0.51", path = "crates/nuo-harness" }
nuo-model-routing = { version = "0.0.51", path = "crates/nuo-model-routing" }
```

### Why Both Fields Are Non-Negotiable:
- **Local Development Velocity (`path`)**: Cargo resolves the local filesystem path directly. Any code changes in `acp`, `nuo-host`, or `nuotc` are immediately reflected across the entire workspace without requiring intermediate local releases or cache invalidations.
- **Publish-Ready Distribution (`version`)**: When running `cargo publish`, Cargo automatically removes the local `path` field and emits a `.crate` tarball specifying only the semver requirement. If `version` is missing, `cargo publish` fails immediately.

### Local-Only Override for the External `nuotc` Substrate

`nuotc` is developed in its own repository and consumed here as a published
crates.io dependency, so the root manifest declares a registry requirement only
(no `path` field):

```toml
[workspace.dependencies]
nuotc = "0.0.3"
```

For day-to-day development against the sibling checkout, a developer keeps an
**untracked** `.cargo/config.toml` (git-ignored via `/.cargo`) that overrides the
registry source with a path patch:

```toml
# .cargo/config.toml  (local only, never committed)
[patch.crates-io]
nuotc = { path = "../nuotc" }
```

Cargo applies that patch **only when the sibling crate's version satisfies the
requirement above**. When they drift, Cargo discards the patch, emits an
`unused patch` warning, and builds the crates.io release instead — which
typically surfaces as a burst of misleading `E0599` method-not-found errors
later in the build. There is no separate guard: keeping the two versions in sync
is a release-process responsibility, and Cargo's own warning is the signal.

Consequences:
- Whenever `nuotc` is bumped, the **same change** must update its version in the
  root `[workspace.dependencies]` (see Playbook 1).
- If a local build suddenly reports `E0599` on `nuotc` APIs, check for
  `warning: patch ... was not used in the crate graph`: it means the sibling and
  the root requirement have drifted apart.

---

## 4. Git Tagging & Release Playbooks

Release workflows are differentiated by Git tag prefixes to cleanly separate autonomous substrate releases from host product releases:

### Playbook 1: Releasing an Autonomous Substrate (`acp` or `nuotc`)

When a standalone crate reaches a new protocol or engine milestone:

1. **Verify Crate Isolation**:
   ```bash
   # Ensure crate compiles and passes all tests completely isolated from the workspace
   cargo test -p acp --no-default-features
   cargo test -p acp --all-features
   ```
2. **Bump Crate Version**:
   - Update `version = "X.Y.Z"` inside `acp/Cargo.toml`.
   - Update `acp = { version = "X.Y.Z", path = "acp" }` in root `Cargo.toml`.
   - For `nuotc` (separate repository), update the registry requirement
     `nuotc = "X.Y.Z"` in root `Cargo.toml`; it is picked up locally through the
     `.cargo/config.toml` path patch (see §3).
   - Update `acp/CHANGELOG.md` with release highlights.
3. **Commit & Tag**:
   ```bash
   git commit -am "chore(release): acp vX.Y.Z"
   git tag acp-vX.Y.Z
   git push origin main --tags
   ```
4. **Publish to crates.io**:
   ```bash
   cargo publish -p acp
   ```

### Playbook 2: Releasing the Unified Host Suite (`nuo`, `nuo-*`)

When releasing a new version of the Nuo product and server/terminal suite:

1. **Run Full Verification**:
   ```bash
   cargo check --workspace
   cargo test --workspace
   ```
2. **Bump Workspace Version**:
   - Update `version = "X.Y.Z"` under `[workspace.package]` in root `Cargo.toml`.
   - Update all Cluster C entries in `[workspace.dependencies]` to `"X.Y.Z"`.
   - Update the root `CHANGELOG.md`.
3. **Commit & Tag**:
   ```bash
   git commit -am "chore(release): vX.Y.Z"
   git tag vX.Y.Z
   git push origin main --tags
   ```
4. **CI Distribution**:
   - The CI runner matching `v*` builds pre-compiled release binary for `nuo` across Linux, macOS, and Windows.
   - Publishes library crates in dependency order (`nuo-tool`, `nuo-host`, `nuo-inference-wire`, `nuo-persistence`, `nuo-model-routing`, etc.).

---

## 5. Architectural Redlines & Anti-Patterns

### 1. Never Relocate Crates to Sibling Directories (`../`)
- **Prohibited**: Moving `acp` or `nuotc` outside the repository to `/data/projects/acp` and referencing them via `path = "../acp"`.
- **Reasoning**: Directly violates `[INV-WS-01]` (*Self-Contained Workspace*). A clean `git clone` by a new developer, a container build, or a CI runner would immediately fail. Multi-repo setups break atomic commits and prevent running unified `cargo test --workspace` sweeps.
- **The Rust Reality**: Compilation and module firewalls in Rust are enforced at the **crate** boundary, not the Git repository boundary. An in-tree crate with zero external dependencies is 100% physically decoupled.

### 2. Pure Tiered Workspace Topology (`[INV-ARCH-TIER-01]`, ADR-0109)
- **Prohibited**: Any crate manifest at the repository root, a fourth grouping directory
  (`substrates/`, `apps/`), nesting deeper than `<tier>/<crate>/`, or a crate directory that
  does not carry its tier's namespace prefix.
- **Sanctioned layout**:
  - `crates/*` — Layers 1–4 core platform crates, including the single application host binary
    `crates/nuo` (`[[bin]]`). Layer 4 is an *architectural* layer, not a root-level filesystem
    exception. Application identity is declared via `default-members = ["crates/nuo"]`.
  - `providers/nuo-provider-*` — provider channel adapters.
  - `tools/nuo-tool-*` — tool plugins.
- **Reasoning**: Each tier answers exactly one question ("which layer?", "which vendor channel?",
  "which tool?"), so no directory needs a tie-breaker. The repository root contains strictly
  the three plural category directories (`crates/`, `providers/`, `tools/`), eliminating category
  mismatch and special-case tooling logic.
- **Why the older root-host and flat-topology rules are gone**: ADR-0055 historically left `nuo/`
  at the root as a half-step. ADR-0109 completes the homogenization: an executable's identity in
  Cargo is defined by `[[bin]]` and `default-members`, not root-level directory privilege.
- **Enforcement**: `nuo::dev::firewall` rule R5 (`cargo test -p nuo --test architecture_firewall`)
  checks the layout itself —
  zero root manifests, per-tier namespace prefixes, nesting depth, and the literal wildcard
  `members` set — and the binary-target census. Mechanisms over exhortations: a layout that is
  only described drifts, and a crate that escapes the topology escapes every other firewall
  rule, because they all enumerate the workspace by glob.

### 3. Never Bypass Cluster Boundaries (`[INV-VER-01]`)
- Autonomous crates (`acp`, `nuotc`) must **never** inherit `version.workspace = true`.
- Host suite crates (`nuo-host`, `nuo-server`, `nuo-harness`) must **never** declare independent version numbers; they must remain strictly in lockstep.

---

## 6. Pre-Release Validation Checklist

Before submitting a version bump PR or pushing a release tag, verify:

- [ ] `cargo check --workspace` compiles cleanly without warnings.
- [ ] `cargo test --workspace` passes all unit and integration tests.
- [ ] Every crate declared in `[workspace.dependencies]` has both `version` and `path`.
- [ ] `acp` and `nuotc` declare explicit version strings without `version.workspace`.
- [ ] Changelogs have been updated with user-facing and breaking API notes.
