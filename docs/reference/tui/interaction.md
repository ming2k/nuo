# TUI Interaction Reference

Factual reference for how `nuo-tui` turns terminal input into state changes and repaints: the event pipeline, the routing path, and the hit-testing registry.

---

## 1. The Pipeline

Input flows through one directional path, from the reader thread to the frame draw:

```text
InputReader thread ─► mpsc channel ─► run_app_loop (batch)
   ─► sgr guard ─► component_input::route (component-local first)
   ─► input::route_event  ─► InputAction
   ─► actions::dispatch_action ─► ActionFlow
   ─► apply::apply (sole App writer)
   ─► render_frame (compose + mount hit rects)
```

| Stage | Symbol | Module |
| :--- | :--- | :--- |
| Reader thread | `InputReader::spawn` | `event_loop/input_reader.rs` |
| Main loop | `run_app_loop` | `event_loop/mod.rs` |
| Too-small freeze | `below_minimum` guard | `event_loop/mod.rs` |
| Mutation drain | `apply::apply` | `event_loop/apply.rs` |
| Event handler | `process_one_event` | `event_loop/mod.rs` |
| Family classification | `input::event_family` | `input/router.rs` |
| Component-local first | `component_input::route` | `event_loop/component_input.rs` |
| Router entry | `input::route_event` → `InputAction` | `input/router.rs`, `input/action.rs` |
| Action dispatch | `actions::dispatch_action` → `ActionFlow` | `event_loop/actions.rs` |
| Frame draw | `render_frame` → `compose_frame` | `event_loop/render.rs` |

The `apply` pass is the **sole writer** of `App` state. Events never mutate the app directly; they are classified, routed, dispatched into an action, and only then applied.

---

## 2. Event Routing

1. **Family classification.** `input::event_family` maps a raw event to a UI family (composer, transcript, sheet, completion). The keyboard path is resolved through `ComponentTree::keyboard_path_for(family)`.
2. **Component-local first.** `component_input::route` gives dropdowns, provider-delete confirmation, and composer selection a chance to consume the event before global routing.
3. **Global routing.** `input::route_event` produces an `InputAction`, consulting the routing context: the active overlay/sheet, the current scene, key overrides, the focused target, and whether the transcript is focused.
4. **Dispatch.** `actions::dispatch_action` maps the action to an `ActionFlow` (the outcome: consumed, bubbled, or a mutation).

Key contexts (`ModalKeys`, `SheetKeys`, `SceneKeys`, `EditBuffer`) select which keymap applies for the current surface.

---

## 3. Hit Testing

Clicks resolve through a geometry registry rather than ad-hoc rect comparisons:

- **Footer rows** are recorded as `(FooterRowId, Rect)` by the declarative footer stack (`crates/nuo-tui/src/footer_stack.rs`); the event loop resolves a click by looking the id up (`rect_of`).
- **Transcript content** is recorded in a `LayoutMap` (`crates/nuo-tui/src/model/layout.rs`) built during render.
- **Every interactive target** is mounted as a `UiKey` (from `crates/nuo-tui/src/ui.rs`) into the `ComponentTree`, then hit-tested on click.

Because the same render pass both paints and registers hit rects, a click and the pixels it lands on can never disagree.

---

## 4. Mutation Vocabulary

Applied changes are described as typed mutations (`AppMutation`, with `Buffer`, `TranscriptEdit`, `ChromeEdit`, and `CompletionSignal` variants) in `crates/nuo-tui/src/event_loop/mutations.rs`. Coordination state that must *not* be mirrored into the app — dirty flags, the viewed session id, responding state, trust-gate dismissal — lives in a separate `UiRuntime` (`crates/nuo-tui/src/event_loop/runtime.rs`).

---

## 5. Per-Frame Reconciliation

After applying mutations, each frame reconciles derived state before drawing: request surfaces, toast timers, transcripts and session sync, navigation signals, and completion signals. The engine then diffs the retained cell grid, so only dirty lines are repainted.

---

## 6. Action Permissibility in Running State (ADR-0110 `[INV-TUI-RUNNING-02]`)

When a component, task, or agent turn is actively executing, user actions are strictly partitioned into three orthogonal categories rather than subjected to a blanket lock:

1. **Mutating Actions (Guarded)**: Trigger messages that would re-submit commands, mutate parameters, or spawn concurrent duplicates are intercepted at the TEA `update(msg)` layer and safely consumed as no-ops (`ActionStatus::is_actionable() == false`).
2. **Observational Actions (Unconditionally Open)**: Read-only inspection—toggling disclosure expansion, scrolling log viewports, selecting text ranges, copying stdout diagnostics, and navigating step cards—remains 100% operational with full foreground text contrast.
3. **Control Actions (Promoted Salience)**: Cancellation and steering affordances (e.g. <kbd>Esc</kbd> interrupts, `Alt+S` steer now) are maintained with high visual and keyboard priority.

---

## 7. Further Reading

- [Terminal Shortcuts, Escape Sequences, and Host Conflicts](../../explanation/terminal-shortcuts-and-conflicts.md): Design trade-offs, POSIX collisions, and emulator setup.
- [Layout Reference](layout.md): Viewport bands and design tokens.
- [Modals Reference](modals.md): Overlay sizing presets and modal lifecycles.

