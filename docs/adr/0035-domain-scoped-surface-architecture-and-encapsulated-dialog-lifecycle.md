---
id: ADR-0035
title: "Domain-Scoped Surface Architecture: Encapsulated Dialog Entities, Context-Bound Stacks, and Deterministic Lifecycle"
status: accepted
date: 2026-10-12
scope: tui/nuo-tui, architecture/surfaces, presentation/modals, interaction/input, lifecycle/navigation
superseded_by: null
negative_knowledge: true
---

# 0035. Domain-Scoped Surface Architecture: Encapsulated Dialog Entities, Context-Bound Stacks, and Deterministic Lifecycle

- Status: Accepted
- Date: 2026-10-12
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, TUI, Runtime, and Human-Interface Architecture Teams
- Informed: Core Engineering, Release Engineering
- Amends: [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md), [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md), [ADR-0025](0025-scene-scoped-interrupt-chord.md)

---

## Context and Problem Statement

The TUI stage-scene-overlay architecture introduced in earlier milestones sought to model workspaces as full-screen **Scenes** (Conversation, Dashboard, Settings, TaskInspection, Aside) and floating modals as **Dialogs** managed by a LIFO overlay stack. 

However, as the application grew in complexity, the lack of a formal ownership and domain-scoping model led to several critical architectural pathologies:

1. **Monolithic Split-Brain State Synchronization**:
   Dialog state is fragmented between an offline registry (`SurfaceStore`, storing generic `DialogState` snapshots) and scattered mutable fields on the god `App` struct (`session_scroll`, `modal_index`, `history_scroll`, `model_scroll`). Manual, error-prone synchronization routines (`save_dialog_state` and `restore_dialog_state`) attempt to copy fields back and forth on focus transitions.
2. **Field Aliasing and Cross-Dialog State Contamination**:
   Multiple distinct dialogs share the exact same raw fields on `App`. For example, `Tools`, `Mcp`, `Skills`, and `Sessions` all project into `self.session_scroll` and `self.session_modal_follow`. If any lifecycle hook is skipped, one dialog's scroll position or follow mode silently corrupts another's.
3. **Destructive Component Borrowing (Composer Hijacking)**:
   The dialog search/filter mechanism relies on `owns_composer_draft` (`Models`, `Connections`, `HistorySearch`), which strips the user's active draft from the main conversation input line (`self.input`) and parks it into `state.draft`. If a scene transition or async dismissal occurs while a dialog is active, this parked draft is stranded or permanently erased.
4. **Lifecycle Bypass on Scene Navigation**:
   When navigating between scenes via `SurfaceRouter::switch_scene()`, the router performs a blunt `self.overlay_stack.clear()`. This completely bypasses `App::deactivate_dialog()`, leaving parked drafts un-restored, and stranding transient UI flags such as `dialog_keys = true` active into the next scene.
5. **Absence of Precondition Scoping and Domain Boundaries**:
   All 13 dialog kinds in `DialogKind::ALL` are unconditionally exposed across the application (notably in the `C-x p` Quick Switcher). A user can summon `HistorySearch` from within the Settings scene (which has no conversation composer) or `Telemetry` and `Queue` when no ambient session exists, inducing undefined focus fighting, invalid state queries, and crashes.
6. **Unimplemented Architectural Intent**:
   While `RetentionPolicy::SessionScoped` was declared in the type system, zero dialogs use it. Consequently, viewed-session changes trigger an indiscriminate `close_all()`, discarding global configuration states (such as provider settings and model picker selections) alongside session-local data.

We need an uncompromising, future-proof architectural redesign that establishes **encapsulated dialog entities**, **three-tier domain scoping**, **type-safe context preconditions**, and **deterministic LIFO stack unwinding**.

---

## Decision Drivers

- **Zero-Baggage Entity Encapsulation (`[INV-SURFACE-01]`)**: Every dialog MUST be a self-contained entity owning its own state, cursor, scroll, and embedded input widgets. No shared mutable fields on `App`. No borrowing or hijacking of the conversation composer line.
- **Three-Tier Domain Scoping (`[INV-SURFACE-02]`)**: Dialogs MUST be explicitly classified by ownership domain: `AppScoped` (global), `SessionScoped` (ambient session bound), and `SceneScoped` (bound to workspace capabilities).
- **Contractual Precondition Enforcing (`[INV-SURFACE-03]`)**: A dialog MUST NOT be opened or advertised in command palettes if its required context (`Session`, `Scene`) is absent.
- **Deterministic Unwinding Pipeline (`[INV-SURFACE-04]`)**: Overlay stack transitions MUST NOT execute blunt truncation (`clear()`). Any transition that pops or clears overlays MUST execute an orderly unwinding pipeline guaranteeing dismissal hooks and resource reclamation.
- **Granular Domain-Isolated Retention (`[INV-SURFACE-05]`)**: Session transitions MUST isolate their lifecycle effects to `SessionScoped` dialogs, leaving global configuration and application-level dialog states intact.

---

## Considered Options

