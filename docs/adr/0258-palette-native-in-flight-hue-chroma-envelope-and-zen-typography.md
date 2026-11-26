---
id: ADR-0258
title: "Palette-Native In-Flight Hue, Chroma Envelope, and Zen Luminance Discipline"
status: accepted
date: 2026-11-26
scope: presentation/ui-primitives, tui/theme, tui/chrome, tui/overlays, docs/reference/tui
superseded_by: null
amends:
  - "[ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) (§2 `[INV-TUI-RUNNING-01]` tone allocation: replaces the hardcoded neon cyan `RUNNING_HUE` with palette-native, chroma-bounded, contrast-verified `running` tokens)"
  - "[ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md) (upgrades `ColorPaletteSpecimen` contrast evaluation to authentic WCAG 2.1 relative luminance and exposes an optional `running` semantic override in `ColorSchemeConfig`)"
negative_knowledge: true
---

# 0258. Palette-Native In-Flight Hue, Chroma Envelope, and Zen Luminance Discipline

- **Status**: Accepted
- **Date**: 2026-11-26
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) §2, [ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)

---

## Context and Problem Statement

The 2026-11-02 amendment to [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md) correctly separated active execution liveness (`theme.running()`) from structural anchors (`theme.info()`) and outcomes (`ok` / `warn` / `err`). However, the implementation introduced a single hardcoded global constant, `RUNNING_HUE = Color::Rgb(56, 222, 235)` (`#38DEEB`, ~82% HSL saturation), applied verbatim to `Zen` and linearly mixed at 80–90% weight into every other chromatic scheme.

In daily use, this global neon constant created four compounding defects:

1. **Contradiction of Zen's Luminance-Led Design Language**: `Zen` is a near-black, stepped-charcoal palette with low-chroma sage, slate, moss, ochre, and terracotta tokens whose HSL saturations sit within 9%–40%. Injecting an 82%-saturation neon cyan (`#38DEEB`) — paired with `Modifier::BOLD` and full-pill backgrounds on focused running actions — created harsh optical glare that clashed with the rest of the interface.
2. **Broken Light-Mode Legibility (`Paper`)**: Mixing `#38DEEB` with 20% dark ink produced `Rgb(53, 187, 198)` on `#F7F6F2`, yielding a WCAG 2.1 contrast ratio of **2.14:1** (far below the 4.5:1 AA readability floor).
3. **Palette Foreignness Across Built-In and Custom Themes**: `Nord`, `Catppuccin`, `Midnight`, and user-authored theme files all received the same foreign electric cyan instead of a tone belonging to their own color grammar, and `ColorSchemeConfig` offered no `running` field to override it.
4. **Residual Running-Tone Drift in Overlays**: Two production surfaces still bypassed `theme.running()`: `dashboard::status_chip` painted `ThreadActivity::Running` in `theme.brand()`, and the thread picker (`overlays/session.rs`) painted `[RUNNING]` in `theme.warning`.

Additionally, `Theme::palette_specimen` computed `SwatchRole::contrast_ratio` using Rec.601 luma (`[0..255]`) instead of WCAG 2.1 sRGB relative luminance, masking the `Paper` readability failure.

---

## Architectural Decisions

### 1. Retire the Global Neon Constant (`RUNNING_HUE`) — Clean Break

`RUNNING_HUE` is deleted from `ui-primitives`. `SemanticPalette` gains a first-class `running: Option<Color>` slot, and `ColorSchemeConfig` gains an optional `running: Option<String>` field (`#RRGGBB`, `#[serde(default, skip_serializing_if = "Option::is_none")]`) so custom schemes and external `ThemeFile`s can declare an explicit in-flight tone.

### 2. Palette-Native Curated In-Flight Tokens

Each built-in chromatic theme defines a curated `running_fg` that belongs to its own pigment family, stays within its chroma envelope, exceeds WCAG 2.1 AA contrast (`>= 4.5:1`) against `app_bg`, and preserves `color_distance >= 90.0` from every structural and outcome token (`info`, `brand`, `ok`, `warn`, `err`, `muted`):

| Theme | `running_fg` | Hex | Role / Family | HSL Sat | WCAG vs `app_bg` |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Zen** | `Rgb(154, 159, 209)` | `#9A9FD1` | Muted slate-indigo | 37.4% | 7.88:1 (AAA) |
| **Midnight** | `Rgb(174, 148, 228)` | `#AE94E4` | Soft twilight violet | 59.7% | 7.70:1 (AAA) |
| **Nord** | `Rgb(214, 178, 208)` | `#D6B2D0` | Lifted Nord15 Aurora | 30.5% | 6.62:1 (AA+ on surface/panel/raised) |
| **Catppuccin** | `Rgb(116, 199, 236)` | `#74C7EC` | Mocha Sapphire | 75.9% | 8.69:1 (AAA) |
| **Paper** | `Rgb(118, 75, 158)` | `#764B9E` | Deep ink-indigo | 35.6% | 5.94:1 (AA+) |

`ANSI 16` retains `Color::LightMagenta` (named slot, never RGB); `Monochrome` retains `Color::Reset`.

