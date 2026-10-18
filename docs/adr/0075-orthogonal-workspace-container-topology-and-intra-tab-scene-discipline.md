---
id: ADR-0075
title: "Orthogonal Workspace Container Topology, Strict Intra-Tab Scene Discipline, and Zero-Conflation Navigation Architecture"
status: accepted
date: 2026-11-16
scope: tui/nuo-tui, presentation/chrome, interaction/tabs, architecture/surfaces
superseded_by: null
negative_knowledge: true
---

# 0075. Orthogonal Workspace Container Topology, Strict Intra-Tab Scene Discipline, and Zero-Conflation Navigation Architecture

- Status: Accepted
- Date: 2026-11-16
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Interaction Design, Product Architecture
- Informed: System Architects, Release Engineering
- Amends: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md), [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md)

---

## Context and Problem Statement

Following the establishment of the Three-Tier Spatial Header ([ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md)) and Tab-Autonomous Navigation Stacks ([ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md)), a critical architectural conflation persisted across the presentation and routing layers of `nuo-tui`:

1. **Conflation of Horizontal Workspaces with Vertical Navigation Depth**:
   Top-level sovereign workspaces (`Dashboard`, `Settings`) remained flatly lumped together in `enum SceneKind` alongside hierarchical drill-in destinations (`Subagent`).
2. **Cross-Container Scene Stack Pollution**:
   Because `switch_scene` accepted any `SceneKind` and pushed it directly onto the active tab's history, calling `switch_scene(Dashboard)` while on a thread tab pushed `Dashboard` into that thread's local history, rendering absurd breadcrumbs such as `‹ thread › dashboard`.
3. **Dual Lifecycle Ambiguity for Dashboard and Settings**:
   The router simultaneously treated opening the dashboard as an autonomous Tab container allocation (`open_tab(ClientTab::dashboard())`) and as an in-place scene transition. This caused routing bifurcation where tests and legacy paths bypassed tab allocation entirely.
4. **Scene Inflation Anti-Pattern**:
   Hypothetical architectural extensions contemplated elevating low-level widgets (such as file diffs) into navigation "Scenes" on the router stack (`thread > subagent > diff`), conflating nested UI components with full-canvas viewport scenes.

A clean break from these legacy assumptions is required to establish an uncompromising, 10-year durable spatial topology.

---

## Decision Drivers

- **Orthogonal Dimensionality**: Strictly segregate the Horizontal Axis (Client Workspaces / Tabs on Row 1) from the Vertical Axis (Intra-Tab Navigation Depth on Row 3).
- **Zero Cross-Container Pollution**: Guarantee that navigating to or opening a top-level workspace (e.g., Dashboard) strictly allocates or focuses a sovereign Tab container and can never pollute an existing thread's scene stack.
- **Strict Intra-Tab Scene Discipline**: Scene stacks (`tab.history`) belong exclusively inside a tab container to track drill-in depth within that workspace.
- **Anti-Inflation Component Boundary**: Maintain an impenetrable firewall between full-canvas Scenes and intra-scene Components (such as code diffs, tool cards, and reasoning disclosures).
- **Mechanisms Over Exhortations**: Enforce container-scene compatibility invariants mechanically within the type and container system rather than through doc-comment guidelines.

---

## Architectural Decisions

```text
═══════════════════════════════════════════════════════════════════════════════
  HORIZONTAL AXIS: WORKSPACES (TABS) — Managed by Client Shell (Row 1)
═══════════════════════════════════════════════════════════════════════════════
       Tab 0: Thread           Tab 1: Dashboard          Tab 2: Settings
  ┌──────────────────────┬──────────────────────┬──────────────────────┐
  │ [Thread: Refactor]   │ [Dashboard]          │ [Settings]           │
  └──────────────────────┴──────────────────────┴──────────────────────┘
             │                      │                      │
═════════════╪══════════════════════╪══════════════════════╪═══════════════════
  VERTICAL AXIS: DEPTH (SCENE STACKS) — Strictly Intra-Tab (Row 3)
═══════════════════════════════════════════════════════════════════════════════
             ▼                      ▼                      ▼
    Thread Scene Stack      Dashboard Scene Stack   Settings Scene Stack
    (Vec<SceneKind>)        (Vec<SceneKind>)        (Vec<SceneKind>)
    ┌────────────────┐      ┌────────────────┐      ┌────────────────┐
d=0 │ 0: Thread      │  d=0 │ 0: Dashboard   │  d=0 │ 0: Settings    │
    ├────────────────┤      └────────────────┘      └────────────────┘
d=1 │ 1: Subagent(id)│
    └────────────────┘
```

### 1. Workspace Container Purity (`[INV-TAB-10]`)

