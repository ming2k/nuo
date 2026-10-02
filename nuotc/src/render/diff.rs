//! Grid diff: compare a dirty back grid against the front grid (what the
//! terminal currently shows) and emit a minimal stream of [`Draw`] commands.
//!
//! This is the nvim `grid_line` analog. It walks only the dirty rows and only
//! from each row's leftmost dirty column, grouping consecutive equal-style
//! cells into run-length packed [`Draw::Cells`] commands. Style changes start
//! a new run; the backend translates each run into one cursor-move + one SGR
//! set + one byte write.
//!
//! The diff is pure: it produces commands but does not touch crossterm. That
//! keeps it unit-testable (feed two grids, assert the commands) and lets the
//! backend own all I/O and capability negotiation (BCE, color depth).

use crate::Style;
use crate::grid::{BandRotation, Grid};

/// One logical draw operation the backend should perform. The backend turns
/// these into escape codes. Keeping them logical (not raw bytes) means the
/// same diff drives a real terminal and a test capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draw {
    /// Scroll a contiguous band of rows by `amount` before painting the rest
    /// of the frame. Content moves **down** by `amount`, exposing `amount`
    /// blank rows at the top of the band (`y`/`height` delimit the band's
    /// screen rows). Emitted when history was prepended above the viewport.
    ScrollDown { y: u16, height: u16, amount: u16 },
    /// Scroll a contiguous band of rows by `amount`: content moves **up**,
    /// exposing `amount` blank rows at the bottom of the band. This is the
    /// streaming-append shape — a new transcript row lands at the bottom and
    /// every settled row above shifts up one — so the whole-viewport repaint
    /// collapses into one terminal scroll plus the genuinely-new row.
    /// Already-identical rows stay identical after the rotation, which is
    /// what keeps streaming from flickering on terminals without
    /// synchronized updates.
    ScrollUp { y: u16, height: u16, amount: u16 },
    /// Paint a run of cells starting at `(x, y)`. Each cell carries its symbol
    /// and the uniform style for the whole run. The run is contiguous and the
    /// cursor should be positioned at `(x, y)` before writing; the cells
    /// occupy `cells.len()` columns (wide continuations are emitted as their
    /// head glyph's implicit trailing column, so callers should skip
    /// continuation cells when counting).
    Cells {
        x: u16,
        y: u16,
        style: Style,
        /// `(symbol, width)` pairs. Wide-glyph continuation cells are omitted
        /// from this list (the head glyph paints both columns); the backend
        /// advances the cursor by each symbol's width.
        cells: Vec<(crate::cell::CompactSymbol, u8)>,
    },
    /// Clear from `(x, y)` to the end of that row with `style`. On BCE
    /// terminals the backend emits `clr_eol`; without BCE it paints `width`
    /// explicit styled spaces.
    ClearEol {
        x: u16,
        y: u16,
        style: Style,
        width: u16,
    },
}

/// A complete diff over a grid: the list of draw commands plus whether the
/// whole region should be considered repainted (used to drive cursor show/hide
/// and scroll bookkeeping).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawCmd {
    pub draws: Vec<Draw>,
    pub w: u16,
    pub h: u16,
}

impl DrawCmd {
    /// The leading scroll translation, if this frame's diff resolved one.
    /// Always `draws[0]` when present; carried separately so callers that
    /// only need the rotation (e.g. `promote`) don't pattern-match the list.
    pub fn scroll(&self) -> Option<&Draw> {
        self.draws
            .first()
            .filter(|d| matches!(d, Draw::ScrollDown { .. } | Draw::ScrollUp { .. }))
    }
}