### Option 1: Ad-Hoc Glue Code and Manual Defensive Guards (Status Quo Extended)
- Retain the god `App` struct fields and `SurfaceStore` copying logic, but add extensive defensive checks, extra booleans, and null checks before calling `open_dialog` or `switch_scene`.
- *Assessment*: Rejected. Does not solve the root cause. Maintains split-brain state, preserves composer theft hacks, and guarantees future regressions as new dialogs and scenes are introduced.

### Option 2: Strictly Nested Per-Scene Overlay Stacks
- Eliminate the global overlay stack entirely and attach an independent `Vec<OverlaySurface>` to every `SceneKind` instance.
- *Assessment*: Rejected. Violates terminal visual and focus realities. A terminal canvas has exactly one physical 2D grid and one focused cursor. Global dialogs (such as Command Palette `C-x p` or Session Switcher `/sessions`) become awkward duplicates across scenes, and focus bubbling becomes deeply tangled.

### Option 3: Domain-Scoped Surface Architecture with Encapsulated Entities and Transactional Unwinding (Chosen)
- Formulate Dialogs as encapsulated entities implementing a unified `DialogView` component contract with private state and self-contained text inputs.
- Decouple logical domain ownership (`AppScoped`, `SessionScoped`, `SceneScoped`) from the physical top-level LIFO z-stack (`OverlayStack`).
- Enforce compile-time or contractual context availability guards (`is_available(scene, session)`).
- Replace blunt `stack.clear()` with a transactional `unwind()` pipeline ensuring reverse-LIFO dismissal hooks.

---

## Rejected Alternatives

Per **[INV-AGENT-01]**, the following alternative approaches were evaluated and discarded:

1. **Rejected: Global Composer Hijacking (`owns_composer_draft`) Preservation**
   - *Reason*: Reusing the chat composer line as an ad-hoc query box for floating dialogs is an archaic legacy shortcut. It conflates chat input history with modal query state, creates fragile draft-parking dependencies, and makes dialogs inoperable on non-chat scenes (Dashboard, Settings). Every dialog requiring text entry must own an embedded `TextInput` component.
2. **Rejected: Shared Mutable Field Pools on `App`**
   - *Reason*: Reusing `session_scroll` across `Tools`, `Mcp`, `Skills`, and `Sessions` violates basic data-hiding and encapsulation principles. It is the primary cause of cross-modal scroll drift.
3. **Rejected: Untyped Dynamic Property Bags / `AnyMap` Contexts**
   - *Reason*: Passing untyped context bags to dialogs shifts invariant verification from compile/design time to late runtime errors, making refactorings hazardous.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Component Contract: The Encapsulated `DialogView` (`[INV-SURFACE-01]`)

Every dialog is implemented as a self-contained component implementing `DialogView`:

```rust
pub trait DialogView: Send + 'static {
    /// Visual layout constraints for modal positioning.
    fn layout_spec(&self) -> ModalSpec;

    /// Self-contained input handling. Consumes raw key events and produces outcomes.
    fn handle_input(&mut self, key: &KeyEvent) -> DialogOutcome;

    /// Autonomous rendering interface; draws entirely from internal state and injected immutable views.
    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme);

    /// Guaranteed dismissal hook executed upon popping or stack unwinding.
    fn on_dismiss(&mut self) {}
}

pub enum DialogOutcome {
    Consumed,
    Dismiss,
    Action(AppAction),
    SwitchTo(Box<dyn DialogView>),
}
```

Dialogs requiring search or text entry (such as `HistorySearch`, `Models`, and `Connections`) embed their own `TextInput` instance:

```rust
pub struct HistorySearchDialog {
    search_input: TextInput,
    results: Vec<HistoryItem>,
    cursor: usize,
    scroll: usize,
}
```
The conversation composer's draft is **never touched, parked, or borrowed** by floating dialogs.

### 2. Domain Scoping Matrix (`[INV-SURFACE-02]`)

Dialogs are classified into three distinct domains of ownership:

```
+─────────────────────────────────────────────────────────────────────────+
| App-Scoped (Global)                                                     |
| - Switcher (`C-x p`), Sessions (`/sessions`), Connections, UsageStats   |
| - Owner: Terminal App Lifecycle. Survives session & scene switches.     |
+─────────────────────────────────────────────────────────────────────────+
                                    │
                                    ▼
+─────────────────────────────────────────────────────────────────────────+
| Session-Scoped (Active Session Bound)                                   |
| - Telemetry, Queue, Asides (`/btw`), SessionTree, Tools, Mcp, Skills    |
| - Owner: Ambient Session. Archived/cleared on session change.           |
+─────────────────────────────────────────────────────────────────────────+
                                    │
                                    ▼
+─────────────────────────────────────────────────────────────────────────+
| Scene-Scoped (Workspace Bound)                                          |
| - HistorySearch (requires Composer Scene), Scene Dropdowns / Inspect    |
| - Owner: Scene Workspace. Unwound and dismissed on scene exit.          |
+─────────────────────────────────────────────────────────────────────────+
```

