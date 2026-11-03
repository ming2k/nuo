---
id: ADR-0146
title: "Live Storybook Engine, Sub-Second Hot Loop, and Broad Catalog Expansion"
status: accepted
date: 2026-03-29
scope: tui/nuo, devtools/storybook, presentation/nuo-ui-primitives, presentation/nuo-view, test/visual-regression
superseded_by: null
negative_knowledge: true
---

# 0146. Live Storybook Engine, Sub-Second Hot Loop, and Broad Catalog Expansion

- Status: Accepted
- Date: 2026-03-29
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, TUI Runtime, Developer Experience, Quality Engineering
- Amends: [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md), [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md)

---

## Context and Problem Statement

[ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md) established the offline component Storybook harness and styled snapshot pipeline, while [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md) unified the dual-tier presentation model, banning parallel mock components. However, practical developer usage revealed three structural deficiencies in the Storybook substrate:

1. **Static Blindness (The Frozen Animation Problem)**:
   The interactive workbench (`Workbench::run_loop`) relied entirely on blocking event consumption (`crossterm::event::read()?`). As a result, in-flight animated states—such as `ToolCard` streaming spinners, elapsed timers, and rotating capability hints—remained completely frozen on screen until an arbitrary key was pressed. Developers could not verify motion cadence, timer increments, or visual stuttering in the sandbox.

2. **Context Loss on Recompilation (The Break in the Dev Loop)**:
   While ADR-0073 envisioned continuity with `cargo watch`, the workbench state was ephemeral in memory. Every code edit triggered a recompile that reset the cursor to index 0, forcing developers to manually navigate back to their active component, re-adjust the viewport boundary, and re-toggle their working theme dozens of times per hour.

3. **Coverage Deficit (The Component Archipelago)**:
   Only three Tier B components (`NoticeCard`, `AskUser`, `ToolCard`, totaling 13 stories) were registered. Meanwhile, dozens of production presentation elements in `nuo-ui-primitives` and `nuo-view` (`Toast`, `WhichKey`, `Modal`, `TurnBand`, `EmptyStateHero`) remained unrepresented in the Storybook catalog, leaving critical styling, bounds clipping, and contrast variations untested outside live integration runs.

4. **The Hot-Reloading Mirage**:
   Developers accustomed to web tooling frequently request dynamic code replacement (e.g. `dlopen` shared-library reloading). In a compiled Rust TUI environment, runtime shared-library hot swapping across ABI boundaries introduces catastrophic undefined behavior, terminal state corruption (lost raw mode, locked Kitty keyboard protocols), and excessive architectural complexity.

We need a definitive, zero-compromise architectural solution that provides continuous real-time animation, sub-second developer turnaround, and complete visual catalog verification without sacrificing Rust's memory safety or zero-cost execution guarantees.

---

## Decision Drivers

- **Zero Terminal Freezing**: Component animation and timer ticks must run smoothly on a nominal cadence (nominal 60ms / ~16 FPS) without maxing out CPU or corrupting terminal buffers.
- **Sub-Second Continuous Feedback**: Modifications to component code must reflect immediately in the active story without losing the developer's cursor, viewport box, or theme context.
- **Rejection of Fragile Dynamic Libraries**: No `dlopen`, C ABI conversions, or unsafe pointer swaps. Development turnaround must rely on fast incremental compilation and hermetic state resumption.
- **Universal Component Representation**: Expand the Storybook catalog from a narrow set of cards into a comprehensive gallery covering Tier A presenters and Tier B TEA containers.
- **Strict Backward-Incompatible Purification**: Refactor traits and structures cleanly. Do not retain deprecated flags, unused wrappers, or dead code paths.

---

## Architectural Decisions

### 1. Continuous Non-Blocking Live Tick Engine (`[INV-TUI-STORY-04]`)

The interactive Storybook workbench transitions from a blocking event reader to a non-blocking poll loop (`crossterm::event::poll(timeout)` with a nominal 60ms interval):

```text
┌────────────────────────────────────────────────────────┐
│           Storybook Continuous Engine Loop             │
├────────────────────────────────────────────────────────┤
│ 1. terminal.draw() -> Frame Rendering                  │
│ 2. event::poll(remaining_timeout)                      │
│    ├─ Event::Key -> Dispatched to Active Story or Nav  │
│    ├─ Event::Resize -> Terminal Redraw Scheduled       │
│    └─ Timeout Expired -> Tick Dispatch                 │
│ 3. If tick elapsed >= 60ms:                            │
│    └─ active_story.tick() -> Advance Spinner / Clock   │
└────────────────────────────────────────────────────────┘
```

The `StorybookComponent` protocol gains a first-class `tick` hook:
```rust
pub trait StorybookComponent: Component {
    // ...
    fn tick(_props: &mut Self::Props, _state: &mut Self::State) -> StoryEventOutcome {
        StoryEventOutcome::Ignored
    }
}
```
Components with active work (such as `ToolCard` running spinners and `EmptyStateHero` carousel rotation) mutate their volatile state deterministically on tick, providing vibrant real-time previews. Pressing `[P]` toggles live ticking on and off.