/// Diff `back` (desired) against `front` (current terminal state), emitting
/// draw commands only for the dirty region. Reads `back`'s dirty bookkeeping
/// and does not modify either grid — promotion is a separate step
/// ([`promote`](crate::grid::Grid) happens after the backend applies the
/// commands).
///
/// This function never mutates either grid. For a proven scroll translation,
/// it compares against a temporary projection of the post-scroll front grid.
/// The real front grid is rotated only by [`promote_scrolled`] after the
/// backend has successfully flushed the command stream. A failed write can
/// therefore never advance the model ahead of the physical terminal.
pub fn diff(back: &Grid, front: &Grid) -> DrawCmd {
    let (w, h) = back.size();
    debug_assert_eq!(
        front.size(),
        (w, h),
        "front grid must match back grid size before diffing"
    );

    let mut draws = Vec::new();

    let Some((lo, hi)) = back.dirty_rows() else {
        return DrawCmd { draws, w, h };
    };

    // Translation stage: a band of rows that moved wholesale (a streaming
    // transcript pushing history up) becomes one scroll op instead of a full
    // repaint. Correctness contract for a candidate shift k:
    //   * every overlapping row must be identical to its shifted source,
    //     regardless of dirty bookkeeping;
    //   * every vacated non-blank row must be dirty so it is repainted;
    //   * at least one row that differs at its current coordinate must become
    //     equal after the shift. Rewritten-but-identical and repeated blank
    //     rows are not scroll evidence.
    // These conditions make translation a proof, not a similarity heuristic.
    let scroll = if hi.saturating_sub(lo) >= 2 && back.scroll_enabled() {
        detect_scroll(back, front, lo, hi, MAX_SCROLL_LINES)
    } else {
        None
    };

    let mut projected_front = None;
    if let Some(ref scroll) = scroll {
        // Project the post-scroll terminal state for row comparisons without
        // mutating the committed front grid.
        let rotation = if scroll.up {
            BandRotation::Up
        } else {
            BandRotation::Down
        };
        let mut projected = front.clone();
        projected.rotate_band(scroll.y, scroll.height, rotation, scroll.amount);
        projected_front = Some(projected);
        let op = if scroll.up {
            Draw::ScrollUp {
                y: scroll.y,
                height: scroll.height,
                amount: scroll.amount,
            }
        } else {
            Draw::ScrollDown {
                y: scroll.y,
                height: scroll.height,
                amount: scroll.amount,
            }
        };
        draws.insert(0, op);
    }

    let comparison_front = projected_front.as_ref().unwrap_or(front);

    for y in lo..=hi {
        let Some(start) = back.dirty_col_of(y) else {
            continue;
        };
        // A terminal scroll blanks each vacated row in full. Its original
        // dirty column was measured against the pre-scroll row and therefore
        // cannot be used as a safe comparison prefix after projection: cells
        // before `start` may have matched the old row but differ from the new
        // blank row. Comparing every dirty row in the translated band from
        // column zero is both exact and cheap (overlapping translated rows
        // compare equal immediately).
        let start = if scroll.is_some() { 0 } else { start };
        diff_row(&mut draws, back, comparison_front, y, start, w);
    }

    DrawCmd { draws, w, h }
}

/// How far a scroll candidate may shift; beyond this a repaint is cheap
/// anyway and the scan would waste work.
const MAX_SCROLL_LINES: u16 = 256;

/// A translation candidate the diff decided to emit as a scroll.
pub(super) struct ScrollOp {
    pub(super) up: bool,
    pub(super) y: u16,
    pub(super) height: u16,
    pub(super) amount: u16,
    saved_rows: usize,
}

/// Full-row equality across the grid width. Blank-vs-None mismatches are
/// tolerated (out-of-range cells don't exist in either grid).
#[inline]
fn rows_equal(back: &Grid, front: &Grid, back_y: u16, front_y: u16, _w: u16) -> bool {
    match (back.row_slice(back_y), front.row_slice(front_y)) {
        (Some(b), Some(f)) => b == f,
        (None, None) => true,
        _ => false,
    }
}

/// Detect a whole-band vertical translation between the front and back grids
/// within the dirty band `lo..=hi`.
///
/// `up = true` models the streaming-append shape: content moves up by `k`, so
/// screen row `y` should hold `front[y + k]` (rows vacated at the band bottom
/// come up blank/new). `up = false` models a prepend: row `y` holds
/// `front[y - k]`.
///
/// See the contract comment at the call site. Rows whose shifted source falls
/// outside the band impose no translation evidence (the caller repaints them
/// or they are the vacated rows). The candidate that saves the most genuinely
/// changed rows wins; ties prefer the smaller shift. The up direction is tried
/// first because streaming append is the common case.
fn detect_scroll(back: &Grid, front: &Grid, lo: u16, hi: u16, max_shift: u16) -> Option<ScrollOp> {
    let mut best: Option<ScrollOp> = None;
    for up in [true, false] {
        if let Some(op) = detect_scroll_direction(back, front, lo, hi, max_shift, up) {
            match best {
                Some(ref current)
                    if current.saved_rows > op.saved_rows
                        || (current.saved_rows == op.saved_rows && current.amount <= op.amount) => {
                }
                _ => best = Some(op),
            }
        }
    }
    best
}

