---
id: ADR-0235
title: "The TabBar Carries Its Own Lifecycle: Hover Reveal, a Visible Close, and a Reserved Add"
status: accepted
date: 2026-12-22
scope: presentation/ui-primitives, presentation/view, tui/chrome, tui/event-loop, docs/reference/tui
superseded_by: null
amends:
  - "[ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md) §3 (`[INV-TAB-08]`'s title budget is now spent on a pill that reserves its close columns at rest)"
  - "[ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md) (Tier 1 gains the strip's own creation affordance; the clean-label rule is unchanged)"
  - "[ADR-0152](0152-universal-text-overflow-column-budget-and-focus-reveal-topology.md) §4 (`[INV-TUI-MARQUEE-01]`'s focused-row-only rule is extended to a pointer-hover reveal with its own one-way cadence)"
  - "[ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md) (the row still carries no *standing keycap*; the `+` is an affordance, and its reason is stated below)"
negative_knowledge: true
---

# 0235. The TabBar Carries Its Own Lifecycle: Hover Reveal, a Visible Close, and a Reserved Add

- **Status**: Accepted
- **Date**: 2026-12-22
- **Amends**: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
  §3, [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md),
  [ADR-0152](0152-universal-text-overflow-column-budget-and-focus-reveal-topology.md)
  §4, [ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md)
- **Related**: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md)
  (tabs are a client-owned workspace; closing is a non-destructive detach),
  [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md)
  (`[INV-TAB-11]`: the strip is container chrome),
  [ADR-0223](0223-graticule-substrate-vt-input-parser-and-all-motion-hover.md)
  (all-motion hover exists, so the strip can know what the pointer is on)

---

## Context and Problem Statement

Tier 1 could *show* tabs and *switch* tabs. It could not manage them.

1. **Closing was a key you had to know.** `C-x w` / `Alt-w` closed the active
   tab, and the palette carried the same command, but the row itself offered no
   close mark. A pointer-driven user had to select a tab and then find a chord,
   which is a two-step version of what every tab strip in the world does in one
   click. Worse, the row *looked* complete: pills, an overflow window, a fault
   badge — everything except the two verbs a tab strip exists to serve.
2. **Creating one had no affordance at all.** `/new`, `Alt-t`, and the palette
   could all mount a tab, and none of them was on the row. When the strip was
   the only chrome in view, the only visible way to add a workspace was to leave
   it.
3. **A truncated title was unreachable.** ADR-0050 §3 caps a tab label by tab
   count (20 → 14 → 10 columns). Past that cap the title is ellipsized, and a
   terminal has no tooltip: the words were simply gone. ADR-0152 solved exactly
   this for table rows by sliding the focused row's overflowing cell, but
   explicitly restricted reveal to the focused row — and Tier 1 rows are not
   focus rows.
4. **The two facts the row needed had no owner.** Which pill is under the
   pointer, and how long it has been there, are *presentation* facts that only
   the strip can resolve, because only the strip knows its own geometry. There
   was no field for either, so nothing could act on them.

The result was a tab bar that could be read and navigated but not operated: no
close, no add, and a title budget that silently discarded information.

## Architectural Decisions

### 1. `[INV-TAB-15]` Hover reveal is one bounded, one-way pass

A pill whose pointer is on it and whose title does not fit reveals the title by
sliding a window toward the tail, then **stops there**.

```text
rest:   Refactor Database C…
pass:   Refactor Database Connection     ← slides right, one column per step
after:  …atabase Connection              ← rests on the tail, no snap-back
```

- The offset is a function of **elapsed time**, never a frame count:
  `marquee_reveal_offset(max_offset, elapsed_ms, cadence)`.
- The pass length is published by the same module
  (`marquee_reveal_len`), so the loop can stop its heartbeat exactly when the
  motion ends instead of re-deriving the arithmetic.
- The travel is bounded (`TAB_TITLE_REVEAL_MAX_TRAVEL = 24`). A label needing a
  longer slide is not a tab label, and a bounded travel bounds the pass.
- The marker discipline of ADR-0152 is inherited unchanged: a window that
  scrolled past the head paints a leading `…`, so a mid-string fragment is never
  mistaken for the whole title.
- **Reveal is one-way, unlike a focused row.** A focused row's window ping-pongs
  because the reader is inspecting a value; a hovered tab title is read once —
  the user wants the part truncation hid. Snapping back to the discarded head
  would take away the text they just waited to read.

### 2. `[INV-TAB-16]` A hovered pill paints its `✕` inside columns it already owned

