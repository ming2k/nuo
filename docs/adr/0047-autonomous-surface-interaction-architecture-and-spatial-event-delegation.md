---
id: ADR-0047
title: "Autonomous Surface Interaction Architecture: First-Class Spatial Event Delegation and Leaky Nav-Field Retirement"
status: accepted
date: 2026-10-24
scope: tui/nuo-tui, architecture/surfaces, interaction/mouse, presentation/modals, input/routing
superseded_by: null
negative_knowledge: true
---

# 0047. Autonomous Surface Interaction Architecture: First-Class Spatial Event Delegation and Leaky Nav-Field Retirement

- Status: Accepted
- Date: 2026-10-24
- Deciders: Nuo Architecture Working Group
- Consulted: TUI, Interaction, Surfaces, and Presentation Maintainers
- Informed: System Architects
- Complements: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)
- Amends: ADR-0035 §Decision Outcome (completes the encapsulation of `DialogView` by migrating viewport scrolling and spatial mouse events into the entity contract, retiring the transitional `nav_fields` leak)

---

## Context and Problem Statement

[ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md) achieved a critical milestone in decoupling `nuo-tui`: it evicted scattered dialog state from the monolithic `App` struct into domain-scoped, encapsulated `DialogView` entities. Under ADR-0035, each dialog owns its identity, selection cursor, visual layout constraints, and discrete keyboard navigation (`ModalUp`, `ModalDown`, `SessionSelect`).

However, **input routing was only half-migrated**. While discrete keyboard navigation was moved into `DialogView::handle_input`, **scrolling (both mouse wheel ticks and keyboard page navigation) and spatial mouse clicks were left behind or unhandled in the outer event loop**:

```text
[ Current Bifurcated Input Pipeline (Legacy Leak) ]

Keyboard Actions (↑ / ↓ / Enter)  ──► ent.handle_input(&action, ...)  ──► Entity Autonomy
                                                                            
Mouse Wheel (ScrollUp / ScrollDown) ┐
Page Keys (PgUp / PgDn / Home / End)┴──► handle_wheel / actions.rs
                                                │
                                                ▼  (Raw pointer leak)
                                       app.modal_scroll_field()
                                                │
                                                ▼
                                    v.nav_fields() -> (&mut usize, Option<&mut bool>)
                                                │
                                                ▼
                                    *scroll = scroll.saturating_add(1);  // Mutated from outside!
```

### The Architectural Flaws & Root-Cause Failure Modes

1. **Inversion of Control via Leaky State (`nav_fields`)**:
   `DialogView` declares:
   ```rust
   fn nav_fields(&mut self) -> (&mut usize, Option<&mut bool>);
   ```
   Instead of dispatching an intent to the component (*"Scroll up 1 tick"*), the outer event loop reaches inside the entity to extract raw mutable references to primitive variables. The event loop then blindly performs arithmetic on those pointers.
2. **Sub-View Disconnect in Hierarchical / Drill-Down Dialogs**:
   Dialogs are not flat 1D lists. In `ThreadsDialog` (`/sessions`) and `ConnectionsDialog` (`/connections`), pressing `i` opens an info inspector (`session_info_detail` / `connection_info_detail`). The renderer uses `info_scroll` to render the detail document.
   However, `nav_fields()` is static and blindly hands back `&mut self.scroll` (the obscured background list). Consequently:
   - When the user scrolls with the mouse wheel inside an info detail view, **the visible detail document remains completely frozen**.
   - The invisible list in the background silently changes its scroll offset.
3. **Loss of Viewport Bounds and Clamping Authority**:
   Because scrolling is performed by external pointer mutation rather than component execution, the entity cannot enforce custom step sizes, content clamping, velocity dampening, or drill-down boundary guards.
4. **Schizophrenic Component Contract**:
   Having keyboard arrows enter through `handle_input` while page keys (`ScrollPageUp`/`ScrollPageDown`/`ScrollTop`/`ScrollBottom`) and mouse wheel ticks (`Wheel`) bypass the entity violates the single-responsibility principle and Chesterton's Fence in component lifecycle design.
