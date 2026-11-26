---
id: ADR-0261
title: "Unified Two-Tier Header Topology and Single-Row Thread SceneBar"
status: accepted
date: 2026-11-28
scope: presentation/ui-primitives, presentation/view, host/nuo, docs/reference
superseded_by: null
amends:
  - "[ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) (unifies untitled `thread <id_tail>` fallback and in-place authentic title replacement inside the single-row Thread SceneBar)"
  - "[ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md) (retires Tier 3 ThreadContextBar and contextbar_bg, unifies all scenes on a 2-row head band, and consolidates thread governance, identity/title, and workspace into Tier 2 SceneBar)"
negative_knowledge: true
---

# 0261. Unified Two-Tier Header Topology and Single-Row Thread SceneBar

- **Status**: Accepted
- **Date**: 2026-11-28
- **Deciders**: Nuo Architecture Working Group
- **Amends**: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md), [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)
- **Related**: [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md), [ADR-0092](0092-uncompromising-scenebar-breadcrumb-topology-bidirectional-navigation-and-symmetric-keybindings.md), [ADR-0152](0152-universal-text-overflow-column-budget-and-focus-reveal-topology.md)

---

## Context and Problem Statement

[ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md) moved `Dashboard`, `Settings`, `CommandLog`, and `Diff` to a two-row head band (`TabBar` + `SceneBar`) while keeping a third row (`ThreadContextBar`, `PAGE_HEADER_ROWS = 3`) exclusively on `Thread` viewports. In day-to-day usage, that third row proved counterproductive:

1. **Vertical Waste and Cross-Tab Layout Jump**:
   Every non-thread scene used 2 top chrome rows, while `Thread` consumed 3. Switching between a thread tab and `Dashboard` or `Settings` shifted the entire viewport top boundary by 1 row, and the thread transcript permanently surrendered a vertical row to sparse chrome.
2. **Sparse Utilization of Both Thread Rows**:
   On a root `Thread` viewport (`breadcrumbs.len() <= 1`), Tier 2 `SceneBar`'s right half was permanently empty, while Tier 3 `ThreadContextBar`'s left half carried only `[ROLE]` in the safe default posture (no `auto`/`open`). Two full rows were spent to render four compact tokens.
3. **Redundant UUID Prefix After Title Assignment**:
   Even after a thread acquired an authentic title (`Fix retry logic in auth loop`), `SceneBar` kept printing `THREAD b3c4` ahead of it. Once a conversation has a human title, the 4-character ID tail is visual noise on the primary focal bar; the ID is only needed as an untitled fallback before a title arrives.

---

## Decision Drivers

- **Uniform Two-Row Head Band**: Every scene (`Thread`, `Subagent`, `Dashboard`, `Settings`, `CommandLog`, `Diff`) occupies exactly 2 header rows (`PAGE_HEADER_ROWS = 2`), eliminating cross-tab vertical jitter.
- **Zero-Displacement Title Lifecycle**: Governance tokens (`[ROLE]`, `auto`, `open`) remain pinned at fixed columns on the left so the appearance of an authentic title replaces `thread <id_tail>` strictly in place without shifting any sibling element.
- **Safety-First Degradation**: Safety posture (`auto`, `open`) remains non-truncatable and outranks identity, title, and workspace on narrow terminals.
- **Clean Break**: Retire `ThreadContextBar`, `draw_thread_context_bar`, `theme.contextbar_bg()`, and `HeadBandRects.context` completely rather than leaving compatibility shims or dead elevation tokens.

---

## Architectural Decisions

### 1. Uniform Two-Tier Head Band (`[INV-HEAD-09]`)

The top chrome is universally two rows across every scene (`PAGE_HEADER_ROWS = 2`):

```text
┌────────────────────────────────────────────────────────────────────────┐
│[Refactor DB]  [Fix Auth]  [dashboard]                              ? h │ ◄ Tier 1: TabBar (`tabbar_bg`)
├────────────────────────────────────────────────────────────────────────┤
│  [DEVELOPER]  auto  open  Fix retry logic in auth loop  ~/projects/nuo │ ◄ Tier 2: SceneBar (`scenebar_bg`)
└────────────────────────────────────────────────────────────────────────┘
```

