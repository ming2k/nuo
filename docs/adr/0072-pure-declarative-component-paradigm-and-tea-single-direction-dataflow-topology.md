---
id: ADR-0072
title: "Pure Declarative Component Paradigm and TEA Single-Direction Dataflow Topology"
status: accepted
date: 2026-11-15
scope: tui/nuo-tui, tui/architecture, tui/components, tui/lifecycle
superseded_by: null
negative_knowledge: true
---

# 0072. Pure Declarative Component Paradigm and TEA Single-Direction Dataflow Topology

- Status: Accepted
- Date: 2026-11-15
- Deciders: Nuo Architecture Working Group, TUI Presentation Team, Interface Architecture Team
- Consulted: Runtime, Usability, Protocol Teams
- Complements: [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)

---

## Context and Problem Statement

Nuo's terminal user interface (`crates/nuo-tui`) was originally structured around an imperative, monolithic state carrier (`App`) with centralized event dispatch and a multi-thousand-line monolithic drawing pipeline (`event_loop/render.rs`). 

As Nuo expanded to accommodate high-density interaction surfaces (multi-turn reasoning disclosures, interactive AskUser permission sheets, streaming tool call diffs, quota dashboards, and multi-tab workflows), this legacy architecture generated severe engineering friction and maintenance bottlenecks:

1. **Monolithic State Contention & Borrowing Gymnastics**:
   Because all view layers borrow directly from the singular `&mut App`, rendering child components frequently encounters Rust aliasing restrictions. Developers were forced into defensive, error-prone maneuvers such as `std::mem::take(&mut app.layout_height_cache)` and complex field re-borrowing across frame transactions.
2. **Global Event Routing Fragility**:
   Keyboard and pointer events were predominantly handled by centralized match towers (e.g., `event_loop/actions/modals.rs`), where global action loops mutated internal component states across hierarchical boundaries. Components lacked sovereign local state transitions, causing unintended keybinding interference and high regression risk.
3. **Absence of Independent Testability**:
   No UI component could be instantiated or verified in isolation. Verifying simple visual adjustments (such as a 1px margin or border color on `AskUserModal`) required booting the entire application topology—including the daemon server (`nuo-server`), local IPC handshakes, and live model connections. This feedback latency (seconds to minutes) crippled UI/UX iteration and rendered edge-case verification unsustainable.

Nuo requires an uncompromising, future-proof architectural model that establishes absolute presentation purity, eliminates monolithic coupling, and provides sub-millisecond local iteration.

---

## Decision Drivers

- **Presentation Purity (`[INV-TUI-DECL-01]`)**: Views must be pure projections of immutable, typed properties: $\text{Frame} = f(\text{Props}, \text{Theme}, \text{Area})$. Views must hold zero references to global application carriers.
- **Sovereign Local Encapsulation (`[INV-TUI-DECL-02]`)**: Interactive components must encapsulate their local event transitions within discrete, typed message dispatch: $(\text{State}, \text{Msg}) \to (\text{NewState}, \text{Cmd})$.
- **Zero-Dependency Hermetic Isolation**: Every component must be constructible and renderable in pure memory without IPC, daemon servers, or background runtimes.
- **Progressive Strangler Migration (`[INV-TUI-DECL-03]`)**: Provide clean generational interop so existing monolithic modules can be methodically replaced slice-by-slice without destabilizing production releases.

---

## Architectural Decisions

### 1. The Elm Architecture (TEA) Component Contract

We establish the canonical declarative contract for `crates/nuo-tui`:

```rust
/// The sovereign declarative component contract in Nuo TUI.
pub trait Component {
    /// Pure, immutable presentation inputs supplied by the parent/host.
    type Props: Clone + Send + 'static;
    /// Self-contained local interaction state (cursor, scroll, selection).
    type State: Default + Clone + Send + 'static;
    /// Discrete local event message family.
    type Msg: Send + 'static;
    /// Outward outcome or command emitted back to the parent.
    type Cmd: Send + 'static;

    /// Pure rendering projection into the terminal canvas.
    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut nuotc::Frame<'_>,
        area: nuotc::Rect,
        theme: &crate::theme::Theme,
    );

    /// Sovereign, deterministic state transition.
    fn update(
        props: &Self::Props,
        state: &mut Self::State,
        msg: Self::Msg,
    ) -> Option<Self::Cmd>;
}
```

