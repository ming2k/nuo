//! Flex layout subsystem.
//!
//! A **pure-Rust flexbox-style layout solver** — no CSS text, stylesheets, or
//! parsers involved. What it implements is the flexbox *layout algorithm
//! itself* (main/cross axes, grow/shrink/basis, justify/align, gap — the
//! geometric math), with semantics aligned to the CSS flexbox specification
//! (<https://drafts.csswg.org/css-flexbox>). It belongs to the same family
//! as Yoga (React Native) and Taffy (Dioxus): declarative API +
//! deterministic solving, no DOM, no selectors, no style text.
//!
//! # Why it lives in the engine
//! Terminal UIs are fundamentally one-dimensional streams of vertical
//! content: transcript entries, footer rows, and overlay stacks are all
//! sequences of "an unbounded count of children, each with its own intrinsic
//! height, some still growing". The flexbox main-axis distribution model
//! (basis sets the benchmark, grow distributes surplus, shrink absorbs
//! deficit) is precisely the right abstraction for that domain — the
//! hand-rolled measure/place loops it replaces (footer stack, transcript
//! cursor advancement) were all special cases of it.
//!
//! # Model
//! A [`Flex`] describes a **single-level** flex container: direction (main
//! axis), main-axis alignment (justify), cross-axis alignment (align), gap
//! between children, plus a set of children ([`FlexItem`]), each carrying
//! grow/shrink/basis and cross-axis constraints. Nesting is achieved by
//! feeding a solved child rect back into an inner `Flex` — terminal layouts
//! are almost always shallow, and a retained tree would only add ownership
//! and lifetime burden.
//!
//! # Solving process
//! [`Flex::solve_with`] proceeds in four strict steps, all integer
//! arithmetic:
//!
//! 1. **Resolve the cross axis**: each child's cross size is decided by the
//!    container `align` and the child's own `cross` override. Under stretch
//!    it fills the container's cross axis; otherwise the child takes its
//!    declared `cross` size, positioned per the alignment.
//! 2. **Resolve main-axis bases**: [`Basis::Auto`] consults the measure
//!    callback to ask the child for its intrinsic main-axis demand *given
//!    the already-resolved cross size* (in terminal terms: "given this
//!    width, how many rows does this entry need?"); [`Basis::Fixed`] takes
//!    the declared value directly. Each base is then clamped to
//!    `min_main`/`max_main`.
//! 3. **Main-axis distribution**: available main = container main − total
//!    gap. When the base sum is below the available space, surplus is
//!    distributed by grow weight (floor + deterministic remainder to the
//!    earlier children, so the distributed total is exactly the surplus);
//!    when it exceeds, the deficit is absorbed weighted by
//!    `shrink × basis` (children with shrink 0 never shrink, per spec).
//! 4. **Positioning**: children are placed along the main axis per
//!    `justify`, with gap inserted between them; the cross axis follows
//!    `align` (or the child's override). After distribution, min/max are
//!    re-applied — min outranks overflow shrink (spec behavior:
//!    min-height beats flex-shrink).
//!
//! Main-axis quantities stay in `usize` internally (row counts must not be
//! bound by u16) and convert to u16 only when positioning; all overflow is
//! saturating, and degenerate inputs (empty container, zero sizes, zero
//! weight sums) never panic — they return empty or zero-sized results.
//!
//! # Relation to [`crate::Layout`]
//! `Layout` is the ratatui-compatible legacy API (`Min/Length/Percentage`
//! 1-D splitting). `Flex` is the more general solver and the preferred API
//! for new code. Semantic mapping: `Length(l)` ≈ [`FlexItem::fixed`];
//! `Min(m)` ≈ [`FlexItem::auto()`] with a measure returning m + `grow(1)`;
//! `Percentage(p)` ≈ `FlexItem::fixed(round(p×total))`.
//!
//! # Example
//! ```
//! use nuotc::flex::{AlignItem, Flex, FlexItem, Justify};
//! use nuotc::Rect;
//!
//! // Three children: fixed 1 row, content-sized 3 rows, fill the rest,
//! // with a 1-row gap.
//! let area = Rect::new(0, 0, 40, 10);
//! let items = vec![
//!     FlexItem::fixed(1),
//!     FlexItem::auto().measure(|_cross| 3),
//!     FlexItem::grow().build(),
//! ];
//! let solved = Flex::column()
//!     .gap(1)
//!     .justify(Justify::FlexStart)
//!     .align(AlignItem::Stretch)
//!     .solve_with(area, &items, &|_i, _cross| 0);
//! assert_eq!(solved.len(), 3);
//! // The first two items plus two gaps take 6 rows; the grow item gets 4.
//! assert_eq!(solved.main(0), 1);
//! assert_eq!(solved.main(1), 3);
//! assert_eq!(solved.main(2), 4);
//! ```

