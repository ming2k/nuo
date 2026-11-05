---
id: ADR-0148
title: "Canonical Extreme Fixtures and Zero-Dependency Developer Watch Supervisor"
status: accepted
date: 2026-03-29
scope: tui/nuo, devtools/storybook, test/fixtures, cli/dev
superseded_by: null
negative_knowledge: true
---

# 0148. Canonical Extreme Fixtures and Zero-Dependency Developer Watch Supervisor

- Status: Accepted
- Date: 2026-03-29
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, Quality Engineering, Developer Experience, TUI Runtime
- Complements: [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md), [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md), [ADR-0115](0115-command-output-fold-geometry-closed-compaction-and-disclosure-ladder.md), [ADR-0142](0142-transcoding-gateway-surfaces-portable-mode-and-dual-form-reasoning-replay.md), [ADR-0146](0146-live-storybook-engine-sub-second-hot-loop-and-broad-catalog-expansion.md)

---

## Context and Problem Statement

Following [ADR-0146](0146-live-storybook-engine-sub-second-hot-loop-and-broad-catalog-expansion.md) and [ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md), Nuo established the continuous live Storybook engine, persistence continuity, and color governance. However, two ergonomic and verification friction points remained:

1. **Third-Party Tool Friction in Dev-Loop Watch Mode**:
   While `cargo watch` provided file watching, `cargo watch` is a third-party cargo extension (`cargo install cargo-watch`) absent on standard clean developer workstations. Developers running `cargo watch` without pre-installation received `error: no such command: watch`, breaking the "All-in-Cargo / Zero External Tooling" principle codified in [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md).

2. **Unrepresented High-Entropy Production Payloads**:
   Two of Nuo's most complex visual mechanisms lacked canonical edge-case fixtures:
   - **Thousand-Line Closed-Compaction Shell Output ([ADR-0115](0115-command-output-fold-geometry-closed-compaction-and-disclosure-ladder.md))**: Shell commands producing thousands of compilation or build lines (`cargo build`, `npm install`) test the closed folding geometry (showing head rows, compaction summary badge, and tail error diagnostics).
   - **Deep Cognitive Reasoning Traces ([ADR-0142](0142-transcoding-gateway-surfaces-portable-mode-and-dual-form-reasoning-replay.md))**: Long-form multi-phase chain-of-thought traces produced by reasoning models (DeepSeek-R1, o1, GLM-5.3 xhigh) test disclosure toggles, indentation gutters, and streaming liveness without freezing.

---

## Decision Drivers

- **Zero-Dependency Native Watcher**: Developers must enjoy automated watch-and-reload capabilities directly via `nuo dev storybook --watch` without installing external Cargo extensions or Python/shell scripts.
- **Deterministic Boundary Testing**: Test fixtures must comprehensively represent production stress cases: 1,000-line shell compilation logs and multi-paragraph reasoning traces.
- **Strict Adherence to Workspace Governance**: Maintain 100% compliance with workspace residency, wire census, and color purity rules.

---

## Architectural Decisions

### 1. Zero-External-Tooling Watch Supervisor (`[INV-TUI-WATCH-01]`)

We introduce native watch mode directly into the `nuo dev storybook` CLI via the `--watch` (or `-w`) flag:
```sh
cargo run -p nuo -- dev storybook --watch
# or
nuo dev ui --watch
```

Using the workspace's native `notify` filesystem backend (`crates/nuo-host/src/fs_watcher.rs`), the supervisor:
1. Spawns the child Storybook process attached to the physical terminal (`Stdio::inherit`).
2. Listens for `.rs` file mutations in `crates/` with a 500ms debounce.
3. Automatically triggers clean child process termination and recompilation.
4. Seamlessly restores active component, theme, and viewport context via the persisted continuity state (`$TMPDIR/nuo_storybook_continuity.json`).

### 2. Canonical Closed-Fold and Reasoning Traces (`[INV-TUI-FIXTURE-01]`)

We expand the canonical Storybook suite with realistic extreme fixtures:
1. **`ToolCard` 1,000-Line Closed-Compaction Fold**:
   Simulates 1,000 lines of workspace compilation output, asserting that the component folds the intermediate 980 lines into a compact summary band without degrading terminal frame rates.
2. **`ReasoningCard` (Model Chain-of-Thought)**:
   - **Exhaustive Deep Deduction**: 4-paragraph multi-stage reasoning trace evaluating invariant proofs and trade-offs.
   - **Streaming In-Flight Thinking**: Active cognitive execution with rotating spinner and live duration timer.
   - **Compacted Summary**: Single-row summary reading `▸ Thought for 18 seconds (2,410 tokens)`.

---

## Normative Invariants Established

- **`[INV-TUI-FIXTURE-01] Closed Fold Geometry & Reasoning Depth Fixtures`**:
  The Storybook suite MUST include canonical fixtures covering 1,000-line closed-compaction command folding ([ADR-0115](0115-command-output-fold-geometry-closed-compaction-and-disclosure-ladder.md)) and multi-stage cognitive reasoning traces ([ADR-0142](0142-transcoding-gateway-surfaces-portable-mode-and-dual-form-reasoning-replay.md)) to prevent visual regression across high-entropy production streams.
- **`[INV-TUI-WATCH-01] Zero-External-Tooling Watch Supervisor`**:
  The developer CLI (`nuo dev storybook --watch`) MUST provide native in-tree filesystem watching and process supervision using `notify`. Developers MUST NOT be required to install external binaries or cargo plugins to achieve hot development loops.

---

## Consequences

- **Positive**:
  - Developers on fresh environments can immediately run `cargo run -p nuo -- dev storybook --watch` with zero external prerequisites.
  - Edge-case testing covers long-form reasoning traces and thousand-line compilation logs.
  - Catalog expands from 9 to 10 components (41 canonical stories total).
- **Negative & Trade-offs**:
  - Incremental compilation during supervisor restarts requires local Cargo compiler availability.

---

## Links

- Complements: [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md), [ADR-0111](0111-elimination-of-scripts-and-consolidation-of-workspace-governance-into-rust.md), [ADR-0115](0115-command-output-fold-geometry-closed-compaction-and-disclosure-ladder.md), [ADR-0142](0142-transcoding-gateway-surfaces-portable-mode-and-dual-form-reasoning-replay.md), [ADR-0146](0146-live-storybook-engine-sub-second-hot-loop-and-broad-catalog-expansion.md)