### 2. Dev-Loop State Continuity & Safe Hot Development (`[INV-TUI-STORY-05]`)

Rather than attempting fragile dynamic library reloading, we implement **Hermetic State Continuity Persistence**. On every navigation step, theme switch, viewport change, or query filter modification, the workbench serializes a compact JSON capsule to temporary storage (`$TMPDIR/nuo_storybook_continuity.json`):

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StorybookContinuityState {
    pub selected_component: String,
    pub selected_story: String,
    pub viewport_mode: String,
    pub theme_index: usize,
    pub search_query: String,
}
```

When coupled with `cargo watch -x 'run -p nuo --example storybook'`:
1. The developer modifies component rendering or layout code.
2. `cargo watch` triggers incremental compilation (sub-second in dev mode).
3. The new binary launches, immediately hydrates its position and environment from the continuity capsule, and resumes focused exactly on the edited scenario.

This delivers the ergonomic speed of hot reload with 100% Rust type safety and zero terminal instability.

### 3. Broad Catalog Expansion

We expand the Storybook catalog across eight canonical domains, encompassing both Tier B interactive containers and Tier A presenter bridges:

1. **`NoticeCard`** (4 stories): Baseline Info, Dangerous Operation Warning, In-Flight Running Action, Long Text Truncation.
2. **`AskUser`** (4 stories): Standard Confirmation, High-Risk Bash Operation, Multi-Choice Branching, Extreme Long Text Wrap.
3. **`ToolCard`** (5 stories): Streaming In-Flight (Live Spinner), Standard Edit Diff, Massive 100-Line Diff Stress, Command Failure Error, Collapsed Summary.
4. **`Toast`** (5 stories): Clipboard Success, Interruption Warning, Command Rejection Error, Slash Command Ack, Multi-line Overflow.
5. **`WhichKey`** (3 stories): Ctrl-X Armed Namespace, Dialog Leaving Context, High-Density Multi-Action Guide.
6. **`ModalDialog`** (3 stories): Confirmation Prompt, Dangerous Sandbox Escape Warning, Help Keybindings Reference.
7. **`TurnBand`** (3 stories): Standard Model Turn (Sonnet), Deep Reasoning Channel (GLM-5.3 xhigh), Multi-Agent Delegated Turn.
8. **`EmptyStateHero`** (3 stories): Standard Figlet Brand Hero, Missing Provider Blocker, Compact Viewport Guidance.

---

## Normative Invariants Established

- **`[INV-TUI-STORY-04] Continuous Non-Blocking Live Tick Loop`**:
  The interactive Storybook workbench MUST run on a non-blocking polling event loop with periodic tick dispatch (nominal 60ms interval). Animated components MUST advance spinner glyphs and timers smoothly without requiring keyboard input or consuming excessive CPU.
- **`[INV-TUI-STORY-05] Dev-Loop State Continuity Persistence`**:
  The Storybook workbench MUST continuously checkpoint active navigation state (selected component, story identifier, viewport mode, theme preset, and filter query) to disk. Restarts MUST resume directly at the active context, guaranteeing zero context loss during recompilation.

---

## Consequences

- **Positive**:
  - Developers can evaluate true live streaming, spinner rotation, and timing animations directly in the offline Storybook.
  - Editing UI code under `cargo watch` immediately resumes at the component under test with preserved viewport and theme.
  - Snapshot coverage expands from 13 to 30 canonical scenarios, catching regression across toasts, modals, which-key overlays, and turn headers.
  - 100% memory safe: zero unsafe dynamic linking or unstable ABI tricks.
- **Negative & Trade-offs**:
  - The default snapshot golden suite expands from 13 to 30 fixtures (still executing in under 0.1s in unit tests).

---

## Rejected Alternatives & Negative Knowledge

### 1. Dynamic Library Hot-Reloading (`libloading` / `dlopen`)
- *Failure Mode*: Loading `.so` or `.dylib` files at runtime in Rust requires exposing `extern "C"` boundaries or maintaining identical memory layout across compiler invocations. Any mismatch in standard library types or crossterm backend state produces segmentation faults, terminal screen corruption, and orphaned processes.
- *Rejection*: Native incremental compilation combined with disk-backed state continuity achieves the same goal with total stability.

### 2. WebAssembly/Browser Emulation for TUI Preview
- *Failure Mode*: Running xterm.js in a browser via Wasm requires a full dual-target toolchain (wasm32-unknown-unknown, WebGL/Canvas bindings) that duplicates the terminal presentation pipeline and introduces browser font rendering differences absent from physical terminal emulators.

---

## Links

- Amends: [ADR-0073](0073-offline-component-storybook-and-hermetic-visual-regression-pipeline.md), [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md)
- Reference Docs: [`docs/reference/tui/interaction.md`](../reference/tui/interaction.md), [`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md)