- A `ClientTab` is strictly a top-level workspace container. The universe of workspaces is defined by typed variants:
  - `TabKind::Thread(String)`: Sovereign multi-turn conversation and task workspace.
  - `TabKind::Dashboard`: Cluster, server, session, and fleet orchestrator workspace.
  - `TabKind::Settings`: Client appearance, configuration, and provider settings workspace.
- Opening or focusing a workspace (via `C-x d`, `C-x ,`, or `/dashboard`) is exclusively a **Tab Container Operation** (`open_tab` or `focus_tab`). It MUST NEVER push a scene onto another tab's stack.

### 2. Container-Scoped Scene Discipline (`[INV-SCENE-02]`)

- Every `ClientTab` owns its own autonomous, isolated `history: Vec<SceneKind>`.
- Scene transitions (`push_scene`) are strictly **intra-tab**:
  - `TabKind::Thread` containers accept ONLY thread-compatible scenes (`SceneKind::Thread`, `SceneKind::Subagent`). Any attempt to push cross-container scenes (like `Dashboard` or `Settings`) is rejected by container invariants.
  - `TabKind::Dashboard` initializes its own scene stack with `[SceneKind::Dashboard]`. It is completely isolated from thread history.
  - `TabKind::Settings` initializes its own scene stack with `[SceneKind::Settings]`.
- Back navigation (`tab.back()`, `Esc`, `< Back`) operates strictly within the active tab's stack. Stepping back at depth 0 is a spent gesture and NEVER switches tabs or closes the container (`[INV-NAV-02]`).

### 3. Component Nesting Boundary (`[INV-COMP-01]`)

- Low-level interactive presentations—including Myers code diffs, command execution outputs, web search results, and reasoning disclosures—are strictly **Components nested within a Scene**.
- Components MUST NOT be elevated to full-canvas Scenes or pushed onto the router's navigation stack.
- Intra-component interactions (scrolling hunks, expanding/collapsing, copying) are handled locally by the component and do not modify the scene breadcrumbs on Row 3.

---

## Normative Invariants

- **`[INV-TAB-10] Workspace Container Purity`**: The Dashboard and Settings surfaces are sovereign Tab containers (`TabKind::Dashboard`, `TabKind::Settings`). Opening a workspace surface MUST allocate or focus its dedicated Tab container and MUST NOT push into another tab's history.
- **`[INV-SCENE-02] Container-Scoped Scene Discipline`**: Each `ClientTab` container strictly owns its own navigation stack. `ClientTab::push_scene` MUST reject cross-container scene injections (e.g. attempting to push `Dashboard` onto a `Thread` tab).
- **`[INV-COMP-01] Component Nesting Boundary`**: Visual inspectors for tool outputs (such as code diffs and process logs) MUST remain encapsulated as components within the active scene canvas and MUST NOT generate navigation stack entries or breadcrumb labels.

---

## Consequences

- **Positive**:
  - Unambiguous mental model: Tabs are what you work in; scene stacks are how deep you drill down within a thread.
  - Eradication of spurious breadcrumbs (`‹ thread › dashboard`).
  - Total isolation: Subagent task inspection in Tab 0 cannot leak into or affect Tab 1 (Dashboard).
  - Clean, predictable `Esc` behavior: `Esc` steps back inside the active tab; `C-x w` closes the tab.
- **Negative & Trade-offs**:
  - Code and tests that historically called `app.switch_scene(Dashboard)` on a single-tab app now allocate or focus the Dashboard tab, requiring tests to check the tab container state.

---

## Rejected Alternatives & Negative Knowledge

### 1. Elevating File Diffs to Navigation Scenes (Rejected Alternative)
- *Failure Mode*: Violates `[INV-COMP-01]`. Elevating diffs to scenes causes scene inflation, requiring bash outputs, search queries, and thought blocks to also be scenes, resulting in unreadable 4-tier breadcrumbs and hijacking universal `Esc` semantics.

### 2. Subagent as an Independent Tab Container (Rejected Alternative)
- *Failure Mode*: Subagents are transient child delegations tied to a single turn of a parent thread. Spawning tabs for ephemeral 5-second subtasks causes severe TabBar clutter and severs the visual causal link to the parent turn.

### 3. Universal Shared Scene Stack across Tabs (Rejected Alternative)
- *Failure Mode*: Violates `[INV-SCENE-02]`. When tabs share or cross-inject scenes, switching to Tab 1 and pressing `Esc` unexpectedly pops scenes that were pushed in Tab 0, violating viewport isolation.

---

## Links

- Related ADRs: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0042](0042-client-viewport-routing-ownership-tab-history-and-scene-self-projection.md), [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md)
- Related Modules: `nuo-tui::surfaces`, `nuo-tui::view_header`, `nuo-tui::event_loop::actions`
