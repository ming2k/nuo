---
id: ADR-0053
title: "Capability-First Orthogonal Terminal Composition, Autonomous Visual Archetype Projection, and Zero-Heuristic Adaptation Topology"
status: accepted
date: 2026-10-27
scope: substrate/nuotc, tui/nuo-tui, render/pipeline, architecture/adaptation
superseded_by: null
negative_knowledge: true
---

# 0053. Capability-First Orthogonal Terminal Composition, Autonomous Visual Archetype Projection, and Zero-Heuristic Adaptation Topology

- Status: Accepted
- Date: 2026-10-27
- Deciders: Nuo Architecture Working Group
- Consulted: Terminal Substrate (`nuotc`), Presentation (`nuo-tui`), Runtime Architecture, Documentation Governance
- Informed: System Architects, Platform Engineering
- Ratifies and replaces provisional: `` (Terminal Capability Profiling and Standards-Based Drivers), `` (Capability-First Visual Archetypes and Responsive Layout Pipeline)
- Complements: [ADR-0003](0003-autonomous-terminal-canvas-substrate-nuotc.md), [ADR-0011](0011-nuo-tui-presentation-and-nuo-server-container.md), [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md), [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md), [ADR-0028](0028-scroll-anchoring-across-terminal-reflow.md), [ADR-0047](0047-autonomous-surface-interaction-architecture-and-spatial-event-delegation.md)

---

## Context and Problem Statement

Terminal User Interfaces (TUIs) must operate across wildly divergent execution environments: from modern GPU-accelerated graphical terminal emulators (Ghostty, Kitty, WezTerm, Alacritty) to terminal multiplexers (tmux, zellij), remote SSH jump hosts, Linux virtual consoles (`/dev/tty1..6`), and physical serial/rescue lines (DEC VT100, getty).

Conventional terminal interface engines exhibit three fatal architectural defects:

1. **The Brittle Terminal-Type Classification Anti-Pattern**:
   Applications attempt to classify terminals by monolithic identity (e.g. `enum TerminalType { Kitty, ITerm2, Alacritty, WindowsTerminal, LinuxVT, XTerm256 }` or ad-hoc sniffing like `is_kitty()`). In real-world developer workflows, terminal capability is never a monolithic property of an executable; it is the dynamic runtime intersection of `[Host OS / PTY] × [Terminal Emulator] × [Multiplexer (tmux/screen)] × [Remote SSH Session] × [Locale / Font]`. Furthermore, ubiquitous environment spoofing (e.g. modern TrueColor terminals emitting `TERM=xterm-256color` for backward compatibility) renders name-based dispatch brittle and perpetually broken.

2. **The Low-Level Capability Leak & Presentation Sprawl**:
   Conversely, naively exposing raw, fragmented hardware flags (`supports_truecolor: bool`, `color_count: u32`, `supports_sync_update: bool`, `charset_ascii: bool`) directly to presentation widgets causes severe combinatorial branching across every card, modal, and button component (`if caps.color_count <= 16 && !caps.supports_mouse ...`). View code degenerates into an unmaintainable tangle of hardware escape decisions.

3. **False Assumptions of Universal Feature Parity**:
   Capabilities such as 24-bit background color luminance steps, atomic frame synchronization (DEC Mode 2026), and modifier-bearing keyboard protocol negotiation are not universally supported. Projecting subtle dark-mode luminance elevation onto monochrome or 16-color consoles produces unreadable low-contrast slop; conversely, forcing heavy ASCII frames onto modern terminals creates unnecessary visual noise that squanders limited horizontal columns.

4. **Provisional Specification Drift**:
Early prototype implementations relied on out-of-sequence provisional references (`` and ``), leaving the capability modeling and visual elevation pipeline without a formally ratified, canonical record in the repository's architectural ledger.

We require a rigorous, forward-looking architecture that eliminates terminal brand heuristics, establishes orthogonal capability composition at the driver layer, provides autonomous two-phase projection into semantic visual archetypes, and guarantees 100% automated, zero-configuration adaptation across all environments.

