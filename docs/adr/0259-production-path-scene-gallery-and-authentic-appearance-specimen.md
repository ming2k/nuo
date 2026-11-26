---
id: ADR-0259
title: "Production-Path Scene Gallery and Authentic Appearance Specimen"
status: accepted
date: 2026-11-26
scope: tui/views, tui/devtools, presentation/ui-primitives
superseded_by: null
amends:
  - "[ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md) (tightens Appearance specimen fidelity so previews paint on the candidate theme's own surface and exercise all live semantic channels)"
negative_knowledge: true
---

# 0259. Production-Path Scene Gallery and Authentic Appearance Specimen

- **Status**: Accepted
- **Date**: 2026-11-26
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)
- **Related**: [ADR-0185](0185-storybook-retirement-and-single-presentation-path.md), [ADR-0258](0258-palette-native-in-flight-hue-chroma-envelope-and-zen-typography.md)

---

## Context and Problem Statement

[ADR-0185](0185-storybook-retirement-and-single-presentation-path.md) retired the old declarative Storybook (`shell/declarative/`, `StorybookComponent`, `nuo dev storybook`) because it maintained a 180 KB parallel component suite (`ToolCard`, `NoticeCard`, etc.) with its own duplicate `ToolStatus` type that drifted from production rendering and compiled into shipped builds.

That retirement solved dual-track drift, yet left a real visual-verification gap exposed during the [ADR-0258](0258-palette-native-in-flight-hue-chroma-envelope-and-zen-typography.md) color overhaul:

1. **Mechanical Tests Cannot Evaluate Optical Glare or Whole-Screen Balance**: Unit distance checks and `insta` text snapshots cannot reveal whether a hue glares against a dark terminal, whether a breathing dot reads calmly on `TabBar`, or how a theme holds together across transcript, diff, dashboard, and overlay scenes.
2. **Appearance Specimen Surface Leakage**: In `/settings` → Appearance, `draw_live_specimen_gallery` rendered `candidate` foreground colors directly onto `active_theme.pane_sunken()`. Previewing the light `Paper` theme while `Zen` was active painted dark-ink text onto an ink-black surface, making cross-polarity preview unreadable. It also omitted key semantic surfaces (text ladder, tab running/attention states, full tool lifecycle matrix, diff row backgrounds, and focused running controls).

We need authentic multi-scene visual inspection **without** reintroducing a second component framework, duplicate status types, release-binary bloat, or extra dependencies.

---

## Architectural Decisions

### 1. Authentic Candidate-Surface Specimen in `/settings` → Appearance

`draw_live_specimen_gallery` (`crates/view/src/views/settings/appearance.rs`) is upgraded so that:

- Its outer border separates cleanly from the settings pane using `active_theme`, while its entire inner preview canvas paints onto **`candidate.surface()`** (falling back to `Color::Reset` on ANSI-16 / Monochrome). Previewing a light theme from a dark theme — or vice versa — now renders true-to-life surface contrast.
- It surfaces the full `ColorPaletteSpecimen` (`Surfaces`, `Text`, `Signals` including `Running`) plus compact, faithful previews of:
  - Tab strip liveness (`Running` breathing dot + `Attention` `!` + active/idle pills on `candidate.tabbar_bg()`),
  - Turn header (`› turn 2`),
  - Tool lifecycle ladder (`Executing`, `Ok`, `Denied`, `Failed`, `Cancelled`),
  - Syntax diff lines painted on actual `candidate.diff_add_bg()` / `candidate.diff_del_bg()`,
  - Focused `ActionStatus::Running` control pill and Toast bubble.

### 2. Zero-Drift Production-Path Scene Gallery (`examples/scene_gallery.rs`)

We introduce a standalone Cargo example target, `crates/nuo/examples/scene_gallery.rs` (`required-features = ["interactive"]`), which:

