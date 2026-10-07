//! Declarative footer stack: one place that owns the footer's row order,
//! heights, and rects.
//!
//! The footer used to compute its total height as one hand-rolled sum and
//! then re-derive every row's `y` by re-adding the same heights in the same
//! order — the height arithmetic existed twice, and adding a bar meant
//! touching both copies plus a bespoke rect-plumbing path. This module makes
//! the stack declarative instead: the caller lists the rows (id, height,
//! visibility) in draw order, and a single [`place`] pass walks the list once
//! to produce each row's screen rect and the stack's total height. The two
//! can no longer drift, and a new bar is one more entry in the list.
//!
//! This is a layout-only abstraction — deliberately **not** a retained widget
//! tree. Rows are plain data; the draw calls stay free functions in their own
//! modules (`chrome.rs`, `composer.rs`) and receive the rect this pass
//! placed. State remains owned by [`crate::app::App`]; nothing is cached
//! between frames (the engine already diffs the cell grid, and per-frame
//! rebuild keeps invalidation trivial).
//!
//! Hit-testing follows the same registry idea the transcript stream already
//! uses (`crate::model::layout::LayoutMap`): every placed row is recorded as
//! `(FooterRowId, Rect)`, and the event loop resolves clicks by looking the
//! id up instead of comparing against three separate bespoke rect fields.

use nuotc::Rect;

/// One row of the footer stack, in draw (top → bottom) order.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FooterRow {
    /// Stable identity of the row. Used as the hit-test key and for
    /// assertions, never for ordering — the list order is the order.
    pub id: FooterRowId,
    /// The row's height in rows. Zero-height rows may stay in the list
    /// (they keep their slot) but place no rect.
    pub height: u16,
}

/// Identifies a footer row across frames.
///
/// Deliberately exhaustive rather than `#[non_exhaustive]`: the ids are the
/// contract between the renderer (which places them) and the event loop
/// (which resolves them), and both live in this crate. A new bar adds its id
/// here and its entry in the stack — the compiler then walks every match to
/// the new arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FooterRowId {
    /// The persistent blank separator between transcript and chrome
    /// (`FOOTER_TOP_GAP_ROWS`). Never interactive; in the registry only so
    /// the stack's geometry is complete in one place.
    TopGap,
    /// The durability-health banner (ADR-0196 D4). Present only while the
    /// server's persistence writer is degraded; cleared by `Healthy`.
    PersistenceHealth,
    /// The ambient outbox summary (`QUEUE n · preview · keys`). Click →
    /// Queue modal.
    Queue,
    /// The background tasks status bar (ADR-0212). Shows running/settled tasks.
    Tasks,
    /// The transient live-status bar (breathing dot + status + elapsed).
    Activity,
    /// The composer (input box). Not click-routed through the registry —
    /// its rect is consumed directly for caret/IME positioning and the
    /// permission sheet's anchor — but kept in the id set so the stack can
    /// place it like any other row.
    Composer,
    /// The model bar (context usage, stream rate, model identity). Its
    /// context-meter segment has its own finer hit rect recorded by
    /// `draw_model_bar`. The Enter-action keys and the `as:` target row live
    /// inside the composer row above, not here.
    ModelBar,
}

/// The result of placing a footer stack: every placed row's rect, plus the
/// stack's total height (including invisible rows' zero contribution).
///
/// Rows place in list order; a hidden row simply has height 0 and places no
/// registry entry, so the rows below it move up exactly as its height
/// collapses.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlacedFooter {
    /// Placed rects in stack order, one per row with `height > 0`.
    pub rows: Vec<(FooterRowId, Rect)>,
}

/// Sum of every row's height — the stack's total demand in rows.
///
/// The caller needs this **before** placing (it feeds the outer layout's
/// `Constraint::Length`, which decides how tall the footer band even is), so
/// it exists as its own pass over the same declared list. Because both it and
/// [`place`] walk the identical `&[FooterRow]`, the split height and the
/// placed rects can never disagree.
pub(crate) fn measure(rows: &[FooterRow]) -> u16 {
    rows.iter().map(|row| row.height).sum()
}

/// Place a footer stack into `area` in a single pass.
///
/// `area` is the full footer band (`chunks[1]`), including the horizontal
/// insets; the pass applies [`crate::design::FOOTER_H_INSET`] itself so every
/// row shares one extent and the caller never re-derives `footer_x`/`footer_w`.
/// The stack never overflows `area` on screen: heights are clamped so the
/// rows stop at the band's bottom edge rather than painting past it (the
/// outer layout guarantees the band is tall enough — it was split from
/// [`measure`]'s total — and clamping is a defensive floor for degenerate
/// frames).
pub(crate) fn place(area: Rect, rows: &[FooterRow]) -> PlacedFooter {
    let inner = Rect::new(
        area.x + super::FOOTER_H_INSET,
        area.y,
        area.width.saturating_sub(2 * super::FOOTER_H_INSET).max(1),
        area.height,
    );
    let mut placed = PlacedFooter {
        rows: Vec::with_capacity(rows.len()),
    };
    let mut y = inner.y;
    let bottom = inner.y.saturating_add(inner.height);
    for row in rows {
        if row.height == 0 {
            continue;
        }
        // Clamp against the band's bottom edge; `saturating_add` keeps the
        // degenerate-frame path (zero-height band) from underflowing.
        let height = row.height.min(bottom.saturating_sub(y));
        if height == 0 {
            break;
        }
        placed
            .rows
            .push((row.id, Rect::new(inner.x, y, inner.width, height)));
        y = y.saturating_add(height);
    }
    placed
}