use crate::layout::Rect;

/// Main-axis direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDirection {
    /// Main axis is vertical (children stack top to bottom), cross axis is
    /// horizontal. The default for terminal streaming content (transcripts,
    /// logs, footer stacks).
    #[default]
    Column,
    /// Main axis is horizontal (children flow left to right), cross axis is
    /// vertical.
    Row,
}

impl FlexDirection {
    /// The container's main-axis size.
    #[inline]
    pub fn main_of(self, area: Rect) -> u16 {
        match self {
            Self::Column => area.height,
            Self::Row => area.width,
        }
    }

    /// The container's cross-axis size.
    #[inline]
    pub fn cross_of(self, area: Rect) -> u16 {
        match self {
            Self::Column => area.width,
            Self::Row => area.height,
        }
    }
}

/// Main-axis alignment (flexbox `justify-content`). Decides how children are
/// distributed along the main axis when surplus space is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    /// Align to the start; surplus stays at the end. The usual shape for
    /// terminal streaming content.
    #[default]
    FlexStart,
    /// Align to the end; surplus stays at the start.
    FlexEnd,
    /// Center; surplus splits evenly between both ends (floor — the extra
    /// row goes to the end).
    Center,
    /// Justified: first and last children touch the edges, surplus is spread
    /// into the gaps between children (in addition to gap; floor, with the
    /// remainder left at the end).
    SpaceBetween,
    /// Distributed around: each child gets `free/(2n)` space on both sides;
    /// adjacent children's margins merge into double (rounding error stays
    /// at the end).
    SpaceAround,
    /// Evenly distributed: n+1 equal gaps including both ends, each
    /// `free/(n+1)`.
    SpaceEvenly,
}

/// Cross-axis alignment (flexbox `align-items`). Decides how children take
/// size and position on the cross axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignItem {
    /// Children stretch to fill the container's cross axis (unless the child
    /// carries its own `cross` override). In a column flow this means "fill
    /// the available width".
    #[default]
    Stretch,
    /// Align to the cross-axis start; the child takes its declared `cross`
    /// size (0 by default).
    FlexStart,
    /// Align to the cross-axis end.
    FlexEnd,
    /// Center on the cross axis.
    Center,
}

/// A child's main-axis base size (flexbox `flex-basis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Basis {
    /// The measure callback is consulted for the child's intrinsic main-axis
    /// demand (given its cross size). In terminal terms: "given this width,
    /// how many rows does this entry need?"
    #[default]
    Auto,
    /// A fixed base; measure is never called.
    Fixed(u16),
}

/// One child declaration in a flex container.
///
/// Builder style: `FlexItem::fixed(3)` / `FlexItem::auto().measure(f)` /
/// `FlexItem::grow().build()`. Defaults: grow 0, shrink 1, basis auto, no
/// min/max, cross follows the container's align.
#[derive(Debug, Clone, Copy)]
pub struct FlexItem {
    /// Main-axis growth weight (flexbox `flex-grow`). Surplus main space is
    /// distributed by grow weight; 0 means the child never grows.
    pub grow: u16,
    /// Main-axis shrink weight (flexbox `flex-shrink`). When base sizes
    /// exceed the available main space, the deficit is absorbed weighted by
    /// `shrink × basis`; 0 means the child never shrinks. Defaults to 1
    /// (shrinkable), matching the flexbox spec; [`FlexItem::fixed`] zeroes
    /// it.
    pub shrink: u16,
    /// Main-axis base (flexbox `flex-basis`).
    pub basis: Basis,
    min_main: Option<u16>,
    max_main: Option<u16>,
    cross: Option<u16>,
    measure: Option<fn(cross: u16) -> u16>,
}

