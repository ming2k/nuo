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
//!   background band derived from the *active palette* (`Theme::row_hover_band`),
//!   chosen so the row's own swatches stay legible against it rather than
//!   fusing into a fixed gray. The band is the same for the mouse pointer and
//!   the keyboard cursor, so both read as one affordance.

use std::path::Path;

use nuotc::{
    Borders, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style, Wrap, widgets::Block,
};
use unicode_width::UnicodeWidthStr;

use super::{ScrollableRects, SettingsProps, render_scrollable_indexed};

/// Column where a row's label begins. Swatches follow one space after the
/// padded label.
const LABEL_COL: usize = 22;

/// Dynamic count of selectable items in the Appearance panel for the active terminal profile.
pub fn item_count(ws_path: Option<&Path>, profile: &nuotc::TerminalProfile) -> usize {
    if profile.supports_color_themes() {
        crate::shell::config::available_color_schemes(ws_path)
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
        if banded
            && props.theme.elevation == nuo_ui_primitives::elevation::ElevationArchetype::Structured
        {
            label_style = label_style.add_modifier(Modifier::REVERSE);
        }

        let mut spans = vec![Span::styled("Monochrome Hardware Mode", label_style)];
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[ Active ]",
            Style::default()
                .fg(
                    if props.theme.elevation
                        == nuo_ui_primitives::elevation::ElevationArchetype::Structured
                    {
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
    let schemes = crate::shell::config::available_color_schemes(ws_path);

    let (list_rect, specimen_rect) = if body.width >= 72 {
        let list_w = 44u16.max(body.width / 2);
        let list = Rect::new(body.x, body.y, list_w, body.height);
        let specimen = Rect::new(
            body.x.saturating_add(list_w + 1),
            body.y,
            body.width.saturating_sub(list_w + 1),
            body.height,
        );
        (list, Some(specimen))
    } else {
        (body, None)
    };
    let body_width = list_rect.width.max(1) as usize;

    for (i, preset) in schemes.iter().enumerate() {
        let is_cursor = i == props.detail_index;
        let is_hover = props.hover_index == Some(i);
        if is_cursor {
            selected_line = Some(lines.len());
        }
        selectable.push((i, lines.len()));

        let is_active = props.color_scheme == preset.id;

        let preview_theme = crate::shell::config::theme_from_color_scheme(
            &preset.id,
            props.custom_color_scheme,
            ws_path,
        );

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
                nuo_ui_primitives::theme::mix(props.theme.muted(), props.theme.dim(), 0.4),
            )
        } else {
            props.theme.muted()
        };
        let wrapped = nuo_view::text_layout::wrap_text(&preset.description, body_width);
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

    let result = render_scrollable_indexed(
        frame,
        list_rect,
        lines,
        props.detail_scroll,
        selected_line,
        &selectable,
        props.theme,
    );

    if let Some(spec_rect) = specimen_rect {
        if !schemes.is_empty() {
            let candidate_idx = props.detail_index.min(schemes.len().saturating_sub(1));
            let candidate_preset = &schemes[candidate_idx];
            let candidate_theme = crate::shell::config::theme_from_color_scheme(
                &candidate_preset.id,
                props.custom_color_scheme,
                ws_path,
            );
            draw_live_specimen_gallery(
                frame,
                spec_rect,
                props.theme,
                &candidate_theme,
                &candidate_preset.label,
            );
        }
    }

    result
}

fn draw_live_specimen_gallery(
    frame: &mut Frame,
    rect: Rect,
    active_theme: &nuo_ui_primitives::theme::Theme,
    candidate: &nuo_ui_primitives::theme::Theme,
    label: &str,
) {
    if rect.width < 24 || rect.height < 8 {
        return;
    }

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(active_theme.dim_fg))
        .style(Style::default().bg(active_theme.pane_sunken()));
    let inner = block.inner(rect);
    frame.render_widget(&mut block, rect);

    if inner.width < 20 || inner.height < 6 {
        return;
    }

    let specimen = candidate.palette_specimen(label);
    let mut lines = Vec::new();

    // 1. Candidate Header
    lines.push(Line::from(vec![
        Span::styled(" Preview: ", Style::default().fg(candidate.muted())),
        Span::styled(
            label,
            Style::default()
                .fg(candidate.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if specimen.is_dark {
                " (Dark)"
            } else {
                " (Light)"
            },
            Style::default().fg(candidate.muted()),
        ),
    ]));
    lines.push(Line::raw(""));

    // 2. Palette Swatches
    lines.push(Line::from(Span::styled(
        "── Semantic Swatches ──────────────",
        Style::default().fg(candidate.dim()),
    )));

    // Surfaces
    let mut surf_spans = vec![Span::styled(
        "Surfaces: ",
        Style::default().fg(candidate.muted()),
    )];
    for s in &specimen.surfaces {
        surf_spans.push(Span::styled("█", Style::default().fg(s.color)));
        surf_spans.push(Span::styled(
            format!(" {} ", s.name),
            Style::default().fg(candidate.fg()),
        ));
    }
    lines.push(Line::from(surf_spans));

    // Signals
    let mut sig_spans = vec![Span::styled(
        "Signals:  ",
        Style::default().fg(candidate.muted()),
    )];
    for s in &specimen.signals {
        sig_spans.push(Span::styled("█", Style::default().fg(s.color)));
        sig_spans.push(Span::styled(
            format!(" {} ", s.name),
            Style::default().fg(candidate.fg()),
        ));
    }
    lines.push(Line::from(sig_spans));
    lines.push(Line::raw(""));

    // 3. Live UI Component Micro-Mockups
    lines.push(Line::from(Span::styled(
        "── Live Component Specimen ────────",
        Style::default().fg(candidate.dim()),
    )));

    // Turn Header
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(candidate.info())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "turn 2  ",
            Style::default()
                .fg(candidate.info())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("claude-3-7-sonnet  ", Style::default().fg(candidate.fg())),
        Span::styled("14:32:05", Style::default().fg(candidate.muted())),
    ]));

    // Syntax Diff
    lines.push(Line::from(vec![
        Span::styled(
            "  + ",
            Style::default()
                .fg(candidate.ok())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "let total = calculate_fast_simd();",
            Style::default().fg(candidate.fg()),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            "  - ",
            Style::default()
                .fg(candidate.err())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "let total = calculate_legacy_slow();",
            Style::default().fg(candidate.muted()),
        ),
    ]));

    // Running Tool (ADR-0110)
    lines.push(Line::from(vec![
        Span::styled("  ▎", Style::default().fg(candidate.running())),
        Span::styled(
            "⠋ edit_text ",
            Style::default()
                .fg(candidate.running())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("| running (1.4s)", Style::default().fg(candidate.running())),
    ]));

    // Toast
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            " ✓ ",
            Style::default().fg(candidate.ok()).bg(candidate.toast_bg),
        ),
        Span::styled(
            "Prompt copied to clipboard ",
            Style::default()
                .fg(candidate.fg())
                .bg(candidate.toast_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    frame.render_widget(&mut Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_ui_primitives::theme::Theme;
    use nuotc::TestTerminal;

    #[test]
    fn test_appearance_item_count() {
        let direct = nuotc::TerminalProfile::direct_color();
        assert!(item_count(None, &direct) >= 6);

        let mono = nuotc::TerminalProfile::dec_vt100_monochrome();
        assert_eq!(item_count(None, &mono), 1);
    }

    #[test]
    fn test_appearance_dual_pane_live_specimen_wide_terminal() {
        let mut term = TestTerminal::new(100, 30);
        let theme = Theme::default();
        let profile = nuotc::TerminalProfile::direct_color();
        let mut scroll = 0;
        let tui_config = crate::shell::config::TuiConfig::default();
        let mut cat_scroll = 0;
        let mut props = SettingsProps {
            category_index: 0,
            detail_index: 1,
            hover_index: None,
            focus: crate::shell::views::settings::ConfigFocus::Detail,
            color_scheme: "zen",
            custom_color_scheme: &Default::default(),
            websearch: None,
            workspace: "",
            category_scroll: &mut cat_scroll,
            detail_scroll: &mut scroll,
            breadcrumbs: None,
            theme: &theme,
            profile: &profile,
            tui_config: &tui_config,
            tab_bar: None,
        };

        term.draw(|f| {
            let res = draw_appearance_detail(f, f.area(), &mut props, true);
            assert!(!res.rows.is_empty(), "Selectable rows must be present");
        });

        let buf = term.buffer();
        let mut text = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf.get_or_empty(x, y).symbol.as_str());
            }
            text.push('\n');
        }
        assert!(
            text.contains("Preview:"),
            "Right pane must render Live Specimen header"
        );
        assert!(
            text.contains("Live Component Specimen"),
            "Right pane must render mock UI elements"
        );
        assert!(
            text.contains("edit_text"),
            "Must contain in-flight running tool indicator"
        );
    }

    #[test]
    fn test_appearance_single_pane_fallback_narrow_terminal() {
        let mut term = TestTerminal::new(60, 20);
        let theme = Theme::default();
        let profile = nuotc::TerminalProfile::direct_color();
        let mut scroll = 0;
        let tui_config = crate::shell::config::TuiConfig::default();
        let mut cat_scroll = 0;
        let mut props = SettingsProps {
            category_index: 0,
            detail_index: 0,
            hover_index: None,
            focus: crate::shell::views::settings::ConfigFocus::Detail,
            color_scheme: "zen",
            custom_color_scheme: &Default::default(),
            websearch: None,
            workspace: "",
            category_scroll: &mut cat_scroll,
            detail_scroll: &mut scroll,
            breadcrumbs: None,
            theme: &theme,
            profile: &profile,
            tui_config: &tui_config,
            tab_bar: None,
        };

        term.draw(|f| {
            let res = draw_appearance_detail(f, f.area(), &mut props, true);
            assert!(!res.rows.is_empty(), "Selectable rows must be present");
        });

        let buf = term.buffer();
        let mut text = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf.get_or_empty(x, y).symbol.as_str());
            }
            text.push('\n');
        }
        assert!(
            !text.contains("Live Component Specimen"),
            "Narrow terminal must suppress right pane to prevent clipping"
        );
    }
}
