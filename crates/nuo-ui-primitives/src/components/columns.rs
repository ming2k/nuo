//! One-pass column budget for multi-column rows: caps, floors, slack.
//!
//! ADR-0152 `[INV-ROW-GRID-01]`. A row of columns has one hard constraint — the
//! sum of every column, every gap, the gutter and any trailing reserve must fit
//! the body width — and the earlier code did not conserve it. `models.rs` sized
//! five columns with independent percentage caps summing to 137% of the dialog
//! width and then derived each column's *position* from its neighbour's
//! *demand*. A long model name therefore pushed the connection column right, and
//! the connection column's own budget was computed as "whatever is left", which
//! is how the one column answering *whose quota am I spending?* got starved to
//! `O…`.
//!
//! This module replaces that with a single solve whose last phase is the
//! invariant that matters: **positions are a function of widths, never of
//! neighbours.** No column can be starved by another column's content, and
//! [`super::overflow::fit`] then guarantees the painted text never exceeds the
//! width the solver granted.
//!
//! # A share cap is a column's own relative maximum width
//!
//! `max_share` is the column's ceiling as a percentage of the body: the answer
//! to *"how much of the row may this field ever own?"*. It applies always — a
//! verbose column is not allowed to monopolize a wide terminal just because its
//! data happens to be long. What the cap is **not** is a guarantee: under
//! contention a column may be pushed below it, down to its `min_width`, because
//! the cap stops monopolies while the floor protects readability.
//!
//! # Phases
//!
//! 0. **Optional-first** — a `droppable` column may never be funded out of an
//!    essential column's *floor*. The question is "can every field that must
//!    stay readable still be read?", not "does the row overflow": a row can sit
//!    perfectly within budget and still have squeezed its identity to a stub.
//! 1. **Clamp** — `target = min(demand, body_width * max_share / 100)`, never
//!    below the floor.
//! 2. **Fit** (only when the clamped targets still exceed the body):
//!    a. shrink by `priority` toward the floors, lowest priority first;
//!    b. **drop** droppable columns that cannot reach their floor — a fragment
//!    of an identity string reads as a *different* identity, so absence is the
//!    honest outcome;
//!    c. last resort, when even the floors do not fit: **drop** columns in
//!    ascending priority (rightmost first among equals — a row reads left to
//!    right, so its identity comes first) until one survivor remains, and
//!    shrink that. A one-column `…` is not a column.
//! 3. **Restore** — space freed by a drop (or by a neighbour's cap biting) flows
//!    back to the fields that were truncated, in ascending `priority`, up to
//!    each column's own demand and **no further**.
//! 4. **Justify** — whatever no demand can absorb is spread into the gaps,
//!    bounded by [`Grid::max_gap`], so the table reads across the width without
//!    the gaps opening into voids.
//! 5. **Place** — one left-to-right accumulation emits every `start`.

use super::overflow::WidthAware;

/// The widest a justified gap may grow, in columns.
///
/// Project-wide, because it is a *reading* property, not a per-surface taste: past
/// roughly four columns the eye stops grouping two fields as one row and starts
/// reading them as two islands. Every table that justifies shares this bound so
/// they all read the same way.
pub const MAX_JUSTIFIED_GAP: usize = 4;

/// What one column asks for, and how far the layout may bend that ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    /// The width the column's content intrinsically wants, measured from the
    /// data (the widest cell in the column), not from a guess.
    pub demand: usize,
    /// The floor. A *droppable* column that cannot be funded to this width is
    /// removed from the row entirely. A non-droppable column may be pushed to it
    /// and no further, until the last-resort phase.
    pub min_width: usize,
    /// The ceiling, as a percentage of the body width: this column's relative
    /// maximum width. `0` means "no ceiling".
    pub max_share: usize,
    /// Shrink order when space runs short. `0` gives way last; larger numbers
    /// give way first.
    pub priority: u8,
    /// Whether the column may vanish rather than render a fragment.
    pub droppable: bool,
}

impl ColumnSpec {
    /// A column that may be shrunk to `min_width`, capped at `max_share`
    /// percent, shrinking last (`priority 0`), never dropped.
    pub fn new(demand: usize, min_width: usize, max_share: usize) -> Self {
        Self {
            demand,
            min_width,
            max_share,
            priority: 0,
            droppable: false,
        }
    }