fn detect_scroll_direction(
    back: &Grid,
    front: &Grid,
    lo: u16,
    hi: u16,
    max_shift: u16,
    up: bool,
) -> Option<ScrollOp> {
    let (w, h) = back.size();
    if w == 0 || h == 0 || hi <= lo {
        return None;
    }
    let band_height = hi - lo + 1;
    let max_shift = max_shift.min(band_height.saturating_sub(1));
    if max_shift == 0 {
        return None;
    }

    // Screen row y after an `up` scroll holds front[y + k]; after a `down`
    // scroll it holds front[y - k]. Sources outside the band are vacated
    // rows — the scroll blanks them and the row diff repaints whatever the
    // back grid wants there.
    let source_row = |y: u16, k: u16| -> Option<u16> {
        if up {
            y.checked_add(k).filter(|&s| s <= hi && s < h)
        } else {
            y.checked_sub(k).filter(|&s| s >= lo)
        }
    };

    let mut best: Option<(usize, u16)> = None;
    let anchor_y = if up { lo } else { hi };
    for k in 1..=max_shift {
        // Fast anchor check: the first row in the shift direction MUST match its shifted source
        if let Some(anchor_src) = source_row(anchor_y, k)
            && !rows_equal(back, front, anchor_y, anchor_src, w)
        {
            continue;
        }

        let mut ok = true;
        let mut saved_changed_rows = 0usize;
        for y in lo..=hi {
            let row_is_dirty = back.dirty_col_of(y).is_some();
            let Some(src) = source_row(y, k) else {
                // Vacated row: no evidence (it will be blanked and, if dirty,
                // repainted by the row diff).
                if !row_is_dirty {
                    // A CLEAN vacated row would be blanked with nobody
                    // repainting it — only safe when the back row is the
                    // terminal's default blank, including style. A visually
                    // blank row with a panel background is not equivalent:
                    // SU/SD exposes terminal-default cells, not styled ones.
                    let terminal_blank = crate::cell::Cell::blank();
                    let blank = back
                        .row_slice(y)
                        .is_none_or(|row| row.iter().all(|c| c == &terminal_blank));
                    if !blank {
                        ok = false;
                        break;
                    }
                }
                continue;
            };
            if !rows_equal(back, front, y, src, w) {
                // Dirty rows are not exempt. Repainting a suffix cannot prove
                // the untouched prefix survived an incorrect translation,
                // and permissive matching turns repeated blank rows into
                // false scroll evidence.
                ok = false;
                break;
            }
            if row_is_dirty && !rows_equal(back, front, y, y, w) {
                saved_changed_rows += 1;
            }
        }
        if ok && saved_changed_rows > 0 {
            match best {
                Some((best_saved, best_k))
                    if best_saved > saved_changed_rows
                        || (best_saved == saved_changed_rows && best_k <= k) => {}
                _ => best = Some((saved_changed_rows, k)),
            }
        }
    }
    let (saved_rows, amount) = best?;
    Some(ScrollOp {
        up,
        y: lo,
        height: band_height,
        amount,
        saved_rows,
    })
}