---

## Decision Drivers

- **Orthogonal Capability Honesty (`[INV-TERM-01]`)**: Terminal capabilities must be detected and modeled as orthogonal dimensions based on hardware and international standards (ITU-T T.416, ECMA-48, ANSI X3.64, ISO/IEC 10646), never by emulator brand or executable name.
- **Two-Phase Archetype Projection (`[INV-TERM-02]`)**: Presentation components must consume semantic elevation intents (`Surface`, `Card`, `Panel`, `Overlay`) resolved against high-level visual archetypes (`ElevationArchetype`), never inspecting raw hardware capability flags.
- **Autonomous Zero-Config Adaptation (`[INV-TERM-03]`)**: Capability detection and graceful visual degradation must execute fully automatically upon startup without requiring manual user "graphics quality" or mode selection. Manual theme selections must adaptively respect the underlying capability ceiling.
- **Spatial Cost Accounting (`[INV-TERM-04]`)**: Visual archetypes requiring physical ASCII bounding boxes must project explicit `SpatialCost` (horizontal and vertical margins) to layout engines and responsive breakpoint solvers (`LayoutTier`).

---

## Considered Options

- **Option 1: Monolithic Terminal-Type Enumeration**: Classify environments into fixed terminal types (`Kitty`, `ITerm2`, `Tmux`, `LinuxConsole`, `VT100`) and dispatch behavior via string sniffing.
- **Option 2: Direct Low-Level Feature Flag Inundation**: Expose 10+ boolean hardware capability flags directly to every UI widget and modal component.
- **Option 3: Two-Phase Orthogonal Composition & Semantic Archetype Projection (Chosen)**:
  - Phase 1 (Substrate & Drivers): Orthogonal capability matrix (`TerminalProfile`) driving standards-based escape sequence emitters (`TerminalDriver`).
  - Phase 2 (Design System & Viewport): Dimensional reduction projecting capabilities into three canonical visual archetypes (`Chromatic`, `Hybrid`, `Structured`) with explicit `SpatialCost` and semantic elevation containers.

---

## Decision Outcome

Chosen Option: **Option 3: Two-Phase Orthogonal Composition & Semantic Archetype Projection**.

