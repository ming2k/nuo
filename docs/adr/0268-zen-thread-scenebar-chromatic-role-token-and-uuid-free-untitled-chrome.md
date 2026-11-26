---
id: ADR-0268
title: "Zen Thread SceneBar: Chromatic Bare Role Token and UUID-Free Untitled Chrome"
status: accepted
date: 2026-11-30
scope: presentation/view, host/nuo, docs/reference
superseded_by: null
amends:
  - "[ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) (retires `thread#<id_tail>` from untitled TabBar pills in favor of human-readable `new thread` / `new thread N`)"
  - "[ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md) (revises `[INV-TAB-09]` so untitled tab projections never embed random UUID tails)"
  - "[ADR-0261](0261-unified-two-tier-header-and-single-row-thread-scenebar.md) (revises `[INV-HEAD-09]` and `[INV-HEAD-10]`: bare uppercase role token on chromatic surfaces with framed bracket fallback, silent untitled Thread SceneBar, and removal of dead `thread_id` / `id_tail` chrome plumbing)"
negative_knowledge: true
---

# 0268. Zen Thread SceneBar: Chromatic Bare Role Token and UUID-Free Untitled Chrome

- **Status**: Accepted
- **Date**: 2026-11-30
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md), [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), [ADR-0261](0261-unified-two-tier-header-and-single-row-thread-scenebar.md)
- **Related**: [ADR-0053](0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md), [ADR-0235](0235-tabbar-carries-its-own-lifecycle-hover-reveal-visible-close-and-reserved-add.md), [ADR-0258](0258-palette-native-in-flight-hue-chroma-envelope-and-zen-typography.md)

---

## Context and Problem Statement

Real-world use of [ADR-0261](0261-unified-two-tier-header-and-single-row-thread-scenebar.md)'s single-row Thread `SceneBar` surfaced two residual noise artifacts in the top chrome:

1. **Redundant ASCII Brackets on Chromatic Role Tokens (`[DEVELOPER]`)**:
   On chromatic (`DirectColor` / `Indexed256`) terminals, Tier 2 `SceneBar` already differentiates every left-hand slot through orthogonal typographic channels: role (`brand` + `BOLD` + `UPPERCASE`), safety posture (`warn` + `BOLD` + `lowercase`), and task title (`heading` + `BOLD` + sentence case). Meanwhile, non-thread `SceneBar` leads (`dashboard`, `settings`, `log`, `diff`) already render bare without brackets, and `[...]` is simultaneously overloaded for count/fault badges (`[2 queued]`, `[!3]`) and framed-surface chrome. Spending 2 columns on `[` and `]` on chromatic surfaces added visual clutter without adding information, whereas non-chromatic framed surfaces (`Ansi16` / `Monochrome`) still genuinely need brackets for structural demarcation.
2. **Meaningless Random UUID Tails on Untitled Threads (`thread 7db7` / `thread#7db7`)**:
   On a fresh empty conversation, Tier 1 `TabBar` painted `thread#7db7` and Tier 2 `SceneBar` immediately beneath it painted `[DEVELOPER]  thread 7db7`. A random 4-hex UUID suffix carries no temporal order, no semantic meaning, and almost zero operator utility on an empty ephemeral thread (which is recycled in place by `/new` or tab replacement). Furthermore, ADR-0261's original motivation for keeping `thread <id_tail>` — preventing left-edge layout shift when an authentic title arrives — does not actually require a placeholder: governance tokens are already pinned to the left edge and the workspace path is already pinned to the right edge, so leaving the title slot silent until a title exists is equally zero-displacement while handing 100% of remaining horizontal space to the workspace path.

---

## Architectural Decisions

### 1. Capability-Adaptive Role Token (`[INV-HEAD-11]`)

Following [ADR-0053](0053-capability-first-orthogonal-terminal-composition-and-autonomous-archetype-projection.md)'s capability-first discipline (mirroring `TabBar`'s chromatic padding vs. framed brackets):

- **Chromatic surfaces (`theme.elevation.is_chromatic()`)**:
  Render the staffed role as a bare uppercase bold brand token: `DEVELOPER` (eliding as `SECURITY-AUDI…` down to a 4-column legible floor `DEV…`, dropped below floor).
- **Non-chromatic / framed surfaces (`Hybrid` / `Structured`)**:
  Retain intact structural brackets: `[DEVELOPER]` (eliding as `[SECURITY-AU…]` down to a 5-column legible floor `[DEV…]`, dropped below floor).
- **Single Source of Truth**:
  `ViewHints::role` carries the staffed role as a typed slot into `draw_scene_bar` so both `SceneKind::Thread` and `SceneKind::Subagent` share the exact same `truncate_role_token` renderer (`brand + BOLD` bare uppercase on chromatic surfaces, intact brackets on framed surfaces) without string-concatenating role text into plain-foreground context strings.

### 2. Silent Untitled Thread `SceneBar` & Human-Readable Untitled Tabs (`[INV-HEAD-12]`)