The close mark is *revealed*, not *grown*:

- Every pill reserves **two** columns after its title at rest — the separator
  space and the glyph.
- Hover paints the glyph in that reserve. The pill's rect, the strip's total
  width, and the title window are identical hovered or not, so the row cannot
  reflow under the pointer.
- The mark sits one column in from the pill's trailing edge on both surface
  grammars: `" ✕ "` inside a chromatic pill, `" ✕]"` inside a framed
  (monochrome / ANSI-16) one.
- The glyph is `✕`, degrading to `x` on the ASCII glyph set. It is deliberately
  **not** the theme's `cross` (`✗`), whose ASCII fallback is the three-column
  `[X]` — that would break the reserve on exactly the terminals that can least
  afford it.
- The pill floor (`TAB_PILL_MIN_WIDTH = 8`) guarantees the reserve always fits,
  so no pill can exist that the pointer cannot close.
- The hover *anchor* is the pill: moving from a pill onto its own `✕` keeps the
  same anchor, so the title reveal does not rewind under the pointer; moving to
  another pill starts a fresh visit.

### 3. `[INV-TAB-17]` The add affordance is reserved before any title is budgeted

The strip ends with a `+` (three columns: `" + "`, or `"[+]"` on framed
surfaces). Its columns come out of the rail's budget **before** titles are
considered:

```text
available_for_tabs = row − insets − fault badge − legend gap − TAB_ADD_WIDTH
```

Creation belongs to the tab strip; a wall of long titles must truncate itself
before it can starve the strip's own verb. The affordance is drawn on the empty
(brand) row too, so a client with no tabs open can create one from the row.

This does **not** reopen ADR-0219. That decision removed a *standing keycap* — a
chord legend for a dialog pressed once, which permanently taxed titles. The `+`
is an affordance for the row's own primary verb, it costs three columns instead
of thirteen, and it is the one thing on the row a user reaches for every
session. The rule ADR-0219 states ("the row exists to name open tabs") is
satisfied by making the add affordance the *first* claim and letting titles take
the remainder, rather than the other way round.

### 4. `[INV-TAB-18]` A hit target exists only where its affordance is painted

The close and add rects are reported by the same draw that decides what the row
can show, and mounted only when painted:

- `TabBarNavRects::close_rects` is empty unless a pill is hovered;
  `add_rect` is `None` when the columns do not exist.
- Close and add are mounted **after** the pills (same layer, later declaration),
  so the mark wins the hit test on its own column while the rest of the pill
  remains a tab-switch target.
- A close is addressed by the **pill it names**, never by "the active tab". The
  pointer names a workspace; a close that resolves to a different one is a
  closed tab the user never pointed at.

### 5. The projection owns the hover fact

`TabBarProps` gains `hover: Option<TabBarHover>` and `hover_elapsed_ms`, and the
app owns `tab_hover` / `tab_hover_since` / `tab_reveal_active`:

- The *hit* answer from the committed scene is the authority for `hover`, because
  the committed scene is what painted the mark. A separate "which pill is under
  the pointer" derivation would be a second source that can disagree with the
  pixels.
- `hover_elapsed_ms` is measured against the app's anchor instant, so a repaint
  never restarts the reveal clock.
- The draw reports `reveal_active` back to the loop, which folds it into the
  existing `animating` predicate — the same one-way reporting the Models dialog
  uses. An idle strip costs no repaints; a hovered overflow finishes its pass and
  then stops waking the loop.
- All five page renderers that draw Tier 1 (transcript, dashboard, settings,
  command log, diff) feed the projection from one helper, so the strip behaves
  identically wherever it appears.

## Normative Invariants Established

- **`[INV-TAB-15]`** A TabBar hover title reveal MUST be driven by elapsed time,
  MUST slide one way toward the tail, MUST disclose a hidden head with a leading
  marker, and MUST stop after one bounded pass. The loop MUST keep its animation
  heartbeat alive exactly while the pass is in flight.
- **`[INV-TAB-16]`** A pill's width MUST NOT depend on hover. The close mark MUST
  be painted inside columns the pill already reserved, and the reserved columns
  MUST be at least the separator plus one glyph column.
- **`[INV-TAB-17]`** The TabBar's add affordance MUST be budgeted out of the rail
  before any title is, and MUST remain present whenever the row is wide enough to
  draw it — including on the empty (brand) row.
- **`[INV-TAB-18]`** The TabBar close and add hit rects MUST be mounted only when
  the matching affordance was painted in the same frame, and a close MUST address
  the tab the pointer names rather than the active tab.