- **Tier 1 (`TabBar`)**: Unchanged. Client-global tab strip on `theme.tabbar_bg()`.
- **Tier 2 (`SceneBar`)**: Universal polymorphic scene bar on `theme.scenebar_bg()`.
  - **Non-Thread scenes** (`Subagent`, `Dashboard`, `Settings`, `CommandLog`, `Diff`): Unchanged — scene label + context on the left, drill-in breadcrumbs on the right.
  - **Thread scene (`SceneKind::Thread`)**: Consolidates thread governance, identity/title, and right-anchored workspace into a single row, separated by pure whitespace (`JOIN_ENUMERATE_COLS = 2`, no punctuation soup):
    - **Untitled (no authentic title yet)**:
      `[ROLE]` `  ` `[auto]` `  ` `[open]` `  ` `thread <id_tail>` `   ...   ` `~/projects/nuo`
      (`thread <id_tail>` styled dimmed; falls back to `thread` if no `thread_id` is available).
    - **Titled (authentic title present)**:
      `[ROLE]` `  ` `[auto]` `  ` `[open]` `  ` `<title>` `   ...   ` `~/projects/nuo`
      (`<title>` styled bold heading; replaces `thread <id_tail>` in place with zero layout shift of `[ROLE]` or posture tokens).
- **Tier 3 Retired**: `ThreadContextBar`, `draw_thread_context_bar`, `HeadBandRects.context`, and `theme.contextbar_bg()` are deleted. The micro-elevation ladder simplifies cleanly to two distinct in-family rungs (`tabbar_bg` ~0.8% offset → `scenebar_bg` ~8.0% offset).

### 2. Fair-Share Budget with Slack Redistribution (`[INV-HEAD-10]`)

Because governance, identity/title, and workspace now share one row, width allocation follows a strict priority and slack-redistribution ladder:

1. **Posture tokens (`auto`, `open`) — highest priority, never truncated**:
   Drawn only on deviation from the safe default (interactive confirmation + workspace-confined file tools). Tokens are admitted whole against the row's text budget; a posture token is never emitted as a clipped prefix (`aut`, `ope`, `op`).
2. **Role badge (`[ROLE]`) — second priority**:
   Allocated from `text_width - posture_w - gap`, elided inside intact brackets (`[SECURITY-AU…]`) via `theme.glyphs.ellipsis`, and dropped below its legible floor (`BADGE_MIN_COLS = 5`) rather than degrading to `[D…]`.
3. **Identity/title and workspace — fair-share with slack redistribution**:
   Let `shared` be the remaining columns after the governance cluster and its `JOIN_ENUMERATE_COLS` gap.
   - **Relaxed fit**: When `identity_full_w + JOIN_ENUMERATE_COLS + ws_full_w <= shared`, both render in full with workspace right-anchored. Toggling posture tokens grows into the inter-cluster gap without shifting the right-anchored workspace path.
   - **Shared pressure** (`usable = shared - JOIN_ENUMERATE_COLS >= SCENE_BAR_WORKSPACE_MIN_COLS + SCENE_BAR_IDENTITY_MIN_COLS`, where `SCENE_BAR_WORKSPACE_MIN_COLS = 4` and `SCENE_BAR_IDENTITY_MIN_COLS = 6`):
     Workspace targets `ws_share = (usable / 3).max(SCENE_BAR_WORKSPACE_MIN_COLS)` and identity targets `id_share = usable - ws_share`.
     - If identity is shorter than `id_share`, its unused slack flows to workspace.
     - If workspace is shorter than `ws_share`, its unused slack flows to identity.
     - Workspace renders first through `PathView` (ancestor-first shortening, preserving the project leaf); any columns saved between `ws_budget` and `ws_actual_w` flow back immediately to identity.
   - **Extreme pressure** (`usable < SCENE_BAR_WORKSPACE_MIN_COLS + SCENE_BAR_IDENTITY_MIN_COLS`):
     Only one element can fit honestly. Because Tier 1 `TabBar` already names the active thread tab, the bar prioritizes workspace (preserving the project leaf down to `SCENE_BAR_WORKSPACE_MIN_COLS = 4`). If workspace is absent or below its floor, identity is drawn if it fits honestly (`>= SCENE_BAR_IDENTITY_MIN_COLS` or fits whole); otherwise both are omitted.
4. **Truncation markers**:
   Every elision across badge, title/identity, and path ladder uses `theme.glyphs.ellipsis` (`…` under Unicode, `...` under ASCII).

---

## Normative Invariants