/// Diff one row from `start_col` to the right edge, appending draw commands.
fn diff_row(draws: &mut Vec<Draw>, back: &Grid, front: &Grid, y: u16, start: u16, w: u16) {
    // Walk the row; whenever the back cell differs from the front cell, start
    // accumulating a run. Runs group cells that share a style; a style change
    // flushes the current run and starts a new one.
    let mut run_x = None;
    let mut run_style = Style::RESET;
    let mut run_cells: Vec<(crate::cell::CompactSymbol, u8)> = Vec::new();

    let mut x = start;
    while x < w {
        #[allow(clippy::unwrap_used)]
        // fallible only as the fallback branch of unwrap_or_else; never panics
        let back_cell = back.get(x, y).unwrap_or_else(|| front.get(x, y).unwrap());
        let front_cell = front.get(x, y).unwrap_or(back_cell);

        // Skip wide continuations in the output: their head (at x-1) paints
        // both columns. We still consume them so the cursor advances.
        if back_cell.is_wide_continuation() {
            x += 1;
            continue;
        }

        if back_cell == front_cell {
            // Cell unchanged: flush any open run, then skip.
            flush_run(draws, &mut run_x, &mut run_style, &mut run_cells, y);
            x += 1;
            continue;
        }

        if let Some((tail_style, tail_width)) = blank_tail(back, y, x, w) {
            flush_run(draws, &mut run_x, &mut run_style, &mut run_cells, y);
            draws.push(Draw::ClearEol {
                x,
                y,
                style: tail_style,
                width: tail_width,
            });
            break;
        }

        // Cell changed. If the style differs from the run's, flush first.
        if run_x.is_none() {
            run_x = Some(x);
            run_style = back_cell.style;
        } else if back_cell.style != run_style {
            flush_run(draws, &mut run_x, &mut run_style, &mut run_cells, y);
            run_x = Some(x);
            run_style = back_cell.style;
        }

        run_cells.push((back_cell.symbol.clone(), back_cell.width));
        x += if back_cell.width == 0 {
            1
        } else {
            back_cell.width as u16
        };
    }

    flush_run(draws, &mut run_x, &mut run_style, &mut run_cells, y);
}

/// Return the uniform style and width of a blank tail starting at `x`, if the
/// desired row from `x..w` is all width-1 spaces with the same style.
fn blank_tail(back: &Grid, y: u16, x: u16, w: u16) -> Option<(Style, u16)> {
    let first = back.get(x, y)?;
    if first.symbol != " " || first.width != 1 {
        return None;
    }
    let style = first.style;
    for col in x + 1..w {
        let cell = back.get(col, y)?;
        if cell.symbol != " " || cell.width != 1 || cell.style != style {
            return None;
        }
    }
    Some((style, w.saturating_sub(x)))
}

/// Emit a pending run as a `Draw::Cells` (if non-empty).
fn flush_run(
    draws: &mut Vec<Draw>,
    run_x: &mut Option<u16>,
    run_style: &mut Style,
    run_cells: &mut Vec<(crate::cell::CompactSymbol, u8)>,
    y: u16,
) {
    if let Some(x) = run_x.take()
        && !run_cells.is_empty()
    {
        draws.push(Draw::Cells {
            x,
            y,
            style: *run_style,
            cells: std::mem::take(run_cells),
        });
    }
    *run_style = Style::RESET;
}

/// Promote the back grid's dirty cells into the front grid, then clear the
/// back grid's dirty bookkeeping. Called by the frame loop *after* the backend
/// has applied the diff's commands — at that point the front grid faithfully
/// mirrors the terminal again.
pub fn promote(back: &mut Grid, front: &mut Grid) {
    promote_impl(back, front, None);
}

/// [`promote`] for a frame whose diff began with a scroll: the scroll rotated
/// whole front rows whose columns the back grid's dirty bookkeeping does not
/// cover, so those rows are copied in full (equal cells are cheap; unequal
/// ones are exactly the drift the rotation introduced).
pub fn promote_scrolled(back: &mut Grid, front: &mut Grid, cmd: &DrawCmd) {
    if let Some(scroll) = cmd.scroll() {
        match *scroll {
            Draw::ScrollDown { y, height, amount } => {
                front.rotate_band(y, height, BandRotation::Down, amount)
            }
            Draw::ScrollUp { y, height, amount } => {
                front.rotate_band(y, height, BandRotation::Up, amount)
            }
            _ => {}
        }
    }
    let band = cmd.scroll().and_then(|scroll| match *scroll {
        Draw::ScrollDown { y, height, .. } | Draw::ScrollUp { y, height, .. } => Some((y, height)),
        _ => None,
    });
    promote_impl(back, front, band);
}