impl Default for FlexItem {
    fn default() -> Self {
        Self {
            grow: 0,
            shrink: 1,
            basis: Basis::Auto,
            min_main: None,
            max_main: None,
            cross: None,
            measure: None,
        }
    }
}

impl FlexItem {
    /// A child with a fixed main-axis size (`flex: 0 0 n` — never grows,
    /// never shrinks).
    pub fn fixed(main: u16) -> Self {
        Self {
            shrink: 0,
            basis: Basis::Fixed(main),
            ..Self::default()
        }
    }

    /// A child whose base is decided by the measure callback
    /// (`flex: 0 1 auto`, shrinkable).
    pub fn auto() -> Self {
        Self::default()
    }

    /// A child that fills the remaining space (`flex: 1 1 0%` — base 0, all
    /// of its size comes from grow distribution; this is "fill" semantics,
    /// not "content-sized"). Returns a builder so the weight can still be
    /// tuned; finish with `.build()`.
    pub fn grow() -> FlexItemBuilder {
        FlexItemBuilder {
            inner: Self {
                grow: 1,
                shrink: 1,
                basis: Basis::Fixed(0),
                ..Self::default()
            },
        }
    }

    /// Main-axis growth weight (surplus distribution). Multiple grow
    /// children split the surplus proportionally by weight.
    pub fn with_grow(mut self, grow: u16) -> Self {
        self.grow = grow;
        self
    }

    /// Main-axis shrink weight (deficit absorption). Multiple shrink
    /// children absorb the overflow weighted by `shrink × basis`.
    pub fn with_shrink(mut self, shrink: u16) -> Self {
        self.shrink = shrink;
        self
    }

    /// Main-axis lower bound, applied after grow/shrink distribution
    /// (flexbox spec: min outranks overflow shrinking).
    pub fn with_min_main(mut self, min: u16) -> Self {
        self.min_main = Some(min);
        self
    }

    /// Main-axis upper bound.
    pub fn with_max_main(mut self, max: u16) -> Self {
        self.max_main = Some(max);
        self
    }

    /// Override the cross-axis size (leaves container-level stretch; the
    /// child is positioned per the container's align).
    pub fn with_cross(mut self, cross: u16) -> Self {
        self.cross = Some(cross);
        self
    }

    /// Attach a measure callback (effective for [`Basis::Auto`] only). The
    /// callback receives the child's already-resolved cross size.
    pub fn measure(mut self, f: fn(cross: u16) -> u16) -> Self {
        self.measure = Some(f);
        self
    }
}

/// The builder returned by [`FlexItem::grow`], allowing chained overrides
/// before converging back via `.build()`.
#[derive(Debug, Clone, Copy)]
pub struct FlexItemBuilder {
    inner: FlexItem,
}

impl FlexItemBuilder {
    /// Override the grow weight.
    pub fn grow(mut self, grow: u16) -> Self {
        self.inner.grow = grow;
        self
    }

    /// Override the shrink weight.
    pub fn shrink(mut self, shrink: u16) -> Self {
        self.inner.shrink = shrink;
        self
    }

    /// Main-axis lower bound (forwards to [`FlexItem::with_min_main`]).
    pub fn with_min_main(self, min: u16) -> FlexItem {
        self.inner.with_min_main(min)
    }

    /// Main-axis upper bound (forwards to [`FlexItem::with_max_main`]).
    pub fn with_max_main(self, max: u16) -> FlexItem {
        self.inner.with_max_main(max)
    }

    /// Converge into a plain [`FlexItem`].
    pub fn build(self) -> FlexItem {
        self.inner
    }
}

impl From<FlexItemBuilder> for FlexItem {
    fn from(b: FlexItemBuilder) -> Self {
        b.inner
    }
}

