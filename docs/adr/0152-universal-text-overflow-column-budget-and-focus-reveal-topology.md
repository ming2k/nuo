---
id: ADR-0152
title: "Universal Text Overflow, One-Pass Column Budgets, and Focus Reveal Topology"
status: accepted
date: 2026-04-02
scope: presentation/nuo-ui-primitives, presentation/nuo-view, shell/nuo, docs/reference/tui
superseded_by: null
negative_knowledge: true
---

# 0152. Universal Text Overflow, One-Pass Column Budgets, and Focus Reveal Topology

- Status: Accepted
- Date: 2026-04-02
- Deciders: Nuo Architecture Working Group
- Consulted: Presentation Substrate, TUI Runtime, Design Systems, Quality Engineering
- Supersedes: the ad-hoc per-column percentage caps in `overlays/provider/models.rs`
  and every call site of `shell/overlays/common::truncate_ellipsis`
- Complements: [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md),
  [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md),
  [ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)

---

## Context and Problem Statement

Three independent defects shared one root cause: the presentation substrate had no
vocabulary for **"this string does not fit"** and no vocabulary for **"this column
owns at most N columns"**. Every surface improvised both, in place.

### 1. Truncation was a single hardcoded policy

`truncate_ellipsis` cut the *head* and appended `…`. That is right for prose and
wrong for identity strings. A model's wire id is
`claude-3-7-sonnet-20250219`; head-truncated it renders `claude-3-7-so…` — the
version and date, the only parts that distinguish it from its predecessor,
are exactly what is destroyed. The same holds for git hashes, semver tags,
token prefixes and file paths. One shape cannot serve all of them, and
`components/path.rs` already proved the substrate knows better by carrying its
own `PathFormatStrategy`.

### 2. Column widths were resolved by bidirectional pushing

`overlays/provider/models.rs` sized five columns with independent percentage caps
that summed to **137%** of the dialog width (label 60% + id 30% + specs 12% +
connection 20% + status 15%), plus a 1-column gutter and four 2-column gaps.
Nothing reconciled the sum against reality. Worse, positions were derived from
*each other*:

- the connection column's **start** was pushed right by the label's demand;
- the connection column's **width** was then computed as
  `body_width - conn_col - status - gap`, i.e. its budget was whatever the push
  happened to leave;
- the specs column was placed by walking *left* from the connection column.

The result is the reported failure: a long model name silently starves the
connection column down to `O…`, so the user cannot tell which account or relay
a model comes from — the one column that answers "whose quota am I spending?"
is the one the algorithm is allowed to eat. Adding a cap to any single column
cannot fix this, because the defect is the *absence of a budget*, not the size
of one.

### 3. Nothing could reveal what truncation hid

A terminal has no tooltip. Whatever the row does not paint is unreachable
information, so the only remedy available to the reader was to widen the
terminal or shrink the font. Meanwhile the modal already runs a 100 ms
animation heartbeat for its spinner and already knows which row is selected.

---

## Decision Drivers

- **Identity strings must degrade in the direction their meaning lives.** A
  versioned id keeps its tail; a path keeps its leaf; prose keeps its head.
- **A budget must be conserved.** The sum of every column's claim, every gap,
  the gutter and the trailing reserve must be reconciled against the body width
  *before* anything is placed. No column may be funded out of another column's
  silent loss.
- **Layout must be one-directional.** A column's geometry may depend on its own
  demand and the global budget. It may never depend on another column's demand.
- **Components, not parents, know how to be narrow.** A cell that is handed 12
  columns should choose its own best 12 columns, instead of being pre-shredded
  by a parent that cannot see its semantics.
- **Reveal on focus, never on the whole list.** Marquee on every row turns a
  scannable table into a wall of motion.
- **The substrate stays domain-free.** `nuotc` owns cells, styles, geometry and
  the differ. It must not learn about columns, columns budgets or marquee.

---

## Architectural Decisions

### 1. `[INV-TEXT-OVERFLOW-01]` One overflow vocabulary, chosen by semantics