/// Resolve the rect placed for `id` this frame, if the row was visible.
///
/// The event loop's click dispatch calls this instead of holding one field
/// per bar.
pub(crate) fn rect_of(placed: &PlacedFooter, id: FooterRowId) -> Option<Rect> {
    placed
        .rows
        .iter()
        .find(|(row_id, _)| *row_id == id)
        .map(|(_, rect)| *rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 10, 80, 8);

    fn full_stack() -> Vec<FooterRow> {
        vec![
            FooterRow {
                id: FooterRowId::TopGap,
                height: 1,
            },
            FooterRow {
                id: FooterRowId::Queue,
                height: 1,
            },
            FooterRow {
                id: FooterRowId::Activity,
                height: 1,
            },
            FooterRow {
                id: FooterRowId::Composer,
                height: 3,
            },
            FooterRow {
                id: FooterRowId::ModelBar,
                height: 1,
            },
        ]
    }

    /// The stack sums exactly and places rows flush, top → bottom, inside the
    /// shared inset extent. `measure` (which feeds the layout split) and the
    /// placed extents must agree — this is the property that replaces the old
    /// hand-rolled height sum.
    #[test]
    fn places_rows_flush_in_draw_order() {
        let rows = full_stack();
        let placed = place(AREA, &rows);
        assert_eq!(measure(&rows), 1 + 1 + 1 + 3 + 1);
        let rects: Vec<(FooterRowId, Rect)> = placed.rows.clone();
        assert_eq!(rects.len(), 5);
        let expect = [
            (FooterRowId::TopGap, 10),
            (FooterRowId::Queue, 11),
            (FooterRowId::Activity, 12),
            (FooterRowId::Composer, 13),
            (FooterRowId::ModelBar, 16),
        ];
        for (idx, (id, y)) in expect.iter().enumerate() {
            assert_eq!(rects[idx].0, *id, "row {idx} id");
            assert_eq!(rects[idx].1.y, *y, "row {idx} y");
            assert_eq!(rects[idx].1.x, 2, "row {idx} shares the inset x");
            assert_eq!(rects[idx].1.width, 76, "row {idx} shares the band width");
        }
        // Heights match each row's declared height.
        assert_eq!(rects[3].1.height, 3, "composer height");
        assert_eq!(rects[4].1.y, 16, "model bar directly below the composer");
    }

    /// A hidden row (height 0) keeps its slot but places nothing; the rows
    /// below slide up by exactly its height. This is the property the old
    /// hand-rolled offsets had to maintain by re-adding the same heights.
    #[test]
    fn hidden_row_collapses_and_rows_below_slide_up() {
        let mut rows = full_stack();
        rows[1].height = 0; // hide Queue
        rows[2].height = 0; // hide Activity
        let placed = place(AREA, &rows);
        assert_eq!(measure(&rows), 1 + 3 + 1);
        assert_eq!(placed.rows.len(), 3);
        assert!(rect_of(&placed, FooterRowId::Queue).is_none());
        assert!(rect_of(&placed, FooterRowId::Activity).is_none());
        assert_eq!(rect_of(&placed, FooterRowId::Composer).unwrap().y, 11);
    }

    /// The measured demand is reported even when the band is too small to
    /// place everything (the outer `Constraint::Length` needs the true
    /// demand, not the clamped on-screen extent).
    #[test]
    fn reports_full_demand_even_when_the_band_clamps() {
        let rows = full_stack();
        assert_eq!(measure(&rows), 7);
        let tiny = Rect::new(0, 10, 80, 2);
        let placed = place(tiny, &rows);
        // Only what fits places; nothing paints past the band's bottom.
        assert!(
            placed
                .rows
                .iter()
                .all(|(_, r)| r.y + r.height <= tiny.y + tiny.height)
        );
    }

    #[test]
    fn empty_stack_places_nothing() {
        let placed = place(AREA, &[]);
        assert_eq!(measure(&[]), 0);
        assert!(placed.rows.is_empty());
    }

    /// Degenerate zero-height band must not underflow or panic.
    #[test]
    fn degenerate_band_does_not_panic() {
        let rows = full_stack();
        let placed = place(Rect::new(0, 10, 80, 0), &rows);
        assert_eq!(measure(&rows), 7);
        assert!(placed.rows.is_empty());
    }

    /// The zero-gap chrome tokens are structural: adjacent footer rows place
    /// flush by construction, so no gap row is ever inserted. These constants
    /// stay in `design.rs` as the recorded decision (and `render.rs` still
    /// adds `COMPOSER_HINT_GAP_ROWS` when extending the permission sheet over
    /// the hint row); this assertion keeps the decision from silently
    /// changing — bump either token and the flush-ness documented in
    /// `docs/reference/tui/layout.md` must be re-reviewed.
    #[test]
    fn chrome_gap_tokens_stay_zero_so_stack_rows_place_flush() {
        assert_eq!(crate::design::ACTIVITY_COMPOSER_GAP_ROWS, 0);
        assert_eq!(crate::design::COMPOSER_HINT_GAP_ROWS, 0);
    }
}
