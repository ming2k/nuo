# TUI Interaction Reference

Factual reference for how the Nuo shell (`nuo::shell`) turns terminal input into state changes and repaints: the event pipeline, the routing path, and the hit-testing registry.

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
3. **Global routing.** `input::route_event` produces an `InputAction`, consulting the routing context: the active overlay/sheet, the current scene, key overrides, and which **peer focus plane** owns the keyboard (ADR-0145).
4. **Dispatch.** `actions::dispatch_action` maps the action to an `ActionFlow` (the outcome: consumed, bubbled, or a mutation).

Key contexts (`ModalKeys`, `SheetKeys`, `SceneKeys`, `EditBuffer`) select which keymap applies for the current surface.

### Peer focus planes (ADR-0145)

A chat scene hosts two coequal surfaces, and exactly one owns the keyboard:

| Plane | Owner | Keys |
| --- | --- | --- |
| `Composer` | the draft input | printable text, caret motion, readline chords, completion, `Enter` send |
| `Transcript` | inspection | `↑`/`↓` and page keys scroll; a highlighted entry also accepts `Space` activate, `y` copy, `Shift-y` diagnostic, `v` view full log |

The state is one value (`crate::shell::focus::Focus`) carrying the plane plus the transcript's sub-state (`Browse` or `Entry(target)`). Transitioning:

| Gesture | Effect |
| --- | --- |
| `Shift-Tab` | crosses to the other plane (an involution: the same press returns, restoring the highlighted entry) |
| `Ctrl+P` / `Ctrl+N` | walk the entry highlight; at either end with nothing left to scroll, `Ctrl+N` crosses back to the composer |
| click into the transcript / a step summary | enters the transcript plane (browse / entry) |
| click into the input box | enters the composer plane |
| `Esc` | **never crosses planes**: it dismisses a completion, interrupts the running round, or is spent |

A printable key pressed while the transcript owns the keyboard is inert — it is never inserted into a draft the user is not looking at.

#### How the crossing reads on screen

Both planes announce ownership in the **background** channel, because exactly one of them owns the keyboard (`[INV-FOCUS-04]`). They differ in *scale*, not in channel: the composer's panel is a four-row block, the transcript is the whole viewport, so the transcript's step is small and the cue is carried by area.

| Plane state | Cue |
| --- | --- |
| `Composer` | the input panel is lit (`input_surface()`); the transcript stays on the page tone |
| `Transcript(Browse)` | the **pane wash**: the whole transcript surface lifted one quiet step (`Theme::pane_lifted()`, the effective tone from `pane_focus()`) |
| `Transcript(Entry)` | the wash, plus one **marker vertical bar** in the highlighted entry's gutter (`Theme::pane_marker_glyph()` in `pane_marker_style()`) |

Two properties are load-bearing:

- **Area, not edges.** The cue covers every column the reader is reading. An earlier rail tinted only the two gutter columns, so the pane announced itself at its extremes and nowhere in the middle.
- **The row is never repainted.** A header row's tone is the disclosure × lifecycle × interaction composition, and painting a background under it inverted the running hue and the collapsed muted rung the renderers had resolved on purpose — the pointed-at entry became a highlighted component instead of part of a live surface. The marker is a quiet vertical bar (`▎`) in the gutter, so the row keeps its composition and nothing needs contrast re-seating.

Both cues are zero-layout: the wash adds no row and moves no column, the marker lives in reserved gutter columns, so crossing never reflows the pane.

The wash is a **theme** fact rather than a post-pass: `draw_transcript` renders the stream with `Theme::pane_lifted()`, so every renderer paints the lifted pane through the struct it already reads. The lift is additive and background-only — every background shifts by the page's delta, no foreground moves — so each band keeps its exact separation from the page.

