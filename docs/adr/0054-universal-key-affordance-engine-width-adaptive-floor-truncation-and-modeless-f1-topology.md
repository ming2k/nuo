---
id: ADR-0054
title: "Universal Key Affordance Engine, Width-Adaptive Floor Truncation, and Modeless F1 Reference Topology"
status: accepted
date: 2026-10-27
scope: tui/nuo-tui, interaction/keymap, presentation/chrome, UX/affordance
superseded_by: null
negative_knowledge: true
---

# 0054. Universal Key Affordance Engine, Width-Adaptive Floor Truncation, and Modeless F1 Reference Topology

- Status: Accepted
- Date: 2026-10-27
- Deciders: Nuo Presentation Working Group, Architecture Governance
- Consulted: Presentation (`nuo-tui`), Input Engine (`input`), Surface Navigation (`surfaces`)
- Informed: System Architects, Documentation Governance
- Complements: [ADR-0020](0020-interactive-component-registry-single-source-of-truth.md), [ADR-0022](0022-minimum-terminal-geometry-and-frozen-input-contract.md), [ADR-0023](0023-scene-namespace-palette-entry-and-borderless-popups.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md), [ADR-0053](0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md)

---

## Context and Problem Statement

Keyboard shortcut guidance ("affordance") is the primary discoverability vehicle in a terminal user interface. In `nuo-tui`, shortcuts and hint strips historically evolved through decentralized patches:
1. **Visual Style Disparity**: Across the codebase, shortcut indicators exhibited competing visual grammars: `composer_hints.rs` paired bold brand labels with unstyled keys; `footer.rs` formatted white bold keys with silver action labels; while tab bar menus rendered inverted label-first layouts (`menu C-x`). The lack of a unified scan anchor increased cognitive reading friction.
2. **Brittle Width Degradation & Dead Truncation**: When terminal widths degraded, footers either collapsed labels into opaque keys-only strings or dropped items blindly. Once dropped, low-frequency shortcuts were permanently lost from sight with zero interactive discovery path.
3. **Modality Collision in Modeless Composer**: Proposing single-glyph characters like `?` for contextual help creates an intractable conflict in `nuo-tui`'s "Composer-first, zero input modality" architecture (`[INV-COMP-01]`). In Composer and live search filters, printable characters flow strictly into the text buffer. Intercepting `?` disrupts typing, while modal switches (normal vs insert modes) violate the terminal readline contract.

We require a consolidated, uncompromising key affordance engine that guarantees visual scan consistency, deterministic width-bounded degradation with overflow recovery, and pure modeless terminal help.

---

## Decision Drivers

1. **High Visual Scanability**: The user's eye must immediately anchor onto high-contrast key tokens, while action descriptions recede into subdued tones.
2. **No Information Cliff**: If a restricted width cannot display all shortcuts, the layout must deterministically reserve space for an interactive overflow trigger (`F1 更多` / `F1 help`), guaranteeing full discoverability.
3. **Strict Modelessness**: Text editing in Composer, search pickers, and prompts must never lose printable characters or suffer input mode bifurcations.
4. **Single Source of Truth (SSOT)**: Affordances must derive reactively from the active focus context rather than ad-hoc string formatting in view files.

---

## Decision & Core Architecture

We establish the **Universal Key Affordance Engine** in `nuo-tui` governed by three foundational pillars:

```text
┌────────────────────────────────────────────────────────────────────────┐
│  Enter 发送    ⇧Enter 换行    Ctrl+R 历史    Tab 补全    F1 更多       │
└────────────────────────────────────────────────────────────────────────┘
    ▲      ▲
    │      └─ [Short Label]: Regular font + muted dark gray (keycap_label_style)
    └──────── [Key Token]:   Bold + Semantic Accent/FG (keycap_accent_style)
```

### 1. High-Contrast Scan-Prioritized Visual Grammar

Every key affordance is formatted as an atomic, compact pair:
- **Key Token (Left)**: Bold text with semantic color hierarchy:
  - `Primary / Action`: Theme accent (`theme.keycap_accent()`, e.g. `Enter send`).
  - `Neutral / Secondary`: High-contrast keycap neutral (`theme.keycap_fg()`, e.g. `Tab`, `↑↓`, `Esc`).
  - `Warning / Interrupt`: Alert tone (`theme.keycap_warn()`, e.g. `Esc Esc interrupt`).
- **Action Label (Right)**: Subdued, readable dark gray (`theme.keycap_label_style()`), separated from the key token by a single space.
- **Inter-Item Gap**: A compact 2-to-3 character spacing between affordance pairs, without enclosing brackets (`[` / `]`).

### 2. Deterministic Width-Adaptive Floor-1 Overflow Math

