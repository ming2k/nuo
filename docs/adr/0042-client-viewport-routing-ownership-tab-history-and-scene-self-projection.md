---
id: ADR-0042
title: "Client Viewport Routing Ownership, Tab-Autonomous Scene Stacks, and Domain Scene Self-Projection"
status: accepted
date: 2026-10-19
scope: tui/nuo-tui, architecture/spatial, presentation/navigation, domain/scenes
superseded_by: null
negative_knowledge: true
---

# 0042. Client Viewport Routing Ownership, Tab-Autonomous Scene Stacks, and Domain Scene Self-Projection

- Status: Accepted
- Date: 2026-10-19
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, Usability, Terminal, Runtime, and Core Infrastructure Teams
- Informed: System Architects, Release Engineering
- Complements: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md), [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md)

---

## Context and Problem Statement

Following the establishment of the Three-Tier Header Topology ([ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)) and Thread Command Purity ([ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md)), an essential architectural distinction required formal ratification: the separation between **Spatial Layout Stratification** (where components visually reside) and **Architectural Ownership** (which subsystem possesses behavioral authority over each plane).

In early implementations, the following architectural ambiguities emerged:

1. **Conflation of Viewport Routing with Domain Execution**:
   The internal navigation stack (`history: Vec<SceneKind>`) was occasionally conflated with server-side dialogue state. When an operator drilled into a subagent task inspection or file diff within a thread, questions arose as to whether the server session had branched, paused, or altered its causality graph. In reality, drill-in inspection is strictly an ephemeral client-side viewport routing transition.
2. **Ambiguity in Scene Context Head Ownership**:
   While Rows 1 and 2 of the header were treated as container-level chrome, Row 3 was frequently implemented through centralized client match arms that introspected and reformatted scene internals. This introduced tight coupling: adding a new scene required modifying the client header renderer rather than allowing the scene to project its own domain metadata autonomously.
3. **Conceptual Conflation of Subagent Drill-in with Thread Forking**:
   Operators and maintainers questioned whether drilling into a spawned subagent's execution stream should be termed a "thread fork". Conflating subagent inspection (a hierarchical, bounded delegation that returns a result to the parent thread) with a thread fork (an orthogonal, divergent session branch that creates a sovereign peer thread) introduced cognitive friction and degraded the integrity of the domain model.

To establish an uncompromising, future-proof terminal interface free of legacy compromises, we must formalize the architectural ownership boundaries across the client viewport shell and domain scenes, codify tab-autonomous scene navigation, and eliminate terminology ambiguity.

---

## Decision Drivers

- **Client Viewport Shell Sovereignty (`[INV-OWN-01]`)**: Row 1 (Client TabBar) and Row 2 (Tab Navigation & History Stack) MUST be strictly owned by the Client Viewport Shell. The server runtime and dialogue harness MUST NOT track or depend upon client-local stack cursors, breadcrumbs, or Back/Forward transitions.
- **Domain Scene Self-Projection (`[INV-OWN-02]`)**: Row 3 (Scene Context Head) and the Content Body Canvas MUST be owned by the projected Domain Scene. The Client shell MUST allocate layout geometry but MUST NOT introspect or hardcode scene-specific metadata formatting.
- **Autonomous Tab History Isolation (`[INV-NAV-01]`)**: Each `ClientTab` MUST independently own its navigation history stack (`history: Vec<SceneEntry>`, `cursor: usize`). Tab switching MUST NOT leak, clobber, or interleave navigation history across parallel tabs.
- **Root Esc Invariant (`[INV-NAV-02]`)**: Pressing `Esc` or `Alt+Left` at the root of a tab's history stack (`cursor == 0`) MUST remain within the root scene. Viewport or tab closure MUST require an explicit client-tier action (`Alt+W` or `/close`).
- **Subagent Inspection vs. Thread Fork Orthogonality (`[INV-SUB-01]`, `[INV-FORK-01]`)**:
  - Subagent drill-in MUST be designated as **`subagent`** (displayed as `subagent: <role>`), representing an in-tab task inspection push onto the active tab's scene stack.
  - Thread forking MUST remain strictly designated as **`Thread Fork`** (`/fork`), creating a sovereign top-level `ClientTab` on Row 1 with an independent causality chain.