    /// Derive the demand and floor from a [`WidthAware`] cell.
    pub fn for_cell<C: WidthAware + ?Sized>(cell: &C, max_share: usize) -> Self {
        Self::new(cell.demand(), cell.min_width(), max_share)
    }

    /// Set how early this column gives way (`0` = last to shrink).
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Allow this column to vanish when it cannot be funded to its floor.
    pub fn droppable(mut self) -> Self {
        self.droppable = true;
        self
    }

    /// The column's ceiling in columns, given the body width.
    pub fn ceiling(&self, body_width: usize) -> usize {
        if self.max_share == 0 {
            usize::MAX
        } else {
            (body_width * self.max_share + 50) / 100
        }
    }
}

/// The resolved layout of one row: the funded columns, and whether the trailing
/// badge is funded too.
///
/// The reserve decision has to travel with the geometry. A solver that silently
/// reclaims the badge's columns while the renderer keeps painting the badge
/// produces a row wider than its body — the exact overflow the reserve existed to
/// prevent. The two halves read the same verdict from one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solved {
    /// The left-hand columns, left to right.
    pub columns: Vec<Column>,
    /// Whether the trailing badge may be painted. `false` when honouring it would
    /// have starved a column the solver kept.
    pub reserve: bool,
}

/// A funded column: where it starts and how many columns it owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    /// Index into the `specs` slice passed to [`Grid::solve`], so a caller can
    /// match the resolved column back to the field it describes.
    pub index: usize,
    /// 0-based column where the cell begins painting.
    pub start: usize,
    /// Columns the cell may paint.
    pub width: usize,
}

impl Column {
    /// The column just past this one's last painted cell.
    pub fn end(&self) -> usize {
        self.start + self.width
    }
}

/// The frame a row's columns are solved into: how wide it is, how it is
/// indented, and what it must leave alone at the trailing edge.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    body_width: usize,
    gutter: usize,
    gap: usize,
    max_gap: usize,
    reserve_tail: usize,
    justify: bool,
}

impl Grid {
    /// A grid across `body_width` columns, with no gutter, gap or reserve.
    pub fn new(body_width: usize) -> Self {
        Self {
            body_width,
            gutter: 0,
            gap: 0,
            max_gap: usize::MAX,
            reserve_tail: 0,
            justify: false,
        }
    }

    /// The leading indent before the first column.
    pub fn gutter(mut self, gutter: usize) -> Self {
        self.gutter = gutter;
        self
    }

    /// The minimum whitespace between two adjacent columns.
    pub fn gap(mut self, gap: usize) -> Self {
        self.gap = gap;
        self
    }

    /// The largest a gap may grow to when justifying. Without a bound, a very
    /// wide terminal would space the columns so far apart that the row stops
    /// reading as a row.
    pub fn max_gap(mut self, max_gap: usize) -> Self {
        self.max_gap = max_gap.max(self.gap);
        self
    }

    /// Columns pinned to the trailing edge (a status badge) that no left-hand
    /// column may encroach on.
    ///
    /// Includes the gap that separates the badge from the last left-hand column,
    /// so the caller passes `badge_width + gap` and the grid adds nothing on top:
    /// counting that gap twice would quietly cost every row two columns.
    ///
    /// The reservation is a **preference, not a right**. A badge annotates a
    /// row; the row's identity *is* the row. When honouring the reserve would
    /// starve an essential column below its floor — leaving a bare `…` where the
    /// model name should be — the badge yields and the columns take its space.
    /// The same information stays reachable in the detail view, whereas a
    /// nameless row is unreadable everywhere.
    pub fn reserve_tail(mut self, reserve: usize) -> Self {
        self.reserve_tail = reserve;
        self
    }

    /// Spread slack left over after every column sits at its cap into the gaps.
    pub fn justify(mut self, justify: bool) -> Self {
        self.justify = justify;
        self
    }

