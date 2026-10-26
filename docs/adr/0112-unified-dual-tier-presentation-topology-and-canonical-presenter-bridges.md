---
id: ADR-0112
title: "Unified Dual-Tier Presentation Topology, Canonical Presenter Bridges, and Elimination of the Storybook Schism"
status: accepted
date: 2026-12-06
scope: presentation/nuo-ui-primitives, presentation/nuo-view, host/nuo, ui/declarative, interaction/affordance
superseded_by: null
negative_knowledge: true
---

# 0112. Unified Dual-Tier Presentation Topology, Canonical Presenter Bridges, and Elimination of the Storybook Schism

- Status: Accepted
- Date: 2026-12-06
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Architecture, Terminal Rendering, Core Runtime, Interaction Teams
- Informed: System Architects, Release Engineering
- Amends: [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md), [ADR-0107](0107-two-tier-presentation-purification-and-nuo-present-retirement.md), [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md)

---

## Context and Problem Statement

Following the extraction of pure zero-IO presentation crates (`nuo-ui-primitives` and `nuo-view` in ADR-0107) and the introduction of the declarative component paradigm (`Component` trait / TEA in ADR-0072 and ADR-0073), Nuo achieved zero-IO presentation purity. 

However, as the application evolved, a severe structural defect emerged across the presentation layer: **The Dual-Track Schism (双轨断层)**:

1. **Production Pipeline vs. Storybook Orphan Duplication**:
   - `crates/nuo/src/shell/declarative/` maintained isolated TEA components (`AskUser`, `ToolCard`, `NoticeCard`) to power the offline Storybook workbench and unit snapshot tests.
   - Meanwhile, production session transcripts and overlays rendered tool executions via `crates/nuo/src/shell/disclosure/renderers/tools.rs` (`draw_tool_step`) and notices via `crates/nuo-view/src/components/notice.rs`.
   - While `AskUser` was successfully bridged into production (`event_loop/render.rs` calling `AskUser::render_with_tree`), `ToolCard` and `NoticeCard` drifted into parallel implementations. Changes made to production diff rendering did not reflect in Storybook, and improvements in declarative cards did not project into production transcripts.
2. **The "Everything Must Be TEA" Extremism Trap**:
   - Confusion arose over whether *every single UI element* (such as a 20-line copy toast, a simple list item in `/models`, or a static breadcrumb) must be forced into the TEA five-piece model (`Props`, `State`, `Msg`, `Cmd`, `update`, `render`).
   - Dogmatically forcing TEA on passive visual atoms generates massive boilerplate taxes (message cascading and matching overhead) and destroys virtual-scrolling performance in large transcripts.
3. **The Terminal Hit-Testing Seam**:
   - Unlike browsers with native DOM event bubbling, terminal UIs rely on retained canvas diffing (`nuotc`) and spatial coordinate hit registries (`LayoutMap` and `ComponentTree`).
   - Pure TEA `Component::render` only receives `(Props, State, Frame, Rect, Theme)`, preventing deep spatial coordinate registration unless a standardized adapter bridge is ratified.

We require an uncompromising architectural clean break that codifies a unified two-tier mental model and permanently eliminates the Storybook schism via canonical presenter bridges.

---

## Decision Drivers

- **Single Source of Truth**: Eliminate dual-track code drift. What is rendered in Storybook must be identical to what is rendered in production.
- **Layered Mental Model Purity**: Standardize when to use stateless pure presenters (Tier A) versus autonomous sovereign TEA state machines (Tier B).
- **Zero-Boilerplate for Stateless Atoms**: Ban dogmatic TEA wrapping of passive visual primitives.
- **Hermetic vs. Connected Dual-Mode Adaptability**: Tier B components must render hermetically in offline memory while offering zero-overhead connected bridges for production layout maps and spatial hit-testing.
- **Strict Anti-Regression Firewalls**: Enforce that all Tier B components remain fully verifiable in offline storybooks without runtime daemons or network I/O.

---

## Architectural Decisions

### 1. The Unified Two-Tier Presentation Topology (`[INV-TUI-TIER-01]`)

All user interface elements across the Nuo presentation substrate are strictly partitioned into two architectural tiers:

```text
┌────────────────────────────────────────────────────────────────────────┐
│               Nuo Unified Two-Tier Presentation Topology               │
├────────────────────────────────────────────────────────────────────────┤
│ Tier B: Sovereign Autonomous TEA Containers (The Elm Architecture)     │
│ - Trait: `Component` (Props, State, Msg, Cmd, update, render)          │
│ - Characteristics: Owns local volatile interaction state (focus,       │
│   scroll offsets, sub-step wizards), guarded re-entrancy (ADR-0110),   │
│   and discrete message handling.                                       │
│ - Scope: Interactive cards (`AskUser`, `ToolCard`, `NoticeCard`),      │
│   configuration editors, and Storybook workbenches.                    │
├────────────────────────────────────────────────────────────────────────┤
│ Tier A: Stateless Pure Presenters & Primitives (Zero-IO Functions)     │
│ - Contract: Pure mathematical projection: f(Props, Theme) -> Visual    │
│ - Characteristics: Completely stateless; no private State, no Msg.    │
│   Invoked directly per-frame with zero allocation or boilerplate tax.  │
│ - Scope: Atoms and Presenters (`Button`, `Keycap`, `ListRow`, `Toast`, │
│   `TurnBand`, `MessageBody`, `DiffLine`, `MarkdownTable`).             │
└────────────────────────────────────────────────────────────────────────┘
```