---

## Considered Options

### Option 1: Monolithic Client Chrome (Client Renders Everything)
- Client container introspects active session, subagent pointers, and workspace roots, rendering all three header rows centrally.
- *Assessment*: Rejected. Violates domain cohesion. Adding or refactoring domain scenes requires modifying central chrome files, leading to fragile match blocks and header layout bloat.

### Option 2: Server-Synchronized Viewport Navigation
- Sync the operator's current scene stack and cursor to the server daemon as part of the session state.
- *Assessment*: Rejected. Viewport navigation is purely ephemeral presentation state. Persisting or synchronizing drill-in inspection pollutes the durable execution ledger and breaks multi-client observation paradigms.

### Option 3: Two-Tier Architectural Ownership with 2+1 Header Topology (Chosen)
- Formulate the system into two distinct architectural planes:
  1. **Client Viewport Shell**: Owns Row 1 (Multi-Tab Orchestration) and Row 2 (Tab-Local Navigation History & Breadcrumbs).
  2. **Domain Scene Projection**: Owns Row 3 (Scene-Autonomous Context Head) and the Body Content Canvas.
- Enforce strict nomenclature: `subagent` for in-tab child delegation inspection, and `/fork` for sovereign thread divergence.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative architectures were evaluated and rejected:

1. **Rejected: Calling Subagent Drill-in "Thread Fork"**
   - *Reason*: A subagent is a delegated child task spawned via `spawn_agent`. It carries strict parent-child causality, bounded tool capabilities, and must settle its summary back into the parent thread. A "fork" is a divergent Git-like branch creating an independent peer thread. Conflating the two misleadingly implies that the parent thread's conversation line was permanently split or severed.
2. **Rejected: Shared Global Router Scene Stack**
   - *Reason*: Storing navigation history globally in `SurfaceRouter` leaks navigation context across tabs. Drilling into a subagent on Tab 1 and then switching to Tab 2 causes `Esc` on Tab 2 to jump back to Tab 1's subagent view.
3. **Rejected: Esc at Stack Root Closing the Tab**
   - *Reason*: Pressing `Esc` in a text composer or idle prompt is a standard cancellation muscle memory. Evicting the entire tab upon pressing `Esc` at `cursor == 0` causes accidental workspace destruction and user disorientation.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Architectural Ownership Matrix (`[INV-OWN-01]`, `[INV-OWN-02]`)

```text
┌────────────────────────────────────────────────────────┐
│ Client Viewport Shell (Client Responsibility)          │
│  ├─ Row 1: Global TabBar (Multi-Tab Orchestration)      │
│  └─ Row 2: Navigation & History Bar (Tab Stack Routing)│
├────────────────────────────────────────────────────────┤
│ Domain-Autonomous Scene (Scene Responsibility)         │
│  ├─ Row 3: Scene Context Head (Self-Projected Metadata)│
│  └─ Body:  Content Canvas (Self-Projected Content)     │
└────────────────────────────────────────────────────────┘
```

| Layer | Component | Owner | Responsibilities |
| :--- | :--- | :--- | :--- |
| **Row 1** | **Client Global TabBar** | Client Shell | Tab lifecycle (`OpenTab`, `CloseTab`, `FocusTab`), `Alt+1..9` direct jumping, background activity indicators (`●`), global `C-x menu` affordance. |
| **Row 2** | **Tab Navigation & History Bar** | Client Shell (Tab-Local) | Browser-grade history controls (`< Back`, `> Fwd`), stack depth tracking, interactive breadcrumb path (`thread > subagent: explore > diff`). |
| **Row 3** | **Scene Context Head** | Active Scene | Autonomous domain metadata self-projection (Thread: session UUID, role badge, workspace; Subagent: role, parent ID, sandbox policy; Dashboard: cluster health). |
| **Body** | **Polymorphic Content Canvas** | Active Scene | Domain transcript, interactive step disclosures, diff tables, or metrics waterfall. |

### 2. Scene Nomenclature & Breadcrumbs (`[INV-SUB-01]`, `[INV-FORK-01]`)