/// A single-level flex container declaration.
///
/// Builder style: `Flex::column().gap(1).justify(Justify::FlexStart)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flex {
    /// Main-axis direction.
    pub direction: FlexDirection,
    /// Main-axis alignment.
    pub justify: Justify,
    /// Container-level cross-axis alignment (flexbox `align-items`).
    pub align: AlignItem,
    /// Gap between children (along the main axis, flexbox `gap`); counted
    /// between children only, never added at the ends.
    pub gap: u16,
}

impl Flex {
    /// A column container (vertical main axis).
    pub fn column() -> Self {
        Self {
            direction: FlexDirection::Column,
            ..Self::default()
        }
    }

    /// A row container (horizontal main axis).
    pub fn row() -> Self {
        Self {
            direction: FlexDirection::Row,
            ..Self::default()
        }
    }

    /// Set the gap between children.
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Set the main-axis alignment.
    pub fn justify(mut self, justify: Justify) -> Self {
        self.justify = justify;
        self
    }

    /// Set the container-level cross-axis alignment.
    pub fn align(mut self, align: AlignItem) -> Self {
        self.align = align;
        self
    }

    /// Set the main-axis direction.
    pub fn direction(mut self, direction: FlexDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Solve: split `area` across `items`, returning each child's rect plus
    /// main-axis statistics.
    ///
    /// `measure` is the global intrinsic-size callback: child i, when
    /// [`Basis::Auto`] without its own measure, is asked via
    /// `(i, cross_size)` for its main-axis demand at that cross size.
    /// Passing `&|_, _| 0` declares every auto child's base as 0.
    ///
    /// Nesting: feed a returned child rect straight back into an inner
    /// `Flex::solve_with`.
    pub fn solve_with(
        &self,
        area: Rect,
        items: &[FlexItem],
        measure: &dyn Fn(usize, u16) -> u16,
    ) -> SolvedFlex {
        solve(self, area, items, measure)
    }
}

/// The result of `solve_with`: a rect list parallel to `items`, with
/// main/cross-axis size accessors so call sites never match on direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolvedFlex {
    direction: FlexDirection,
    rects: Vec<Rect>,
    /// Each child's main-axis offset (including the justify lead), at full
    /// `usize` precision — terminal rects saturate at u16 coordinates, but
    /// streaming-content row accounting (e.g. transcript scroll extents)
    /// must not be bound by u16.
    offsets: Vec<usize>,
    /// Solved main-axis sizes, at full `usize` precision (including cases
    /// where `max_main` exceeds u16).
    mains: Vec<usize>,
    /// The solved total of child main sizes (gaps included).
    pub used_main: usize,
    /// The container's full main-axis length.
    pub container_main: usize,
}

impl SolvedFlex {
    /// Number of children.
    pub fn len(&self) -> usize {
        self.rects.len()
    }

    /// Whether there are no children.
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Child i's rect (out-of-bounds panics, matching `Layout::split`'s
    /// indexing convention).
    pub fn rect(&self, i: usize) -> Rect {
        self.rects[i]
    }

    /// Child i's main-axis size (height for Column, width for Row).
    pub fn main(&self, i: usize) -> u16 {
        self.direction.main_of(self.rects[i])
    }

    /// Child i's main-axis offset (including the justify lead), at full
    /// `usize` precision.
    pub fn main_offset(&self, i: usize) -> usize {
        self.offsets[i]
    }

    /// Child i's main-axis size at full `usize` precision (including sizes
    /// beyond u16 via `max_main`).
    pub fn main_exact(&self, i: usize) -> usize {
        self.mains[i]
    }

    /// Child i's cross-axis size.
    pub fn cross(&self, i: usize) -> u16 {
        self.direction.cross_of(self.rects[i])
    }

    /// Iterate over all child rects.
    pub fn iter(&self) -> std::slice::Iter<'_, Rect> {
        self.rects.iter()
    }

    /// Main-axis surplus (container main − used main; 0 when overflowing).
    pub fn free_main(&self) -> usize {
        self.container_main.saturating_sub(self.used_main)
    }
}