`nuo-ui-primitives/src/components/overflow.rs` is the single implementation of
"fit a string into N columns". Every truncation in production presentation code
routes through it; `shell/overlays/common::truncate_ellipsis` is **deleted**.

```rust
pub enum Overflow {
    End,                      // keep the head   — prose, titles, labels
    Middle,                   // keep both ends  — wire ids, hashes, semver
    Start,                    // keep the tail   — paths, breadcrumbs
    Window { offset: usize }, // slide the window — the focused row only
}
```

`fit(text, budget, overflow) -> Fitted` is grapheme-aware (no glyph is ever
split, wide glyphs are never over-budget) and returns the *evidence* of what it
did, not just a string:

```rust
pub struct Fitted {
    pub text: String,        // exactly what to paint
    pub hidden: usize,       // columns the text does not show (0 = fits)
    pub overflow: Overflow,  // the strategy that produced this fit
    pub head_chars: usize,   // chars kept at the head
    pub tail_chars: usize,   // chars kept at the tail
    pub skipped_chars: usize,// chars dropped between head and tail
    pub total_chars: usize,  // chars in the source
    pub lead_marker: bool,   // a leading marker discloses a hidden head
}
```

`Fitted::rebase(char_index)` maps a fuzzy-match character index from the *source*
string onto the *painted* string, so search highlighting survives truncation
instead of silently highlighting the wrong glyph. `fit` also flattens control
graphemes to a single space, so a fitted cell can never break a row — the
`one_line` + `truncate_ellipsis` pair collapses into one call.

`fit_with` takes the marker glyph as an argument, because the marker belongs to
the theme: an ASCII terminal must degrade `…` to `...` everywhere at once, and a
module that bakes in a character the theme cannot reach cannot honour that.

`fit_line` keeps the one behaviour that is genuinely different: it **stops at
the first line break** rather than flattening it. It exists for transcript fold
summaries, where a multi-line summary must be cut at the newline and not
flattened into a run-on sentence — and it still appends the marker, because a
cut at a newline is a truncation and must say so.

### 2. `[INV-ROW-GRID-01]` One-pass column budget with caps, floors, and slack

`nuo-ui-primitives/src/components/columns.rs` resolves a row's columns before a
single cell is fitted:

```rust
pub struct ColumnSpec {
    pub demand: usize,     // intrinsic need, measured from the data
    pub min_width: usize,  // floor: below this the field is not readable
    pub max_share: usize,  // this column's relative maximum width, % of body
    pub priority: u8,      // 0 highest; shrink order when space is short
    pub droppable: bool,   // may vanish rather than render a fragment
}
Grid::new(body).gutter(g).gap(n).max_gap(m).reserve_tail(r).justify(true).solve(&specs)
```

`max_share` answers exactly the question this ADR was opened with — *how much of
the row may this field ever own?* — and it applies always, so a verbose column
cannot monopolize a wide terminal merely because its data happens to be long. It
is a ceiling, not a guarantee: under contention a column may be pushed below it,
down to its floor.

Cap and floor conflict on a narrow body, and readability wins: a cap that shreds
every column into a fragment while the row still has room is not a layout, it is
a bug. So phase 1 clamps to `min(demand, cap)` but never below the floor, and
phase 2 resolves the resulting deficit by dropping columns rather than silently
truncating them all.

The solve proceeds in strictly sequential phases:

1. **Optional-first** — a `droppable` column may never be funded out of an
   essential column's *floor*. The test is "can every field that must stay
   readable still be read?", not "does the row overflow": a row can be perfectly
   within budget and still have squeezed its identity into a fragment.
2. **Clamp** — `target = min(demand, cap)`, never below the floor.
3. **Fit**, escalating only as far as the row requires:
   a. shrink by `priority` toward the floors, lowest priority first;
   b. **drop** droppable columns while the row still cannot fund every floor —
      a fragment of an identity string reads as a *different* identity, so
      absence is honest;
   c. last resort, when even the floors do not fit: **drop** columns in
      ascending priority (rightmost first among equals, because a row reads left
      to right and its identity comes first) until only one survivor remains,
      and shrink *that* one. A one-column `…` is not a column.