### 1. Architectural Topology: The Two-Phase Pipeline

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                      Phase 1: Substrate & Protocol                      │
├─────────────────────────────────────────────────────────────────────────┤
│ [Runtime Environment] TERM, COLORTERM, NO_COLOR, standards inspection   │
│                                   │                                     │
│                                   ▼                                     │
│ [TerminalProfile]  • ColorStandard (DirectColor | Indexed256 |          │
│                                     Ansi16 | Monochrome)                │
│                    • CharsetStandard (Utf8 | Ascii)                     │
│                    • supports_sync_update (DEC Mode 2026)               │
│                    • keyboard_enhancement (Kitty Protocol)              │
│                    • supports_mouse (SGR Mode 1002 Button/Wheel)        │
│                    • supports_bracketed_paste                           │
│                                   │                                     │
│                     ┌─────────────┴─────────────┐                       │
│                     ▼                           ▼                       │
│     [Escape Drivers (TerminalDriver)]   [Phase 2: Semantic Projection]  │
│     • DirectColorDriver (24-bit + 2026)         │                       │
│     • Indexed256Driver (Quantize 6x6x6)         ▼                       │
│     • Ansi16Driver (Redmean Perceptual) [ElevationArchetype]            │
│     • MonochromeDriver (SGR 7 Reverse)  • Chromatic (SpatialCost::ZERO) │
│                                         • Hybrid (SpatialCost::ZERO)    │
│                                         • Structured (SpatialCost: 2x2) │
├─────────────────────────────────────────────────────────────────────────┤
│                     Phase 2: Presentation & Viewport                    │
├─────────────────────────────────────────────────────────────────────────┤
│ [ElevationContainer] Surface -> Card -> Panel -> Overlay               │
│ [Responsive Breakpoints] LayoutTier::from_rect(area, archetype)         │
│ [Components] Composer, Markdown, Modals consume pure Semantic Intents   │
└─────────────────────────────────────────────────────────────────────────┘
```

### 2. Standards Grounding & Protocol Drivers

Capabilities are anchored exclusively to international standards rather than application heuristics:
1. **DirectColor (ITU-T T.416 / ISO/IEC 8613-6)**: 24-bit RGB TrueColor via `\x1b[38;2;r;g;bm` / `\x1b[48;2;r;g;bm`. Accompanied by DEC Mode 2026 (`BeginSynchronizedUpdate` / `EndSynchronizedUpdate`) for atomic, zero-flicker streaming rendering.
2. **Indexed 256-Color**: 8-bit palette via `\x1b[38;5;Nm` / `\x1b[48;5;Nm`. Colors outside the palette are quantized via a 6×6×6 color cube (indices 16..231) and a 24-step grayscale ramp (indices 232..255).
3. **ANSI 16-Color (ECMA-48 / aixterm)**: 16 named high-contrast colors. RGB inputs are quantized using Redmean-weighted perceptual Euclidean distance. Backgrounds in high-elevation cards default to `Color::Reset` to prevent garish blocks.
4. **Monochrome (ANSI X3.64 / DEC VT100)**: Serial lines, rescue consoles, and `NO_COLOR=1`. Zero color escape sequences are emitted (`Color::Reset`). Visual emphasis strictly uses DEC VT100 standard **SGR 7 (Reverse Video)**, **SGR 4 (Underline)**, and **SGR 1 (Bold)**.

### 3. Visual Elevation Archetypes & Spatial Cost

Visual hierarchy is decoupled from raw hardware and represented by three canonical archetypes:

- **`ElevationArchetype::Chromatic`**:
  Surfaces are distinguished by subtle luminance gradients (`app_bg` → `surface` → `code_bg` → `panel_bg`). Floating overlays, dropdowns, and cards are rendered **borderless**. Spatial cost is zero (`SpatialCost::ZERO`), granting 100% of terminal dimensions to content.
- **`ElevationArchetype::Hybrid`**:
  Surfaces use standard 16-color high-contrast foreground accents and clean single-character dividers. Spatial cost is zero.
- **`ElevationArchetype::Structured`**:
  Because background differentiation is impossible on monochrome consoles, elevation MUST be structural. Overlays and cards are framed with explicit ASCII/CP437 boxes (`+--+`, `|`). Active focus and selection strictly employ Reverse Video. Spatial cost is framed (`SpatialCost { horizontal: 2, vertical: 2 }`).
- **Responsive Layout Accounting**: `LayoutTier::from_rect(rect, archetype)` automatically subtracts the archetype's spatial cost from the raw dimensions before evaluating Wide vs Compact breakpoint thresholds, preventing layout overflow in framed environments.

### 4. Input Protocols and Safety Fences

- **Progressive Keyboard Enhancement**: Negotiates the Kitty Keyboard Protocol (`supports_keyboard_enhancement()` + `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`). Supported in Windows Terminal (ConPTY) and modern Linux/macOS emulators, disambiguating `Ctrl+M` from `Enter`, and enabling `Shift+Enter` multi-line input.
- **Button-Event Mouse Fence (`[INV-SURFACE-09]`)**: Mouse tracking is strictly bounded to Mode 1002 (button press, release, drag, wheel) and SGR-1006. Mode 1003 all-motion hover tracking is strictly prohibited to prevent SGR buffer truncation leaks from spilling raw escape sequences into the Composer.
- **Minimum Geometry Contract (`[INV-TUI-MIN-01..06]`)**: Terminals resized below `44 × 12` freeze user input and paint a clean notice, preventing blind mutation and state drift.
- **Scroll Anchoring Across Reflow (`[INV-ANCHOR-01..06]`)**: Reading position during resize reflow is anchored by `(message_id, row_offset)`, preserving the user's viewport without visual jumps.

---

## Invariants & Behavioral Boundaries

### `[INV-TERM-01]` Orthogonal Capability Composition
Terminal adaptation must never branch on emulator identity, program name, or client brand. Capabilities must be detected and modeled as orthogonal standard dimensions (`ColorStandard`, `CharsetStandard`, `supports_sync_update`, `supports_mouse`, `keyboard_enhancement`).

### `[INV-TERM-02]` Two-Phase Archetype Projection
Visual components must never directly evaluate low-level capability flags (e.g. `color_count`, `supports_truecolor`, `is_vt100`). Components must declare semantic elevation intents (`Surface`, `Card`, `Panel`, `Overlay`) resolved against `ElevationArchetype` and `Theme`.

### `[INV-TERM-03]` Autonomous Zero-Config Adaptation
Terminal capability profiling and archetype degradation must operate fully autonomously upon startup with zero manual user configuration. Manual color scheme overrides must adaptively honor the underlying capability ceiling (e.g. degrading to Monochrome under `NO_COLOR`).

### `[INV-TERM-04]` Spatial Cost Accounting
Any visual archetype requiring structural framing (`Structured`) must project non-zero `SpatialCost` to layout solvers and responsive breakpoints (`LayoutTier`), ensuring that border cell overhead is deducted before evaluating content bounds.

---

## Negative Knowledge & Rejected Alternatives

### 1. Why Not Terminal-Type Enumeration / Brand Sniffing?
- **Why considered**: Familiar pattern used in early CLI prototypes (`if term == "kitty"`).
- **Why rejected**: Brittle and untruthful. A terminal's actual runtime capabilities are a dynamic intersection of emulator, multiplexer (tmux), SSH proxy, and OS PTY. Emulators frequently spoof `TERM` (e.g. `xterm-256color`), and every newly released terminal breaks the hardcoded enumeration.

### 2. Why Not Direct Low-Level Feature Flag Leaks to UI Widgets?
- **Why considered**: Gives individual widgets maximum granular control.
- **Why rejected**: Triggers combinatorial sprawl and cognitive exhaustion. Requiring every button, modal, and text block to evaluate 8+ boolean hardware flags guarantees inconsistent degradation, visual glitches, and massive test complexity.

### 3. Why Not Universal Framed Borders Across All Terminals?
- **Why considered**: Drawing borders everywhere simplifies layout by avoiding archetype differentiation.
- **Why rejected**: Visual noise and wasted space. On modern TrueColor displays, borders add cognitive clutter and consume 2 horizontal columns and 2 vertical rows per box. Borderless elevation provides a cleaner, modern GUI-grade aesthetic.

### 4. Why Not Manual User "Graphics Quality" Settings?
- **Why considered**: Allows users to force specific modes.
- **Why rejected**: Violates user experience simplicity. An interactive AI agent harness must just work out of the box across any terminal without manual configuration hurdles.

---

## Architectural Consequences

### Positive
- **Guaranteed Zero-Flicker Streaming**: High-frequency LLM token generation (60+ Hz) renders seamlessly via double-buffered diffing and DEC Mode 2026.
- **Robust Cross-Platform Portability**: Seamless degradation from 24-bit TrueColor down to physical serial VT100 consoles without code changes in UI components.
- **Headless Server Purity**: `nuo-server` has zero dependencies on `nuotc` or `crossterm`, maintaining clean containerization and headless scriptability.
- **Canonical Architecture Record**: Ratifies and replaces provisional `` and `` references with a single authoritative standard.

### Negative
- Custom theme authors must design palettes using semantic tokens (`app_bg`, `surface`, `panel`) rather than hardcoding raw RGB values for borders or backgrounds.

---

## Compliance and Verification

1. **Unit and Snapshot Tests**: `cargo test -p nuo-tui tests::elevation_pipeline` verifies mapping from `TerminalProfile` to `ElevationArchetype`, spatial cost deduction, and VT100 reverse video behavior.
2. **Workspace Verification**: `cargo check --workspace` and `cargo test --workspace` verify architecture constraints and regressions.