- **Adds zero bytes to shipped `nuo` binaries**, zero `cli.rs` subcommands, and zero new crate dependencies (honoring `[INV-TUI-SINGLE-RENDER-02]` strictly).
- **Introduces zero parallel component abstractions or duplicate status enums** (honoring `[INV-TUI-SINGLE-RENDER-01]` strictly). Every scene constructs genuine domain/view fixtures (`TranscriptMessage`, `TabBarProps`, `ThreadContext`, `ModelBarProps`, `ComposerProps`, `MonitoredThread`, `DiffViewerState`, `SettingsProps`, `PermissionRequest`, `UserQuestionRequest`) and invokes the exact production renderers (`draw_transcript`, `draw_model_bar`, `draw_composer_with_options`, `draw_dashboard`, `draw_settings_view`, `draw_diff_scene`, `draw_permission_sheet`, `AskUser::render_with_tree`, `draw_notice_toast`).
- **Covers eight canonical, unoccluded production scenes**:
  1. `Transcript · Running` (3-tier head band with breathing running tab + attention tab, executing/ingesting/ok/failed/denied/cancelled tool steps, breathing `ActivityStrip`, `ModelBar`, focused `Composer`),
  2. `Transcript · Focus & Browse` (`FocusPlane::Transcript` pane wash + gutter marker, hovered collapsed step, `QueueBar`, `TasksBar`, inactive composer),
  3. `Diff Scene` (fullscreen `draw_diff_scene` multi-file patch inspection),
  4. `Dashboard · Fleet` (fullscreen `draw_dashboard` with `Running`, `NeedsApproval`, `Failed`, `Idle` threads and lineage fork),
  5. `Settings · Appearance` (fullscreen `draw_settings_view` showing the dual-pane scheme list and upgraded specimen),
  6. `Overlay · Permission Sheet` (live transcript + unoccluded `draw_permission_sheet` + toast),
  7. `Overlay · AskUser` (recessed transcript behind `AskUser::render_with_tree`),
  8. `Overlay · Threads Picker [RUNNING]` (`draw_sessions_modal` with live `[⠋ RUNNING]` badges and lineage tree).
- **Operates in two modes**:
  - **Interactive TTY workbench** (`cargo run -p nuo --example scene_gallery`): 60ms live breathing animation tick, instant theme cycling (`Zen` / `Midnight` / `Nord` / `Catppuccin` / `Paper` / `ANSI 16` / `Monochrome`), scene switching (`1`–`8`, `←`/`→`), viewport simulation (`Full`, `120×34`, `80×24`, `60×20`), and animation pause (`Space`).
  - **Headless ANSI-truecolor dump** (`--dump [--theme <id|all>] [--scene <1..8|all>] [--width N] [--height N] [--plain]`): renders frames via `graticule::TestTerminal` and writes 24-bit ANSI output (or plain text) to stdout for rapid terminal inspection or CI smoke verification.

### 3. Invariants

- **`[INV-TUI-PREVIEW-01]` Candidate-Surface Specimen Fidelity**: The Appearance settings specimen MUST paint its inner preview surface using `candidate.surface()`, never leaking `active_theme`'s background into another theme's preview.
- **`[INV-TUI-PREVIEW-02]` Zero-Drift Production Scene Gallery**: Any offline visual scene gallery MUST live outside the shipped binary (Cargo example target), MUST NOT add CLI subcommands or external watcher dependencies, and MUST render exclusively by invoking production shell/view renderer entry points over domain fixtures rather than parallel mock components.

---

## Consequences & Trade-offs

- **Positive**: Designers and engineers can inspect real production frames across all 7 built-in themes, 6 canonical scenes, and 4 viewport geometries in seconds — interactively with live 60ms animations or via `--dump` truecolor output — with zero possibility of production/preview drift and zero release-binary footprint.
- **Negative & Incurred Costs**: Refactoring a production renderer's props signature requires updating the corresponding fixture in `examples/scene_gallery.rs`, which is enforced immediately at compile time (`cargo check --examples`) rather than drifting silently.

---

## Discarded Alternatives (Negative Knowledge)

### Alternative A — Restore `shell/declarative/` Under `feature = "dev-storybook"`
- **Why considered**: Reuses the historical component registry model from ADR-0073 / ADR-0146.
- **Why rejected**: Rejected for the same reason ADR-0185 retired it — a separate component suite inevitably drifts from the real transcript, disclosure, diff, and overlay renderers, so inspecting it validates the wrong UI.

### Alternative B — Add a `nuo dev preview` CLI Subcommand
- **Why considered**: Convenient single-binary invocation without Cargo.
- **Why rejected**: Violates `[INV-TUI-SINGLE-RENDER-02]` and bloats the shipped `nuo` binary and CLI surface with developer-only fixture data. A Cargo example (`cargo run -p nuo --example scene_gallery`) keeps the release artifact completely clean.

---

## Links

- [ADR-0147: Semantic Color Governance Firewall and First-Class Palette Specimen Topology](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)
- [ADR-0185: The Offline Storybook Is Retired; the Production Renderer Is the Sole Presentation Path](0185-storybook-retirement-and-single-presentation-path.md)
- [ADR-0258: Palette-Native In-Flight Hue, Chroma Envelope, and Zen Luminance Discipline](0258-palette-native-in-flight-hue-chroma-envelope-and-zen-typography.md)