4. **Restore** — space freed by a dropped column (or by a neighbour's cap
   biting) flows back to the fields that were truncated, in ascending priority,
   up to each column's **own demand and no further**. Growing to the cap instead
   would pad a 16-character name out to 55 columns, which adds no readability and
   merely pushes the next column away — a hole in the middle of the row.
5. **Justify** — whatever no demand can absorb is spread into the gaps, bounded
   by `max_gap`. Unbounded, a wide body pushes two columns forty columns apart
   and the eye stops reading them as one row.
6. **Place** — every `start` is emitted by a single left-to-right accumulation.

**The trailing reserve is a preference, not a right.** A badge annotates a row;
the row's identity *is* the row. When honouring the reserve would leave a column
the solver kept below its floor — a bare `…` where the model name belongs — the
badge yields and the columns take its space. The same fact is still reachable in
the detail view, whereas a nameless row is unreadable everywhere.

This verdict has to travel **with** the geometry, which is why the solver returns
`Solved { columns, reserve }` rather than a bare column list. A solver that
silently reclaims the badge's columns while the renderer keeps painting the badge
produces a row wider than its body — the exact overflow the reserve exists to
prevent. Both halves read one decision from one place.

Phase 6 is the invariant that matters: **positions are a function of widths,
never of neighbours.** No column can be starved by another column's content, and
`[INV-TEXT-OVERFLOW-01]` then guarantees the painted text never exceeds the
width the solver granted. `reserve_tail` funds a right-pinned trailing badge so
it cannot be shifted by any left-hand column either.

### 3. `[INV-ROW-GRID-02]` Width-aware components (`WidthAware`)

A parent no longer pre-shreds a string. It hands the component a budget and the
component decides what its own narrow form is:

```rust
pub trait WidthAware {
    fn demand(&self) -> usize;                 // width it wants
    fn min_width(&self) -> usize;              // width below which it says nothing true
    fn render_width(&self, budget: usize) -> Vec<Span<'static>>;  // exactly `budget` columns
}
```

`TextCell` is the canonical implementation: text + `Overflow` + styles +
match positions, rendering itself into whatever budget it is given and rebasing
its highlights. This is the container-query contract for the terminal: layout is
a negotiation between a parent that knows the budget and a child that knows its
own semantics.

### 4. `[INV-TUI-MARQUEE-01]` Reveal belongs to the focused row and always discloses itself

Only the row that owns focus may slide its overflowing cells. Every other row
stays static and truncates with the column's semantic strategy. Cadence is
declarative and time-based, never frame-count-based:

```rust
pub struct MarqueeCadence { hold_ms, step_ms, tail_hold_ms }  // 900 / 180 / 700
pub fn window_max_offset(total, budget, marker_w) -> usize
pub fn marquee_offset(max_offset, elapsed_ms, cadence) -> usize  // ping-pong
```

**A window is still a truncation, so it must still say so.** Two rules follow,
and both are tested:

- A window that has scrolled past the head paints a **leading** marker, so a
  mid-string fragment is never mistaken for the whole value.
- A window that stops short of the tail paints a **trailing** marker. This is
  why `window_max_offset` holds one column back: without it, the very end of the
  string would be the one position where the reader is told nothing was skipped.

A cell that fits never moves, and a row that is not focused never moves at all.

A dialog reports its own liveness back to the runtime: `ModelsDialog` records
`reveal_active` while a focused cell is windowed, and the event loop folds that
into its `animating` predicate. The 100 ms heartbeat that already drives the
spinner therefore drives reveal too — no second timer, no unconditional 10 fps
repaint while the list merely sits open.

---

## Normative Invariants Established

