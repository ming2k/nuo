---
id: ADR-0076
title: "Canonical Unselectable List Item Topology, Inert Skip Navigation, and Gestalt Spacing Firewall"
status: accepted
date: 2026-11-17
scope: tui/nuo-tui, ui/components, interaction/modals, presentation/chrome
superseded_by: null
negative_knowledge: true
---

# 0076. Canonical Unselectable List Item Topology, Inert Skip Navigation, and Gestalt Spacing Firewall

- Status: Accepted
- Date: 2026-11-17
- Deciders: Nuo Architecture Working Group
- Consulted: TUI Presentation, Interaction Design, Product Architecture
- Informed: System Architects, Release Engineering
- Amends: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0047](0047-autonomous-surface-interaction-architecture-and-spatial-event-delegation.md)

---

## Context and Problem Statement

In keyboard-driven terminal user interfaces, list pickers (`/models`, `/connections`, `/tools`) and dropdowns serve as rapid decision engines. Prior to this decision, the treatment of disabled, locked, or unavailable list items suffered from severe architectural bifurcation and UX defects:

1. **The Phantom Focus Anti-Pattern**:
   Navigating list dialogs (e.g. `ModelsDialog`) used a naive circular stepping iterator (`(current + 1) % count`) that stepped onto unusable rows. When landing on an unusable row, the surface suppressed the brand selection background, creating an invisible "ghost cursor" where the user could not locate their position.
2. **Toast-on-Activation Dead Ends**:
   Pressing <kbd>Enter</kbd> while hovering on an unusable model dispatched an activation action intercepted by an ad-hoc local toast refusal (`"This model is unavailable: Requires subscription or account"`). This violated terminal ergonomics: forcing users to spend keypresses traversing non-actionable items only to hit a delayed refusal dead-end.
3. **Gestalt Spacing Breakdown and Text Smashing**:
   In `ListRow::finish`, when the column position exceeded the computed `trailing_start` (e.g., in narrow modal viewports or with long refusal strings), zero whitespace padding was emitted between preceding fixed/columnar atoms and trailing badges. This produced catastrophic string smashing such as:
   ```text
   Gemini 4 Argonlocked: Requires subscription or account
   ```
4. **Grammar Inconsistency**:
   Individual dialogs invented arbitrary text-join punctuation (e.g. `locked: reason`) that contradicted the repository's Visual Language specification ([`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md)), which mandates em-dash joins (`locked — reason`).

A decisive, uncompromising clean break from this legacy burden is required to establish a universal, canonical pattern for all current and future list/picker surfaces.

---

## Decision Drivers

- **Mechanisms Over Exhortations**: Replace fragmented procedural checks across dialog handlers with a centralized, mathematical skip-navigation contract (`rotate_selectable`).
- **Terminal Ergonomic Floor**: Every selectable stop of the keyboard cursor must represent an actionable entity. Unusable items must be automatically bypassed during navigation.
- **Strict Inertia**: Unusable items must be completely inert to both keyboard activation and pointer clicks; they cannot acquire focus, emit toasts, or trigger requests.
- **Gestalt Spacing Firewall**: Layout builders (`ListRow`) must enforce an unconditional minimum gap (`GROUP_GAP`) before trailing atoms, making text collision physically impossible regardless of width constraints.
- **Pre-attentive Visual Demarcation**: Disabled rows must visually declare their status through uniform subdued palette (`theme.dim()`), stripped bold modifiers, and canonical em-dash grammar.

---

## Architectural Decisions

### 1. Universal Skip-Navigation Contract (`[INV-TUI-INERT-01]`)

All selectable list surfaces must navigate via the canonical `rotate_selectable` state machine:

```rust
pub fn rotate_selectable<F>(
    cur: usize,
    count: usize,
    forward: bool,
    mut is_selectable: F,
) -> usize
where
    F: FnMut(usize) -> bool,
```

- **Forward & Backward Skipping**: When pressing <kbd>Down</kbd>, <kbd>Up</kbd>, or wrapping around list boundaries, `rotate_selectable` tests items against `is_selectable(index)`. The cursor jumps directly to the next selectable index, bypassing all intermediate unusable rows.
- **Inert Pointer Events**: Spatial clicks landing on an unselectable row are consumed as no-ops. They never update `index`, focus the row, or trigger activation.
- **All-Unusable Stability**: If all items in a list (or filtered search results) are unselectable, `rotate_selectable` exhausts `count` cycles and returns the current index clamped to `count.saturating_sub(1)`. The interface remains stable without hanging or throwing index out-of-bounds panics, and activation is inert.
- **Initial Focus Hygiene**: When opening a dialog, the initial index must resolve to the active usable item or the first usable item (`rows.iter().position(|r| r.usable)`), never landing on a disabled item by default.

