---
id: ADR-0147
title: "Semantic Color Governance Firewall and First-Class Palette Specimen Topology"
status: accepted
date: 2026-03-29
scope: presentation/nuo-ui-primitives, presentation/nuo-view, shell/nuo, governance/nuo-governance, settings/appearance
superseded_by: null
negative_knowledge: true
---

# 0147. Semantic Color Governance Firewall and First-Class Palette Specimen Topology

- Status: Accepted
- Date: 2026-03-29
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, TUI Runtime, Design Systems, Quality Engineering
- Complements: [ADR-0053](0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md), [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md), [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md), [ADR-0146](0146-live-storybook-engine-sub-second-hot-loop-and-broad-catalog-expansion.md)

---

## Context and Problem Statement

Nuo's presentation architecture enforces strict single-direction dependency flow: presentation components consume design tokens purely through `&Theme` ([ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md)), respecting terminal capability archetypes such as `Chromatic`, `Hybrid` (ANSI-16), and `Structured` (Monochrome VT100, [ADR-0053](0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md)).

However, architectural review revealed two critical vulnerabilities in the color pipeline:

1. **The Dirty Write Vulnerability (Lack of Enforcement Mechanics)**:
   While the presentation layer historically maintained high discipline, nothing in the compiler or CI suite prevented developers from accidentally introducing hardcoded color literals (e.g. `Color::Rgb(r, g, b)`, `Color::White`, or `Color::Black`) directly into component renderers. In light terminal themes (such as `Paper`) or monochrome serial lines, hardcoded colors produce catastrophic illegibility (e.g. white text on light cream backgrounds). The repository lacked a mechanical firewall to reject non-semantic color writes at build and CI time.

2. **The Theme Blind-Box Dilemma (End-User Experience Gap)**:
   Color swatches, design token ladders, and live component previews were treated as developer-only concerns confined to offline test sandboxes or Storybook. In the user-facing Appearance settings panel (`/settings` -> Appearance), users were forced to choose themes based solely on scheme names and 6 tiny text squares (`██████`). Users had no way to preview syntax diffs, in-flight running indicators, or turn headers without blindly committing a theme switch and navigating back and forth across views.

3. **Absence of a Formal Swatch Specimen Model**:
   Theme data was expressed either as raw TOML schema (`color_scheme.rs`) or discrete runtime token methods (`theme.rs`), with no unified, iterable design token specimen model capable of powering both user-facing settings previewers and developer harnesses without code drift.

---

## Decision Drivers

- **Zero Tolerance for Dirty Color Writes**: Mechanical enforcement must guarantee that no presentation component in `nuo-view`, `nuo::shell`, or `nuo-ui-primitives::components` contains hardcoded color literals.
- **First-Class End-User Preview**: Style previewing and color swatches are primary product features for all users, not developer-only toys. The settings panel must provide zero-latency, what-you-see-is-what-you-get (WYSIWYG) specimen previews upon cursor hover.
- **Canonical Presenter Reuse**: Specimen previews in the user-facing settings panel must directly invoke canonical presenters (`TurnBand`, `ToolCard`, `Toast`) via candidate themes, with zero mock or duplicated UI code.
- **Automated CI Firewall**: Governance tests must run on every CI pass to permanently prevent color drift.

---

## Architectural Decisions

### 1. Semantic Color Governance Firewall (`[INV-TUI-COLOR-01]`)

We introduce an automated color purity scanner in `nuo-governance` (`check_color_purity`) that enforces complete absence of raw color literals in all presentation source trees (`crates/nuo/src/shell`, `crates/nuo-view/src`, `crates/nuo-ui-primitives/src/components`):

- **Forbidden Literals**: `Color::Rgb(...)`, `Color::White`, `Color::Black`, `Color::Red`, `Color::Green`, `Color::Yellow`, `Color::Blue`, `Color::Magenta`, `Color::Cyan`, `Color::Gray`, `Color::DarkGray`, and all `Color::Light*` tokens.
- **Permitted Constructs**:
  1. `Color::Reset` (standard terminal attribute clear).
  2. Color destructuring patterns (e.g. `Color::Rgb(r, g, b) => ...` in mathematical lerp/luminance routines).
  3. Palettes defined authoritatively inside `nuo-ui-primitives/src/theme.rs` and `color_scheme.rs`.
  4. Explicit test modules (`#[cfg(test)]`).