- **Props**: Immutable data pushed downward. Components cannot mutate their Props.
- **State**: Strictly local volatile interaction variables (e.g., active dropdown index, focused button).
- **Msg**: Discrete, strongly-typed user intentions (e.g., `AskUserMsg::SelectNext`, `AskUserMsg::Confirm`).
- **Cmd**: Typed outward actions communicated to the parent orchestration layer (e.g., `AskUserCmd::Resolve(Decision)`).

### 2. Elimination of the Monolithic `render.rs`

The legacy `compose_frame` routine in `event_loop/render.rs` is demoted to a declarative composition coordinator:
- It constructs the top-level immutable `AppProps` snapshot once per frame.
- It delegates rendering downwards through the component tree (`Header`, `TranscriptArea`, `OverlaysContainer`, `Footer`).
- Subcomponents render strictly into their assigned `Rect` boundaries via `Component::render`. Direct mutable access to `App` inside component draw routines is strictly abolished.

### 3. Progressive Strangler Migration

Migration proceeds across three isolated domain slices:
1. **Slice 1: Overlays & Modals**: `AskUser`, `PermissionSheet`, `QuotaModal`, `CommandPalette`.
2. **Slice 2: Disclosure & Messages**: `ToolStep` (Diff, Shell Exec, Ast), `ReasoningBlock`, `CompactedContext`.
3. **Slice 3: Chrome & Global Frame**: `TabBar`, `ActivityBar`, `TasksBar`, `Composer`.

During migration, a thin adapter bridge allows legacy surfaces to mount declarative components and vice-versa, guaranteeing zero disruption to master branch stability while legacy code is deleted.

---

## Normative Invariants Established

- **`[INV-TUI-DECL-01]` Stateless Presentation Purity**:
  Component render implementations must never accept `&mut App` or borrow mutable application state. All rendering logic must be an immutable projection of `(&Props, &State, &Theme)` into a `nuotc::Frame`.
- **`[INV-TUI-DECL-02]` Sovereign Local Event Encapsulation**:
  Component interaction state transitions must be handled exclusively through the component's `update` method via typed `Msg` enums. External event loops are prohibited from directly mutating private internal indices of child components.
- **`[INV-TUI-DECL-03]` Strangler Migration Boundary**:
  New components added to `crates/nuo-tui` must implement `Component`. Any modification to an existing legacy component must migrate it to the declarative contract, preventing legacy accretion.

---

## Consequences

### Positive
- **Instantaneous Unit Isolation**: Any component can be instantiated in a 5-line unit test or offline harness in under 1ms.
- **Zero Aliasing Panics**: Total elimination of `std::mem::take` hacks and runtime borrow conflicts during rendering.
- **Rock-Solid Predictability**: Component behavior is mathematically deterministic: identical state and props guarantee identical cell rendering.
- **Paves the Way for Sandboxes**: Enables native offline storybooks and pixel-perfect visual regression testing without mocks or servers.

### Negative & Trade-offs
- **Migration Overhead**: Rewiring legacy monolithic components requires initial upfront refactoring.
- **State Lifting Discipline**: Shared cross-component state must be explicitly lifted to parent props rather than lazily read from global carriers.

---

## Rejected Alternatives & Negative Knowledge

- **Retaining Imperative Drawing with Ad-Hoc Props Structs**: Rejected because passing props into imperative functions without sovereign `update` state machines still leaves event routing centralized, tangled, and brittle.
- **Pushing Declarative Runtime into `nuotc`**: Rejected per ADR-0003 and ADR-0011. `nuotc` must remain an autonomous retained-mode grid engine; declarative UI patterns and domain component models belong strictly in `nuo-tui`.