Whether a palette can carry a wash is **measured**, not declared: `has_pane_wash()` requires an RGB page and channel headroom above every band the transcript builds, and the declared tone is scaled back to the largest step the palette can absorb. A palette with no honest step (ANSI-16 / VT100's `Reset` page, or one whose bands sit at a channel limit) takes the gutter fallback — the largest region those profiles can afford to tint. On `Structured` (VT100 / monochrome) the fallback and the marker both degrade to reverse video, the archetype's only background vocabulary.

The marker is **not** `selected_bg`: "the keyboard is on this entry" must never read as "this text is selected".

---

## 3. Hit Testing

Clicks resolve through a geometry registry rather than ad-hoc rect comparisons:

- **Footer rows** are recorded as `(FooterRowId, Rect)` by the declarative footer stack (`crates/nuo-view/src/footer_stack.rs`); the event loop resolves a click by looking the id up (`rect_of`).
- **Transcript content** is recorded in a `LayoutMap` (`crates/nuo-view/src/model/layout.rs`) built during render.
- **Every interactive target** is mounted as a `UiKey` (from `crates/nuo/src/shell/ui.rs`) into the `ComponentTree`, then hit-tested on click.

Because the same render pass both paints and registers hit rects, a click and the pixels it lands on can never disagree.

---

## 4. Mutation Vocabulary

Applied changes are described as typed mutations (`AppMutation`, with `Buffer`, `TranscriptEdit`, `ChromeEdit`, and `CompletionSignal` variants) in `crates/nuo/src/shell/event_loop/mutations.rs`. Coordination state that must *not* be mirrored into the app — dirty flags, the viewed session id, responding state, trust-gate dismissal — lives in a separate `UiRuntime` (`crates/nuo/src/shell/event_loop/runtime.rs`).

---

## 5. Per-Frame Reconciliation

After applying mutations, each frame reconciles derived state before drawing: request surfaces, toast timers, transcripts and session sync, navigation signals, and completion signals. The engine then diffs the retained cell grid, so only dirty lines are repainted.

### Unsent drafts (ADR-0247)

A draft belongs to the **thread** it was typed into, not to the tab showing it. `ComposerState::draft_thread` records which thread currently owns the composer buffer, and `App::hand_composer_to_current_thread` is the one hand-off protocol: park the departing thread's draft in the per-thread ledger, install the arriving thread's. It runs on tab activation, tab replacement, tab close, scene entry, the viewed-session change hook, the boot hand-off, and the exit seal — so a switch, a re-attach, a `/new`, or a process restart cannot lose it.

Parking is a **signal comparison**, not a copy: an unchanged draft (same text hash, caret, view offset, and attachment shape) is never cloned and never written. Transitions park synchronously; a one-second tick covers a crash mid-sentence. Drafts live under `$XDG_STATE_HOME/nuo/drafts/`, one JSON file per thread, and a thread holding a draft is **not** empty, so `/new` or `/threads` cannot recycle it. While `↑`/`↓` recall or <kbd>Ctrl-R</kbd> search borrows the composer line, the thread's draft is the stash (`stashed_draft`), and a transition parks that — never the borrowed row.

---

## 6. Action Permissibility in Running State (ADR-0110 `[INV-TUI-RUNNING-02]`)

When a component, task, or agent turn is actively executing, user actions are strictly partitioned into three orthogonal categories rather than subjected to a blanket lock:

1. **Mutating Actions (Guarded)**: Trigger messages that would re-submit commands, mutate parameters, or spawn concurrent duplicates are intercepted at the TEA `update(msg)` layer and safely consumed as no-ops (`ActionStatus::is_actionable() == false`).
2. **Observational Actions (Unconditionally Open)**: Read-only inspection—toggling disclosure expansion, scrolling log viewports, selecting text ranges, copying stdout diagnostics, and navigating step cards—remains 100% operational with full foreground text contrast.
3. **Control Actions (Promoted Salience)**: Cancellation and steering affordances (e.g. <kbd>Esc</kbd> interrupts, `Alt+S` steer now) are maintained with high visual and keyboard priority.

---

## 7. The Command Palette (RFC-0005, ADR-0218)

<kbd>Ctrl-Shift-p</kbd> opens the palette from any scene, including over an open dialog. It lists what you can do *where you are*:

| Group | Contents | Shown when |
|---|---|---|
| *Dialog name* | The verbs of the dialog under the palette (e.g. **Models**: use, favorite, block, refresh) | the palette was opened over that dialog |
| *Scene name* | The current scene's verbs (e.g. **Diff viewer**: next file, mark reviewed) | no dialog is open |
| Recent | Up to five entries you ran recently | you have run something before |
| Tabs | Every other open tab, with its <kbd>Alt-N</kbd> chord | more than one tab is open |
| Go to · Session · Actions · App | Destinations and global commands | always |

Each row shows the key that runs the same verb without the palette. That key reflects your `[keybindings]` remaps, and a test resolves every advertised key through the real router. An entry that applies here but cannot run right now (*Interrupt* while idle, *Copy entry* with nothing highlighted) is dimmed and shows why. Pressing <kbd>Enter</kbd> on it repeats the reason and leaves the palette open.

Typing filters by fuzzy match on the title and highlights the matched letters. Aliases such as `/stats` also match. A leading sigil narrows the search:

| Sigil | Searches |
|---|---|
| `:` | only this dialog's or scene's verbs |
| `@` | only places: tabs and destinations |
| `>` | only actions, never places |

Running an entry closes the palette and dispatches the exact action its key would. Dialog verbs therefore act on the dialog's highlighted row.

---

## 8. Further Reading

- [Terminal Shortcuts, Escape Sequences, and Host Conflicts](../../explanation/terminal-shortcuts-and-conflicts.md): Design trade-offs, POSIX collisions, and emulator setup.
- [Layout Reference](layout.md): Viewport bands and design tokens.
- [Modals Reference](modals.md): Overlay sizing presets and modal lifecycles.