### 2. Gestalt Spacing Firewall & Text Hygiene (`[INV-TUI-INERT-02]`)

`ListRow::finish` is fortified with an unconditional minimum gap firewall:

```rust
if trailing_count > 0 {
    let min_trailing_start = if col > self.gutter {
        col + GROUP_GAP
    } else {
        col
    };
    let start = trailing_start.max(min_trailing_start);
    if start > col {
        spans.push(pad(start - col, bg));
        col = start;
    }
    // Render trailing atoms...
}
```

- **Collision Impossibility**: Even when viewport geometry is constrained or text overflows available width, trailing badges are guaranteed at least `GROUP_GAP` (2 columns) of background separation from preceding atoms. Collisions such as `Argonlocked` are mathematically prevented.
- **Canonical Join Grammar**: Per [`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md), status explanations must follow an em dash:
  `locked — {reason}`
  Arbitrary colon joins (`locked: {reason}`) are abolished.

### 3. Subdued Visual Presentation

A disabled/locked list item must never compete visually with actionable choices:
- **No Bold Weight**: Modifiers (`Modifier::BOLD`) are stripped from all spans within the disabled row (labels, wire IDs, connections, and badges).
- **Uniform Dimmed Palette**: The foreground color for all text elements is set to `theme.dim()` (or `theme.muted()`).
- **No Selection Brand**: An unselectable row never renders the brand highlight fill (`ChoiceTone::Filled`), remaining grounded on `theme.panel()`.

---

## Normative Invariants

- **`[INV-TUI-INERT-01] Inert Disabled Item Navigation`**:
  All selectable list and dialog surfaces MUST route navigation through skip-aware iterators (`rotate_selectable`). The cursor MUST NEVER land on or select an unusable item during keyboard navigation. Unusable items MUST be strictly inert to pointer clicks and activation inputs, with zero activation side-effects or toast interruptions.
- **`[INV-TUI-INERT-02] Gestalt Spacing Firewall & Text-Join Hygiene`**:
  Row layout builders (`ListRow`) MUST enforce an unconditional minimum separation (`GROUP_GAP`) between fixed/columnar atoms and trailing groups. Trailing status reasons MUST strictly follow the canonical em-dash grammar (`locked — {reason}`).

---

## Consequences

- **Positive**:
  - **Flawless Ergonomics**: Users navigate rapid-fire with arrow keys without dead stops or ghost cursors.
  - **Zero Text Smashing**: Structural spacing guarantees labels and tags never collide.
  - **High Signal-to-Noise**: Disabled models remain visible in catalogs (so users know what exists and what requires upgrades) while remaining completely out of the way of operational selection.
  - **Codebase Unification**: Retires fragmented ad-hoc toast handlers and custom loops in favor of a universal primitive.
- **Negative & Trade-offs**:
  - Direct tests that manually assigned `modal_index` to an unusable row without exercising navigation must account for the subdued presentation tokens.

---

## Rejected Alternatives & Negative Knowledge

### 1. Toast-on-Activation (The Legacy Anti-Pattern)
- *Failure Mode*: Allowing the arrow cursor to highlight or land on a locked row and waiting for the user to press <kbd>Enter</kbd> before displaying `"This model is unavailable: Requires subscription"` wastes cognitive attention and finger travel. In terminal environments, non-actionable rows should not present as active navigation targets.

### 2. Dropping Locked Models from the Catalog Entirely
- *Failure Mode*: Removing unavailable models from the `/models` list obscures catalog parity. Users with free tiers or pending entitlements could not verify whether a model was supported by the provider integration or what capabilities an account upgrade would unlock. Retaining the row as a dimmed, skipped entry provides complete epistemic visibility without interaction friction.

### 3. Ad-Hoc Per-Dialog Skip Loops
- *Failure Mode*: Implementing inline `while !item.usable` loops in individual dialog input handlers led directly to bugs where some dialogs checked usability on arrow keys, others only on Enter, and others only on mouse click. A single generic primitive (`rotate_selectable`) is mandatory.

---

## Links

- Related ADRs: [ADR-0035](0035-domain-scoped-surface-architecture-and-encapsulated-dialog-lifecycle.md), [ADR-0040](0040-cross-domain-action-matrix-and-polymorphic-tui-spatial-topology.md), [ADR-0047](0047-autonomous-surface-interaction-architecture-and-spatial-event-delegation.md)
- Related Modules: `nuo-tui::components::row`, `nuo-tui::surfaces::dialogs`, `nuo-tui::overlays::provider::models`
- Documentation: [`docs/reference/tui/visual-language.md`](../reference/tui/visual-language.md), [`docs/reference/tui/modals.md`](../reference/tui/modals.md)