### 3. Algorithmic Derivation for Custom Palettes

When a custom `ColorSchemeConfig` or `ThemeFile` omits `running`, `derive_running_color` derives a palette-native in-flight tone from the palette's own semantic tokens:

1. Measures the palette's chroma envelope from `{accent, success, warning, error}` and clamps derived saturation to a comfortable non-glaring band (`[0.24, 0.58]`).
2. Selects polarity-aware lightness (`~0.70` on dark canvases, `~0.42` on light canvases) and iteratively adjusts lightness until WCAG 2.1 contrast against `background` reaches at least `4.5:1`.
3. Searches candidate hues in the cool/active wheel sector (`185°..305°`, avoiding warning amber and error red) and picks the candidate that maximizes minimum perceptual (`redmean`) distance from `{info, accent, success, warning, error, muted}`.

### 4. Authentic WCAG 2.1 Contrast Math in `ColorPaletteSpecimen`

`color_math` exposes `wcag_contrast_ratio(a, b)` using standard IEC 61966-2-1 sRGB linearization (`(L1 + 0.05) / (L2 + 0.05)`). `Theme::palette_specimen` uses this exact metric for `SwatchRole::contrast_ratio`.

### 5. Complete Elimination of Running-Tone Drift and Zen Copy Refinement

- `dashboard::status_chip(ThreadActivity::Running, theme)` resolves through `theme.running()` (matching `dashboard::preview::status_style`).
- `overlays/session.rs` paints `[<spin> RUNNING]` in `theme.running()` + `Modifier::BOLD` (separating it from `tree_badge` in `theme.brand()`), eliminating the last `theme.warning` running-state violation, and aligns loading/refreshing spinners (`session.rs`, `provider/models.rs`), `McpRow::Connecting` (`overlays/common.rs`), and `RequestUsageStatus::InFlight` (`overlays/telemetry/model.rs`, `overlays/usage_stats.rs`) onto `theme.running()`.
- `Zen`'s preset description is refined to `"Low-chroma charcoal with muted sage"` to state its luminance-led, low-chroma discipline accurately.

### 6. Invariants

- **`[INV-TUI-COLOR-03]` Palette-Native In-Flight Tone & Chroma Envelope**: Every chromatic theme's `theme.running()` MUST belong to its palette's chroma discipline (no global hardcoded neon constant; built-in and derived tones stay within the palette's saturation envelope and never exceed 0.78 HSL saturation), MUST achieve a WCAG 2.1 contrast ratio of at least `4.5:1` against `app_bg`, `panel()`, and `raised()` on every built-in chromatic theme, and MUST remain perceptually distinct (`color_distance >= 90.0`) from `info`, `brand`, `ok`, `warn`, `err`, and `muted`.
- **`[INV-TUI-COLOR-04]` Zero Running-Tone Drift**: Every active in-flight indicator across all TUI surfaces (transcript tool steps, subagent peeks, `ActionStatus::Running`, `ActivityStrip`, `TabBar`, `Dashboard` fleet chip and dossier, `SessionsPicker` running badge and loading spinners, `RequestUsageStatus::InFlight` telemetry/usage rows, `McpRow::Connecting`, `PreAttach`, and Appearance Specimen) MUST resolve its hue exclusively through `theme.running()`.

---

## Consequences & Trade-offs

- **Positive**: `Zen` regains its calm, low-chroma mineral aesthetic while keeping liveness immediately distinguishable from `› turn N` headers and outcomes; `Paper` jumps from an illegible 2.14:1 contrast to 5.94:1; `Nord` and `Catppuccin` use authentic tones from their own design systems; custom themes inherit automatic contrast-safe derivation or explicit `running` overrides.
- **Negative & Incurred Costs**: Snapshot or unit assertions checking the old `#38DEEB` literal or the old `"Quiet charcoal"` preset subtitle are updated in lockstep.

---

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Keep `RUNNING_HUE` as Cyan and Simply Desaturate It Globally
- **Why considered**: Minimal diff (changing a single constant in `presets.rs`).
- **Why rejected**: In `Zen`, `info` is already muted slate-teal (`#80999C`, 186°); desaturating cyan moves `running` straight back onto `info`, recreating the exact structural collision ADR-0110 banned. A single global constant also fails on `Paper` (light polarity) and clashes with `Nord` and `Catppuccin`.

### Alternative B — Reuse `theme.brand()` or `theme.warning` for Running States
- **Why considered**: Avoids allocating a distinct semantic hue.
- **Why rejected**: `brand` owns selection/identity and `warning` owns caution/denial outcomes. Reusing either violates `[INV-TUI-RUNNING-01]` and conflates live progress with identity or pre-announced failure.

---

## Links

- [ADR-0110: Running-State Interaction Topology, Action Permissibility Discipline, and Visual Demarcation Purity](0110-running-state-interaction-topology-and-action-permissibility-discipline.md)
- [ADR-0147: Semantic Color Governance Firewall and First-Class Palette Specimen Topology](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)
- [ADR-0259: Production-Path Scene Gallery and Authentic Appearance Specimen](0259-production-path-scene-gallery-and-authentic-appearance-specimen.md)
