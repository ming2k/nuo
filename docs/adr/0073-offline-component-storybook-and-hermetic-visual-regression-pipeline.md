---
id: ADR-0073
title: "Offline Component Storybook and Hermetic Visual Regression Pipeline"
status: accepted
date: 2026-11-15
scope: tui/nuo-tui, ci/acceptance, devtools/storybook, test/visual-regression
superseded_by: null
negative_knowledge: true
---

# 0073. Offline Component Storybook and Hermetic Visual Regression Pipeline

- Status: Accepted
- Date: 2026-11-15
- Deciders: Nuo Architecture Working Group, TUI Presentation Team, Quality Engineering
- Consulted: Interface, Usability, Runtime Teams
- Complements: [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md)

---

## Context and Problem Statement

Following the establishment of the pure declarative component paradigm in [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), the presentation layer requires a modern, uncompromising verification workflow.

Prior to this decision, UI/UX verification in Nuo suffered from severe systemic deficiencies:

1. **Full-Stack Dependency for Visual Polish**:
   Verifying the layout, typography, or interaction feel of components (e.g., streaming code diffs, AskUser approval options, quota exhaustion sheets) required booting the entire application topology. Developers had to coordinate background server daemons, authenticate sessions, and contrive specific prompt sequences. This made edge-case verification (e.g., 1000-line diffs, 429 quota exhaustion, narrow-terminal truncation) virtually impossible to verify quickly.
2. **Ad-Hoc Environment Hacks**:
   Developers resorted to hardcoded, transient environment variables (e.g., `NUO_DEV_TOAST`, `NUO_DEV_TOAST_PINNED`) to force components to render. These hacks created dead code, leaked developer switches into production paths, and could not scale across diverse components.
3. **The Plaintext Snapshot Blind Spot**:
   Existing Tier 2 snapshot tests (`snapshot_tests.rs`) utilized `nuotc::TestTerminal` and `insta` to assert on rendered ASCII text. Plaintext snapshots completely ignore ANSI 256/TrueColor foreground and background palettes, bold/dim text weights, inverted highlights, and cursor styles. A severe visual regression (such as alert text inadvertently rendering in muted grey instead of high-contrast red) went completely unnoticed by CI.

Nuo requires an offline, hermetic developer workbench and an automated visual regression pipeline that bridges immediate human ergonomic review with CI enforcement.

---

## Decision Drivers

- **Sub-10ms Hermetic Developer Loop (`[INV-TUI-STORY-01]`)**: Developers must inspect, navigate, and interact with any UI component offline in sub-10ms without launching `nuo-server`, local daemons, or network links.
- **Exhaustive Canonical Scenarios (`[INV-TUI-STORY-02]`)**: Components must declare standardized edge-case fixtures (Happy Path, Extreme Overflow, Narrow Truncation, Degraded Error, In-Flight Streaming) alongside their source files.
- **Style-Preserving Visual Regression (`[INV-TUI-STORY-03]`)**: Automated CI snapshot assertions must evaluate cell-level color, background, and modifier state, ending the plaintext snapshot blind spot.
- **Zero Production Binary Bloat**: Storybook harnesses, mock scenario fixtures, and offline dev tools must be conditionally compiled under `feature = "dev-storybook"` and completely excised from production release binaries.

---

## Architectural Decisions

### 1. The Component Story Specification

Components implementing `Component` ([ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md)) declare a static suite of canonical scenarios via the `StorybookComponent` trait:

```rust
pub struct Story<P, S> {
    pub name: &'static str,
    pub description: &'static str,
    pub props: P,
    pub initial_state: S,
}

pub trait StorybookComponent: Component {
    /// Category partition ("Overlays", "Disclosure", "Chrome", "Tools").
    fn category() -> &'static str;
    /// Component identity name.
    fn name() -> &'static str;
    /// The suite of canonical edge-case scenarios.
    fn stories() -> Vec<Story<Self::Props, Self::State>>;
}
```

Every component suite must cover the **5 Canonical Archetypes**:
1. **Standard (Happy Path)**: Normal baseline parameters.
2. **Massive Overflow**: 1000-line diffs, 50-item lists, virtual scroll stress tests.
3. **Narrow Truncation**: Viewport constrained to 60 or 80 columns, verifying text wrapping and path contraction.
4. **Degraded Error**: Network timeouts, 429 quota exhaustion, non-zero process exits.
5. **In-Flight / Streaming**: Active reasoning spinners, streaming token progress, partial state.

### 2. The Interactive Offline Workbench (`nuo dev storybook`)

We introduce an offline full-screen TUI workbench binary (`nuo-storybook`), accessible in debug builds as `nuo dev storybook` or via `cargo run -p nuo-tui --features dev-storybook --bin storybook`:

```text
┌─ nuo storybook ─────────────────────────────────────── [Theme: Dark] [Size: 80x24] ┐
│ 🔍 Search stories...     │ Virtual Viewport Box (80 x 24)                          │
│                          │ ┌─────────────────────────────────────────────────────┐ │
│ ▼ Overlays               │ │ ⚠ Dangerous Operation Detected                      │ │
│   ● AskUserModal         │ │                                                     │ │
│     - Standard           │ │ Command: `rm -rf /`                                 │ │
│   > - High-Risk Bash     │ │ Reason: Recursive deletion requested                │ │
│     - Multi-choice       │ │                                                     │ │
│     - Super Long Reason  │ │ [1] Deny (Recommended)  [2] Allow Once              │ │
│   ● QuotasModal          │ └─────────────────────────────────────────────────────┘ │
├──────────────────────────┴─────────────────────────────────────────────────────────┤
│ [Tab] Nav/Stage | [Space] Interact | [T] Theme | [V] Viewport Box | [Q] Quit       │
└────────────────────────────────────────────────────────────────────────────────────┘
```

#### Core Workbench Capabilities:
- **Virtual Viewport Boxing**: The `[V]` key toggles between `Full`, `80x24`, `100x30`, and `120x40`. A visual dashed perimeter constrains component rendering, instantly revealing clipping and wrap anomalies.
- **Live Theme Hot-Swapping**: The `[T]` key cycles between `Dark`, `Light`, and `High-Contrast` themes in real time, validating contrast ratios across terminal color schemes.
- **Interactive Pass-Through**: Pressing `<Space>` yields input focus to the component's `update(msg)` handler. Developers interact with cursor movement, scroll dampening, and selection keys directly in the sandbox; `Esc` returns to workbench control.
- **Dev-Loop Continuity**: Preserves the last-selected component and story index in a local temporary cache. Compiling with `cargo watch` resumes directly at the active component.

### 3. Dual-Engine Visual Acceptance & Styled Snapshots

The same `Story` fixtures that drive the interactive workbench automatically drive Tier 2 CI regression testing:

1. **Cell-Level Styled Grid Serialization**:
   We extend snapshot capture beyond symbols to encode foreground, background, and modifier attributes:
   $$\text{CellFingerprint} = (\text{char}, \text{fg\_color}, \text{bg\_color}, \text{modifiers})$$
2. **Automated CI Golden Suite**:
   A single test (`tests/storybook_visual_regression.rs`) iterates through `StoryRegistry::all()` across standard terminal dimensions (`80x24`, `120x30`) and asserts snapshots via `insta`. Zero boilerplate is required for newly authored components.
3. **PR Artifact Generation (SVG / ANSI)**:
   The harness supports an export mode (`nuo dev export <dir>`), generating deterministic terminal snapshots for inclusion in pull request descriptions.

---

## Normative Invariants Established

- **`[INV-TUI-STORY-01]` Offline Hermetic Isolation**:
  Storybook execution and component story initialization must execute in pure memory without launching `nuo-server`, establishing IPC sockets, or querying remote networks.
- **`[INV-TUI-STORY-02]` Exhaustive Canonical Scenarios**:
  Components designated as core interaction surfaces must maintain canonical stories covering standard, overflow, narrow-geometry, and error scenarios.
- **`[INV-TUI-STORY-03]` Style-Preserving Snapshot Parity**:
  Component visual regression snapshots must assert on complete styled cell buffers (characters, foreground, background, modifiers), strictly prohibiting plain unstyled text assertions for UI regression gates.

---

## Consequences

### Positive
- **Lightning Feedback Loop**: Sub-second inner-loop visual tweaking replaces cumbersome multi-step full-app reproduction.
- **Single Source of Truth**: Story fixtures serve simultaneously as manual interactive sandboxes and automated CI regression baselines.
- **Total Elimination of Plaintext Blind Spots**: Visual style regressions (contrast, color shifts, weight anomalies) are caught automatically in CI.
- **Zero Production Footprint**: Strict conditional compilation (`dev-storybook`) prevents test fixtures and sandbox code from shipping in public release binaries.

### Negative & Trade-offs
- **Fixture Maintenance**: As component Props evolve, their canonical stories must be updated alongside them.
- **Terminal Emulator Color Variance**: Cross-platform snapshot rendering requires normalized color quantization in headless buffers.

---

## Rejected Alternatives & Negative Knowledge

- **Ad-Hoc Developer Environment Variables (`NUO_DEV_*`)**: Formally abolished. Transient environment flags create hidden control flows, fail to test edge geometries, and pollute application state.
- **External Mock Daemons**: Rejected because orchestrating mock daemons still incurs process spawn overhead, socket delays, and complex IPC lifecycle coordination, violating the sub-10ms feedback requirement.