- **`[INV-TEXT-OVERFLOW-01]`** Every production truncation in
  `crates/nuo/src/shell` and `crates/nuo-view` MUST route through
  `nuo_ui_primitives::components::overflow`. Local truncation helpers are
  forbidden; `truncate_ellipsis` no longer exists. The overflow strategy MUST be
  chosen by the *semantics* of the field: identity strings use `Middle`, paths
  use `Start`, prose uses `End`.
- **`[INV-ROW-GRID-01]`** A multi-column row MUST resolve its columns through
  `columns::solve` (or an equivalent single-pass budget) before fitting any
  cell. Column geometry MUST be derived from widths alone. A column MAY drop when
  it cannot be funded; it MUST NOT be rendered as an unreadable fragment.
- **`[INV-ROW-GRID-02]`** A cell's narrow form is the cell's own decision,
  expressed through `WidthAware`. Parents MUST NOT slice text on a child's
  behalf.
- **`[INV-TUI-MARQUEE-01]`** Marquee reveal is reserved for the focused row and
  is driven by elapsed time. Static rows MUST NOT animate. A revealed window
  MUST disclose hidden text on the side it hides it: a leading marker for a
  skipped head, a trailing marker for a skipped tail.

---

## Consequences

### Positive

- The connection/provider column can no longer be starved by a long model name:
  its floor is funded before any slack exists to distribute.
- A wire id keeps its discriminating tail (`claude…0219`) instead of losing it
  to a head-ellipsis, and reveals in full when the reader focuses the row.
- Column budget arithmetic is one function with unit tests instead of five
  interacting percentages inside a renderer.
- Search highlighting stays correct under truncation and under marquee.
- Every future table (MCP, tools, connections, sessions) inherits the same
  solver and the same overflow vocabulary.

### Negative & Trade-offs

- On a narrow body the wire-id column drops rather than degrading; the id is
  then reachable only through the focused-row reveal or the detail sub-view.
  This is deliberate: a fragment of the only typeable handle is worse than its
  absence.
- Marquee costs one extra frame per 180 ms while a focused row overflows. The
  runtime gates it on `marquee_active`, so an idle list still sleeps.

---

## Rejected Alternatives & Negative Knowledge

### 1. Tuning the existing percentages

- *Failure Mode*: the caps summed to 137% of the body. Any set of percentages
  that sums to ≤ 100% still lets a long neighbour push a short column to zero,
  because the push is expressed through *positions*. Retuning hides the symptom
  until the next long model name.

### 2. Per-column `min`/`max` without a solver

- *Failure Mode*: independent clamps do not conserve the budget. Either the row
  overflows (cells overwrite each other, since `ListRow` clamps a colliding
  group to `col + 1`) or the leftover is silently dropped instead of being given
  to a column that can use it.

### 3. Marquee on every overflowing row

- *Failure Mode*: a table in which every row drifts is unreadable and burns CPU
  proportional to row count. Motion must mark focus, not decorate the list.

### 4. Teaching `nuotc` about columns or marquee

- *Failure Mode*: `nuotc` is deliberately domain-free — cells, styles, geometry,
  differ. Column budgets are a *component* concern and belong in
  `nuo-ui-primitives`, which already owns row anatomy (`ListRow`, `RowGroup`) and
  semantic text (`PathView`). No change to `../nuotc` is required or wanted.

### 5. A tooltip/hover-popover for hidden text

- *Failure Mode*: a popover occludes the very rows the reader is comparing, and
  requires pointer presence — keyboard users get nothing. In-place reveal keeps
  the column grid intact and works identically for both input modes.

---

## Links

- Complements: [ADR-0054](0054-universal-key-affordance-engine-width-adaptive-floor-truncation-and-modeless-f1-topology.md),
  [ADR-0112](0112-unified-dual-tier-presentation-topology-and-canonical-presenter-bridges.md),
  [ADR-0147](0147-semantic-color-governance-firewall-and-first-class-palette-specimen-topology.md)
- Reference: `docs/reference/tui/modals.md` §3 (identity column) and §4 (column budget)