1. **Main Conversation Scene**:
   - Nomenclature: **`thread`** (in Row 2 breadcrumb: `thread`).
   - Role: Root scene of a conversation tab (`cursor = 0`).
2. **Subagent Task Inspection Scene**:
   - Nomenclature: **`subagent`** (in Row 2 breadcrumb: `thread > subagent: <role>`).
   - Role: Drill-in inspection of a spawned child agent task. Sits at `cursor = 1` within the tab's navigation stack.
   - Context Head (Row 3): Displays `subagent: <role>`, parent task ID, and execution status.
3. **Thread Divergent Branch**:
   - Nomenclature: **`Thread Fork`** (Command: `/fork`).
   - Role: Creates a new sovereign `ClientTab::Thread` on Row 1 (e.g. `[2* 🔨refactor-db:fork]`), leaving the original tab's causality intact.

### 3. Navigation Stack Contract

```rust
pub struct ClientTab {
    pub kind: TabKind,
    pub title: String,
    pub history: Vec<SceneEntry>,
    pub cursor: usize,
}

pub struct SceneEntry {
    pub kind: SceneKind,
    pub label: String,
}

pub enum SceneKind {
    Thread,
    Subagent,
    TaskDiff,
    Dashboard,
    Settings,
}
```

- **Drill-in (`PushScene`)**: When the operator activates a subagent card, diff block, or trace waterfall, the active tab truncates forward history (`cursor + 1..`), pushes the new `SceneEntry`, and increments `cursor`.
- **Back Navigation (`PopScene`)**: Pressing `Esc` or `Alt+Left` decrements `cursor` if `cursor > 0`, restoring the prior scene and composer state with zero layout shift.
- **Forward Navigation (`ForwardScene`)**: Pressing `Alt+Right` increments `cursor` up to `history.len() - 1`.

---

## Invariants & Boundaries

- **[INV-OWN-01] Strict Client Viewport Shell Ownership**: Row 1 and Row 2 MUST be owned, rendered, and arbitrated exclusively by the Client Viewport Shell. Server daemons and dialogue harness drivers MUST remain agnostic of tab configurations and stack cursors.
- **[INV-OWN-02] Domain Scene Self-Projection**: Row 3 MUST be rendered via polymorphic delegation to the active `Scene::render_head()`. The Client shell MUST NOT format or introspect domain metadata directly.
- **[INV-NAV-01] Autonomous Tab History Isolation**: Each `ClientTab` MUST maintain its own isolated `history` vector. Navigating within one tab MUST NOT alter or leak into the history stack of any other tab.
- **[INV-NAV-02] Root Esc Preservation**: Pressing `Esc` or `Alt+Left` when `cursor == 0` MUST NOT close, detach, or switch the tab. Tab closure MUST be exclusively dispatched via `Alt+W` or `/close`.
- **[INV-SUB-01] Subagent Drill-in Nomenclature**: Child tasks spawned via `spawn_agent` MUST be designated as `subagent` in all breadcrumb headers and context labels.
- **[INV-FORK-01] Sovereign Thread Forking**: Forking a thread MUST spawn a top-level peer `ClientTab` on Row 1, never an in-tab sub-scene.

---

## Positive Consequences

- Unambiguous mental model: Operators easily distinguish between temporary task inspection (`subagent`) and permanent session branching (`/fork`).
- Zero architectural leakage: Background sessions and multi-client connections operate on stable domain primitives without entangled presentation state.
- Component extensibility: New scenes (such as full-screen diff viewers or memory inspectors) can be added simply by implementing `render_head()` and pushing an entry to the tab stack, with zero modifications to client chrome routing.

---

## Links

- Related ADRs: [ADR-0038](0038-thread-entity-server-ssot-fanout-and-orthogonal-action-lifecycle.md) (Thread SSOT), [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md) (ClientTab Workspace), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md) (Three-Tier Header Topology), [ADR-0041](0041-thread-command-purity-viewport-workspace-separation-and-tab-management-topology.md) (Thread Command Purity)
- Related Modules: `nuo-tui::surfaces`, `nuo-tui::view_header`, `nuo-tui::event_loop::actions`