- **`[INV-HEAD-09]` Unified Two-Tier Head Band and Single-Row Thread SceneBar**: The top chrome MUST occupy at most `PAGE_HEADER_ROWS = 2` rows across all scenes (`TabBar` + `SceneBar`). On `SceneKind::Thread`, `SceneBar` MUST render `[ROLE]` (when staffed), silent-by-default lowercase posture tokens (`auto`, `open`), and the anchor-stable identity slot (`thread <id_tail>` dimmed when untitled, replaced in place by the bold authentic title when present) on the left, with the `PathView`-shortened workspace path anchored to the right edge. Peer elements MUST separate via `JOIN_ENUMERATE_COLS` pure whitespace without punctuation glyphs.
- **`[INV-HEAD-10]` Fair-Share Thread SceneBar Degradation**: On `SceneKind::Thread`, width budgeting MUST enforce strict priority **posture (whole tokens only, never truncated) > role badge (intact brackets or omitted) > fair-share identity/workspace with slack redistribution**. Below the combined floor (`SCENE_BAR_WORKSPACE_MIN_COLS + SCENE_BAR_IDENTITY_MIN_COLS`), the row MUST prefer workspace over title because `TabBar` already projects tab identity, and MUST omit any element below its honesty floor rather than painting a misleading fragment.
- Revises `[INV-HEAD-05]` ([ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)): retires Tier 3 `ThreadContextBar` so all scenes share the two-tier `TabBar` + `SceneBar` topology.
- Revises `[INV-HEAD-06]` ([ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)): reduces the micro-elevation ladder to two in-family rungs (`tabbar_bg` → `scenebar_bg`) and removes `contextbar_bg`.
- Revises `[INV-HEAD-07]` and `[INV-HEAD-08]` ([ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md)): relocates silent-by-default posture tokens, role-badge yielding, and right-anchored `PathView` workspace directly into the single-row `Thread` `SceneBar`, and tightens posture budgeting so tokens are admitted whole even below `MIN_TERMINAL_COLS`.

---

## Consequences & Trade-offs

- **Positive**:
  - Reclaims 1 vertical row on every thread viewport and eliminates the 2↔3 row top-chrome jump when switching between `Thread` and `Dashboard` / `Settings` / `CommandLog` / `Diff`.
  - Eliminates the redundant `THREAD <id_tail>` prefix once a thread is titled while keeping `[ROLE]` and safety posture rock-steady at fixed screen columns.
  - Simplifies `HeadBandRects`, `Theme`, `design.rs`, and `view_header` by removing an entire chrome tier and elevation color.
- **Negative & Incurred Costs**:
  - Once a thread has a title, its 4-character ID tail is no longer shown in the header (it remains inspectable via the Dashboard and thread commands).
  - On narrow terminals with long titles and deep workspace paths, both share Tier 2's horizontal budget via the fair-share ladder rather than occupying separate rows.

---

## Rejected Alternatives & Negative Knowledge

### 1. Reordering Elements Between Untitled and Titled States (`thread <id> [ROLE] auto` → `[ROLE] auto <title>`)
- **Why considered**: Putting `thread <id>` first on an untitled session visually echoed the old `SceneBar` lead.
- **Why rejected**: As soon as a thread's title arrived asynchronously after the first turn, `[ROLE]` and safety posture tokens (`auto`, `open`) jumped leftward across the bar. Pinning `[ROLE]` and posture at the left edge makes the title transition an in-place swap of `thread <id_tail>` → `<title>` with zero visual reflow.

### 2. Joining Governance and Title With a Middle Dot (`[DEVELOPER] auto · Fix auth`)
- **Why considered**: An explicit glyph separator between metadata and title.
- **Why rejected**: Violates the repository's visual-language rule (`docs/reference/tui/visual-language.md` §3): same-rank peers use `JOIN_ENUMERATE_COLS` pure whitespace, never punctuation soup (`·`, `|`). Structural brackets + brand tone on `[ROLE]`, lowercase warning tone on posture, and dimmed vs. bold heading tones on identity/title already separate the slots unambiguously.

### 3. Letting Workspace Starve Title Unboundedly (or Letting Title Push Workspace Off-Screen)
- **Why considered**: Giving either the full title or the full workspace path unconditional first dibs is simpler to implement.
- **Why rejected**: Unbounded workspace paths (`~/projects/org/monorepo/services/payments`) on 80-column terminals would crush titles to a few characters; conversely, unbounded titles would erase the workspace path on every long conversation. Fair-share budgeting with slack redistribution preserves both in typical widths and cleanly degrades to the project leaf at extreme widths.