5. **Spatial Click Vacuum in Surface Lists**:
   While `click_outside_dismiss` allows closing a dialog by clicking the backdrop, clicks landing on dialog list entries (`Models`, `Connections`, `Threads`, `Tools`) were swallowed as no-ops. Meanwhile, in root scenes such as `Settings`, the left Category navigation lacked pointer target mounting entirely, creating an ergonomic fracture between mouse-navigable tabs and keyboard-locked category panes.

---

## Decision Drivers

- **Zero Legacy Burden & Uncompromising Architectural Purity**: Complete the encapsulation envisioned in ADR-0035. Eliminate half-way shims, global pointer reflections, and raw pointer leaks.
- **Hierarchical & Multi-Layer Viewport Correctness**: Surfaces with drill-downs, multi-pane tables, and detail cards must autonomously direct scroll actions to the active sub-layer without external guessing.
- **Unified Surface Event Pipeline**: Both discrete keyboard actions and spatial mouse events targeting a mounted dialog must pass through the same front door: `DialogView::handle_input`.
- **Modernization & Future-Proofing**: Pave the architectural path for interactive mouse clicks on dialog items (`ClickTarget`), scrollbar thumb dragging, and smooth mouse navigation across the TUI.
- **Terminal Protocol Safety**: Maintain absolute adherence to `EnableButtonMouse` (Mode 1002 + SGR 1006); reject all-motion hover tracking (Mode 1003) to prevent SGR buffer truncation leaks.

---

## Considered Options

- **Option 1 (Chosen)**: **Autonomous Surface Message-Passing** — Unify scrolling under `DialogView::handle_input`, retire `nav_fields()`, and delegate hit-tested spatial events directly to the active entity.
- **Option 2 (Rejected)**: **Dynamic `nav_fields` Inspection** — Keep `nav_fields()`, but patch its return value to branch on internal flags (e.g. `if self.info_detail { (&mut self.info_scroll, None) }`).
- **Option 3 (Rejected)**: **Full DOM-Style Event Bubble/Capture Tree** — Implement a universal hierarchical GUI event-propagation tree across all ratatui widgets.
- **Option 4 (Rejected)**: **High-Frequency All-Motion Hover Routing (Mode 1003)** — Re-enable crossterm all-motion tracking to support mouse hover over dialog widgets.

---

## Decision Outcome

Chosen option: **Option 1**, because it establishes a clean, modern, message-passing component model, completely eliminates raw pointer inversion, guarantees sub-view scroll correctness, and leaves zero legacy baggage.

### Invariants & Behavioral Boundaries

- **`[INV-SURFACE-06] Sovereign Viewport Navigation`**: Every `DialogView` entity MUST autonomously own its scrolling, paging, and viewport bounds. Leaking raw mutable references (`&mut usize`, `&mut bool`) from a surface entity to the outer event loop or `App` is strictly prohibited. `DialogView::nav_fields()` is formally retired.
- **`[INV-SURFACE-07] First-Class Spatial Event Delegation`**: When a spatial mouse event (`Wheel`, `SelectionStart` / `Click`) hits a focused `DialogView` bounding box (`UiKey::Overlay`), the event loop MUST delegate the interaction directly to `DialogView::handle_input` as a typed `InputAction`. Bypassing the entity to mutate external state is forbidden. Clicking a valid content entry moves the selection cursor (`Click-to-Select`).
- **`[INV-SURFACE-08] Multi-Layer Sub-View Parity`**: Dialog entities containing drill-down sub-layers (`info_detail`, attempt inspectors, secondary panes) MUST route scroll and navigation actions to the visually active sub-layer. A drill-down sub-view must never scroll an obscured parent list.
- **`[INV-SURFACE-09] Button-Event Mouse Fence`**: Mouse support for surface components MUST operate strictly within Mode 1002 (button press, release, drag, wheel) and SGR-1006 coordinates. Enabling Mode 1003 all-motion tracking or emitting non-button terminal hover events is strictly prohibited to prevent SGR buffer truncation leaks ([ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md)).
- **`[INV-SURFACE-10] Scene Spatial Navigation Parity & Non-Destructive Click-to-Toggle`**: In full-screen configuration scenes (`SceneKind::Settings`), spatial pointer interaction MUST maintain symmetry between navigation and content zones:
  1. Category navigation MUST mount interactive pointer targets (`UiKey::SettingsCategory`) enabling click-to-switch category without keyboard chording.
  2. Detail options MUST enforce click-to-focus and immediate non-destructive click-to-toggle for Boolean and theme-selection affordances, retiring redundant enter/space chording for switch controls.
  3. All interactions operate strictly within the button-event fence (`[INV-SURFACE-09]`).

