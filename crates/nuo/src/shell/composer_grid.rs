//! The composer's visual text grid — the single authority for how a draft maps
//! onto the rows the input box paints.
//!
//! The composer draws **wrapped** rows, so every caret question ("which row is
//! the caret on?", "what is one row up?", "where does the caret paint?") is a
//! question about this grid, never about the buffer's `\n`-separated lines.
//! Keeping the whole model here is what makes its projections provably
//! consistent, because they are all derived from one row list:
//!
//! | projection | direction |
//! |---|---|
//! | [`TextGrid::new`] | buffer + width → rows |
//! | [`TextGrid::cell`] | caret byte → (row, display column) |
//! | [`TextGrid::byte_at`] | (row, display column) → caret byte |
//! | [`TextGrid::shift_row`] | the ↑/↓ motion those two compose |
//!
//! ## The two invariants this module exists to hold
//!
//! 1. **A row's byte range is closed: `start..=end`.** That is the set of caret
//!    positions the row can display. A soft-wrap seam (`end == next.start`)
//!    belongs to the *next* row, because that is the cell the caret glyph paints
//!    in. Resolving with a half-open `start..end` made zero-width rows
//!    unreachable: an empty line was painted but no caret byte could ever map
//!    onto it, so ↑/↓ walked straight past it (the "Ctrl-J leaves an empty line
//!    the arrows cannot visit" defect).
//! 2. **Rows are a pure function of (text, width).** The caret projects onto
//!    the grid; it never feeds back into it. The old model appended a synthetic
//!    "caret row" only while the caret sat past the last row, which made the
//!    box grow/shrink with the caret, made an empty trailing line vanish the
//!    moment the caret left it, and made layout depend on caret history.
//!
//! ## Sticky column
//!
//! ↑/↓ remember the display column they aim for ([`CaretMotion`]) instead of
//! walking the caret's *clamped* column, so passing through a short or empty
//! line and back out restores the original column — the behaviour every editor
//! has, and the only way ↑/↓ can stay predictable across rows of different
//! widths. The memory is anchored to the caret byte it produced, so any other
//! edit or horizontal move invalidates it without a reset call anywhere.
use nuotc::cursor_column;
use unicode_segmentation::UnicodeSegmentation;

use crate::shell::text_layout::{WrappedLine, wrap_text};

/// The direction a vertical caret move walks the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vertical {
    Up,
    Down,
}

/// The caret's vertical-motion memory: the display column ↑/↓ aim for.
///
/// Anchored to the caret byte the last vertical move produced. Honouring the
/// memory only while the caret still sits exactly there makes it
/// self-invalidating — every other edit and horizontal motion moves the byte
/// and therefore discards the goal, with no reset bookkeeping at the call
/// sites.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaretMotion {
    anchor: Option<(usize, usize)>,
}

impl CaretMotion {
    /// The column a vertical move from `byte` should aim for: the remembered
    /// goal while the caret is still where the last vertical move left it,
    /// otherwise the caret's own column at `byte`.
    fn goal(&self, byte: usize, column: usize) -> usize {
        match self.anchor {
            Some((at, goal)) if at == byte => goal,
            _ => column,
        }
    }

    /// Record a completed vertical move: `byte` is the caret it landed on,
    /// `goal` the column it aimed for (which may be wider than that row).
    fn arrive(&mut self, byte: usize, goal: usize) {
        self.anchor = Some((byte, goal));
    }
}

/// The wrapped rows of a composer draft, plus the byte ↔ cell projections
/// between the buffer and that grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextGrid {
    rows: Vec<WrappedLine>,
}

impl TextGrid {
    /// Build the grid for `input` laid out at `width` display columns.
    ///
    /// The grid always holds **one row per visual line**, including the empty
    /// lines a newline creates and the empty last line a trailing newline
    /// leaves behind. `wrap_text` emits a row *before* each `\n`, so the
    /// buffer's final line has to be added here; without it an empty last line
    /// (the one Ctrl-J creates) would exist in the buffer but never on screen.
    pub fn new(input: &str, width: usize) -> Self {
        let mut rows = wrap_text(input, width.max(1));
        let tail_missing = rows
            .last()
            .is_none_or(|row| row.end_byte < input.len() || input.ends_with('\n'));
        if tail_missing {
            rows.push(WrappedLine {
                text: String::new(),
                start_byte: input.rfind('\n').map_or(0, |idx| idx + 1),
                end_byte: input.len(),
            });
        }
        Self { rows }
    }

