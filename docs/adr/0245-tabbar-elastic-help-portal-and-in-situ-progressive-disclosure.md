---
id: ADR-0245
title: "TabBar Elastic Help Portal and In-Situ Progressive Disclosure"
status: accepted
date: 2026-11-24
scope: presentation/ui-primitives, presentation/view, tui/chrome, tui/event-loop, docs/reference/tui
superseded_by: null
amends:
  - "[ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md) (replaces the unconditional omission of a TabBar help entrance with a yield-first elastic help portal resolved from the live keymap)"
  - "[ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md) §2 (the right-edge help entrance returns in elastic form, never as a fixed 13-column tax, and carries only the live-resolved chord)"
negative_knowledge: true
---

# 0245. TabBar Elastic Help Portal and In-Situ Progressive Disclosure

- **Status**: Accepted
- **Date**: 2026-11-24
- **Amends**: [ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md),
  [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md) §2
- **Related**: [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md),
  [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md),
  [ADR-0235](0235-tabbar-carries-its-own-lifecycle-hover-reveal-visible-close-and-reserved-add.md)

---

## Context and Problem Statement

[ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md)
removed the constant `F1 help` keycap from the Tier 1 `TabBar` because a fixed
7-column label plus the 6-column `BAR_LEGEND_GAP_MIN` burned 13 columns on every
terminal — nearly a third of the 44-column minimum width — even when tab titles
were overflowing.

In production usage, retiring the TabBar entrance completely exposed two
structural gaps in progressive disclosure:

1. **Zero On-Screen Safety Net After the First Message**: The empty-state
   capability carousel rotates every 8 seconds and disappears the moment a
   session has messages (and never appears when resuming an existing thread).
   Once inside a live session — or on non-thread scenes (`Dashboard`,
   `Settings`, `CommandLog`, `Diff`) — users have no visible, client-global
   portal to rediscover shortcuts or commands, creating shortcut anxiety and
   FOMO. Ironically, the only remaining `F1` hint in the UI was the Floor-1
   overflow trigger (`F1 more`) inside the composer, which appeared *only* when
   the terminal became too narrow.
2. **State-Relevant Actions Without In-Situ Nudge**: When a session approaches
   its context-window ceiling (`>= 70%`), the `ModelBar` turns the percentage
   warning/critical without naming the immediate remedy (`/compact`) beside the
   gauge, forcing users to remember slash commands out of band.

Progressive disclosure requires two complementary halves: an always-reachable,
client-global safety net so users never fear forgetting a binding, paired with
quiet, pure-state in-situ nudges that appear only where and when a specific
condition holds — without turning the transient `ActivityStrip` into a rotating
tip ticker.

---

## Architectural Decisions

### 1. `[INV-TAB-19]` Yield-First Elastic Help Portal on Tier 1 TabBar

Tier 1 (`TabBar`) is the only chrome band guaranteed to be present across every
scene (`Thread`, `Dashboard`, `Settings`, `CommandLog`, `Diff`), making it the
canonical home for a client-level help entrance. Rather than restoring ADR-0131's
fixed 13-column reservation, the portal resolves the active binding for
`CommandId::OpenHelp` from the live keymap (`Option<Key>`) and degrades across a
three-rung ladder:

1. **Expanded (`{key} help`, e.g. `F1 help`)**: Painted **only** when all open
   tabs plus `+` fit without overflow and the remaining slack accommodates
   `BAR_LEGEND_GAP_MIN` plus any `[!N]` fault badge and the expanded label. The
   `" help"` word spends strictly idle columns — it never causes tab overflow or
   truncates a tab title.
2. **Compact (`{key}`, e.g. `F1`)**: Painted when the row is tighter or tabs
   overflow, separated from the rail by `TAB_HELP_GAP_MIN` (`2` columns) when no
   fault badge leads the cluster (or `BAR_LEGEND_GAP_MIN` when `[!N]` leads the
   cluster, with `JOIN_ENUMERATE_COLS` = `2` between `[!N]` and `{key}`). Under
   default `F1`, the compact portal costs only 4 columns total (2 glyph + 2 gap),
   down from 13.