Any raw color write in production presentation code triggers immediate CI failure.

### 2. First-Class Color Palette Specimen Model (`[INV-TUI-COLOR-02]`)

We formalize `ColorPaletteSpecimen` and `SwatchRole` in `nuo-ui-primitives::theme`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct SwatchRole {
    pub name: &'static str,
    pub description: &'static str,
    pub color: Color,
    pub contrast_ratio: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorPaletteSpecimen {
    pub theme_id: String,
    pub is_dark: bool,
    pub surfaces: Vec<SwatchRole>,
    pub text_ladder: Vec<SwatchRole>,
    pub signals: Vec<SwatchRole>,
    pub components: Vec<SwatchRole>,
}
```

Every theme generates its canonical specimen via `Theme::palette_specimen(id)`, establishing an iterable, contrast-evaluated projection of all design roles (Surface staircase, Text ladder, Signals, and Component accents).

### 3. User-Facing Live Specimen & Palette Gallery in Settings

In `crates/nuo/src/shell/views/settings/appearance.rs`, when terminal width is $\ge 72$ columns, the panel transitions to a dual-pane live preview layout:
- **Left Column**: Selectable theme preset list.
- **Right Column (Live Specimen Canvas)**: Dynamically renders the theme currently under cursor (`props.detail_index`) without requiring user commit:
  1. **Palette Token Grid**: Visual swatches for Surface, Panel, Raised, Brand, Info, Success, Warning, Error, and Running.
  2. **Micro UI Specimen**: Live ReAct turn header (`› turn 2`), syntax diff (`+` and `-` lines), running tool indicator (`▎⠋ edit_text · running`), and toast bubble (`✓ Prompt copied to clipboard`).

Users receive instantaneous visual feedback on contrast, readability, and liveness hues before applying any configuration.

---

## Normative Invariants Established

- **`[INV-TUI-COLOR-01] Semantic Color Purity Firewall`**:
  Presentation code across `crates/nuo/src/shell`, `crates/nuo-view`, and `crates/nuo-ui-primitives/src/components` MUST NOT hardcode `Color::*` literals (excluding `Color::Reset`). All styling MUST resolve exclusively through `Theme` semantic methods (`theme.text()`, `theme.brand()`, `theme.running()`, `theme.surface()`), mechanically verified by `nuo-governance`.
- **`[INV-TUI-COLOR-02] First-Class Color Palette Specimen`**:
  The presentation substrate MUST expose an authoritative `ColorPaletteSpecimen` model representing the complete design token role catalog. User-facing settings previews and developer Storybooks MUST share this model to eliminate visual discrepancy.

---

## Consequences

- **Positive**:
  - Eliminates the class of bugs where light themes render invisible white text due to stray hardcoded colors.
  - End users gain immediate, high-fidelity visual preview of themes and custom color schemes.
  - 100% code reuse between production settings preview and developer Storybook fixtures.
  - Mechanical CI gate ensures permanent immunity to color drift.
- **Negative & Trade-offs**:
  - Small terminals (< 72 columns) suppress the live specimen gallery, falling back cleanly to the compact theme list.

---

## Rejected Alternatives & Negative Knowledge

### 1. Hardcoding Theme Demos with Mock HTML/Text
- *Failure Mode*: Maintaining handwritten mock string templates for theme previews rapidly diverges from production components as layouts, turn headers, or badge glyphs evolve.
- *Rejection*: Reusing the canonical presenters directly guarantees permanent parity.

### 2. Storybook-Only Theme Inspection
- *Failure Mode*: Confining rich previews to dev tools creates an elitist toolchain where developers have full visual inspection but paying users must navigate blind.

---

## Links

- Complements: [ADR-0053](0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md), [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md), [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md), [ADR-0146](0146-live-storybook-engine-sub-second-hot-loop-and-broad-catalog-expansion.md)
