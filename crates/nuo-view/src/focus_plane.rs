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

use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Rect};

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
/// `row`: one solid cell in the entry's reserved left gutter.
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
    frame.put(band.x - 1, row, style, " ");
}

/// The marker cell's style, exposed so a test can assert the painted cell without
/// re-deriving it (and so a caller can pre-compose a row that already owns the
/// column).
pub fn entry_marker_style(theme: &Theme) -> nuotc::Style {
    theme.pane_marker_style()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTER: Rect = Rect::new(0, 1, 20, 4);
    const BAND: Rect = Rect::new(2, 1, 16, 4);

    fn lum(color: nuotc::Color) -> f32 {
        match color {
            nuotc::Color::Rgb(r, g, b) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32,
            _ => 0.0,
        }
    }

    /// The theme a renderer paints with, for a given plane.
    fn renderer_theme(theme: &Theme, plane: FocusPlane) -> Theme {
        transcript_theme(theme, plane)
    }

    /// The pane cue reaches **every** column of the page, not just its edges.
    ///
    /// This is the assertion the earlier rail design failed: the cue lived in the two
    /// gutter columns and the interior — every column the reader is looking at — was
    /// untouched.
    #[test]
    fn the_pane_cue_lifts_the_whole_page() {
        let theme = Theme::default();
        let focused = renderer_theme(&theme, FocusPlane::Transcript);
        assert_ne!(
            focused.surface(),
            theme.surface(),
            "the pane's page must move while it owns the keyboard"
        );
        // The cue is a property of the surface, so it applies at every column by
        // construction — there is no column that "is not the pane".
        assert_eq!(
            focused.surface(),
            theme.pane_focus(),
            "the lifted page is exactly the derived wash tone"
        );
    }

    /// Blurring the transcript returns the page exactly: the cue is a *state*.
    #[test]
    fn the_composer_plane_owns_the_unlifted_page() {
        let theme = Theme::default();
        let blurred = renderer_theme(&theme, FocusPlane::Composer);
        assert_eq!(blurred.surface(), theme.surface());
    }

    /// Every band inside the pane keeps its **exact** separation from the page.
    ///
    /// The regression this locks: an implementation that only repainted the cells on
    /// the page tone preserved each band's absolute colour — which reads as correct in
    /// a unit test — while the page rose to meet them, eating more than half of a code
    /// band's elevation. A translation is the only thing that preserves the ladder.
    #[test]
    fn the_lift_preserves_every_relative_step_inside_the_pane() {
        let theme = Theme::default();
        let focused = renderer_theme(&theme, FocusPlane::Transcript);
        for band in theme.pane_bands() {
            let before = lum(band) - lum(theme.surface());
            // Find the corresponding band on the lifted theme: it is the same token,
            // shifted. `pane_lifted` moves the whole background half, so the mapping is
            // by construction, and the *relative* value is what must hold.
            let after = lum(shift_by_lift(band, &theme)) - lum(focused.surface());
            assert!(
                (after - before).abs() < 1.0,
                "a band moved relative to the page: {before:.1} -> {after:.1}"
            );
        }
    }

    /// Shift one colour by the theme's own lift — the test-side mirror of
    /// `Theme::pane_lifted`, so the assertion cannot accidentally agree with a bug by
    /// re-using the same code path.
    fn shift_by_lift(color: nuotc::Color, theme: &Theme) -> nuotc::Color {
        let Some(delta) = theme.pane_focus_delta() else {
            return color;
        };
        match color {
            nuotc::Color::Rgb(r, g, b) => {
                let f = |c: u8, d: i16| (c as i16 + d).clamp(0, 255) as u8;
                nuotc::Color::Rgb(f(r, delta[0]), f(g, delta[1]), f(b, delta[2]))
            }
            other => other,
        }
    }

    /// The plane question must be asked of the **base** theme, never the lifted one.
    ///
    /// This is the double-lift regression, and it is subtle: on a lifted theme `app_bg`
    /// already *is* the wash, so `pane_focus_delta()` reads zero and `has_pane_wash()`
    /// reports `false`. A caller that hands the lifted theme back to `draw_pane_focus`
    /// therefore gets the gutter fallback painted **on top of an already-lifted page** —
    /// the pane ends up with the wrong cue, and the bug is invisible on a palette whose
    /// headroom happens to be large.
    #[test]
    fn the_plane_question_is_asked_of_the_base_theme() {
        let base = Theme::default();
        assert!(base.has_pane_wash());

        let lifted = transcript_theme(&base, FocusPlane::Transcript);
        assert_eq!(
            lifted.surface(),
            base.pane_focus(),
            "the lifted theme's page *is* the wash"
        );
        assert!(
            !lifted.has_pane_wash(),
            "asking the lifted theme reports no headroom — which is exactly why it must \
             not be asked"
        );
        // Consequently the lifted theme would take the gutter path…
        assert!(lifted.pane_focus_delta().is_none());
        // …so a correct caller passes `base`, and the post-pass stays inert.
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            f.render_widget(
                &mut nuotc::Block::default().style(nuotc::Style::default().bg(lifted.surface())),
                OUTER,
            );
            draw_pane_focus(f, OUTER, BAND, FocusPlane::Transcript, &base);
        });
        for y in OUTER.y..OUTER.bottom() {
            for x in OUTER.x..OUTER.right() {
                assert_eq!(
                    terminal.buffer().get(x, y).unwrap().style.bg,
                    lifted.surface(),
                    "({x},{y}) was over-painted by a fallback that should not have run"
                );
            }
        }
    }

    /// The post-pass is a **no-op** wherever the theme already carries the cue — the
    /// whole point of moving it to the source.
    #[test]
    fn the_post_pass_touches_nothing_when_the_theme_carries_the_cue() {
        let theme = Theme::default();
        assert!(theme.has_pane_wash());
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            f.render_widget(
                &mut nuotc::Block::default().style(nuotc::Style::default().bg(theme.surface())),
                OUTER,
            );
            draw_pane_focus(f, OUTER, BAND, FocusPlane::Transcript, &theme);
        });
        for y in OUTER.y..OUTER.bottom() {
            for x in OUTER.x..OUTER.right() {
                assert_eq!(
                    terminal.buffer().get(x, y).unwrap().style.bg,
                    theme.surface(),
                    "({x},{y}) was restyled by a post-pass that should be inert"
                );
            }
        }
    }

    /// A palette that cannot lift its page falls back to the gutter, which is the one
    /// place an edge cue survives — and it survives for a structural reason, not an
    /// aesthetic one.
    #[test]
    fn reset_surfaces_fall_back_to_the_gutter() {
        let theme = Theme::ansi16();
        assert!(!theme.has_pane_wash(), "ANSI-16 owns no page tone to lift");
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            draw_pane_focus(f, OUTER, BAND, FocusPlane::Transcript, &theme);
        });

        let edge = theme.pane_focus_edge_style();
        for x in [0, 1, 18, 19] {
            assert_eq!(
                terminal.buffer().get(x, 2).unwrap().style,
                edge,
                "gutter column {x} carries the fallback cue"
            );
        }
        // Content columns are untouched: the fallback is the largest region these
        // profiles can afford, not a licence to tint the text.
        for x in BAND.x..BAND.right() {
            assert_ne!(
                terminal.buffer().get(x, 2).unwrap().style,
                edge,
                "the content band itself is never tinted"
            );
        }
    }

    /// The fallback still belongs to the transcript plane alone.
    #[test]
    fn reset_surface_fallback_is_plane_scoped() {
        let theme = Theme::ansi16();
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            draw_pane_focus(f, OUTER, BAND, FocusPlane::Composer, &theme);
        });
        assert_ne!(theme.pane_focus_edge_style(), nuotc::Style::default());
        for x in [0, 1, 18, 19] {
            assert_ne!(
                terminal.buffer().get(x, 2).unwrap().style,
                theme.pane_focus_edge_style(),
                "a composer-owned pane must not paint the fallback"
            );
        }
    }

    /// The fallback spans the content rows and stops — the footer below belongs to the
    /// composer plane.
    #[test]
    fn fallback_spans_exactly_the_viewport() {
        let theme = Theme::ansi16();
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            draw_pane_focus(
                f,
                Rect::new(0, 2, 20, 6),
                Rect::new(2, 2, 16, 3),
                FocusPlane::Transcript,
                &theme,
            );
        });
        let edge = theme.pane_focus_edge_style();
        for y in 2..5 {
            assert_eq!(
                terminal.buffer().get(0, y).unwrap().style,
                edge,
                "row {y} inside the viewport carries the fallback"
            );
        }
        for y in [0, 1, 5, 6, 7] {
            assert_ne!(
                terminal.buffer().get(0, y).unwrap().style,
                edge,
                "row {y} outside the viewport must not carry it"
            );
        }
    }

    /// VT100 has no color, so the fallback is reverse video — the archetype's only
    /// background vocabulary (ADR-0053). The cue must still exist.
    #[test]
    fn structured_fallback_is_reverse_video() {
        let theme = Theme::monochrome();
        assert!(!theme.has_pane_wash());
        assert!(
            theme
                .pane_focus_edge_style()
                .add
                .contains(nuotc::Modifier::REVERSE)
        );
    }

    /// The marker lives in the reserved gutter — one cell, immediately beside the
    /// content, and never inside the content band.
    #[test]
    fn marker_sits_in_the_gutter_beside_the_row() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            draw_entry_marker(f, OUTER, BAND, 2, &theme);
        });
        let cell = terminal.buffer().get(BAND.x - 1, 2).unwrap();
        assert_eq!(cell.style, theme.pane_marker_style());
        for x in BAND.x..BAND.right() {
            assert_ne!(
                terminal.buffer().get(x, 2).unwrap().style,
                theme.pane_marker_style(),
                "column {x} is content: the marker must not cover it"
            );
        }
    }

    /// A band flush with the outer rect has no gutter to mark into; the marker
    /// must degrade to nothing rather than wrap to the far column (which would put
    /// a focus cue on the pane's content edge).
    #[test]
    fn marker_degrades_without_a_gutter() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(20, 8);
        terminal.draw(|f| {
            draw_entry_marker(f, Rect::new(0, 0, 20, 4), Rect::new(0, 0, 20, 4), 2, &theme);
        });
        for x in 0..20 {
            assert_ne!(
                terminal.buffer().get(x, 2).unwrap().style,
                theme.pane_marker_style(),
                "no gutter, no marker"
            );
        }
    }
}