Given an available horizontal width $W$ and an affordance set ordered by priority rank:
1. If the entire set fits within $W$ including inter-item gaps, all items are rendered.
2. If the entire set exceeds $W$:
   - Let $M$ be the maximum number of items that would physically fit in $W$ without an overflow indicator.
   - The engine computes $K = \max(1, M - 1)$, reserving the remaining width budget for the universal overflow slot: `F1 更多` (or `F1 help`).
   - The primary strip renders the top $K$ high-frequency items followed by the `F1` overflow trigger.
   - The remaining low-frequency items are retained in the contextual reference registry, accessible immediately via `F1`.

```text
[ Available Width W: 36 columns ]
Full Affordances (Demand: 52 cols):
  Enter send (10)  ·  ⇧Enter newline (15)  ·  Ctrl+R history (14)  ·  Tab complete (12)
Fit Calculation:
  Max fit M = 2 items (27 cols).
  Floor-1 rule: K = M - 1 = 1 item.
Rendered Result:
  Enter send      F1 more
  └── top 1 ──┘   └── reserved overflow slot ──┘
```

### 3. Modeless Universal Reference: `F1` (Zero Text Interception)

In adherence to `nuo-tui`'s "Composer-first, zero input modality" invariant, printable characters must never be hijacked:
- **Universal Advertised Reference Key**: `F1` is the canonical key token displayed across all footers, chrome strips, and popovers.
- **Modeless Help Dispatch**:
  - In Dialog surfaces: `F1` toggles `dialog_keys` (contextual in-dialog key reference).
  - In Main Chat/Composer: `F1` toggles the Command Palette / Action Reference (`InputAction::ViewSwitcherToggle`), providing complete shortcut discoverability.
- **Silent Ergonomic Aliases**:
  - For keyboards with media-mapped function keys (e.g. macOS), `Alt+?` is bound as a silent alias at router level.
  - In non-editing, read-only dialog browsing modes, `?` continues to function as an ergonomic fallback, but the visual affordance strictly advertises `F1`.

### 4. Focus-Driven Reactive Affordance Projection

All surfaces (Composer quadrants, Dialogs, Sheet modals, Scene views) project their affordances via typed `KeyAffordance` descriptors to the unified engine. Hardcoded footer strings are replaced with standard ranked specifications.

---

## Invariants & Behavioral Boundaries

- **`[INV-AFFORDANCE-01]` Key-First Visual Structure**: Every key affordance MUST position the key on the left (bold + semantic color) and the concise action label on the right (regular + dark gray). Enclosing brackets (`[`/`]`) are strictly prohibited.
- **`[INV-AFFORDANCE-02]` Floor-1 Width Overflow**: Whenever an affordance strip overflows its bounding rectangle, it MUST allocate the final visible slot to the `F1` overflow trigger, displaying no more than `floor(max_fit) - 1` regular items.
- **`[INV-AFFORDANCE-03]` Modeless Help SSOT**: The visible help shortcut token across all UI surfaces MUST be `F1`. Single-letter printable characters (`?`, `h`) MUST NEVER be advertised as help shortcuts in input-capable or composer contexts.
- **`[INV-AFFORDANCE-04]` Non-Destructive Interactive Recovery**: Every overflowed affordance set MUST have an interactive path via `F1` to inspect all available keybindings in that focus state.
- **`[INV-AFFORDANCE-05]` Zero Modality In Composer**: Composer input buffers MUST NOT switch into vim-like modal states to support key guidance. Typing must flow unhindered to the cursor.

---

## Rejected Alternatives (Negative Knowledge)

- **Rejected: Modal `?` for Help in Composer**:
  *Why rejected*: Violates `[INV-COMP-01]` and `[INV-AFFORDANCE-05]`. Intercepting `?` breaks natural language typing when composing questions to AI models, while introducing normal/insert modes degrades the terminal CLI editing flow.
- **Rejected: Truncation with Ellipsis (`...`)**:
  *Why rejected*: Pure ellipsis truncation creates an information dead-end. The user sees that keys are missing but has no way to invoke or inspect them. Reserving the final slot for an interactive `F1` trigger transforms degradation into discoverability.
- **Rejected: Universal Accent Color on All Keycaps**:
  *Why rejected*: Rendering every keycap in bright theme accent causes high visual noise and eye fatigue. Differentiating primary actions (accent) from neutral navigation (keycap neutral) and warnings (warn tone) preserves visual hierarchy.
- **Rejected: Per-Component Ad-hoc Hint Formatting**:
  *Why rejected*: Letting individual modals construct disparate spans results in rapid UI divergence and untestable layout breaks under resizing.
