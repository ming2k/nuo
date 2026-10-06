# Crate Versioning & Release Operations Guide

This document defines the operational procedures, versioning invariants, Git tagging conventions, and release playbooks for all crates within the **Nuo** repository.

It translates the architectural mandate of **[ADR-0004](../adr/0004-federated-cluster-semver-and-release-topology.md)** (*Federated Cluster SemVer and Release Topology*) into daily development rules and release engineering runbooks.

---

## 1. Principles & The Federated SemVer Model

In a multi-crate repository spanning different technical layers, two common extremes lead to architectural decay:

1. **The Monolithic Lockstep Anti-Pattern**: Forcing domain-free graphics engines (`nuotc`) or universal open standards (`acp`) to bump their versions whenever an upper application bug in `nuo` is patched. This pollutes external consumers' dependency trees and misleads the open-source ecosystem regarding stability.
2. **The Anarchic Independent Anti-Pattern**: Allowing every in-house crate (`nuo-client`, `nuo-host`, `nuo-persistence`, `nuo-harness`) to drift on completely independent SemVer numbers. This causes severe maintenance friction, manual changelog sprawl, and diamond dependency resolution failures during rapid host development.

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
│  • nuo (daemon & unified CLI coordinator)              │
│  • nuo-server (headless daemon container runtime)      │
│  • nuo-tui (terminal presentation view library)        │
│  • nuo-client (SDK), nuo-host (host environment)       │
│  • nuo-agent (cognitive loop), nuo-wire (domain & wire) │
│  • nuo-tool, nuo-tool-derive (tool specifications)     │
│  • nuo-model-codec (dialect translation & streaming)   │
│  • nuo-harness (orchestrator), nuo-persistence (store) │
│  • nuo-providers (catalog & auth), nuo-mcp (MCP)       │
│  • nuo-code (code intelligence), tools/* (tools)       │
│  ➜ Versioning: Unified Lockstep                        │
│  ➜ Manifest: Declares `version.workspace = true`       │
└────────────────────────────────────────────────────────┘
```

### Detailed Crate Matrix

| Crate | Cluster | Responsibility | Version Declaration | Release Artifact |
| :--- | :--- | :--- | :--- | :--- |
| **`acp`** | **Cluster B** | Universal inter-agent communication & channels | Explicit `version = "0.0.1"` | Independent crates.io package |
| **`nuotc`** | **Cluster A** | Domain-free 2D terminal canvas & diffing | Explicit `version = "0.0.1"` | Independent crates.io package |
| **`nuo`** | **Cluster C** | Unified public executable (daemon, CLI, TUI runner) | `version.workspace = true` | Binary distribution |
| **`nuo-server`** | **Cluster C** | Headless daemon container runtime & session host | `version.workspace = true` | crates.io package |
| **`nuo-tui`** | **Cluster C** | Semantic terminal interactive presentation view library | `version.workspace = true` | crates.io package |
| **`nuo-agent`** | **Cluster C** | Cognitive loop, session turns & token compaction | `version.workspace = true` | crates.io package |
| **`nuo-client`** | **Cluster C** | Standalone Rust Client SDK & Wire DTOs | `version.workspace = true` | crates.io package |
| **`nuo-wire`** | **Cluster C** | Wire envelopes, session entities & shared domain contracts | `version.workspace = true` | crates.io package |
| **`nuo-host`** | **Cluster C** | Host environment, sandboxing & native tools | `version.workspace = true` | crates.io package |
| **`nuo-tool`** | **Cluster C** | Zero-agent-runtime tool specification & schemas | `version.workspace = true` | crates.io package |
| **`nuo-tool-derive`** | **Cluster C** | Compile-time JSON schema derive macro | Direct path companion to `nuo-tool` | crates.io package |
| **`nuo-model-codec`**| **Cluster C** | LLM dialect translation & SSE streaming codec | `version.workspace = true` | crates.io package |
| **`nuo-persistence`**| **Cluster C** | SQLite session store, migrations & memory | `version.workspace = true` | crates.io package |
| **`nuo-harness`** | **Cluster C** | Host execution harness, approvals & policy | `version.workspace = true` | crates.io package |
| **`nuo-providers`** | **Cluster C** | Multi-vendor model catalog & OAuth engine | `version.workspace = true` | crates.io package |
| **`nuo-mcp`** | **Cluster C** | Model Context Protocol client & server transport | `version.workspace = true` | crates.io package |
| **`nuo-code`** | **Cluster C** | Code query, syntax analysis & AST operations | `version.workspace = true` | crates.io package |
| **`nuo-tool-*`** | **Cluster C** | Decoupled tool capability crates (`fs`, `exec`, `web`, `ast`)| `version.workspace = true` | crates.io package |

---

## 3. Dual-Resolving Dependencies Invariant

All internal inter-crate dependencies declared in the root `Cargo.toml` under `[workspace.dependencies]` **must declare both `version` and `path`** (`[INV-VER-02]`):

```toml
[workspace.dependencies]
# Autonomous protocol and substrate crates
acp = { version = "0.1.0", path = "acp" }
nuotc = { version = "0.1.0", path = "nuotc" }

# Host suite crates
nuo-client = { version = "0.1.0", path = "nuo-client" }
nuo-host = { version = "0.1.0", path = "nuo-host" }
nuo-tool = { version = "0.1.0", path = "nuo-tool" }
nuo-model-codec = { version = "0.1.0", path = "nuo-model-codec" }
nuo-persistence = { version = "0.1.0", path = "nuo-persistence" }
nuo-providers = { version = "0.1.0", path = "nuo-providers" }
nuo-harness = { version = "0.1.0", path = "nuo-harness" }
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
nuotc = "0.0.2"
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

### Playbook 2: Releasing the Unified Host Suite (`nuo`, `nuox`, `nuo-*`)

When releasing a new version of the Nuo product and daemon/terminal suite:

1. **Run Full Verification**:
   ```bash
   docgov check
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
   - Publishes library crates in dependency order (`nuo-tool`, `nuo-host`, `nuo-model-codec`, `nuo-persistence`, `nuo-client`, etc.).

---

## 5. Architectural Redlines & Anti-Patterns

### 1. Never Relocate Crates to Sibling Directories (`../`)
- **Prohibited**: Moving `acp` or `nuotc` outside the repository to `/data/projects/acp` and referencing them via `path = "../acp"`.
- **Reasoning**: Directly violates `[INV-WS-01]` (*Self-Contained Workspace*). A clean `git clone` by a new developer, a container build, or a CI runner would immediately fail. Multi-repo setups break atomic commits and prevent running unified `cargo test --workspace` sweeps.
- **The Rust Reality**: Compilation and module firewalls in Rust are enforced at the **crate** boundary, not the Git repository boundary. An in-tree crate with zero external dependencies is 100% physically decoupled.

### 2. Never Introduce Arbitrary Grouping Directories (`substrates/`, `crates/`)
- **Prohibited**: Nesting crates inside `crates/acp` or `substrates/nuotc`.
- **Reasoning**: Violates `[INV-ARCH-FLAT-01]` (*Flat Workspace Structure*). ADR-0005 proved that nested directories introduce artificial path indirection (`../../`) and taxonomy paralysis without delivering any engineering isolation.

### 3. Never Bypass Cluster Boundaries (`[INV-VER-01]`)
- Autonomous crates (`acp`, `nuotc`) must **never** inherit `version.workspace = true`.
- Host suite crates (`nuo-host`, `nuo-client`, `nuo-harness`) must **never** declare independent version numbers; they must remain strictly in lockstep.

---

## 6. Pre-Release Validation Checklist

Before submitting a version bump PR or pushing a release tag, verify:

- [ ] `docgov check` succeeds with zero errors or invariant warnings.
- [ ] `cargo check --workspace` compiles cleanly without warnings.
- [ ] `cargo test --workspace` passes all unit and integration tests.
- [ ] Every crate declared in `[workspace.dependencies]` has both `version` and `path`.
- [ ] `acp` and `nuotc` declare explicit version strings without `version.workspace`.
- [ ] Changelogs have been updated with user-facing and breaking API notes.
