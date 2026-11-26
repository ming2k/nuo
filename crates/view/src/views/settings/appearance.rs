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

use graticule_canvas::{
    Borders, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style, Wrap, widgets::Block,
};
use unicode_width::UnicodeWidthStr;

use super::{ScrollableRects, SettingsProps, render_scrollable_indexed};

/// Column where a row's label begins. Swatches follow one space after the
/// padded label.
const LABEL_COL: usize = 22;

/// Dynamic count of selectable items in the Appearance panel for the active terminal profile.
pub fn item_count(
    theme_files: &[ui_primitives::ThemeFile],
    profile: &graticule_canvas::TerminalProfile,
) -> usize {
    if profile.supports_color_themes() {
        ui_primitives::Theme::available_color_schemes_from(theme_files)
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
            && props.theme.elevation == ui_primitives::elevation::ElevationArchetype::Structured
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
                        == ui_primitives::elevation::ElevationArchetype::Structured
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

    let schemes = ui_primitives::Theme::available_color_schemes_from(props.theme_files);

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

        let preview_theme = ui_primitives::Theme::from_color_scheme_from(
            &preset.id,
            props.custom_color_scheme,
            props.theme_files,
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
                ui_primitives::theme::mix(props.theme.muted(), props.theme.dim(), 0.4),
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

    let result = render_scrollable_indexed(
        frame,
        list_rect,
        lines,
        props.detail_scroll,
        selected_line,
        &selectable,
        props.theme,
    );

    if let Some(spec_rect) = specimen_rect
        && !schemes.is_empty()
    {
        let candidate_idx = props.detail_index.min(schemes.len().saturating_sub(1));
        let candidate_preset = &schemes[candidate_idx];
        let candidate_theme = ui_primitives::Theme::from_color_scheme_from(
            &candidate_preset.id,
            props.custom_color_scheme,
            props.theme_files,
        );
        draw_live_specimen_gallery(
            frame,
            spec_rect,
            props.theme,
            &candidate_theme,
            &candidate_preset.label,
        );
    }

    result
}

fn draw_live_specimen_gallery(
    frame: &mut Frame,
    rect: Rect,
    active_theme: &ui_primitives::theme::Theme,
    candidate: &ui_primitives::theme::Theme,
    label: &str,
) {
    if rect.width < 24 || rect.height < 8 {
        return;
    }

    // Outer frame separates from the settings pane in `active_theme`, while the
    // inner preview canvas paints in `candidate.surface()` (`[INV-TUI-PREVIEW-01]`,
    // ADR-0259) so cross-polarity previews (e.g. previewing light `Paper` while
    // `Zen` is active) show authentic foreground/background contrast.
    let preview_bg = candidate.surface();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(active_theme.dim_fg))
        .style(Style::default().bg(preview_bg));
    let inner = block.inner(rect);
    frame.render_widget(&mut block, rect);

    if inner.width < 20 || inner.height < 6 {
        return;
    }

    let specimen = candidate.palette_specimen(label);
    let mut lines = Vec::new();
    let base = Style::default().bg(preview_bg);

    // 1. Candidate Header
    let polarity = if candidate.is_monochrome() {
        " (Monochrome)"
    } else if candidate.is_ansi16() {
        " (ANSI 16)"
    } else if specimen.is_dark {
        " (Dark)"
    } else {
        " (Light)"
    };
    lines.push(
        Line::from(vec![
            Span::styled(" Preview: ", base.fg(candidate.muted())),
            Span::styled(
                label.to_string(),
                base.fg(candidate.brand()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(polarity, base.fg(candidate.muted())),
        ])
        .style(base),
    );
    lines.push(Line::raw("").style(base));

    // 2. Palette Swatches (Surfaces, Text Ladder, Signals)
    let section_rule = |title: &str| -> Line<'static> {
        let prefix = format!("── {title} ");
        let fill = (inner.width as usize).saturating_sub(prefix.width());
        Line::from(Span::styled(
            format!("{prefix}{}", "─".repeat(fill)),
            base.fg(candidate.dim()),
        ))
        .style(base)
    };
    lines.push(section_rule("Semantic Swatches"));

    let max_cols = inner.width as usize;
    let push_swatch_group =
        |lines: &mut Vec<Line<'static>>,
         label: &'static str,
         swatches: &[ui_primitives::theme::SwatchRole]| {
            let indent = " ".repeat(label.width());
            let mut row: Vec<Span<'static>> = vec![Span::styled(label, base.fg(candidate.muted()))];
            let mut row_w = label.width();
            let mut items_on_row = 0usize;

            for s in swatches {
                let item_label = format!(" {} ", s.name);
                let item_w = 1 + item_label.width();
                if items_on_row > 0 && row_w + item_w > max_cols {
                    lines.push(Line::from(row).style(base));
                    row = vec![Span::styled(indent.clone(), base)];
                    row_w = indent.width();
                    items_on_row = 0;
                }
                row.push(Span::styled("█", base.fg(s.color)));
                row.push(Span::styled(item_label, base.fg(candidate.fg())));
                row_w += item_w;
                items_on_row += 1;
            }
            lines.push(Line::from(row).style(base));
        };

    push_swatch_group(&mut lines, "Surfaces: ", &specimen.surfaces);
    push_swatch_group(&mut lines, "Text:     ", &specimen.text_ladder);
    push_swatch_group(&mut lines, "Signals:  ", &specimen.signals);
    lines.push(Line::raw("").style(base));

    // 3. Live UI Component Specimen (candidate-surface faithful)
    lines.push(section_rule("Live Component Specimen"));

    // Tab strip preview (Running breathing dot + Attention `!` + Idle)
    let tab_bg = candidate.tabbar_bg();
    let active_tab_bg = candidate.raised();
    lines.push(
        Line::from(vec![
            Span::styled(" ", base.bg(tab_bg)),
            Span::styled(
                format!(" {} ", candidate.glyphs.dot),
                Style::default()
                    .fg(candidate.running())
                    .bg(active_tab_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "refactor ",
                Style::default()
                    .fg(candidate.fg())
                    .bg(active_tab_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().bg(tab_bg)),
            Span::styled(
                "!",
                Style::default()
                    .fg(candidate.warn())
                    .bg(tab_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" gate ", Style::default().fg(candidate.muted()).bg(tab_bg)),
            Span::styled("docs ", Style::default().fg(candidate.dim()).bg(tab_bg)),
        ])
        .style(Style::default().bg(tab_bg)),
    );

    // Turn Header (structural `info`)
    lines.push(
        Line::from(vec![
            Span::styled("› ", base.fg(candidate.info()).add_modifier(Modifier::BOLD)),
            Span::styled(
                "turn 2  ",
                base.fg(candidate.info()).add_modifier(Modifier::BOLD),
            ),
            Span::styled("sonnet  ", base.fg(candidate.fg())),
            Span::styled("14:32", base.fg(candidate.muted())),
        ])
        .style(base),
    );

    // Running Tool (ADR-0110 / ADR-0258)
    lines.push(
        Line::from(vec![
            Span::styled("  ", base),
            Span::styled(
                "⠋ edit_text",
                base.fg(candidate.running()).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  running (1.4s)", base.fg(candidate.running())),
        ])
        .style(base),
    );

    // Tool Lifecycle Ladder (Ok / Denied / Failed / Cancelled — compact so it never mid-wraps)
    lines.push(
        Line::from(vec![
            Span::styled("  ", base),
            Span::styled("✓ ok", base.fg(candidate.ok())),
            Span::styled(" ", base),
            Span::styled("▲ deny", base.fg(candidate.warn())),
            Span::styled(" ", base),
            Span::styled("✕ fail", base.fg(candidate.err())),
            Span::styled(" ", base),
            Span::styled("○ dim", base.fg(candidate.dim())),
        ])
        .style(base),
    );

    // Syntax Diff on candidate's actual diff bands
    let add_bg = candidate.diff_add_bg();
    let del_bg = candidate.diff_del_bg();
    lines.push(
        Line::from(vec![
            Span::styled(
                "  + ",
                Style::default()
                    .fg(candidate.ok())
                    .bg(add_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "let sum = fast_simd();",
                Style::default().fg(candidate.fg()).bg(add_bg),
            ),
        ])
        .style(Style::default().bg(add_bg)),
    );
    lines.push(
        Line::from(vec![
            Span::styled(
                "  - ",
                Style::default()
                    .fg(candidate.err())
                    .bg(del_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "let sum = slow_loop();",
                Style::default().fg(candidate.muted()).bg(del_bg),
            ),
        ])
        .style(Style::default().bg(del_bg)),
    );

    // Focused Running Action Pill + Toast
    lines.push(
        Line::from(vec![
            Span::styled("  ", base),
            Span::styled(
                " ⠋ Running (2s) ",
                Style::default()
                    .fg(candidate.surface())
                    .bg(candidate.running())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", base),
            Span::styled(
                " ✓ ",
                Style::default().fg(candidate.ok()).bg(candidate.toast_bg),
            ),
            Span::styled(
                "Copied ",
                Style::default()
                    .fg(candidate.fg())
                    .bg(candidate.toast_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
        .style(base),
    );

    frame.render_widget(
        &mut Paragraph::new(lines).style(base).wrap(Wrap { trim: false }),
        inner,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use graticule_canvas::TestTerminal;
    use ui_primitives::theme::Theme;

    #[test]
    fn test_appearance_item_count() {
        let direct = graticule_canvas::TerminalProfile::direct_color();
        assert!(item_count(&[], &direct) >= 6);

        let mono = graticule_canvas::TerminalProfile::dec_vt100_monochrome();
        assert_eq!(item_count(&[], &mono), 1);
    }

    #[test]
    fn test_appearance_dual_pane_live_specimen_wide_terminal() {
        let mut term = TestTerminal::new(100, 30);
        let theme = Theme::default();
        let profile = graticule_canvas::TerminalProfile::direct_color();
        let mut scroll = 0;
        let tui_config = crate::tui_config::TuiConfig::default();
        let mut cat_scroll = 0;
        let mut props = SettingsProps {
            category_index: 0,
            detail_index: 1,
            hover_index: None,
            focus: crate::views::settings::ConfigFocus::Detail,
            color_scheme: "zen",
            custom_color_scheme: &Default::default(),
            websearch: None,
            workspace: "",
            theme_files: &[],
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
        let profile = graticule_canvas::TerminalProfile::direct_color();
        let mut scroll = 0;
        let tui_config = crate::tui_config::TuiConfig::default();
        let mut cat_scroll = 0;
        let mut props = SettingsProps {
            category_index: 0,
            detail_index: 0,
            hover_index: None,
            focus: crate::views::settings::ConfigFocus::Detail,
            color_scheme: "zen",
            custom_color_scheme: &Default::default(),
            websearch: None,
            workspace: "",
            theme_files: &[],
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

    #[test]
    fn test_appearance_specimen_paints_candidate_surface_cross_polarity() {
        let mut term = TestTerminal::new(110, 32);
        let active_zen = Theme::default();
        let paper_idx = Theme::color_scheme_index("paper");
        let expected_paper = Theme::from_color_scheme("paper", &Default::default());
        let profile = graticule_canvas::TerminalProfile::direct_color();
        let mut scroll = 0;
        let tui_config = crate::tui_config::TuiConfig::default();
        let mut cat_scroll = 0;
        let mut props = SettingsProps {
            category_index: 0,
            detail_index: paper_idx,
            hover_index: None,
            focus: crate::views::settings::ConfigFocus::Detail,
            color_scheme: "zen",
            custom_color_scheme: &Default::default(),
            websearch: None,
            workspace: "",
            theme_files: &[],
            category_scroll: &mut cat_scroll,
            detail_scroll: &mut scroll,
            breadcrumbs: None,
            theme: &active_zen,
            profile: &profile,
            tui_config: &tui_config,
            tab_bar: None,
        };

        term.draw(|f| {
            draw_appearance_detail(f, f.area(), &mut props, true);
        });

        let buf = term.buffer();
        // Right pane starts at x >= 56; its inner canvas must carry Paper's
        // light surface (`[INV-TUI-PREVIEW-01]`, ADR-0259), never Zen's dark bg.
        let inner_cell = buf.get_or_empty(70, 3);
        assert_eq!(
            inner_cell.style.bg,
            expected_paper.surface(),
            "specimen canvas must paint candidate theme's surface, not active theme's background"
        );
    }
}