### 3. Contractual Precondition Guarding (`[INV-SURFACE-03]`)

Before a dialog can be opened or advertised in the Command Palette / Quick Switcher, its preconditions are evaluated against the current environment:

```rust
pub enum DialogScope {
    Global,
    Session,
    Scene(SceneKind),
}

impl DialogKind {
    pub fn scope(&self) -> DialogScope {
        match self {
            Self::Switcher | Self::Sessions | Self::Models 
            | Self::Connections | Self::UsageStats => DialogScope::Global,
            Self::Telemetry | Self::Queue | Self::Asides | Self::SessionTree
            | Self::Tools | Self::Mcp | Self::Skills | Self::Permissions => DialogScope::Session,
            Self::HistorySearch => DialogScope::Scene(SceneKind::Conversation),
        }
    }

    pub fn is_available(&self, current_scene: SceneKind, has_session: bool) -> bool {
        match self.scope() {
            DialogScope::Global => true,
            DialogScope::Session => has_session,
            DialogScope::Scene(required_scene) => current_scene == required_scene && has_session,
        }
    }
}
```

In the Command Palette / Quick Switcher, items where `is_available() == false` are filtered out or rendered disabled. Direct keyboard shortcut dispatch returns a no-op or friendly hint when preconditions are unsatisfied.

### 4. Deterministic Stack Unwinding Pipeline (`[INV-SURFACE-04]`)

The `OverlayStack` maintains top-level LIFO ordering for visual elevation and input arbitration, but transitions execute structured unwinding instead of truncation:

```rust
pub struct OverlayStack {
    entries: Vec<StackEntry>,
}

struct StackEntry {
    kind: DialogKind,
    scope: DialogScope,
    view: Box<dyn DialogView>,
}

impl OverlayStack {
    /// Pop the top overlay, executing its on_dismiss hook.
    pub fn pop(&mut self) -> Option<Box<dyn DialogView>> {
        if let Some(mut entry) = self.entries.pop() {
            entry.view.on_dismiss();
            Some(entry.view)
        } else {
            None
        }
    }

    /// Transactional scene exit: safely unwinds all scene-bound dialogs in reverse LIFO order.
    pub fn unwind_scene(&mut self, leaving_scene: SceneKind) {
        let mut idx = self.entries.len();
        while idx > 0 {
            idx -= 1;
            if matches!(self.entries[idx].scope, DialogScope::Scene(s) if s == leaving_scene) {
                let mut removed = self.entries.remove(idx);
                removed.view.on_dismiss();
            }
        }
    }

    /// Transactional session transition: unwinds session-scoped dialogs, preserving global overlays.
    pub fn unwind_session(&mut self) {
        let mut idx = self.entries.len();
        while idx > 0 {
            idx -= 1;
            if matches!(self.entries[idx].scope, DialogScope::Session) {
                let mut removed = self.entries.remove(idx);
                removed.view.on_dismiss();
            }
        }
    }
}
```

### 5. Granular Domain-Isolated Retention (`[INV-SURFACE-05]`)

`SurfaceStore` retention is domain-partitioned:
- **Global Store**: Preserves scroll offsets, filter queries, and cursor positions for global dialogs across sessions and scenes.
- **Session Store**: Indexed by `SessionId`. When switching to a different session, the previous session's modal states are cleanly stored under its session record, and the new session's states are reinstated. Global dialog states are untouched.

---

## Invariants & Behavioral Boundaries

- **`[INV-SURFACE-01]` Autonomous Entity Invariant**: Every floating dialog MUST be a self-contained entity maintaining its own layout state, cursor, and text-input components. Dialogs MUST NOT read, write, or alias shared scratchpad fields on `App`, and MUST NOT borrow or park the conversation composer line.
- **`[INV-SURFACE-02]` Domain Scoping Invariant**: Every dialog MUST be assigned exactly one domain scope: `Global`, `Session`, or `Scene`.
- **`[INV-SURFACE-03]` Precondition Gating Invariant**: No dialog may be opened, focused, or advertised in palette discovery when its domain preconditions are unsatisfied.
- **`[INV-SURFACE-04]` Deterministic Unwinding Invariant**: The overlay stack MUST NOT be reset via raw truncation (`clear()`). Every overlay eviction MUST execute its `on_dismiss()` lifecycle hook in reverse LIFO order.
- **`[INV-SURFACE-05]` Domain-Partitioned Retention Invariant**: Session changes MUST NOT invalidate or reset global dialog retention states (`Models`, `Connections`, `UsageStats`).

---

## Positive Consequences

- Eliminates draft loss and composer cursor corruption across dialog invocations and scene switches.
- Completely prevents cross-modal scroll and cursor crosstalk by replacing shared fields on `App` with private entity state.
- Formally prevents invalid modal invocations (e.g., `HistorySearch` in `Settings`, or `Telemetry` without an active session).
- Makes modal components unit-testable in complete isolation without instantiating the monolithic `App` structure.
- Prepares the TUI architecture for multi-window, split-pane, and headless virtual sessions without architectural rework.