    /// Resolve `specs` into concrete column geometry.
    ///
    /// Columns whose demand is zero are omitted and cost no gap. The result is
    /// in left-to-right order, so a dropped column simply does not appear.
    pub fn solve(&self, specs: &[ColumnSpec]) -> Vec<Column> {
        self.layout(specs).columns
    }

    /// Resolve the row **and** report whether the trailing badge is funded.
    pub fn layout(&self, specs: &[ColumnSpec]) -> Solved {
        if self.body_width == 0 {
            return Solved {
                columns: Vec::new(),
                reserve: false,
            };
        }
        let columns = self.solve_with_reserve(specs);
        if self.reserve_tail == 0 || !self.reserve_starves_an_essential(&columns, specs) {
            return Solved {
                columns,
                reserve: self.reserve_tail > 0,
            };
        }
        // The badge yields: re-solve with the whole body available to the columns.
        Solved {
            columns: Grid {
                reserve_tail: 0,
                ..*self
            }
            .solve_with_reserve(specs),
            reserve: false,
        }
    }

    /// Whether honouring the reserve starved a **kept** column.
    ///
    /// Only columns the solver chose to keep are judged: a column it dropped was
    /// dropped because the body could not fund it at all, and reclaiming the
    /// badge's columns would not change that. This is the case that matters — a
    /// column the solver *did* keep, but below its floor, is a field rendered as a
    /// bare `…` while an annotation holds real estate.
    fn reserve_starves_an_essential(&self, columns: &[Column], specs: &[ColumnSpec]) -> bool {
        columns.iter().any(|c| {
            let spec = &specs[c.index];
            !spec.droppable
                && spec.demand > 0
                && c.width < spec.min_width.min(spec.demand)
                && self.gutter + self.reserve_tail + c.width <= self.body_width
        })
    }

    fn solve_with_reserve(&self, specs: &[ColumnSpec]) -> Vec<Column> {
        let mut slots: Vec<Slot> = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| Slot {
                index,
                spec: *spec,
                target: spec.demand,
                active: spec.demand > 0,
            })
            .collect();

        self.drop_optionals_that_starve_essentials(&mut slots);
        self.clamp(&mut slots);
        self.fit(&mut slots);
        self.restore(&mut slots);

        let mut gaps = vec![self.gap; slots.iter().filter(|s| s.costs()).count().saturating_sub(1)];
        if self.justify {
            self.justify_into(&slots, &mut gaps);
        }