impl std::ops::Index<usize> for SolvedFlex {
    type Output = Rect;
    fn index(&self, i: usize) -> &Rect {
        &self.rects[i]
    }
}

/// The solve proper. Four steps: resolve the cross axis → resolve bases
/// (with min/max clamping) → distribute grow/shrink along the main axis →
/// position per justify/align.
fn solve(
    flex: &Flex,
    area: Rect,
    items: &[FlexItem],
    measure: &dyn Fn(usize, u16) -> u16,
) -> SolvedFlex {
    let n = items.len();
    let container_main = flex.direction.main_of(area) as usize;
    if n == 0 {
        return SolvedFlex {
            direction: flex.direction,
            rects: Vec::new(),
            offsets: Vec::new(),
            mains: Vec::new(),
            used_main: 0,
            container_main,
        };
    }

    let container_cross = flex.direction.cross_of(area);
    let gap = flex.gap as usize;
    // Available main = container main − total gap (n−1 gaps).
    let total_gap = gap.saturating_mul(n - 1);
    let available_main = container_main.saturating_sub(total_gap);

    // Step 1: cross-axis sizes
    // Stretch without a cross override → fill; otherwise take the child's
    // declared cross (0 by default).
    let crosses: Vec<u16> = items
        .iter()
        .map(|it| match (flex.align, it.cross) {
            (AlignItem::Stretch, None) => container_cross,
            (_, Some(c)) => c,
            (_, None) => 0,
        })
        .collect();

    // Step 2: main-axis bases (usize math for weighted distribution)
    let mut mains: Vec<usize> = Vec::with_capacity(n);
    for (i, it) in items.iter().enumerate() {
        let raw = match it.basis {
            Basis::Fixed(v) => v as usize,
            Basis::Auto => match it.measure {
                Some(f) => f(crosses[i]) as usize,
                None => measure(i, crosses[i]) as usize,
            },
        };
        let lo = it.min_main.map_or(0, usize::from);
        let hi = it.max_main.map_or(usize::MAX, usize::from);
        mains.push(raw.clamp(lo, hi));
    }

    // Step 3: main-axis grow / shrink distribution
    let bases_sum: usize = mains.iter().sum();
    if bases_sum < available_main {
        // Surplus distributed by grow weight: floor first, then the
        // remainder goes to the earliest grow children, so the distributed
        // total is exactly the surplus and the order is deterministic.
        let surplus = available_main - bases_sum;
        let grow_sum: usize = items.iter().map(|it| it.grow as usize).sum();
        if surplus > 0 && grow_sum > 0 {
            let mut assigned = vec![0usize; n];
            let mut acc = 0usize;
            for (i, it) in items.iter().enumerate() {
                let share = surplus * it.grow as usize / grow_sum;
                assigned[i] = share;
                acc += share;
            }
            let mut remainder = surplus - acc;
            for (i, it) in items.iter().enumerate() {
                if remainder == 0 {
                    break;
                }
                if it.grow > 0 {
                    assigned[i] += 1;
                    remainder -= 1;
                }
            }
            for (m, a) in mains.iter_mut().zip(assigned) {
                *m += a;
            }
        }
    } else if bases_sum > available_main {
        // Overflow absorbed weighted by shrink × basis (shrink = 0 children
        // never participate).
        let deficit = bases_sum - available_main;
        let weighted: usize = items
            .iter()
            .zip(&mains)
            .map(|(it, b)| it.shrink as usize * b)
            .sum();
        if deficit > 0 && weighted > 0 {
            let mut assigned = vec![0usize; n];
            let mut acc = 0usize;
            for (i, it) in items.iter().enumerate() {
                let w = it.shrink as usize * mains[i];
                if w == 0 {
                    continue;
                }
                let share = (deficit * w / weighted).min(mains[i]);
                assigned[i] = share;
                acc += share;
            }
            let mut remainder = deficit.saturating_sub(acc);
            for i in 0..n {
                if remainder == 0 {
                    break;
                }
                let w = items[i].shrink as usize * mains[i];
                if w == 0 {
                    continue;
                }
                let room = mains[i] - assigned[i];
                let extra = room.min(remainder);
                assigned[i] += extra;
                remainder -= extra;
            }
            for (m, a) in mains.iter_mut().zip(assigned) {
                *m -= a;
            }
        }
    }

    // Re-clamp min/max after distribution: min outranks shrinking (spec
    // behavior), max caps growth. When a raised min pushes the total back
    // over the container, overflow is allowed (justify still lays out from
    // the main-axis start; the overflow end is simply clipped).
    for (m, it) in mains.iter_mut().zip(items) {
        let lo = it.min_main.map_or(0, usize::from);
        let hi = it.max_main.map_or(usize::MAX, usize::from);
        *m = (*m).clamp(lo, hi);
    }

    // Step 4: justify / align positioning
    let used: usize = mains.iter().sum::<usize>() + total_gap;
    let free = container_main.saturating_sub(used);
    let (lead, extra_per_gap) = justify_offsets(flex.justify, free, n);

    let mut rects = Vec::with_capacity(n);
    let mut offsets = Vec::with_capacity(n);
    let mut cursor = lead;
    for (i, &main_size) in mains.iter().enumerate() {
        if i > 0 {
            cursor += gap + extra_per_gap;
        }
        offsets.push(cursor);
        let (cross_offset, cross_final) =
            cross_placement(flex.align, items[i].cross, container_cross);
        let rect = match flex.direction {
            FlexDirection::Column => Rect::new(
                area.x.saturating_add(cross_offset),
                area.y
                    .saturating_add(u16::try_from(cursor).unwrap_or(u16::MAX)),
                cross_final,
                u16::try_from(main_size).unwrap_or(u16::MAX),
            ),
            FlexDirection::Row => Rect::new(
                area.x
                    .saturating_add(u16::try_from(cursor).unwrap_or(u16::MAX)),
                area.y.saturating_add(cross_offset),
                u16::try_from(main_size).unwrap_or(u16::MAX),
                cross_final,
            ),
        };
        rects.push(rect);
        cursor += main_size;
    }

    SolvedFlex {
        direction: flex.direction,
        rects,
        offsets,
        mains,
        used_main: used,
        container_main,
    }
}