fn promote_impl(back: &mut Grid, front: &mut Grid, scrolled_band: Option<(u16, u16)>) {
    let (w, _h) = back.size();
    let (band_lo, band_hi) = scrolled_band
        .map(|(y, height)| (y, y.saturating_add(height)))
        .unwrap_or((u16::MAX, u16::MAX));
    if let Some((lo, hi)) = back.dirty_rows() {
        for y in lo..=hi {
            let in_band = y >= band_lo && y < band_hi;
            let Some(start) = back.dirty_col_of(y) else {
                // A clean row inside the rotated band must still be synced:
                // the rotation moved its front content even though nothing in
                // the back row changed.
                if in_band {
                    for x in 0..w {
                        if let Some(cell) = back.get(x, y) {
                            let cell = cell.clone();
                            if let Some(dst) = front.cell_mut(x, y) {
                                *dst = cell;
                            }
                        }
                    }
                }
                continue;
            };
            let start = if in_band { 0 } else { start };
            for x in start..w {
                if let Some(cell) = back.get(x, y) {
                    let cell = cell.clone();
                    if let Some(dst) = front.cell_mut(x, y) {
                        *dst = cell;
                    }
                }
            }
        }
    }
    back.clear_dirty();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;
    use crate::cell::Cell;
    use crate::grid::Fit;

    /// A grid whose rows spell the given labels, one per row (rest blank).
    fn labeled(w: u16, labels: &[&str]) -> Grid {
        let mut g = Grid::new(w, labels.len() as u16);
        for (y, label) in labels.iter().enumerate() {
            g.put(0, y as u16, Fit::Clip, Style::default(), label);
        }
        g.clear_dirty();
        g
    }

    /// Streaming-transcript shape: the back grid holds the same history as
    /// the front but shifted **up** one row, with one new tail row. This is
    /// the shape a bottom-following transcript produces every time a middle
    /// component (thinking trace, tool output) grows by one row.
    #[test]
    fn wholesale_up_shift_becomes_one_scroll_plus_new_row() {
        // Front (current screen): h1 h2 h3 blank.
        let front = labeled(10, &["h1", "h2", "h3", ""]);
        // Back (wanted): history shifted up one + NEW at the bottom.
        let mut back = labeled(10, &["h2", "h3", "NEW", ""]);
        // The writer repaints the moved band (rows 0..=2).
        back.clear_dirty();
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = diff(&back, &front);
        match cmd.scroll() {
            Some(Draw::ScrollUp { y, height, amount }) => {
                assert_eq!(
                    (*y, *height, *amount),
                    (0, 3, 1),
                    "band rows 0..=2 scroll up by 1, got y={y} h={height} a={amount}"
                );
            }
            other => panic!(
                "expected a scroll op, got {other:?} — draws: {:?}",
                cmd.draws
            ),
        }
        // The genuinely-new row still repaints; the moved rows must NOT.
        let repaint_rows: Vec<u16> = cmd.draws[1..]
            .iter()
            .filter_map(|d| match d {
                Draw::Cells { y, .. } => Some(*y),
                _ => None,
            })
            .collect();
        assert!(
            repaint_rows.iter().all(|&y| y == 2),
            "only the new row repaints, got {repaint_rows:?}"
        );
    }

    #[test]
    fn vacated_scroll_row_repaints_prefix_that_was_clean_before_scroll() {
        let front = labeled(8, &["aaaaAAAA", "bbbbBBBB", "keepOLD!"]);
        let mut back = front.clone();
        back.put(0, 0, Fit::Clip, Style::default(), "bbbbBBBB");
        back.put(0, 1, Fit::Clip, Style::default(), "keepOLD!");
        // Only the suffix differs at this coordinate before scrolling, so the
        // retained dirty marker begins at column four. ScrollUp will blank the
        // whole row, however, and the unchanged `keep` prefix must be painted
        // again from column zero.
        back.put(0, 2, Fit::Clip, Style::default(), "keepNEW!");

        let cmd = diff(&back, &front);
        assert!(matches!(
            cmd.scroll(),
            Some(Draw::ScrollUp { amount: 1, .. })
        ));
        assert!(cmd.draws.iter().any(|draw| {
            matches!(draw, Draw::Cells { x: 0, y: 2, cells, .. }
                if cells.iter().map(|(symbol, _)| symbol.as_str()).collect::<String>()
                    == "keepNEW!")
        }));
    }

    /// Prepend shape: content inserted above the viewport pushes history
    /// **down**. The scroll resolves to the down direction.
    #[test]
    fn wholesale_down_shift_becomes_one_scroll() {
        let front = labeled(10, &["h2", "h3", ""]);
        let mut back = labeled(10, &["NEW", "h2", "h3"]);
        back.clear_dirty();
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = diff(&back, &front);
        match cmd.scroll() {
            Some(Draw::ScrollDown { y, height, amount }) => {
                assert_eq!(
                    (*y, *height, *amount),
                    (0, 3, 1),
                    "rows 0..=2 scroll down by 1"
                );
            }
            other => panic!(
                "expected a scroll op, got {other:?} — draws: {:?}",
                cmd.draws
            ),
        }
    }

    /// A no-shift frame (in-place cell edits) must not emit a scroll.
    #[test]
    fn in_place_edits_do_not_scroll() {
        let mut back = labeled(10, &["one", "two"]);
        let front = labeled(10, &["one", "two"]);
        back.set(0, 0, Cell::narrow("O", Style::default()));
        let cmd = diff(&back, &front);
        assert!(
            cmd.scroll().is_none(),
            "an in-place edit must never translate to a scroll: {:?}",
            cmd.draws
        );
    }

    #[test]
    fn full_dirty_local_edit_with_repeated_blank_rows_does_not_scroll() {
        let front = labeled(12, &["Header", "", "body", "", "", ""]);
        let mut back = front.clone();
        back.set(0, 0, Cell::narrow("h", Style::default()));
        // Full-frame background/widget passes can conservatively mark every
        // row. Repeated blanks must never be treated as translation evidence.
        back.mark_all_dirty();

        let cmd = diff(&back, &front);
        assert!(
            cmd.scroll().is_none(),
            "a local edit must not become a terminal scroll: {:?}",
            cmd.draws
        );
    }

    #[test]
    fn scroll_rejects_clean_vacated_row_with_nondefault_style() {
        let mut front = labeled(6, &["A", "B", "C", "", "D"]);
        let panel = Style::default().bg(Color::Rgb(8, 9, 10));
        front.fill_rect(0, 3, 6, 1, panel);
        front.clear_dirty();

        let mut back = front.clone();
        // Desired rows 0..=2 are an exact two-row upward translation. Row 3
        // remains a clean, panel-colored blank; row 4 is genuinely new.
        for y in 0..=2 {
            for x in 0..6 {
                back.set(x, y, front.get(x, y + 2).unwrap().clone());
            }
        }
        back.put(0, 4, Fit::Clip, Style::default(), "NEW");

        let cmd = diff(&back, &front);
        assert!(
            cmd.scroll().is_none(),
            "scrolling would erase the clean row's panel background: {:?}",
            cmd.draws
        );
    }

    #[test]
    fn diff_does_not_mutate_front_before_successful_promotion() {
        let front = labeled(10, &["h1", "h2", "h3", ""]);
        let original: Vec<String> = (0..4)
            .map(|y| front.get(0, y).unwrap().symbol.to_string())
            .collect();
        let mut back = labeled(10, &["h2", "h3", "NEW", ""]);
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = diff(&back, &front);
        assert!(cmd.scroll().is_some());
        let after_diff: Vec<String> = (0..4)
            .map(|y| front.get(0, y).unwrap().symbol.to_string())
            .collect();
        assert_eq!(
            after_diff, original,
            "planning a scroll must not advance committed terminal state"
        );
    }

    /// Mismatched history (content changed AND moved) must not scroll — the
    /// safe fallback is the ordinary row repaint.
    #[test]
    fn mismatched_shift_does_not_scroll() {
        // Front history differs from what the back's shift implies.
        let front = labeled(10, &["hx", "h3", ""]);
        let mut back = labeled(10, &["h3", "NEW", ""]);
        back.clear_dirty();
        for (y, ch) in [(0u16, '3'), (1, 'N')] {
            back.set(1, y, Cell::narrow(ch.to_string(), Style::default()));
        }
        let cmd = diff(&back, &front);
        assert!(
            cmd.scroll().is_none(),
            "a shift over changed history must fall back to repaint: {:?}",
            cmd.draws
        );
    }

    /// A frame whose only change is a shift with nothing new to paint is
    /// not worth a scroll — no anchor rows means no repaint, so the diff
    /// must resolve to nothing (the terminal already matches after any
    /// hypothetical scroll, and matches without one).
    #[test]
    fn pure_shift_without_repaint_is_not_a_scroll() {
        let front = labeled(10, &["a", "b", "c"]);
        let mut back = labeled(10, &["a", "b", "c"]);
        // Mark rows dirty with content equal to what's already there — the
        // degenerate "writer rewrote identical bytes" frame.
        back.set(0, 1, Cell::narrow("b", Style::default()));
        back.set(0, 2, Cell::narrow("c", Style::default()));
        let cmd = diff(&back, &front);
        assert!(cmd.scroll().is_none(), "no-repaint frame must not scroll");
        assert!(
            cmd.draws
                .iter()
                .all(|d| !matches!(d, Draw::ScrollDown { .. })),
            "no scroll variant may appear: {:?}",
            cmd.draws
        );
    }

    /// promote after a scrolled diff leaves front mirroring back exactly.
    #[test]
    fn promote_after_scroll_syncs_front_to_back() {
        let mut front = labeled(10, &["h1", "h2", "h3", ""]);
        let mut back = labeled(10, &["h2", "h3", "NEW", ""]);
        back.clear_dirty();
        for y in 0..=2 {
            back.mark(0, y);
        }

        let cmd = diff(&back, &front);
        assert!(cmd.scroll().is_some());
        promote_scrolled(&mut back, &mut front, &cmd);

        // Front must now equal back row-for-row.
        for y in 0..4u16 {
            for x in 0..10u16 {
                assert_eq!(
                    back.get(x, y).map(|c| c.symbol.clone()),
                    front.get(x, y).map(|c| c.symbol.clone()),
                    "row {y} diverged after scrolled promote"
                );
            }
        }
        // And a second diff against the promoted front is empty.
        assert!(diff(&back, &front).draws.is_empty());
    }

    /// Grid band rotation sanity: content moves, exposed rows blank, and the
    /// operation is a no-op on out-of-range inputs.
    #[test]
    fn rotate_band_moves_content_and_blanks_exposed_rows() {
        let mut g = labeled(4, &["aa", "bb", "cc"]);
        g.rotate_band(0, 3, BandRotation::Down, 1);
        assert_eq!(g.get(0, 0).unwrap().symbol, " ");
        assert_eq!(g.get(0, 1).unwrap().symbol, "a");
        assert_eq!(g.get(0, 2).unwrap().symbol, "b");

        let mut g2 = labeled(4, &["aa", "bb", "cc"]);
        g2.rotate_band(0, 3, BandRotation::Up, 1);
        assert_eq!(g2.get(0, 0).unwrap().symbol, "b");
        assert_eq!(g2.get(0, 1).unwrap().symbol, "c");
        assert_eq!(g2.get(0, 2).unwrap().symbol, " ");

        // Degenerate inputs are no-ops.
        let mut g3 = labeled(4, &["aa"]);
        g3.rotate_band(0, 3, BandRotation::Up, 0);
        g3.rotate_band(0, 3, BandRotation::Up, 3);
        g3.rotate_band(9, 3, BandRotation::Up, 1);
        assert_eq!(g3.get(0, 0).unwrap().symbol, "a");
    }

    fn grid(text: &str, w: u16, h: u16) -> Grid {
        let mut g = Grid::new(w, h);
        g.put(0, 0, crate::grid::Fit::Clip, Style::default(), text);
        g.clear_dirty();
        g
    }

    #[test]
    fn identical_grids_emit_nothing() {
        let back = grid("abc", 4, 1);
        let mut front = grid("abc", 4, 1);
        let cmd = diff(&back, &front);
        assert!(cmd.draws.is_empty());
        promote(&mut Grid::new(4, 1), &mut front); // no-op smoke
    }

    #[test]
    fn single_cell_change_emits_one_run() {
        let mut back = grid("abc", 4, 1);
        // Change 'b' to 'B'.
        back.set(1, 0, Cell::narrow("B", Style::default()));
        let front = grid("abc", 4, 1);

        let cmd = diff(&back, &front);
        assert_eq!(cmd.draws.len(), 1);
        match &cmd.draws[0] {
            Draw::Cells { x, y, cells, .. } => {
                assert_eq!(*x, 1);
                assert_eq!(*y, 0);
                assert_eq!(cells, &vec![("B".into(), 1)]);
            }
            other => panic!("expected Cells, got {other:?}"),
        }
    }

    #[test]
    fn run_breaks_on_style_change() {
        let mut back = grid("abcd", 4, 1);
        // Restyle 'c' (col 2) with a different fg.
        back.set(
            2,
            0,
            Cell::narrow("c", Style::default().fg(Color::Rgb(1, 1, 1))),
        );
        // And change 'd' too with the same new style → same run as 'c'.
        back.set(
            3,
            0,
            Cell::narrow("D", Style::default().fg(Color::Rgb(1, 1, 1))),
        );
        let front = grid("abcd", 4, 1);

        let cmd = diff(&back, &front);
        // One run: cols 2..4, uniform style.
        assert_eq!(cmd.draws.len(), 1);
    }

    #[test]
    fn wide_glyph_head_emitted_continuation_skipped() {
        let mut back = grid("", 6, 1);
        back.put(0, 0, crate::grid::Fit::Clip, Style::default(), "😀a");
        let front = Grid::new(6, 1);

        let cmd = diff(&back, &front);
        // The continuation cell at col 1 is skipped; we get one run with the
        // wide head and 'a'.
        assert_eq!(cmd.draws.len(), 1);
        match &cmd.draws[0] {
            Draw::Cells { cells, .. } => {
                assert_eq!(cells[0], ("😀".into(), 2));
                assert_eq!(cells[1], ("a".into(), 1));
                assert_eq!(cells.len(), 2);
            }
            other => panic!("expected Cells, got {other:?}"),
        }
    }

    #[test]
    fn clean_rows_outside_dirty_range_are_skipped() {
        let mut back = grid("abc", 4, 3);
        // Dirty only row 2.
        back.clear_dirty();
        back.set(0, 2, Cell::narrow("Z", Style::default()));
        let front = grid("abc", 4, 3);

        let cmd = diff(&back, &front);
        assert!(
            cmd.draws
                .iter()
                .all(|d| matches!(d, Draw::Cells { y, .. } if *y == 2))
        );
    }

    #[test]
    fn promote_syncs_front_and_clears_dirty() {
        let mut back = grid("abc", 4, 1);
        back.set(1, 0, Cell::narrow("B", Style::default()));
        let mut front = grid("abc", 4, 1);

        promote(&mut back, &mut front);
        assert_eq!(front.get(1, 0).unwrap().symbol, "B");
        assert!(!back.is_dirty());
        // A second diff against the promoted front is now empty.
        assert!(diff(&back, &front).draws.is_empty());
    }

    #[test]
    fn wide_glyph_selection_toggle_diffs_head_only() {
        use crate::grid::Fit;
        let panel = Color::Rgb(18, 19, 19);
        let sel = Color::Rgb(38, 48, 44);
        let w = 6u16;
        // front: wide glyph UNselected + panel tail
        let mut front = Grid::new(w, 1);
        front.put(
            0,
            0,
            Fit::Clip,
            Style::default().bg(panel).fg(Color::White),
            "中",
        );
        for x in 2..w {
            front.set(x, 0, Cell::blank_styled(Style::default().bg(panel)));
        }
        front.clear_dirty();
        // back: wide glyph SELECTED + panel tail
        let mut back = Grid::new(w, 1);
        back.put(
            0,
            0,
            Fit::Clip,
            Style::default().bg(sel).fg(Color::White),
            "中",
        );
        for x in 2..w {
            back.set(x, 0, Cell::blank_styled(Style::default().bg(panel)));
        }
        let cmd = diff(&back, &front);
        // The wide head must be emitted with the SELECTED bg.
        assert!(cmd.draws.iter().any(|d| matches!(d,
                Draw::Cells { style, cells, .. }
                if style.bg == sel && cells.iter().any(|(s, _)| s == "中"))));
        // No ClearEol may clobber the wide glyph's columns (0..2).
        for d in &cmd.draws {
            if let Draw::ClearEol { x, .. } = d {
                assert!(*x >= 2, "ClearEol at x={x} would clobber the wide glyph");
            }
        }
    }
}
