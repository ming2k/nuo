---
id: ADR-0091
title: "Sovereign Tab Context Isolation and TabBar Boundary Purity"
status: accepted
date: 2026-11-26
scope: tui/nuo-tui, architecture/surfaces, presentation/chrome, interaction/tabs
superseded_by: null
negative_knowledge: true
---

# 0091. Sovereign Tab Context Isolation and TabBar Boundary Purity

- Status: Accepted
- Date: 2026-11-26
- Deciders: Core Engineering, Architecture Working Group
- Amends: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md), [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), [ADR-0075](0075-orthogonal-workspace-container-topology-and-intra-tab-scene-discipline.md)

---

## Context and Problem Statement

Following [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md) and [ADR-0075](0075-orthogonal-workspace-container-topology-and-intra-tab-scene-discipline.md), Nuo established an orthogonal spatial topology: the **Horizontal Axis** on Row 0 (Client Workspace Containers / `ClientTab`) is strictly segregated from the **Vertical Axis** (Intra-Tab Scene Navigation and Content).

However, an architectural layer inversion persisted between the global `TabBar` chrome and the inner `Thread` scene context:

1. **Cross-Layer Side Effect Leakage**:
   In `event_loop/actions/mouse.rs`, clicking the empty background of `UiKey::TabBar` (Layer 19) or clicking the already-active `UiKey::TabBarItem` executed `app.transcript_focused = true` (blurring the composer). This treated the top-level container chrome as an intra-scene focus modifier, reaching deep into the `Thread` scene to mutate its editing/browse mode.

2. **The "Two-Click" Interaction Fracture**:
   Because `UiKey::TabBar` spans the entire screen width on Row 0, user clicks intending to switch tabs that landed on TabBar padding, inter-tab separators, or the already-active tab inadvertently triggered composer blur without switching tabs. Users were forced to click a second time (hitting the exact bounding box of the target `TabBarItem`) to execute the switch, creating cognitive and operational friction.

3. **Context Destruction Across Workspace Switches**:
   Intra-tab interaction state (`transcript_focused`, `scroll`, uncommitted `input` draft, and `cursor_position`) was flattened onto the global monolithic `App` struct. Switching from a Thread to Dashboard or Settings wiped the user's browse mode and scroll position by unconditionally hardcoding `app.transcript_focused = false` upon re-entry. Users could not preserve their reading or drafting context when laterally navigating between workspaces.

A clean break from these legacy assumptions is required to establish sovereign tab contexts and strict boundary purity for Tier 1 chrome.

---

## Decision Drivers

- **Strict Hierarchical Purity**: Maintain an impenetrable firewall between Tier 1 Container Chrome (`TabBar`) and Intra-Tab Content State (`ThreadContext`). Container-level chrome MUST NOT mutate inner scene focus or content.
- **Sovereign Tab Context Preservation**: Each `ClientTab` must autonomously preserve its interaction context (focus mode, scroll offset, uncommitted drafts, and cursor position) across lateral workspace transitions.
- **Single-Click Switch Fidelity**: Clicking any inactive tab item must switch workspaces immediately on the first press; clicking the active tab or TabBar background must be clean, idempotent, and non-destructive.
- **Mechanisms Over Exhortations**: Enforce per-tab context isolation and checkpoint/restore mechanics in the core surfaces state machine rather than relying on manual handler hygiene.

---

## Architectural Decisions

```text
═══════════════════════════════════════════════════════════════════════════════
  TIER 1 CONTAINER CHROME: TabBar (Row 0) — Pure Container Operations Only
═══════════════════════════════════════════════════════════════════════════════
       Tab 0: Thread           Tab 1: Dashboard          Tab 2: Settings
  ┌──────────────────────┬──────────────────────┬──────────────────────┐
  │ [Thread: Refactor]   │ [Dashboard]          │ [Settings]           │
  └──────────────────────┴──────────────────────┴──────────────────────┘
     ▲              │
     │              │ (Select / Switch Workspace)
     │              ▼
  ┌────────────────────────────────────────────────────────────────────────┐
  │ Checkpoint & Restore Pipeline (INV-TAB-12)                             │
  │ • Checkpoint departing tab context (scroll, input draft, caret, focus) │
  │ • Restore arriving tab context                                         │
  └────────────────────────────────────────────────────────────────────────┘
                    │
                    ▼
═══════════════════════════════════════════════════════════════════════════════
  INTRA-TAB SOVEREIGN CONTEXT (Content / Scene Level)
═══════════════════════════════════════════════════════════════════════════════
  TabContextState {
      transcript_focused: bool,            // Browse mode vs Composer active
      focused_target: Option<Interactive>, // Selected tool step / trace
      scroll: u16,                         // Reading viewport offset
      input: String,                       // Active uncommitted draft
      cursor_position: usize,              // Caret byte/grapheme index
      input_scroll: usize,                 // Multiline input view offset
  }
```

