//! Keyboard-ownership presentation for the transcript pane (ADR-0145).
//!
//! The Thread scene hosts two **peer** planes and exactly one owns the keyboard.
//! The composer has always said so in the background channel — a lit
//! `input_surface()` panel against a recessed `input_surface_inactive()` one.
//! The transcript said so only in the foreground channel, and only *sometimes*:
//! a hue tint on one summary line, suppressed entirely on an expanded step. The
//! crossing therefore had one visible half.
//!
//! This module owns the transcript's half, so it is stated once and every
//! sub-state agrees:
//!
//! | State | Cue |
//! | --- | --- |
//! | `FocusPlane::Composer` | nothing — the composer's lit panel is the cue |
//! | `FocusPlane::Transcript` (browse) | the **pane rail** on the transcript's left gutter |
//! | `FocusPlane::Transcript` (entry) | the rail *plus* the focused entry's **band** |
//!
//! The rail is what makes the crossing itself visible: `Shift-Tab` lands on
//! *browse* (no entry highlighted), so an entry-only cue would leave the primary
//! gesture with nothing to show for it.

use nuotc::{Frame, Rect, Style};

use nuo_ui_primitives::theme::Theme;

/// Which peer plane owns the keyboard.
///
/// The shell owns the authoritative [`Focus`](../focus) value; this is its
/// paint-time projection, because a presentation substrate must not depend on
/// the shell's interaction state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusPlane {
    /// The composer owns the keyboard: keystrokes land in the draft.
    #[default]
    Composer,
    /// The transcript owns the keyboard: keystrokes inspect the stream.
    Transcript,
}

impl FocusPlane {
    pub const fn is_transcript(self) -> bool {
        matches!(self, FocusPlane::Transcript)
    }
}

/// Paint the transcript pane's **focus rail**: the left and right gutter
/// columns ([`TRANSCRIPT_H_INSET`] each) in [`Theme::entry_focus_bg`], spanning
/// the transcript viewport.
///
/// Zero layout cost by construction: `band` is the already-inset content rect
/// inside `outer`, so the columns it paints are gutters nothing else touches.
/// That is exactly why the cue lives there — a focus indicator must never move
/// content, or the pane reflows every time the keyboard crosses.
///
/// **Both** gutters are filled, not just the one adjacent to the text. A single
/// tinted column reads as a stray artifact; framing the pane on both sides reads
/// as a surface that changed state — the same visual weight the composer's lit
/// panel carries on the other plane, so the crossing looks like a swap.
///
/// Rows outside the viewport are skipped so the rail can never bleed into the
/// footer stack.
pub fn draw_transcript_focus_rail(
    frame: &mut Frame<'_>,
    outer: Rect,
    band: Rect,
    plane: FocusPlane,
    theme: &Theme,
) {
    if !plane.is_transcript() || band.is_empty() || band.width == 0 {
        return;
    }
    let style = theme.entry_focus_rail_style();
    // Left gutter `[outer.x, band.x)` and right gutter `[band.right(), outer.right())`.
    // Derived from the two rects rather than from the inset token, so the rail
    // tracks whatever inset the layout actually applied.
    let left_end = band.x.min(outer.right());
    let right_start = band.right().min(outer.right());
    for y in band.y..band.bottom() {
        for x in outer.x..left_end {
            frame.put(x, y, style, " ");
        }
        for x in right_start..outer.right() {
            frame.put(x, y, style, " ");
        }
    }
}

/// The focus rail's cell style, exposed so the rail and the focused entry's
/// band can be asserted to be the same visual family (and so a test can paint
/// the rail without going through a full [`Frame`]).
pub fn focus_rail_style(theme: &Theme) -> Style {
    theme.entry_focus_rail_style()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint the rail into a 20x8 terminal and hand back the buffer.
    fn paint_rail(plane: FocusPlane, outer: Rect, band: Rect) -> (nuotc::TestTerminal, Theme) {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            draw_transcript_focus_rail(f, outer, band, plane, &theme);
        });
        (terminal, theme)
    }

    const OUTER: Rect = Rect::new(0, 1, 20, 4);
    const BAND: Rect = Rect::new(2, 1, 16, 4);

    /// The rail exists only while the transcript owns the keyboard, and it is
    /// painted in the gutters — never inside the content band, so the pane can
    /// never reflow because the keyboard crossed.
    #[test]
    fn rail_marks_the_transcript_plane_in_the_gutters() {
        let (terminal, theme) = paint_rail(FocusPlane::Transcript, OUTER, BAND);
        for x in [0, 1, 18, 19] {
            assert_eq!(
                terminal.buffer().get(x, 2).unwrap().style,
                theme.entry_focus_rail_style(),
                "gutter column {x} carries the rail"
            );
        }
        assert_ne!(
            terminal.buffer().get(2, 2).unwrap().style,
            theme.entry_focus_rail_style(),
            "the content band itself is never tinted by the rail"
        );
    }

    /// Blurring the transcript removes the rail completely: the rail must be a
    /// *state*, not a permanent pane decoration.
    #[test]
    fn rail_absent_on_the_composer_plane() {
        let (terminal, theme) = paint_rail(FocusPlane::Composer, OUTER, BAND);
        for x in [0, 1, 18, 19] {
            assert_ne!(
                terminal.buffer().get(x, 2).unwrap().style,
                theme.entry_focus_rail_style(),
                "a composer-owned pane must not paint the rail"
            );
        }
    }

    /// Framing **both** gutters is the point: one tinted column reads as a stray
    /// artifact, a framed pane reads as a surface that changed state — the same
    /// weight the composer's lit panel carries on the other plane.
    #[test]
    fn rail_frames_the_pane_on_both_sides() {
        let (terminal, theme) = paint_rail(FocusPlane::Transcript, OUTER, BAND);
        let tinted = |y: u16| {
            (0..20)
                .filter(|&x| {
                    terminal.buffer().get(x, y).unwrap().style == theme.entry_focus_rail_style()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(tinted(2), vec![0, 1, 18, 19]);
    }

    /// A band flush with the outer rect has no gutter to paint into; the rail
    /// must degrade to nothing rather than wrap to the far column (which would
    /// put a focus cue on the pane's content edge).
    #[test]
    fn rail_degrades_without_a_gutter() {
        let (terminal, theme) = paint_rail(
            FocusPlane::Transcript,
            Rect::new(0, 0, 20, 4),
            Rect::new(0, 0, 20, 4),
        );
        for y in 0..4 {
            for x in 0..20 {
                assert_ne!(
                    terminal.buffer().get(x, y).unwrap().style,
                    theme.entry_focus_rail_style(),
                    "no rail without a gutter column"
                );
            }
        }
    }

    /// The rail spans the viewport and stops at its bottom edge — the footer
    /// stack below it belongs to the composer plane.
    #[test]
    fn rail_spans_exactly_the_viewport() {
        let (terminal, theme) = paint_rail(
            FocusPlane::Transcript,
            Rect::new(0, 2, 20, 6),
            Rect::new(2, 2, 16, 3),
        );
        for y in 2..5 {
            assert_eq!(
                terminal.buffer().get(0, y).unwrap().style,
                theme.entry_focus_rail_style(),
                "row {y} inside the viewport carries the rail"
            );
        }
        for y in [0, 1, 5, 6, 7] {
            assert_ne!(
                terminal.buffer().get(0, y).unwrap().style,
                theme.entry_focus_rail_style(),
                "row {y} outside the viewport must not carry the rail"
            );
        }
    }
}