3. **Omitted**: Painted nothing when `OpenHelp` has no binding (`None`,
   reaffirming `[INV-AFFORDANCE-08]`) or when even the compact portal would
   prevent the active tab window, `+`, or `[!N]` fault badge from fitting.

Priority on the row is strictly: **`[!N]` fault badge > active tab + `+` > help
portal (`Compact` > `Expanded`)**.

### 2. `[INV-TAB-20]` Help Portal Hit-Target Honesty and Hover Lift

- `TabBarNavRects::help_rect` is `Some` if and only if the help portal was
  painted in the same frame, matching the exact columns of the rendered label
  (no phantom hit target, consistent with `[INV-TAB-18]`).
- All five scene renderers (`Thread`, `Dashboard`, `Settings`, `CommandLog`,
  `Diff`) receive `help_key` from the single shared client projection helper and
  mount `UiKey::TabBarHelp` from `help_rect`.
- Hovering `UiKey::TabBarHelp` lifts the portal via `TabBarHover::Help`
  without changing its width; clicking toggles `DialogKind::Help` with the same
  container purity as other TabBar chrome actions (`[INV-TAB-11]`).

### 3. Pure-State In-Situ Context Nudge on `ModelBar`

- When `context_max > 0` and context usage reaches or exceeds
  `CONTEXT_USAGE_WARN_THRESHOLD` (`70%`), `ModelBar` displays a quiet in-situ
  `/compact` nudge beside the context gauge (`89.2k (74%)  /compact`).
- The nudge is a pure function of frame state: it appears automatically at the
  warning threshold, escalates subtly from `theme.dim()` to `theme.warn()` at
  `CONTEXT_USAGE_CRIT_THRESHOLD` (`90%`), and disappears automatically as soon
  as context drops below `70%`.
- In `ModelBar`'s width-degradation ladder, `/compact` is **first to drop** —
  before keycaps, `@instance`, reasoning effort, context usage, and model
  identity — so it never crowds telemetry or identity and adds zero rows.

### 4. `ActivityStrip` Remains Purely Operational

`ActivityStrip` stays strictly bounded to active execution state, pending
permissions, staged queue items, and background tasks (`[INV-FOOTER-02]`),
collapsing to 0 rows when idle. It MUST NOT host rotating tips or idle
onboarding tickers.

---

## Normative Invariants Established

- **`[INV-TAB-19]` Yield-First Elastic TabBar Help Portal**: The Tier 1 `TabBar`
  help portal MUST read its key label exclusively from the caller-resolved
  `CommandId::OpenHelp` binding (`Option<Key>`, never a hardcoded literal), MUST
  paint the expanded `{key} help` form only when no tab overflows and idle slack
  covers the legend gap, MUST shrink to compact `{key}` under width or tab
  pressure, and MUST omit itself whenever unbound or whenever keeping it would
  starve the active tab window, `+`, or `[!N]` fault badge. Enforced by
  `view_header::tests::tab_bar::row1_help_portal_*`.
- **`[INV-TAB-20]` Help Portal Hit-Target Honesty Across All Scenes**:
  `TabBarNavRects::help_rect` MUST be `Some` if and only if the help portal was
  painted in the same frame, MUST match the painted columns exactly, and MUST
  toggle `DialogKind::Help` when clicked via `UiKey::TabBarHelp`. Enforced by
  `view_header::tests::tab_bar::row1_help_portal_hit_rect_matches_painted_state`
  and `event_loop::actions::tests::clicking_tabbar_help_toggles_help_dialog`.
- **`[INV-AFFORDANCE-08]` (reaffirms ADR-0131) Chrome Renders Only Resolved Chords**:
  Remapping `OpenHelp` updates the TabBar portal label automatically, and
  unbinding `OpenHelp` suppresses the portal cleanly.