- **Tier 2 `SceneBar` (`SceneKind::Thread`)**:
  When a thread has no authentic title, the title slot is **silent** (zero columns):
  ```text
  ┌────────────────────────────────────────────────────────────────────────┐
  │ new thread   dashboard                                             ? h │ ◄ Tier 1: TabBar
  ├────────────────────────────────────────────────────────────────────────┤
  │  DEVELOPER                                              ~/projects/nuo │ ◄ Tier 2: SceneBar
  └────────────────────────────────────────────────────────────────────────┘
  ```
  When an authentic title arrives, it paints in bold heading immediately after the governance cluster (`DEVELOPER  Fix retry loop` or `DEVELOPER  auto  open  Fix retry loop`) with zero displacement of the role, posture tokens, or right-anchored workspace path.
- **Tier 1 `TabBar` & Command Palette / `[▾]` Tab Overview**:
  Untitled thread tabs project `new thread` when only one untitled thread tab is open, and `new thread`, `new thread 2`, `new thread 3`, … (in left-to-right tab order) when multiple untitled thread tabs coexist simultaneously. `PaletteContext::capture` consumes the exact same `project_tab_items` projection so the TabBar and the `[▾]` Tab Overview / Command Palette can never drift or leak `Thread <short_id>` strings. Opening a second empty tab never reflows the first tab's pill width, and once a tab receives an authentic title it swaps in place.
- **Operator Surfaces Retain IDs**:
  Thread IDs remain accessible on management and diagnostic surfaces (`Dashboard`, `/threads` picker details, `nuo thread ls`, session trace/inspectors) where explicit ID addressing is actually useful.
- **Clean Break**:
  Delete the now-unused `thread_id` parameter from `draw_scene_bar`, `draw_thread_scene_bar`, `TranscriptProps`, `LogSceneProps`, and `DiffSceneProps`, and delete `view::view_header::thread_id_tail` / `thread_bar::id_tail`.

### 3. Invariants

- **`[INV-HEAD-11]` Chromatic Bare Role Token and Framed Bracket Fallback**: On chromatic surfaces (`theme.elevation.is_chromatic()`), Tier 2 `SceneBar` (`Thread` and `Subagent`) MUST render the staffed role as a bare uppercase bold brand token (`DEVELOPER`, 4-column truncation floor `DEV…`) without brackets; on non-chromatic framed surfaces, it MUST fall back to intact structural brackets (`[DEVELOPER]`, 5-column truncation floor `[DEV…]`).
- **`[INV-HEAD-12]` UUID-Free Untitled Primary Chrome**: Untitled threads MUST keep the Tier 2 Thread `SceneBar` title slot silent (0 columns, giving all remaining slack to the right-anchored workspace path with zero displacement on title arrival). Tier 1 `TabBar` MUST project untitled thread tabs as `new thread` (and `new thread 2`, `new thread 3`, … only for additional concurrent untitled thread tabs), never raw or tail-sliced UUID hex fragments.
- Revises `[INV-HEAD-09]` and `[INV-HEAD-10]` ([ADR-0261](0261-unified-two-tier-header-and-single-row-thread-scenebar.md)) and `[INV-TAB-09]` ([ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md)).

---

## Consequences & Trade-offs

- **Positive**:
  - Eliminates two layers of low-signal visual noise (`[...]` brackets on chromatic surfaces and duplicate random hex suffixes across Tier 1 and Tier 2) while preserving framed/monochrome clarity.
  - Reclaims 2 horizontal columns on every chromatic thread header and 11+ columns on untitled narrow rows, allowing deeper workspace paths (`.../crates/nuo-tui` instead of `nuo-tui`) and longer titles (`Refactor database migra…`) to survive intact.
  - Removes dead `thread_id` plumbing across `draw_scene_bar`, `TranscriptProps`, `LogSceneProps`, `DiffSceneProps`, and `view::view_header::thread_id_tail`.
- **Negative & Incurred Costs**:
  - Operators who previously glanced at the header for an untitled thread's 4-hex suffix must use `Dashboard`, `/threads`, or `nuo thread ls` to read raw IDs (which already display full/short IDs alongside metadata).

---

## Negative Knowledge & Rejected Alternatives

### Rejected Alternative A: Dropping Brackets Unconditionally on All Terminal Profiles
Removing `[...]` even on `Monochrome` / `Ansi16` terminals would leave `DEVELOPER` visually indistinguishable from an all-caps title prefix (`DEVELOPER  HTTP/3 retry`) when no chromatic hue separates `theme.brand()` from `theme.heading()`. Capability-adaptive fallback (`is_chromatic()`) matches `TabBar`'s established `ADR-0053` pattern cleanly.

### Rejected Alternative B: Painting a Chromatic Pill Background Behind `DEVELOPER`
Adding a third micro-elevation pill background onto Tier 2 `SceneBar` would complicate the two-rung head-band elevation ladder (`tabbar_bg` → `scenebar_bg`) established by [ADR-0261](0261-unified-two-tier-header-and-single-row-thread-scenebar.md) and draw disproportionate visual weight to a static role label compared to the active task title. Bare `brand + BOLD + UPPERCASE` matches non-thread SceneBar leads (`dashboard`, `settings`, `log`, `diff`) without adding a new background color.

### Rejected Alternative C: Numbering the First Untitled Tab (`new thread 1`)
Always suffixing `1` spends 2 columns on the overwhelmingly common single-empty-tab case and implies a sibling tab exists. Using `new thread` for ordinal 0 and `new thread 2`, `new thread 3`, … only when multiple untitled tabs coexist keeps the default state minimal and avoids reflowing Tab 1 when Tab 2 opens.