---

### Architectural Specification

#### 1. Contract Extension (`DialogView::handle_input`)
The `DialogView` trait contract receives typed scroll actions:
- `InputAction::ScrollUp` / `InputAction::ScrollDown` (1-line scroll tick / wheel)
- `InputAction::ScrollPageUp` / `InputAction::ScrollPageDown` (viewport page step)
- `InputAction::ScrollTop` / `InputAction::ScrollBottom` (jump to top/bottom)
- `InputAction::SelectionStart { x, y }` (spatial entry hit testing / click-to-select)

```rust
pub trait DialogView: std::fmt::Debug + Send + 'static {
    fn handle_input(
        &mut self,
        action: &InputAction,
        app: &mut crate::App,
        viewed_session_id: &str,
    ) -> DialogOutcome;
    ...
}
```

#### 2. Default Macro Implementation & Context-Aware Overrides
For standard 1D list dialogs, the `dialog_entity!` macro provides default scroll behavior:

```rust
match action {
    InputAction::ScrollUp => {
        self.scroll = self.scroll.saturating_sub(1);
        self.follow = false;
        DialogOutcome::Consumed
    }
    InputAction::ScrollDown => {
        self.scroll = self.scroll.saturating_add(1);
        self.follow = false;
        DialogOutcome::Consumed
    }
    InputAction::ScrollTop => {
        self.scroll = 0;
        self.follow = false;
        DialogOutcome::Consumed
    }
    InputAction::ScrollBottom => {
        self.scroll = usize::MAX;
        self.follow = false;
        DialogOutcome::Consumed
    }
    _ => ...
}
```

For hierarchical entities (`ThreadsDialog`, `ConnectionsDialog`), the entity directs the action to the active sub-layer:

```rust
match action {
    InputAction::ScrollUp => {
        if self.info_detail {
            self.info_scroll = self.info_scroll.saturating_sub(1);
        } else {
            self.scroll = self.scroll.saturating_sub(1);
            self.follow = false;
        }
        DialogOutcome::Consumed
    }
    InputAction::ScrollDown => {
        if self.info_detail {
            self.info_scroll = self.info_scroll.saturating_add(1);
        } else {
            self.scroll = self.scroll.saturating_add(1);
            self.follow = false;
        }
        DialogOutcome::Consumed
    }
    ...
}
```

#### 3. Spatial Event Router Delegation (`handle_wheel` & `handle_selection_start`)
In `event_loop/actions.rs`, when a mouse wheel tick lands within an active dialog's bounds:

```rust
Some(UiKey::Overlay(OverlaySurface::Dialog(_)))
    if app.ui.contains(UiKey::Overlay(overlay), x, y) =>
{
    if let Some(mut ent) = app.surfaces.take_active_view() {
        let action = if up { InputAction::ScrollUp } else { InputAction::ScrollDown };
        let outcome = ent.handle_input(&action, app, viewed_session_id);
        app.surfaces.put_active_view(ent);
        if outcome == DialogOutcome::Consumed {
            return;
        }
    }
}
```

When a pointer press (`SelectionStart { x, y }`) hits within the open dialog panel bounding box, it delegates directly to `DialogView::handle_input`. Standard list dialogs calculate the clicked line offset against `body_y` and update `self.index`:

```rust
let visual_row = (*y - body_y) as usize;
let clicked_index = self.scroll + visual_row;
if clicked_index < total_rows {
    self.index = clicked_index;
    self.follow = true;
    DialogOutcome::Consumed
}
```

#### 4. Settings Scene Spatial Parity
The settings view (`views/settings`) exposes category and detail row bounding rects:
- `draw_categories_pane` records visible category rects and mounts `UiKey::SettingsCategory(index)`.
- Clicking a category switches `app.config_category = index` and resets detail scroll/index.
- Detail options mount `UiKey::SettingsRow(index)` and `UiKey::SettingsOption(index)`. Clicking toggles switches immediately for `Components` and applies themes for `Appearance` (`Click-to-Toggle`), while parking the selection cursor on complex detail entries without unexpected side-effects.

---

### Positive Consequences

- **Sub-View Scroll Integrity**: Detail cards, inspector drill-downs, and turns tables (`Threads › Info`, `Connections › Info`, `Session Trace`) immediately respond to mouse wheel scrolling without freezing or drifting.
- **Elimination of Leaky Trait Surface**: Deleting `nav_fields` removes an awkward API where internal entity fields were exposed as raw mutable pointers.
- **Architectural Uniformity**: All interactions with a dialog (keyboard shortcuts, arrow navigation, page navigation, mouse wheel ticks, and spatial row clicks) enter through the same `handle_input` pipeline.
- **Click-to-Select & Click-to-Toggle Ergonomics**: List items in dialogs can be focused by clicking, and Settings scene categories and toggle switches respond naturally to pointer clicks without requiring keyboard navigation chording.

### Negative Consequences & Trade-offs

- **Entity Implementation Responsibility**: Each specialized dialog entity that manages internal sub-views must explicitly handle scroll and click actions in its `input_dialog` implementation.
- *Mitigation*: The `dialog_entity!` macro and shared `input_dialog` provide universal defaults for standard dialogs, restricting custom handling only to entities with complex sub-views.

---

## Rejected Alternatives & Negative Knowledge

### Option 2: Dynamic `nav_fields` Inspection (Rejected)
- **Why Considered**: Allowed fixing the `info_detail` bug with fewer line changes by having `nav_fields()` return `&mut self.info_scroll` when `self.info_detail` is active.
- **Why Rejected**: Retained the anti-pattern of exposing mutable internal references. It kept `App::modal_scroll_field()` alive as a monolithic mediator and failed to provide component autonomy for page steps, velocity, or scroll events.

### Option 3: Full DOM-Style Event Bubble/Capture Tree (Rejected)
- **Why Considered**: Standard paradigm in web and rich desktop GUI frameworks (e.g. Qt, WPF).
- **Why Rejected**: Massive cognitive overhead and abstraction penalty. Terminal applications have a flat spatial elevation model (root -> viewport -> overlay -> popup); introducing full DOM event bubbling would add thousands of lines of synchronization and borrow-checker friction with zero benefit over direct spatial delegation.

### Option 4: High-Frequency All-Motion Hover Routing (Mode 1003) (Rejected)
- **Why Considered**: Might allow tracking the mouse hover position across dialog rows without clicking.
- **Why Rejected**: Explicitly forbidden by `[INV-TUI-MIN-02]` and `nuo-tui/src/mouse_mode.rs`. Mode 1003 turns the terminal into a firehose of SGR sequences that break across 1 KiB kernel read boundaries, causing split escape sequences to spill into the composer as raw garbage characters. Terminal stability strictly demands button-event tracking (Mode 1002) only.

---

## Links

- Enforces: [ADR-0035: Domain-Scoped Surface Architecture and Encapsulated Dialog Lifecycle](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md)
- Aligns: [ADR-0040: Cross-Domain Action Matrix and Polymorphic TUI Spatial Topology](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md)
- Aligns: [ADR-0022: Minimum Terminal Geometry and the Frozen-Input Contract](0022-minimum-terminal-geometry-and-frozen-input-contract.md)