- **`[INV-TAB-18]` (reaffirms ADR-0235) A Hit Target Exists Only Where Painted**:
  An omitted help portal mounts no `UiKey::TabBarHelp` target.

---

## Consequences & Trade-offs

- **Positive**:
  - Users always have a visible, clickable, client-global escape hatch to the Help
    dialog across every scene, eliminating shortcut FOMO without crowding any
    scene's body or footer.
  - Wide or lightly populated tab strips spend zero active tab columns to show
    `F1 help`; tight or overflowing strips pay only 4 columns (`2` gap + `F1`)
    instead of ADR-0131's 13 columns, and extreme widths yield completely.
  - Context-pressure guidance (`/compact`) appears in situ at the exact gauge
    that signals the problem and vanishes when the pressure clears, with zero
    added rows and zero persistence state.
  - `ActivityStrip` preserves its binary liveness semantics and zero-frame idle
    discipline.
- **Negative & Incurred Costs**:
  - At compact widths with overflowing tabs, the 4-column `F1` portal trims 4
    columns from the tab window compared to ADR-0219's complete omission. This
    is bounded and yields before starving the active tab or `[!N]`.
  - Users who unbind `OpenHelp` lose the TabBar portal as well (deliberate
    honesty under `[INV-AFFORDANCE-08]`; `/help` remains available).

---

## Discarded Alternatives (Negative Knowledge)

### Alternative A: Rotating Tips Carousel in the Idle `ActivityStrip` Slot (Rejected)
- **Why considered**: Reuses the 1-row `ActivityStrip` slot while idle, avoids
  adding new chrome zones, and eliminates the 0↔1 row footer transition.
- **Why rejected**: Violates three core principles: (1) peripheral motion
  directly above the active composer distracts reading and composition; (2)
  occupying the activity slot when idle dilutes the binary liveness signal of
  `ActivityStrip` (`[INV-FOOTER-02]`); (3) breaks zero-frame idle discipline
  and permanently costs 1 row (25% of the transcript viewport at
  `MIN_TERMINAL_ROWS = 12`).

### Alternative B: A Bare `?` Icon on the `TabBar` (Rejected)
- **Why considered**: Only 1–3 columns wide and visually familiar from web apps.
- **Why rejected**: In a chat-first TUI where the composer is focused by default,
  advertising `?` induces users to press `?`, typing literal question marks into
  their prompt — the exact pitfall already rejected by ADR-0131. By contrast,
  compact `F1` is only 2 columns wide, works identically for mouse clicks, and
  accurately names the non-printable chord that actually fires.

### Alternative C: Persistent "Graduated / Seen-Once" Tip Tracking (Rejected)
- **Why considered**: Hides tips permanently after a user uses a feature once.
- **Why rejected**: Humans forget low-frequency shortcuts after weeks away;
  graduating tips away without a standing safety net reintroduces FOMO while
  adding persistent UI state complexity. Pairing a standing elastic `F1` portal
  with stateless, condition-driven in-situ nudges solves both discovery and
  recall without state machines.

### Alternative D: Placing the Universal Portal Only in the Composer Meta Row (Rejected)
- **Why considered**: The idle composer meta row has abundant horizontal slack
  between `Shift-Tab transcript` and `Enter send`.
- **Why rejected**: The composer exists only on the `Thread` scene; `Dashboard`,
  `Settings`, `CommandLog`, and `Diff` would still lack a unified client-level
  portal. Tier 1 `TabBar` is the sole band present everywhere.

---

## Links

- Amends: [ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md),
  [ADR-0131](0131-help-dialog-architecture-tabbar-status-cluster-and-model-bar-purification.md)
- Related: [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md),
  [ADR-0062](0062-polymorphic-scenebar-projection-sovereign-thread-context-and-consolidated-activity-strip.md),
  [ADR-0235](0235-tabbar-carries-its-own-lifecycle-hover-reveal-visible-close-and-reserved-add.md)
- Reference: [TUI Layout Reference](../reference/tui/layout.md)
