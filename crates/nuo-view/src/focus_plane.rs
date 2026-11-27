//! Keyboard-ownership presentation for the transcript pane (ADR-0145).
//!
//! The Thread scene hosts two **peer** planes and exactly one owns the keyboard.
//! The composer has always said so in the background channel — a lit
//! `input_surface()` panel against a recessed `input_surface_inactive()` one.
//! The transcript said so only in the foreground channel, and only *sometimes*:
//! a hue tint on one summary line, suppressed entirely on an expanded step.
//!
//! This module owns the transcript's half, and it states it as two facts at two
//! scales instead of two stripes at the edges:
//!
//! | State | Cue |
//! | --- | --- |
//! | `FocusPlane::Composer` | nothing — the composer's lit panel is the cue |
//! | `FocusPlane::Transcript` (browse) | the **pane wash**: the whole viewport lifted one quiet step |
//! | `FocusPlane::Transcript` (entry) | the wash, plus one **marker cell** in that entry's gutter |
//!
//! The wash is what makes the crossing itself visible: `Shift-Tab` lands on
//! *browse* (no entry highlighted), so an entry-only cue would leave the primary
//! gesture with nothing to show for it. Being a whole-surface state, it carries
//! the cue in *area* rather than in contrast — which is why the step is small and
//! why it composes with every tone the renderers resolved for the page instead of
//! inverting them.
//!
//! ## Why not a rail, and why not a band
//!
//! The first implementation painted the two gutter columns down the entire
//! viewport. That reads as two stray vertical bars: the cue exists at the
//! extreme left and right of the pane and is *absent* everywhere the user is
//! actually looking. The natural reading of "the keyboard is in this pane" is a
//! property of the pane's surface, not of its border — the composer does not
//! outline itself, it *is* a lit panel.
//!
//! The entry half has the mirror-image problem: a header row is the busiest
//! thing on the page (disclosure luminance × lifecycle hue × interaction), so a
//! background behind it has to invert everything the renderers resolved on
//! purpose. One marker cell in the reserved gutter says the same thing and
//! touches nothing.

use graticule_canvas::{Frame, Rect};
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

/// The theme a transcript renderer must paint with for this keyboard-ownership
/// state (ADR-0145, amended).
///
/// This is the whole-pane cue, done at the **source**. The renderers already paint
/// the page tone, the code bands, the cards and the selection *through this struct*,
/// so lifting the struct means they paint the lifted pane directly. The cue then
/// costs one struct copy per frame instead of a per-cell restyle over the viewport,
/// which measured at ~5.8 ms on a 400x120 pane — thirteen times the cost of painting
/// the entire frame, and not something to spend on a focus indicator.
///
/// The equivalence with a per-cell restyle is what makes this safe at the source at
/// all, and it is asserted by
/// `nuo_ui_primitives::theme::tests::pane_lifted_theme_matches_a_cell_wise_restyle`:
/// every **background** moves by the same delta, every **foreground** stays put, and
/// so every separation in the document survives exactly. Foregrounds are semantic —
/// a running hue or a muted rung means the same thing at either page level — which is
/// why only the background half moves.
///
/// A palette with no headroom gets the theme back unchanged; its cue comes from
/// [`draw_pane_focus`]'s gutter form, which is a handful of cells rather than a
/// viewport.
pub fn transcript_theme(theme: &Theme, plane: FocusPlane) -> Theme {
    if plane.is_transcript() && theme.has_pane_wash() {
        theme.pane_lifted()
    } else {
        *theme
    }
}

/// Draw the part of the pane cue that **cannot** come from the theme.
///
/// On a palette that can lift its page this is a no-op: the wash already arrived
/// through [`transcript_theme`], and repainting the viewport here would reintroduce
/// exactly the per-cell cost the theme path exists to avoid.
///
/// On a palette that cannot — a `Reset` page, or one with no channel headroom above
/// its own bands — it paints the **gutter columns**, the largest region those
/// profiles can afford. The reason is structural, not aesthetic: a page that *is*
/// the terminal's own background cannot be shifted, and a page with no headroom
/// cannot be shifted without flattening the bands above it.
///
/// Zero-layout either way: no cell added, no column moved, no glyph changed.
pub fn draw_pane_focus(
    frame: &mut Frame<'_>,
    area: Rect,
    band: Rect,
    plane: FocusPlane,
    theme: &Theme,
) {
    if !plane.is_transcript() || area.is_empty() || theme.has_pane_wash() {
        return;
    }
    draw_pane_focus_edges(frame, area, band, theme);
}

/// The `Reset`-surface fallback: the pane's two gutter columns in
/// [`Theme::pane_focus_edge_style`], spanning the content rows only.
///
/// It is confined to `band.y..band.bottom()` so the cue can never bleed into the
/// footer stack below the transcript — those rows belong to the composer plane.
fn draw_pane_focus_edges(frame: &mut Frame<'_>, outer: Rect, band: Rect, theme: &Theme) {
    if band.is_empty() || band.width == 0 || band.y < outer.y {
        return;
    }
    let style = theme.pane_focus_edge_style();
    // Derived from the two rects rather than from the inset token, so the cue tracks
    // whatever gutter the layout actually left. One `fill` per gutter column band
    // rather than a `put` per cell: this path runs on every frame for the profiles
    // that cannot carry a wash, so it must not be a viewport walk.
    let left_end = band.x.min(outer.right());
    let right_start = band.right().min(outer.right());
    let rows = band.y..band.bottom().min(outer.bottom());
    if rows.is_empty() {
        return;
    }
    let top = rows.start;
    let height = rows.end - rows.start;
    if left_end > outer.x {
        frame.paint(
            Rect::new(outer.x, top, left_end - outer.x, height),
            |paint| {
                paint.fill(Rect::new(outer.x, top, left_end - outer.x, height), style);
            },
        );
    }
    if outer.right() > right_start {
        frame.paint(
            Rect::new(right_start, top, outer.right() - right_start, height),
            |paint| {
                paint.fill(
                    Rect::new(right_start, top, outer.right() - right_start, height),
                    style,
                );
            },
        );
    }
}

/// Paint the **entry marker** for the highlighted entry whose header row is at
/// `row`: one vertical accent bar in the entry's reserved left gutter.
///
/// The gutter is `[outer.x, band.x)`, i.e. the columns the transcript layout
/// reserves and no content ever paints into, so the marker costs no columns and
/// cannot collide with text. `outer`/`band` are the two rects the layout already
/// resolved, so the marker tracks whatever inset was actually applied rather
/// than a design token that could drift from it.
///
/// Degrades to nothing when there is no gutter (a full-width transcript has no
/// margin to annotate) rather than wrapping onto the content edge, where it would
/// cover the first character of the row.
pub fn draw_entry_marker(frame: &mut Frame<'_>, outer: Rect, band: Rect, row: u16, theme: &Theme) {
    if band.x <= outer.x || row < band.y || row >= band.bottom() {
        return;
    }
    let style = theme.pane_marker_style();
    // Sit immediately against the content: a marker separated from what it marks
    // by a column of background reads as an unrelated artifact.
    frame.put(band.x - 1, row, style, theme.pane_marker_glyph());
}

/// The marker cell's style, exposed so a test can assert the painted cell without
/// re-deriving it (and so a caller can pre-compose a row that already owns the
/// column).
pub fn entry_marker_style(theme: &Theme) -> graticule_canvas::Style {
    theme.pane_marker_style()
}

/// The glyph used for the entry marker in the gutter.
pub fn entry_marker_glyph(theme: &Theme) -> &'static str {
    theme.pane_marker_glyph()
}

#[cfg(test)]
mod tests;