#[cfg(test)]
mod cost {
    use super::*;

    /// The pane cue must not cost a per-cell pass over the viewport.
    ///
    /// The first implementation restyled every cell of the transcript pane after the
    /// stream — measured at 5.8 ms on a 400x120 pane, thirteen times the cost of
    /// painting the whole frame, spent on a focus indicator. The cue now arrives
    /// through [`transcript_theme`], so the marginal cost is one theme copy on a
    /// palette that can carry the wash and nothing at all on one that cannot.
    ///
    /// This test pins the *shape* of the fix, not a wall-clock number: it asserts the
    /// per-cell path is gone, which is the property that actually protects the frame
    /// budget.
    #[test]
    fn the_pane_cue_does_not_restyle_the_viewport() {
        let theme = Theme::default();
        let (w, h) = (400u16, 120u16);
        let area = Rect::new(0, 0, w, h);
        let band = Rect::new(2, 0, w - 4, h);

        // A wash-capable palette: the post-pass must be a no-op, because the cue
        // already arrived through the theme.
        let mut terminal = nuotc::TestTerminal::new(w, h);
        let baseline = theme.surface();
        terminal.draw(|f| {
            f.render_widget(
                &mut nuotc::Block::default().style(nuotc::Style::default().bg(baseline)),
                area,
            );
            draw_pane_focus(f, area, band, FocusPlane::Transcript, &theme);
        });
        assert_eq!(
            terminal.buffer().get(w / 2, h / 2).unwrap().style.bg,
            baseline,
            "on a wash-capable palette the theme already carries the cue; the \
             post-pass must touch nothing"
        );

        // The theme path is where the cue comes from, and it is O(1).
        let lifted = transcript_theme(&theme, FocusPlane::Transcript);
        assert_ne!(lifted.surface(), theme.surface());
        assert_eq!(
            transcript_theme(&theme, FocusPlane::Composer).surface(),
            theme.surface(),
            "the composer plane gets the page back unchanged"
        );
    }

    /// The fallback still owns the viewport edge on a palette that cannot lift, and it
    /// is bounded to a few columns rather than the whole pane.
    #[test]
    fn the_fallback_costs_only_the_gutters() {
        let theme = Theme::ansi16();
        assert!(!theme.has_pane_wash());
        let (w, h) = (400u16, 120u16);
        let mut terminal = nuotc::TestTerminal::new(w, h);
        let area = Rect::new(0, 0, w, h);
        let band = Rect::new(2, 0, w - 4, h);
        terminal.draw(|f| {
            draw_pane_focus(f, area, band, FocusPlane::Transcript, &theme);
        });
        let style = theme.pane_focus_edge_style();
        let touched = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|(x, y)| terminal.buffer().get(*x, *y).unwrap().style == style)
            .count();
        assert_eq!(
            touched,
            2 * 2 * h as usize,
            "the fallback is the two gutter columns, not the viewport"
        );
    }
}