        // ---- Place: one left-to-right accumulation ----------------------
        let mut columns = Vec::new();
        let mut cursor = self.gutter.min(self.body_width);
        let mut placed = 0usize;
        for slot in &slots {
            if !slot.costs() {
                continue;
            }
            if placed > 0 {
                cursor += gaps[placed - 1];
            }
            // A column is never allowed to start past the body: whatever does
            // not fit is not drawn, which keeps the row inside the frame even
            // on an absurdly narrow body.
            if cursor >= self.body_width {
                break;
            }
            let width = slot.target.min(self.body_width - cursor);
            columns.push(Column {
                index: slot.index,
                start: cursor,
                width,
            });
            cursor += width;
            placed += 1;
        }
        columns
    }

    /// Phase 0: an **optional** column (`droppable`) may never be funded out of
    /// an essential column's *demand*.
    ///
    /// This is the law the reported bug violated. A wire-id column that only
    /// annotates a handful of rows used to be funded to its floor while the
    /// model-name column — which every row needs — was squeezed to its cap and
    /// truncated. The row was "within budget" and still unreadable. So the test
    /// is not "does the row overflow" but "can every essential column still
    /// draw what it needs": the moment the answer is no, the optionals go, in
    /// ascending priority, and the space returns to the fields that earn it.
    fn drop_optionals_that_starve_essentials(&self, slots: &mut [Slot]) {
        loop {
            if self.excess_at_demand(slots, &[]) <= 0 {
                return;
            }
            let victim = self
                .shrink_order(slots)
                .into_iter()
                .find(|&i| slots[i].spec.droppable && slots[i].costs());
            match victim {
                Some(i) => slots[i].active = false,
                None => return,
            }
        }
    }

    /// Phase 1: honour every column's own relative maximum width, but never
    /// below its floor.
    ///
    /// The cap and the floor answer different questions and can conflict on a
    /// narrow body: the cap says *"this field may not monopolize the row"*, the
    /// floor says *"this field is unreadable below N columns"*. Readability
    /// wins — a cap that shreds every column into a fragment while the row has
    /// room to spare is not a layout, it is a bug. When the floors then exceed
    /// the body, phase 2 drops columns rather than silently truncating them all.
    fn clamp(&self, slots: &mut [Slot]) {
        for slot in slots.iter_mut().filter(|s| s.costs()) {
            let ceiling = slot.spec.ceiling(self.body_width);
            let floor = slot.spec.min_width.min(slot.target);
            slot.target = slot.target.min(ceiling).max(floor);
        }
    }

    /// Phase 2: make the clamped targets fit, escalating only as far as needed.
    fn fit(&self, slots: &mut [Slot]) {
        // (a) Shrink by priority toward the floors.
        for &i in &self.shrink_order(slots) {
            let deficit = self.excess(slots, &[]);
            if deficit <= 0 {
                break;
            }
            let floor = slots[i].spec.min_width.min(slots[i].target);
            let room = slots[i].target.saturating_sub(floor);
            let take = room.min(deficit as usize);
            slots[i].target -= take;
        }

        // (b) The row still cannot fund every floor, so a droppable column has
        // to go rather than be shredded. Drop them lowest priority first; each
        // drop also reclaims that column's gap, which is why the loop re-measures
        // instead of assuming a fixed saving. A fragment of an identity string
        // reads as a *different* identity, so absence is the honest outcome.
        while self.excess(slots, &[]) > 0 {
            let victim = self
                .shrink_order(slots)
                .into_iter()
                .find(|&i| slots[i].spec.droppable);
            match victim {
                Some(i) => slots[i].active = false,
                None => break,
            }
        }

        // (c) Last resort: the row may never overflow, and a one-column `…` is
        // not a column — it identifies nothing, exactly like a truncated identity
        // fragment. So when even the floors do not fit, columns are **dropped**
        // in ascending priority (rightmost first among equals) until the row
        // fits, keeping only the last survivor. Only that survivor may be pushed
        // below its floor: something has to be drawn.
        loop {
            let deficit = self.excess(slots, &[]);
            if deficit <= 0 {
                return;
            }
            let live: Vec<usize> = (0..slots.len()).filter(|&i| slots[i].costs()).collect();
            if live.len() <= 1 {
                break;
            }
            // Lowest priority first; among equals, the **rightmost** — a row
            // reads left to right, so the field nearest its start is the one the
            // reader needs first. Dropping the leftmost would sacrifice the
            // row's identity to keep its annotation.
            let victim = live
                .iter()
                .max_by_key(|&&i| (slots[i].spec.priority, i))
                .copied();
            let Some(victim) = victim else { return };
            slots[victim].active = false;
        }
        // One column left and still short: shrink it to whatever fits.
        let survivor = (0..slots.len()).find(|&i| slots[i].costs());
        if let Some(i) = survivor {
            let deficit = self.excess(slots, &[]);
            if deficit > 0 {
                let room = slots[i].target.saturating_sub(1);
                let take = room.min(deficit as usize);
                slots[i].target -= take;
            }
        }
    }

    /// Phase 3: give back what phase 2 took, in ascending `priority` order.
    ///
    /// A column can legitimately be below its ceiling here for exactly one
    /// reason: phase 2 shrank it to resolve a deficit, and then dropping an
    /// optional column (or the caps of its neighbours biting) freed the space
    /// again. Restoring it — toward `min(demand, cap)` and no further — is what
    /// makes a dropped column's columns flow back to the fields that earn them.
    ///
    /// Growth stops at the column's own **demand**, never at its cap. Padding a
    /// column out to its ceiling adds no readability: it only pushes the next
    /// column away, so a 16-character name in a 55-column column renders as a
    /// hole in the middle of the row. Whatever slack the demands cannot absorb
    /// belongs to the gaps (bounded by [`Self::max_gap`]) and then to the row's
    /// trailing edge.
    fn restore(&self, slots: &mut [Slot]) {
        let excess = self.excess(slots, &[]);
        if excess >= 0 {
            return;
        }
        let mut slack = excess.unsigned_abs();
        let mut order: Vec<usize> = (0..slots.len()).filter(|&i| slots[i].costs()).collect();
        order.sort_by(|&a, &b| {
            slots[a]
                .spec
                .priority
                .cmp(&slots[b].spec.priority)
                .then(slots[b].index.cmp(&slots[a].index))
        });
        for i in order {
            if slack == 0 {
                return;
            }
            let ceiling = slots[i].spec.ceiling(self.body_width);
            let room = slots[i]
                .spec
                .demand
                .min(ceiling)
                .saturating_sub(slots[i].target);
            let give = room.min(slack);
            slots[i].target += give;
            slack -= give;
        }
    }

    /// Phase 4: spread the remaining slack into the gaps, bounded by
    /// [`Self::max_gap`].
    ///
    /// The bound is what keeps a table a table. Unbounded, a wide body would push
    /// the gaps apart until two columns sit forty columns from each other — the
    /// eye stops reading them as one row and starts reading them as two islands,
    /// and the very slack that was meant to make the layout breathe turns into a
    /// hole. Whatever `max_gap` cannot absorb stays as trailing padding, which is
    /// exactly where the eye already expects emptiness.
    fn justify_into(&self, slots: &[Slot], gaps: &mut [usize]) {
        if gaps.is_empty() {
            return;
        }
        let excess = self.excess_with(slots, gaps);
        if excess >= 0 {
            return;
        }
        let mut slack = excess.unsigned_abs();
        while slack > 0 {
            let mut progressed = false;
            for gap in gaps.iter_mut() {
                if slack == 0 {
                    break;
                }
                if *gap < self.max_gap {
                    *gap += 1;
                    slack -= 1;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    /// Shrink order: the lowest priority gives way first; within a priority the
    /// widest column yields first (it has the most to give back), then the
    /// rightmost. Fully deterministic, so the same data always paints the same
    /// frame.
    fn shrink_order(&self, slots: &[Slot]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..slots.len()).filter(|&i| slots[i].costs()).collect();
        order.sort_by(|&a, &b| {
            slots[b]
                .spec
                .priority
                .cmp(&slots[a].spec.priority)
                .then(slots[b].target.cmp(&slots[a].target))
                .then(slots[b].index.cmp(&slots[a].index))
        });
        order
    }

    /// Columns the row consumes minus `body_width`. Positive means it overflows.
    fn excess(&self, slots: &[Slot], gaps: &[usize]) -> isize {
        self.excess_with(slots, gaps)
    }

    /// The excess measured with every **essential** column at its *floor*.
    ///
    /// Answers "can every field that must stay readable still be read, given the
    /// optional ones?" — the question phase 0 asks before letting an optional
    /// column exist at all.
    ///
    /// The floor, not the demand, is the right yardstick: an essential column may
    /// be truncated (that is what `Overflow` is for), it may just not be starved
    /// below readability. Measuring against the demand instead would drop an
    /// optional column while every essential field was still comfortably legible,
    /// and then leave a third of the row empty — trading a visible annotation for
    /// dead space.
    fn excess_at_demand(&self, slots: &[Slot], gaps: &[usize]) -> isize {
        self.excess_with_targets(slots, gaps, |slot| {
            if slot.spec.droppable {
                slot.target
            } else {
                slot.spec.min_width.min(slot.spec.demand)
            }
        })
    }

    fn excess_with(&self, slots: &[Slot], gaps: &[usize]) -> isize {
        self.excess_with_targets(slots, gaps, |slot| slot.target)
    }

    fn excess_with_targets(
        &self,
        slots: &[Slot],
        gaps: &[usize],
        target: impl Fn(&Slot) -> usize,
    ) -> isize {
        let live: Vec<&Slot> = slots.iter().filter(|s| s.costs()).collect();
        // `reserve_tail` already includes the gap that separates the badge from
        // the last left-hand column, so it is added once and only once.
        let mut total = self.gutter.min(self.body_width) + self.reserve_tail;
        if !live.is_empty() {
            if gaps.len() == live.len().saturating_sub(1) {
                total += gaps.iter().sum::<usize>();
            } else {
                total += self.gap * (live.len() - 1);
            }
        }
        total += live.iter().map(|s| target(s)).sum::<usize>();
        total as isize - self.body_width as isize
    }
}

/// One column's working state while the budget is being solved.
struct Slot {
    index: usize,
    spec: ColumnSpec,
    target: usize,
    active: bool,
}

impl Slot {
    fn costs(&self) -> bool {
        self.active && self.target > 0
    }
}

/// Look up a resolved column by its spec index.
pub fn column(columns: &[Column], index: usize) -> Option<Column> {
    columns.iter().copied().find(|c| c.index == index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths(columns: &[Column]) -> Vec<usize> {
        columns.iter().map(|c| c.width).collect()
    }

    fn starts(columns: &[Column]) -> Vec<usize> {
        columns.iter().map(|c| c.start).collect()
    }

    fn grid(body: usize) -> Grid {
        Grid::new(body).gutter(1).gap(2).max_gap(2)
    }

    #[test]
    fn every_column_fits_and_the_row_never_overflows() {
        let specs = [
            ColumnSpec::new(40, 12, 30),
            ColumnSpec::new(30, 12, 16).droppable(),
            ColumnSpec::new(6, 0, 10).priority(3),
            ColumnSpec::new(20, 10, 26),
        ];
        for body in 0..=200 {
            for justify in [false, true] {
                for max_gap in [2, 6, usize::MAX] {
                    let cols = grid(body).justify(justify).max_gap(max_gap).solve(&specs);
                    for c in &cols {
                        assert!(
                            c.end() <= body.max(1),
                            "body {body} justify {justify} max_gap {max_gap}: \
                             column {} ends at {}",
                            c.index,
                            c.end()
                        );
                    }
                    for pair in cols.windows(2) {
                        assert!(
                            pair[0].end() <= pair[1].start,
                            "body {body}: {} overlaps {}",
                            pair[0].index,
                            pair[1].index
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_share_cap_is_a_relative_maximum_even_with_room_to_spare() {
        // The requested behaviour: a column may never own more than its share
        // of the row, however long its data happens to be.
        let specs = [ColumnSpec::new(90, 12, 30)];
        let cols = Grid::new(100).gutter(1).gap(2).solve(&specs);
        assert_eq!(widths(&cols), vec![30], "capped at 30% of 100");
    }

    #[test]
    fn an_optional_column_never_displaces_an_essential_columns_demand() {
        // The reported bug. The optional wire-id column used to be funded to its
        // floor while the name — which every row needs — was squeezed into a
        // fragment. "Within budget" is not the same as "readable": the optional
        // column goes first.
        let specs = [
            ColumnSpec::new(22, 16, 34),             // name: essential
            ColumnSpec::new(12, 12, 18).droppable(), // wire id: optional
            ColumnSpec::new(24, 10, 24),             // connection: essential
        ];
        // 34 columns: the essentials fit only if the optional id is not funded.
        let cols = grid(34).solve(&specs);
        assert!(
            column(&cols, 1).is_none(),
            "the optional id is dropped before the name is squeezed: {cols:?}"
        );
        let name = column(&cols, 0).expect("name survives");
        assert!(
            name.width >= 16,
            "the name keeps a readable width instead of a fragment: {}",
            name.width
        );
        for c in &cols {
            assert!(c.end() <= 34, "the row still fits: {cols:?}");
        }
    }

    #[test]
    fn an_optional_column_is_kept_once_every_essential_demand_is_met() {
        let specs = [
            ColumnSpec::new(22, 16, 34),
            ColumnSpec::new(12, 12, 18).droppable(),
            ColumnSpec::new(10, 10, 24),
        ];
        let cols = grid(60).solve(&specs);
        assert!(
            column(&cols, 1).is_some(),
            "with room to spare the optional column stays: {cols:?}"
        );
    }

    #[test]
    fn under_contention_a_column_may_be_pushed_below_its_cap_but_not_its_floor() {
        let specs = [
            ColumnSpec::new(90, 12, 30),
            ColumnSpec::new(30, 12, 16).droppable(),
            ColumnSpec::new(6, 0, 10).priority(3),
            ColumnSpec::new(20, 10, 26),
        ];
        let cols = grid(40).solve(&specs);
        let name = column(&cols, 0).expect("name survives");
        assert!(name.width >= 12, "the name keeps its floor: {}", name.width);
        assert!(name.end() <= 40, "and the row still fits: {cols:?}");
    }

    #[test]
    fn a_long_neighbour_cannot_starve_the_connection_column() {
        // The reported defect: a long model name used to push the connection
        // column off the row. Its floor is funded before any slack exists.
        let specs = [
            ColumnSpec::new(80, 12, 30),
            ColumnSpec::new(30, 12, 16).droppable(),
            ColumnSpec::new(6, 0, 10).priority(3),
            ColumnSpec::new(12, 10, 26),
        ];
        for body in 40..=120 {
            let cols = grid(body).solve(&specs);
            let conn = column(&cols, 3).expect("connection column survives");
            assert!(
                conn.width >= 10,
                "body {body}: connection keeps its floor, got {}",
                conn.width
            );
        }
    }

    #[test]
    fn a_column_that_cannot_be_funded_drops_instead_of_rendering_a_fragment() {
        let specs = [
            ColumnSpec::new(30, 12, 40),
            ColumnSpec::new(30, 12, 22).droppable(),
            ColumnSpec::new(10, 10, 20),
        ];
        // Narrow enough that the id cannot hold 12 columns.
        let cols = grid(30).solve(&specs);
        assert!(
            column(&cols, 1).is_none(),
            "an unfundable droppable column vanishes: {cols:?}"
        );
        assert!(column(&cols, 0).is_some(), "the name column survives");
    }

    #[test]
    fn when_even_the_floors_do_not_fit_the_row_drops_columns_rather_than_showing_stubs() {
        // A one-column `…` is not a column: it identifies nothing, which is the
        // same reason an unfundable optional column drops. So a body too narrow
        // for two floors keeps the higher-priority field whole and drops the
        // other, instead of painting two unreadable slivers.
        let specs = [
            ColumnSpec::new(60, 20, 40),
            ColumnSpec::new(60, 20, 40).priority(2),
        ];
        let cols = grid(30).solve(&specs);
        assert_eq!(
            cols.len(),
            1,
            "only one column can be honest at 30 columns: {cols:?}"
        );
        assert_eq!(cols[0].index, 0, "the higher-priority field survives");
        assert!(cols[0].end() <= 30, "{cols:?}");
    }

    #[test]
    fn the_last_surviving_column_is_shrunk_rather_than_dropped() {
        // Something has to be drawn: with a single column and no room for its
        // floor, it is truncated rather than removed.
        let specs = [ColumnSpec::new(60, 20, 40)];
        let cols = grid(10).solve(&specs);
        assert_eq!(cols.len(), 1, "{cols:?}");
        assert!(cols[0].end() <= 10, "{cols:?}");
    }

    #[test]
    fn a_column_is_never_padded_past_its_own_demand() {
        // The defect this pins: a 20-column name in a 100-column body used to be
        // inflated to its 40% cap, which reads as a hole between the columns —
        // the name is not more legible at 40 columns than at 20, the next column
        // is just further away.
        let specs = [ColumnSpec::new(20, 12, 40), ColumnSpec::new(10, 4, 20)];
        let cols = Grid::new(100).gutter(1).gap(2).max_gap(2).solve(&specs);
        assert_eq!(
            widths(&cols),
            vec![20, 10],
            "each column draws exactly its demand: {cols:?}"
        );
    }

    #[test]
    fn space_freed_by_a_dropped_column_flows_back_to_the_survivors() {
        // The real reason growth exists: phase 2 shrank a column to resolve a
        // deficit, then dropped the optional neighbour. The space it freed must
        // return to the field that was truncated, not evaporate.
        // 40 columns, gutter 1, gap 2. The name needs 30 and the optional id
        // needs 30, so the id cannot be funded and drops; the name must then draw
        // its demand in full rather than staying at whatever phase 2 shrank it to.
        let specs = [
            ColumnSpec::new(30, 12, 80),
            ColumnSpec::new(30, 30, 40).droppable().priority(1),
        ];
        let cols = grid(40).solve(&specs);
        assert!(column(&cols, 1).is_none(), "the optional column drops");
        let name = column(&cols, 0).expect("name survives");
        assert_eq!(
            name.width, 30,
            "the name reclaims the space the drop freed: {cols:?}"
        );
    }

    #[test]
    fn justify_spreads_the_unused_width_into_the_gaps_up_to_the_bound() {
        let specs = [
            ColumnSpec::new(10, 4, 0),
            ColumnSpec::new(10, 4, 0),
            ColumnSpec::new(10, 4, 0),
        ];
        let cols = Grid::new(80)
            .gutter(1)
            .gap(2)
            .max_gap(6)
            .justify(true)
            .solve(&specs);
        assert_eq!(widths(&cols), vec![10, 10, 10]);
        assert_eq!(
            starts(&cols),
            vec![1, 17, 33],
            "gaps grow to the bound, not into voids"
        );
    }

    #[test]
    fn without_justify_the_row_packs_left() {
        let specs = [ColumnSpec::new(10, 4, 0), ColumnSpec::new(10, 4, 0)];
        let cols = Grid::new(80).gutter(1).gap(2).solve(&specs);
        assert_eq!(starts(&cols), vec![1, 13]);
    }

    #[test]
    fn zero_demand_columns_are_invisible_and_cost_no_gap() {
        let specs = [
            ColumnSpec::new(10, 0, 40),
            ColumnSpec::new(0, 0, 22),
            ColumnSpec::new(10, 0, 20),
        ];
        let cols = grid(60).solve(&specs);
        assert_eq!(cols.len(), 2);
        assert_eq!(starts(&cols), vec![1, 13]);
    }

    #[test]
    fn a_trailing_reserve_is_never_encroached_on_while_the_columns_can_meet_their_floors() {
        let specs = [ColumnSpec::new(40, 12, 30), ColumnSpec::new(20, 10, 26)];
        let reserve = 12; // badge + its gap
        // Every body wide enough to fund both floors *and* the badge keeps the
        // badge's columns untouched.
        for body in 40..=120 {
            let cols = grid(body).reserve_tail(reserve).solve(&specs);
            let last = cols.last().expect("columns exist");
            assert!(
                last.end() <= body.saturating_sub(reserve),
                "body {body}: the badge keeps its reserve: {cols:?}"
            );
        }
    }

    #[test]
    fn the_reserve_yields_rather_than_starving_the_rows_identity() {
        // A badge annotates a row; the row's identity *is* the row. On a body too
        // narrow to hold both, the badge gives way instead of leaving a bare `…`
        // where the model name belongs.
        // 24 columns: honouring a 10-column badge would leave the name 13,
        // below its 16-column floor, so the badge yields and the name takes 22.
        let specs = [ColumnSpec::new(30, 16, 40)];
        let cols = grid(24).reserve_tail(10).solve(&specs);
        let name = cols.first().expect("the identity survives");
        assert!(
            name.width >= 16,
            "the name is funded by the badge's columns, got {}: {cols:?}",
            name.width
        );
    }
}

#[cfg(test)]
mod width_accounting_tests {
    use super::*;

    /// The reserved badge plus the gap before it is charged exactly once.
    ///
    /// A double charge is invisible in a screenshot and expensive in practice: it
    /// silently costs every row two columns, which is enough to push a truncated
    /// name one ellipsis earlier than necessary.
    #[test]
    fn a_reserved_badge_is_charged_once_including_its_gap() {
        let gap = 2;
        let badge = 8;
        let specs = [ColumnSpec::new(10, 4, 0), ColumnSpec::new(10, 4, 0)];
        // gutter 1 + col 10 + gap 2 + col 10 = 23; badge 8 + its gap 2 = 10.
        let body = 23 + badge + gap;
        let cols = Grid::new(body)
            .gutter(1)
            .gap(gap)
            .reserve_tail(badge + gap)
            .justify(true)
            .max_gap(gap)
            .solve(&specs);
        let last = cols.last().expect("columns exist");
        assert_eq!(
            last.end() + gap + badge,
            body,
            "the row plus the badge fills the body exactly: {cols:?}"
        );
    }
}
