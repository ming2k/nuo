//! Appearance settings panel: themes and palette swatches.
//!
//! ## Row grammar
//!
//! A scheme row is two visual lines: the identity line (`label` + palette
//! swatches) and, on its own line beneath, the scheme's one-line description
//! (flush from column 0). Keeping the description on a second line removes the
//! ragged mid-row wrap the single-line form produced at narrow widths, and lets
//! the identity columns align cleanly.
//!
//! State is carried by color, never by a `●`/`○` glyph:
//! - **Applied** (this scheme is live) → the label text is highlighted
//!   (`theme.brand()`, bold).
//! - **Hover / keyboard cursor** → the whole row is painted with a full-width
//!   background band derived from the *active palette* ([`Theme::row_hover_band`]),
//!   chosen so the row's own swatches stay legible against it rather than
//!   fusing into a fixed gray. The band is the same for the mouse pointer and
//!   the keyboard cursor, so both read as one affordance.

use std::path::Path;

use nuotc::{Color, Frame, Line, Modifier, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::{ScrollableRects, SettingsProps, render_scrollable_indexed};
use crate::render::Theme;

/// Column where a row's label begins. Swatches follow one space after the
/// padded label.
const LABEL_COL: usize = 22;

/// Dynamic count of selectable items in the Appearance panel for the active terminal profile.
pub fn item_count(ws_path: Option<&Path>, profile: &nuotc::TerminalProfile) -> usize {
    if profile.supports_color_themes() {
        Theme::available_color_schemes_with_workspace(ws_path)
            .len()
            .max(1)
    } else {
        1
    }
}

/// Whether this row is dressed by the transient hover/cursor band.
fn is_banded(is_hover: bool, is_cursor: bool, focused: bool) -> bool {
    is_hover || (is_cursor && focused)
}

/// A background-filled pad span of `n` columns.
fn pad(n: usize, bg: Color) -> Span<'static> {
    Span::styled(" ".repeat(n), Style::default().bg(bg))
}

pub(super) fn draw_appearance_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut SettingsProps<'_>,
    focused: bool,
) -> ScrollableRects {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selectable: Vec<(usize, usize)> = Vec::new();
    let mut selected_line = None;
    let body_width = body.width.max(1) as usize;

    if !props.profile.supports_color_themes() {
        // DEC VT100 Monochrome / NO_COLOR mode: a single fixed row. There is no
        // choice to indicate, so no selection dot; the hover/cursor cue is DEC
        // SGR 7 (Reverse Video) since backgrounds are barred on this hardware.
        let is_cursor = props.detail_index == 0;
        let is_hover = props.hover_index == Some(0);
        selected_line = Some(lines.len());
        selectable.push((0, lines.len()));

        let banded = is_banded(is_hover, is_cursor, focused);
        let row_bg = if banded {
            props.theme.row_hover_band(&[])
        } else {
            Color::Reset
        };
        let mut label_style = Style::default().add_modifier(Modifier::BOLD);
        if banded && props.theme.elevation == nuotc::ElevationArchetype::Structured {
            label_style = label_style.add_modifier(Modifier::REVERSE);
        }

        let mut spans = vec![Span::styled("Monochrome Hardware Mode", label_style)];
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[ Active ]",
            Style::default()
                .fg(
                    if props.theme.elevation == nuotc::ElevationArchetype::Structured {
                        Color::Reset
                    } else {
                        props.theme.brand()
                    },
                )
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
        let desc_style = Style::default().fg(props.theme.muted());
        spans.push(Span::styled(
            "DEC VT100 / NO_COLOR standard (visual distinction uses SGR 7 Reverse Video and ASCII framing)",
            desc_style,
        ));

        lines.push(Line::from(spans).style(Style::default().bg(row_bg)));
        lines.push(Line::from(""));

        return render_scrollable_indexed(
            frame,
            body,
            lines,
            props.detail_scroll,
            selected_line,
            &selectable,
            props.theme,
        );
    }

    let ws_path = if props.workspace.is_empty() {
        None
    } else {
        Some(Path::new(props.workspace))
    };
    let schemes = Theme::available_color_schemes_with_workspace(ws_path);

    for (i, preset) in schemes.iter().enumerate() {
        let is_cursor = i == props.detail_index;
        let is_hover = props.hover_index == Some(i);
        if is_cursor {
            selected_line = Some(lines.len());
        }
        selectable.push((i, lines.len()));

        let is_active = props.color_scheme == preset.id;

        let preview_theme =
            Theme::from_color_scheme_with_workspace(&preset.id, props.custom_color_scheme, ws_path);

        let swatches = [
            preview_theme.body(),
            preview_theme.panel(),
            preview_theme.brand(),
            preview_theme.info(),
            preview_theme.ok(),
            preview_theme.warn(),
        ];

        let banded = is_banded(is_hover, is_cursor, focused);
        let band = if banded {
            props.theme.row_hover_band(&swatches)
        } else {
            Color::Reset
        };

        // The applied row reads through its highlighted text; every other row
        // rests on the plain foreground. When the row is banded, the text is
        // re-contrasted against the band so it never sinks into the highlight.
        let base_label = if is_active {
            props.theme.brand()
        } else {
            props.theme.fg()
        };
        let label_fg = props.theme.band_text(band, base_label);

        let mut label_row = vec![
            Span::styled(
                format!("{:<width$}", preset.label, width = LABEL_COL),
                Style::default().fg(label_fg).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ];
        for swatch in swatches {
            label_row.push(Span::styled("█", Style::default().fg(swatch)));
        }

        // Fill the identity line edge-to-edge so a banded row is an unbroken
        // highlight rather than a highlight that stops at the swatches.
        let used: usize = label_row.iter().map(|span| span.width()).sum();
        if used < body_width {
            label_row.push(pad(body_width - used, band));
        }
        lines.push(Line::from(label_row).style(Style::default().bg(band)));

        // Description on its own line, flush from column 0 (no indent), wrapped
        // to the full width; each wrapped row is padded so the band stays
        // full-width.
        let desc_fg = if banded {
            props.theme.band_text(
                band,
                crate::theme::mix(props.theme.muted(), props.theme.dim(), 0.4),
            )
        } else {
            props.theme.muted()
        };
        let wrapped = crate::text_layout::wrap_text(&preset.description, body_width);
        for segment in &wrapped {
            let mut spans = vec![Span::styled(
                segment.text.clone(),
                Style::default().fg(desc_fg),
            )];
            let used = segment.text.width();
            if used < body_width {
                spans.push(pad(body_width - used, band));
            }
            lines.push(Line::from(spans).style(Style::default().bg(band)));
        }

        lines.push(Line::from(""));
    }

    render_scrollable_indexed(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        &selectable,
        props.theme,
    )
}