## Consequences & Trade-offs

- **Positive**:
  - A tab can be closed and created from the row itself, one click each, which is
    the ergonomics every other tab strip in a user's day already taught them.
  - A title that truncation hid is reachable without widening the terminal, on
    both pointer and (via the existing chords) keyboard paths.
  - The strip cannot reflow, jump, or re-truncate under the pointer: hover
    changes only what is painted inside columns that already existed.
  - An idle strip costs zero animation frames; a hovering one costs one bounded
    pass.
- **Negative & Incurred Costs**:
  - Every pill pays two columns of reserve and the row pays three for the add
    affordance. On the 44-column minimum terminal that is real: fewer pills fit
    before the overflow window engages. The trade is deliberate — a strip you
    cannot operate is not made usable by fitting one more truncated title.
  - Hover is a pointer-only affordance. The keyboard path is unchanged
    (`Alt-w` closes, `Alt-t` adds), and the empty-state tour teaches those; the
    row does not advertise them, per ADR-0219.
  - The reveal exists only while the pointer rests on a pill; a user who never
    hovers still loses the tail to the ellipsis. The full title for the *active*
    tab remains reachable on the SceneBar below.

## Discarded Alternatives (Negative Knowledge)

### 1. A close mark on every pill, always visible

- **Why considered**: Zero discovery cost — the affordance is simply there.
- **Why rejected**: It spends two columns per pill permanently (the exact
  failure ADR-0219 documents) and turns the strip into a row of `✕` glyphs that
  compete with the titles. The mark is a *verb*, not an identity; it belongs to
  the pill the pointer is on, not to all of them at once.

### 2. Growing the pill on hover to fit the mark

- **Why considered**: The mark would never crowd the title.
- **Why rejected**: A row that reflows under the pointer moves the target the
  user is aiming at. Reserving the columns at rest is what makes hover a pure
  reveal; the cost is two columns per pill, paid once, at layout time.

### 3. The theme's `cross` glyph (`✗`) for the close mark

- **Why considered**: Reuse the existing semantic glyph instead of adding one.
- **Why rejected**: Its ASCII fallback is `[X]` — three columns. The reserve
  would have to be three columns wide on every terminal to keep the geometry
  glyph-set-independent, or the layout would change with the glyph set. A
  dedicated 1-column glyph with a 1-column ASCII fallback keeps the reserve a
  width rather than a branch.

### 4. Ping-pong reveal (reuse the focused-row cadence verbatim)

- **Why considered**: One cadence, one implementation, nothing new to test.
- **Why rejected**: A ping-pong is an *inspection* affordance — the reader
  compares both ends repeatedly. A hovered tab title is read once. Sliding back
  to the head would erase the text the user was waiting to see, and would keep
  the row animating for as long as the pointer rests there.

### 5. A dropdown menu attached to the strip instead of inline affordances

- **Why considered**: One menu can hold close, add, rename, and everything else.
- **Why rejected**: A menu costs two interactions for the two most frequent verbs
  on the row. Inline affordances make close and add one click; a menu remains the
  right home for the rare verbs, and it is not what this ADR adds.

### 6. Resolving a close against the active tab

- **Why considered**: The existing close path (`handle_close_tab`) closes the
  active tab, and reusing it would be the smallest change.
- **Why rejected**: The pointer names a pill. Whenever the strip and the
  selection disagree — which is exactly the frame in which a click lands — that
  resolution closes a workspace the user did not point at. The close is therefore
  addressed by index (`handle_close_tab_at`), with the keyboard path delegating
  to it using the active index.

## Links

- Amends: [ADR-0050](0050-authentic-thread-title-lifecycle-clean-scene-head-and-semantic-tab-projection.md)
  §3, [ADR-0060](0060-three-tier-spatial-header-topology-governance-band-and-clean-tab-stack.md),
  [ADR-0152](0152-universal-text-overflow-column-budget-and-focus-reveal-topology.md)
  §4, [ADR-0219](0219-tabbar-carries-no-standing-keycap-and-help-discovery-moves-to-the-empty-state.md)
- Related: [ADR-0039](0039-client-tab-workspace-scene-unification-and-reactive-surface-sync.md),
  [ADR-0091](0091-sovereign-tab-context-isolation-and-tabbar-boundary-purity.md),
  [ADR-0223](0223-graticule-substrate-vt-input-parser-and-all-motion-hover.md)
- Reference: `docs/reference/tui/layout.md` §2 (Tier 1) and
  `crates/nuo/src/shell/view_header/tab_bar.rs`