    /// Build the grid without wrapping: one row per logical line. This is the
    /// model before any frame has published the composer's geometry (the very
    /// first key of a session), and the one single-line fields resolve against.
    pub fn unwrapped(input: &str) -> Self {
        Self::new(input, usize::MAX)
    }

    /// The rows to paint, top to bottom. Never empty: an empty draft is one
    /// empty row.
    pub fn rows(&self) -> &[WrappedLine] {
        &self.rows
    }

    /// Number of visual rows — at least one, always (an empty draft is a row).
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The last byte offset the grid covers (the buffer length).
    fn end_byte(&self) -> usize {
        self.rows.last().map_or(0, |row| row.end_byte)
    }

    /// The grid cell the caret at `byte` occupies: `(row, display column)`.
    ///
    /// A byte belongs to the first row whose closed range `start..=end`
    /// contains it, except at a soft-wrap seam (`end == next.start`), which
    /// belongs to the next row — that is the cell the caret paints in, and it is
    /// what keeps the completion popup's leading edge on the continuation row.
    /// Zero-width rows (empty lines) therefore claim their single byte instead
    /// of being skipped.
    pub fn cell(&self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.end_byte());
        for (row, line) in self.rows.iter().enumerate() {
            if byte > line.end_byte {
                continue;
            }
            let seam = byte == line.end_byte
                && self
                    .rows
                    .get(row + 1)
                    .is_some_and(|next| next.start_byte == byte);
            if seam {
                continue;
            }
            let local = byte.saturating_sub(line.start_byte).min(line.text.len());
            return (row, cursor_column(&line.text, local));
        }
        let row = self.rows.len().saturating_sub(1);
        let line = &self.rows[row];
        (row, cursor_column(&line.text, line.text.len()))
    }

    /// The caret byte for display `column` of `row`.
    ///
    /// The column is clamped to the row's width and snaps left out of the middle
    /// of a wide glyph, so the result is always a grapheme boundary inside the
    /// row — never a byte borrowed from the row below.
    pub fn byte_at(&self, row: usize, column: usize) -> usize {
        let Some(line) = self.rows.get(row) else {
            return self.end_byte();
        };
        let mut offset = 0;
        for (idx, grapheme) in line.text.grapheme_indices(true) {
            let end = idx + grapheme.len();
            if cursor_column(&line.text, end) > column {
                break;
            }
            offset = end;
        }
        (line.start_byte + offset).min(line.end_byte)
    }

    /// Move the caret one visual row up or down, honouring the sticky column.
    ///
    /// `None` means the caret already sits on the grid's top (or bottom) row:
    /// the caller owns what happens at that edge — the chat surface hands off to
    /// inline history recall, a selection extension clamps.
    pub fn shift_row(
        &self,
        byte: usize,
        vertical: Vertical,
        motion: &mut CaretMotion,
    ) -> Option<usize> {
        let (row, column) = self.cell(byte);
        let goal = motion.goal(byte, column);
        let target = match vertical {
            Vertical::Up => row.checked_sub(1)?,
            Vertical::Down => row.checked_add(1)?,
        };
        self.rows.get(target)?;
        let landed = self.byte_at(target, goal);
        motion.arrive(landed, goal);
        Some(landed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(input: &str, width: usize) -> TextGrid {
        TextGrid::new(input, width)
    }

    /// The regression this module was written for: an empty line created by
    /// Ctrl-J is a caret-addressable row, both in the middle of a draft and at
    /// its end.
    #[test]
    fn empty_rows_are_caret_addressable() {
        // Ctrl-J with the caret at the end of the first line of "a\nb".
        let g = grid("a\n\nb", 40);
        assert_eq!(g.row_count(), 3);
        assert_eq!(g.cell(2), (1, 0), "the created empty line owns its row");
        assert_eq!(g.cell(1), (0, 1), "end of line 1 stays on line 1");
        assert_eq!(g.cell(3), (2, 0));

        // Trailing newline: the empty last line is a row, whether or not the
        // caret rests on it.
        let g = grid("abc\n", 40);
        assert_eq!(g.row_count(), 2);
        assert_eq!(g.cell(3), (0, 3));
        assert_eq!(g.cell(4), (1, 0));

        // Two Ctrl-J presses leave two empty rows, both addressable.
        let g = grid("abc\n\n", 40);
        assert_eq!(g.row_count(), 3);
        assert_eq!(g.cell(4), (1, 0));
        assert_eq!(g.cell(5), (2, 0));
    }

    #[test]
    fn rows_are_a_pure_function_of_text_and_width() {
        // No caret argument exists any more: the row list cannot drift with the
        // caret, so the box never grows/shrinks as ↑/↓ moves.
        assert_eq!(grid("abc\n", 40), grid("abc\n", 40));
        assert_eq!(
            grid("", 40).row_count(),
            1,
            "an empty draft is one empty row"
        );
        assert_eq!(
            grid("abc", 40).row_count(),
            1,
            "no trailing row without a newline"
        );
        assert_eq!(grid("\n", 40).row_count(), 2);
    }

    #[test]
    fn soft_wrap_seam_belongs_to_the_continuation_row() {
        // 12 columns at width 10: the seam byte is the first cell of row 1.
        let g = grid("abcdefghijkl", 10);
        assert_eq!(g.row_count(), 2);
        assert_eq!(g.cell(10), (1, 0));
        assert_eq!(g.cell(9), (0, 9));
        assert_eq!(g.cell(12), (1, 2));
        // A line that exactly fills the width still yields one row, and its
        // end-of-line caret stays on it.
        let g = grid("abcdefghij", 10);
        assert_eq!(g.row_count(), 1);
        assert_eq!(g.cell(10), (0, 10));
    }

    #[test]
    fn vertical_walk_visits_every_visual_row() {
        let g = grid("a\n\nb", 40);
        let mut motion = CaretMotion::default();
        // Down from the top walks a -> empty -> b, one row at a time.
        let byte = g.shift_row(0, Vertical::Down, &mut motion).unwrap();
        assert_eq!(byte, 2, "down must stop on the empty line");
        let byte = g.shift_row(byte, Vertical::Down, &mut motion).unwrap();
        assert_eq!(byte, 3);
        assert_eq!(g.shift_row(byte, Vertical::Down, &mut motion), None);

        // Up from the bottom walks back through the same empty row,
        // keeping the display column across the empty line's zero width.
        let byte = g.shift_row(4, Vertical::Up, &mut motion).unwrap();
        assert_eq!(byte, 2, "up from 'b' stops on the empty line");
        let byte = g.shift_row(byte, Vertical::Up, &mut motion).unwrap();
        assert_eq!(byte, 1, "and keeps column 1 on the first line");
        assert_eq!(g.shift_row(byte, Vertical::Up, &mut motion), None);
    }

    #[test]
    fn vertical_walk_keeps_the_column_across_short_rows() {
        // The sticky column: aim at column 11, pass through a 1-wide row, and
        // come back out at column 11 instead of the clamped 1.
        let g = grid("longer line\nx\nmore text", 40);
        assert_eq!(g.cell(11), (0, 11));
        let mut motion = CaretMotion::default();
        let mid = g.shift_row(11, Vertical::Down, &mut motion).unwrap();
        assert_eq!(mid, 13, "the short row clamps the caret to its end");
        let out = g.shift_row(mid, Vertical::Down, &mut motion).unwrap();
        assert_eq!(out, 23, "the goal column, not the clamped one, is restored");
        assert_eq!(g.cell(out), (2, 9));
    }

    #[test]
    fn horizontal_motion_invalidates_the_goal_column() {
        let g = grid("longer line\nx\nmore text", 40);
        let mut motion = CaretMotion::default();
        assert_eq!(g.shift_row(11, Vertical::Down, &mut motion), Some(13));
        // The caret now sits somewhere the last vertical move did not leave it,
        // so the next vertical move aims at that caret's own column (0) instead
        // of the remembered 11.
        assert_eq!(g.shift_row(12, Vertical::Down, &mut motion), Some(14));
    }

    #[test]
    fn byte_at_clamps_wide_glyphs_to_their_start() {
        // "中文" is 4 display columns; column 1 falls inside the first glyph.
        let g = grid("中文", 40);
        assert_eq!(g.byte_at(0, 0), 0);
        assert_eq!(g.byte_at(0, 1), 0, "never the middle of a wide glyph");
        assert_eq!(g.byte_at(0, 2), 3);
        assert_eq!(g.byte_at(0, 99), 6, "clamped to the row's end");
    }

    #[test]
    fn cell_is_total_for_out_of_range_bytes() {
        let g = grid("abc", 40);
        assert_eq!(g.cell(99), (0, 3));
        assert_eq!(g.byte_at(99, 0), 3);
    }
}