/// Compute the main-axis lead offset and the extra spacing added to each gap
/// (beyond gap, from justify's surplus distribution). Returns
/// `(lead, extra_per_gap)`; `n` is the child count.
///
/// - FlexStart: `(0, 0)`; FlexEnd: `(free, 0)`; Center: `(free/2, 0)`;
/// - SpaceBetween: `(0, free/(n−1))` (degenerates to FlexStart at n = 1);
/// - SpaceAround: `free/(2n)` on each side of every child, adjacent margins
///   merging into double → `(free/(2n), free/n)`;
/// - SpaceEvenly: n+1 equal gaps → `(free/(n+1), free/(n+1))`.
///
/// Integer rounding error always lands at the end, keeping the distribution
/// deterministic and offsets non-negative.
fn justify_offsets(justify: Justify, free: usize, n: usize) -> (usize, usize) {
    match justify {
        Justify::FlexStart => (0, 0),
        Justify::FlexEnd => (free, 0),
        Justify::Center => (free / 2, 0),
        Justify::SpaceBetween => {
            if n < 2 {
                (0, 0)
            } else {
                (0, free / (n - 1))
            }
        }
        Justify::SpaceAround => (free / (2 * n), free / n),
        Justify::SpaceEvenly => (free / (n + 1), free / (n + 1)),
    }
}