#### Rule of Application:
- **Use Tier A (Stateless Presenters)** when an element's presentation is an immutable projection of external data and requires no local volatile interaction variables.
- **Use Tier B (TEA Containers)** strictly when an element encapsulates independent multi-step interactions, local focus movement, or guarded async execution lifecycles.

### 2. Canonical Presenter Bridges (`[INV-TUI-TIER-02]`)

To eliminate the dual-track schism, all Tier B components must serve as the single authoritative presentation source for their domain through **Canonical Presenter Bridges**:

1. **Domain Extraction Bridge**:
   Every Tier B component must provide zero-cost conversion from domain wire entities to its immutable `Props`:
   ```rust
   impl ToolCardProps {
       pub fn from_transcript_message(
           msg: &TranscriptMessage,
           workspace_root: Option<&std::path::Path>,
       ) -> Option<Self>;
   }
   ```
2. **Dual-Mode Rendering Bridge (`[INV-TUI-TIER-03]`)**:
   Tier B components must implement two complementary render interfaces:
   - **Hermetic Mode (`Component::render`)**: Used by offline Storybook runners, headless unit test snapshots, and isolated canvas previews. Operates in pure memory with zero external dependencies.
   - **Connected Mode (`render_with_tree` / `render_with_layout`)**: Used by the live production event loop (`event_loop/render.rs`). Accepts production spatial hit registries (`&mut ComponentTree` or `&mut LayoutMap`), delegating internal painting to the canonical presenter while registering interactive hit rects without code duplication.

```text
       ┌────────────────────────┐
       │   TranscriptMessage    │
       └───────────┬────────────┘
                   │ ToolCardProps::from_transcript_message
                   ▼
       ┌────────────────────────┐
       │     ToolCardProps      │
       └─────┬────────────┬─────┘
             │            │
   Hermetic  │            │ Connected
   (Storybook)            │ (Production Event Loop)
             ▼            ▼
 ┌─────────────────┐  ┌──────────────────────────────────┐
 │Component::render│  │ToolCard::render_with_layout     │
 │(Memory Frame)   │  │(Frame + LayoutMap + Hit Testing) │
 └─────────────────┘  └──────────────────────────────────┘
```

### 3. Elimination of Orphan Implementations

- The parallel, diverging diff and command block drawing logic in `ToolCard` is permanently unified with `nuo-view::tools::diff` and `nuo-view::tools::execute_command`.
- Storybook stories for `ToolCard` and `NoticeCard` exercise the exact same presenter logic that paints production transcript steps.
- Any attempt to introduce an offline Storybook component that duplicates or forks production rendering logic is an architectural violation rejected by review.

---

## Normative Invariants

- **`[INV-TUI-TIER-01] Two-Tier Presentation Topology`**:
  Presentation code MUST strictly adhere to the two-tier mental model. Stateless visual atoms MUST remain pure projection functions without artificial TEA wrapper boilerplate. Sovereign multi-state interactive entities MUST implement the `Component` contract.
- **`[INV-TUI-TIER-02] Canonical Presenter Bridge Invariant`**:
  Offline Storybook components and live production presentation pipelines MUST share a single authoritative presenter engine through canonical bridges. Maintaining parallel, drifting implementations across production and Storybook is strictly prohibited.
- **`[INV-TUI-TIER-03] Dual-Mode Rendering Adaptation`**:
  Tier B components MUST implement both hermetic memory rendering (`Component::render`) and zero-overhead connected spatial rendering (`render_with_tree` / `render_with_layout`) to ensure 100% fidelity between offline test snapshots and production runtime frames.

---

## Consequences

- **Positive**:
  - **Zero Code Drift**: Eliminates the risk of Storybook showing a phantom UI that does not match production behavior.
  - **Crystal-Clear Mental Model**: Developers never wonder whether to write a TEA component or a function: stateful containers go to Tier B (`declarative/`), stateless visual atoms go to Tier A (`primitives/` or `presenters`).
  - **Preserved Performance**: No message-cascading tax or allocation overhead on high-frequency transcript streaming lines.
  - **100% Hermetic Testability**: Every Tier B component remains instantaneously testable in sub-millisecond offline unit tests.
- **Negative & Trade-offs**:
  - Adding a new Tier B component requires defining both `Props::from_*` extraction and the connected rendering bridge.

---

## Rejected Alternatives & Negative Knowledge

### 1. Dogmatic "Everything Is TEA" Extremism
- *Failure Mode*: Wrapping every breadcrumb, toast, and diff line in TEA boilerplate multiplied lines of code by 5x and required bubbling trivial clicks through 4 levels of parent message enums. Retained mode terminal layout maps already handle hit-testing far more efficiently than artificial component message trees.

### 2. Standalone Toy Storybook Components
- *Failure Mode*: Writing simplified "toy" components purely for Storybook display that do not share underlying code with production views leads to rapid divergence: the Storybook looks pristine while production crashes or degrades.

---

## Links

- Amends: [ADR-0072](0072-pure-declarative-component-paradigm-and-tea-single-direction-dataflow-topology.md), [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md), [ADR-0107](0107-two-tier-presentation-purification-and-nuo-present-retirement.md), [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md)
- Reference Docs: [`docs/reference/tui/interaction.md`](../reference/tui/interaction.md), [`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md)