### 1. TabBar Container Purity (`[INV-TAB-11]`)

- Tier 1 TabBar chrome (`UiKey::TabBar` and `UiKey::TabBarItem`) operates strictly as a container affordance.
- Clicks landing on `UiKey::TabBar` (empty background, margins, or affordance tags) are inert container gestures: they cancel active drags and release pointer capture, but **MUST NOT** alter `transcript_focused`, `selection`, or `focused_target` of the active scene.
- Clicks landing on the already-active `UiKey::TabBarItem` are strictly idempotent no-ops: they preserve the active tab's focus mode and caret intact.
- Blurring the composer into transcript browse mode belongs exclusively to scene-level interactions: clicking within the transcript content rect (`ClickTarget::Content`, `ClickTarget::ContentGap`, `ClickTarget::Dead`) or discrete keyboard navigation chords (`Esc`, `Ctrl+O`).

### 2. Sovereign Tab Context Preservation (`[INV-TAB-12]`)

- Every `ClientTab` maintains an autonomous `TabContextState` tracking its intra-tab interaction context:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Default)]
  pub struct TabContextState {
      pub transcript_focused: bool,
      pub focused_target: Option<InteractiveTarget>,
      pub scroll: u16,
      pub input: String,
      pub cursor_position: usize,
      pub input_scroll: usize,
  }
  ```
- During workspace transitions (`activate_tab_at_index`, `enter_scene`, `handle_close_tab`):
  1. **Checkpoint**: If departing an active thread tab, its live `transcript_focused`, `focused_target`, `scroll`, `input`, `cursor_position`, and `input_scroll` are saved into `active_tab.context`.
  2. **Transition**: The active tab index updates and MRU history records the navigation.
  3. **Restore**: If arriving at a thread tab, its preserved `TabContextState` is restored onto `App`.
- Re-entering a previously visited thread tab restores its exact browse/edit mode, reading scroll offset, and uncommitted draft text, eliminating operating habit loss.

---

## Consequences

- **Positive**:
  - Eliminates the confusing "click twice" defect: clicking an inactive tab switches immediately; clicking the active tab or background is harmlessly idempotent.
  - Full operating habit retention: users reading long transcripts in browse mode can switch to Dashboard or Settings and return without being kicked back to composer mode or losing their scroll position.
  - Zero draft loss: uncommitted text drafts in the composer are preserved when switching between workspaces.
  - Clean architectural layering: Container Chrome (Row 0) no longer reaches down to modify inner Scene focus states.

- **Negative & Trade-offs**:
  - `ClientTab` carries a small in-memory state bundle per tab (~80 bytes), which is negligible for TUI scale (1-10 tabs).
  - Tests that historically expected background TabBar clicks to blur the composer must be updated to align with `[INV-TAB-11]` container purity.

---

## Rejected Alternatives & Negative Knowledge

- **Rejected: TabBar Blanket Blur Surface**:
  Treating clicks anywhere on the TabBar as an implicit "blur composer" gesture was rejected. It violated container/scene stratification and caused accidental blur when users misclicked near tab headers.
- **Rejected: Unconditional Composer Activation on Re-entry**:
  Forcing `app.transcript_focused = false` whenever entering a thread scene was rejected. It destroyed reading workflow for users investigating lengthy execution outputs.
- **Rejected: Global-Only Context Storage**:
  Maintaining a single global focus and scroll register on `App` and forcing tabs to share it was rejected. It coupled independent workspaces and made multi-tab exploration jarring.