/// Cross-axis placement: returns `(offset, final_cross)`.
///
/// - Child with a cross override: positioned per the container's align
///   (stretch behaves as start);
/// - No override + stretch: offset 0, size = the container's full cross
///   axis;
/// - No override + anything else: zero size, positioned per the alignment
///   (degenerate but deterministic).
fn cross_placement(align: AlignItem, item_cross: Option<u16>, container_cross: u16) -> (u16, u16) {
    match item_cross {
        Some(c) => {
            let offset = match align {
                AlignItem::Stretch | AlignItem::FlexStart => 0,
                AlignItem::FlexEnd => container_cross.saturating_sub(c),
                AlignItem::Center => container_cross.saturating_sub(c) / 2,
            };
            (offset, c)
        }
        None => match align {
            AlignItem::Stretch => (0, container_cross),
            AlignItem::FlexStart => (0, 0),
            AlignItem::FlexEnd => (container_cross, 0),
            AlignItem::Center => (container_cross / 2, 0),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 40, 10);

    fn heights(solved: &SolvedFlex) -> Vec<u16> {
        (0..solved.len()).map(|i| solved.main(i)).collect()
    }

    #[test]
    fn fixed_items_stack_from_top_with_gap() {
        let items = vec![FlexItem::fixed(2), FlexItem::fixed(3)];
        let solved = Flex::column().gap(1).solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(heights(&solved), vec![2, 3]);
        assert_eq!(solved.rect(0), Rect::new(0, 0, 40, 2));
        assert_eq!(solved.rect(1), Rect::new(0, 3, 40, 3));
        assert_eq!(solved.used_main, 6);
        assert_eq!(solved.free_main(), 4);
    }

    #[test]
    fn auto_items_call_measure_with_cross_size() {
        let items = vec![FlexItem::auto(), FlexItem::auto()];
        let solved = Flex::column().solve_with(AREA, &items, &|_i, cross| cross / 10);
        // Cross axis is 40 → 4 rows each.
        assert_eq!(heights(&solved), vec![4, 4]);
    }

    #[test]
    fn item_level_measure_overrides_global() {
        let items = vec![FlexItem::auto().measure(|_| 7), FlexItem::auto()];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 1);
        assert_eq!(heights(&solved), vec![7, 1]);
    }

    #[test]
    fn grow_splits_surplus_by_weight() {
        let items = vec![
            FlexItem::fixed(1),
            FlexItem::grow().build(),
            FlexItem::grow().grow(3).build(),
        ];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 0);
        // Surplus 9 split 1:3 → floors 2/6, remainder 1 to the earlier grow
        // child → 3/6.
        assert_eq!(heights(&solved), vec![1, 3, 6]);
        assert_eq!(solved.used_main, 10);
    }

    #[test]
    fn grow_surplus_is_exact_and_deterministic() {
        // 10 rows, three grow=1 children → floors 3/3/3 + remainder 1 to
        // the first → 4/3/3.
        let items = vec![
            FlexItem::grow().build(),
            FlexItem::grow().build(),
            FlexItem::grow().build(),
        ];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(heights(&solved), vec![4, 3, 3]);
        assert_eq!(solved.used_main, 10);
        assert_eq!(solved.free_main(), 0);
    }

    #[test]
    fn shrink_distributes_deficit_weighted_by_basis() {
        // 10 rows cannot fit 6+6: weighted shrink×basis = 6:6, each gives 1.
        let items = vec![
            FlexItem::fixed(6).with_shrink(1),
            FlexItem::fixed(6).with_shrink(1),
        ];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(heights(&solved), vec![5, 5]);
    }

    #[test]
    fn shrink_zero_items_do_not_shrink() {
        let items = vec![
            FlexItem::fixed(8).with_shrink(1),
            FlexItem::auto().with_shrink(0).measure(|_| 4),
        ];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 0);
        // 12 rows demanded, 10 available: only the first shrinks (8→6).
        assert_eq!(heights(&solved), vec![6, 4]);
    }

    #[test]
    fn min_main_survives_shrink_even_in_overflow() {
        let items = vec![
            FlexItem::fixed(8).with_shrink(1).with_min_main(7),
            FlexItem::fixed(4).with_shrink(1),
        ];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 0);
        // 12 rows demanded, 10 available: weighted shrink brings the first
        // to 6, but min = 7 raises it back; the total 11 exceeds the
        // container by 1 — min outranks shrink, overflow is allowed (spec
        // behavior).
        assert_eq!(heights(&solved), vec![7, 4]);
        assert_eq!(solved.used_main, 11);
    }

    #[test]
    fn max_main_caps_growth() {
        let items = vec![FlexItem::grow().with_max_main(3)];
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(heights(&solved), vec![3]);
        assert_eq!(solved.free_main(), 7);
    }

    #[test]
    fn justify_end_center_between() {
        let items = vec![FlexItem::fixed(2), FlexItem::fixed(2)];
        let flex = Flex::column();
        let s = flex
            .justify(Justify::FlexEnd)
            .solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(s.rect(0).y, 6);
        assert_eq!(s.rect(1).y, 8);
        let s = flex
            .justify(Justify::Center)
            .solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(s.rect(0).y, 3);
        let s = flex
            .justify(Justify::SpaceBetween)
            .solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(s.rect(0).y, 0);
        assert_eq!(s.rect(1).y, 8);
    }

    #[test]
    fn justify_around_and_evenly() {
        // free = 6, n = 2: around → margins 6/4 = 1, gaps 6/2 = 3 → y = 1
        // and y = 1 + 2 + 3 = 6.
        let items = vec![FlexItem::fixed(2), FlexItem::fixed(2)];
        let s = Flex::column()
            .justify(Justify::SpaceAround)
            .solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(s.rect(0).y, 1);
        assert_eq!(s.rect(1).y, 6);
        // evenly: three gaps of 6/3 = 2 → y = 2 and y = 2 + 2 + 2 = 6.
        let s = Flex::column()
            .justify(Justify::SpaceEvenly)
            .solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(s.rect(0).y, 2);
        assert_eq!(s.rect(1).y, 6);
    }

    #[test]
    fn row_direction_lays_out_horizontally() {
        let items = vec![FlexItem::fixed(10), FlexItem::grow().build()];
        let solved = Flex::row().gap(2).solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(solved.rect(0), Rect::new(0, 0, 10, 10));
        assert_eq!(solved.rect(1), Rect::new(12, 0, 28, 10));
        assert_eq!(solved.main(1), 28);
        assert_eq!(solved.cross(1), 10);
    }

    #[test]
    fn cross_override_positions_item() {
        let items = vec![FlexItem::fixed(2).with_cross(10)];
        let solved = Flex::column()
            .align(AlignItem::Center)
            .solve_with(AREA, &items, &|_, _| 0);
        assert_eq!(solved.rect(0), Rect::new(15, 0, 10, 2));
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        // No children.
        let solved = Flex::column().solve_with(AREA, &[], &|_, _| 0);
        assert!(solved.is_empty());
        // Zero-sized area: degenerate but deterministic geometry.
        let zero = Rect::new(0, 0, 0, 0);
        let solved = Flex::column().solve_with(zero, &[FlexItem::fixed(3)], &|_, _| 0);
        assert_eq!(solved.rect(0), Rect::new(0, 0, 0, 3));
        assert_eq!(solved.free_main(), 0);
        // Huge basis: u16 conversion saturates.
        let solved = Flex::column().solve_with(AREA, &[FlexItem::fixed(u16::MAX - 5)], &|_, _| 0);
        assert_eq!(heights(&solved), vec![u16::MAX - 5]);
    }

    #[test]
    fn fixed_items_never_consult_measure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let items = vec![FlexItem::fixed(1), FlexItem::fixed(1)];
        let calls = AtomicUsize::new(0);
        let solved = Flex::column().solve_with(AREA, &items, &|_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            99
        });
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "fixed items must not consult measure"
        );
        assert_eq!(heights(&solved), vec![1, 1]);
    }

    #[test]
    fn nesting_by_feeding_child_rect_back_in() {
        // Outer: header 1 row + body grow; inner: body split in half.
        let outer = Flex::column().solve_with(
            AREA,
            &[FlexItem::fixed(1), FlexItem::grow().build()],
            &|_, _| 0,
        );
        let body = outer.rect(1);
        assert_eq!(body, Rect::new(0, 1, 40, 9));
        let inner = Flex::column().gap(1).solve_with(
            body,
            &[FlexItem::grow().build(), FlexItem::grow().build()],
            &|_, _| 0,
        );
        assert_eq!(inner.rect(0), Rect::new(0, 1, 40, 4));
        assert_eq!(inner.rect(1), Rect::new(0, 6, 40, 4));
    }
}
