//! Drawing a [`Projection`]. The view decides nothing: every row, keycap,
//! reason and highlight comes from the projection.

use graticule::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::keymap::{Availability, DangerLevel};
use crate::shell::render::Theme;
use ui_primitives::components::list::row_style;
use ui_primitives::design::INPUT_PROMPT_PREFIX;
use ui_primitives::primitives::{
    FixedModalSpec, FooterHint, modal_area, modal_frame, modal_header, render_modal_footer,
};
use view::components::selectable_body::{SelectableRow, render_selectable_body};
use view::model::layout::LayoutMap;
use view::model::selection::SelectionState;

use super::query::Facet;
use super::rank::{Projection, Ranked, Row};

pub(crate) struct PaletteProps<'a> {
    pub query: &'a str,
    pub cursor_position: usize,
    pub projection: &'a Projection,
    /// Index into `projection.entries`.
    pub selected: usize,
    /// Scroll offset over `projection.rows`.
    pub scroll: &'a mut usize,
    pub show_caret: bool,
}

fn placeholder() -> String {
    let facets: Vec<String> = Facet::SIGILS
        .iter()
        .map(|(sigil, _, label)| format!("{sigil} {label}"))
        .collect();
    format!("Search actions  ·  {}", facets.join("  "))
}

/// Draw the palette and return its outer rect.
pub(crate) fn draw(
    frame: &mut Frame,
    props: PaletteProps<'_>,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let PaletteProps {
        query,
        cursor_position,
        projection,
        selected,
        scroll,
        show_caret,
    } = props;
    let outer = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, outer, theme, true, true);
    modal_header(
        frame,
        f.header,
        &format!("Commands · {}", projection.here),
        theme,
    );

    let body = f.body;
    let line = |offset: u16| Rect {
        x: body.x,
        y: body.y + offset,
        width: body.width,
        height: 1,
    };

    // Query line.
    let query_rect = line(0);
    let prefix_width = INPUT_PROMPT_PREFIX.width();
    let field_width = (query_rect.width as usize).saturating_sub(prefix_width);
    let (offset, visible) =
        crate::shell::overlays::common::field_viewport(query, cursor_position, field_width);
    let caret = crate::shell::overlays::common::caret_column(query, cursor_position);
    let query_span = if query.is_empty() {
        Span::styled(placeholder(), Style::default().fg(theme.muted()))
    } else {
        Span::styled(
            visible,
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )
    };
    frame.render_widget(
        &mut Paragraph::new(Line::from(vec![
            Span::styled(
                INPUT_PROMPT_PREFIX,
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            query_span,
        ])),
        query_rect,
    );
    if show_caret {
        let local = (caret as usize).saturating_sub(offset).min(field_width);
        let x = (query_rect.x + (prefix_width + local) as u16)
            .min(query_rect.right().saturating_sub(1));
        frame.set_cursor_position((x, query_rect.y));
    }

    // A detail line needs room for at least a few list rows above it.
    let show_detail = body.height >= 8;
    let chrome_rows = if show_detail { 4 } else { 2 };
    let separator = |frame: &mut Frame, rect: Rect| {
        frame.render_widget(
            &mut Paragraph::new(Line::from(Span::styled(
                "─".repeat(rect.width as usize),
                Style::default().fg(theme.muted()),
            ))),
            rect,
        );
    };
    separator(frame, line(1));
    let list_rect = Rect {
        x: body.x,
        y: body.y + 2,
        width: body.width,
        height: body.height.saturating_sub(chrome_rows),
    };

    let width = list_rect.width as usize;
    let selected_row = projection.row_of(selected);
    let rows: Vec<SelectableRow> = if projection.rows.is_empty() {
        vec![SelectableRow::from_line(Line::from(Span::styled(
            "  Nothing matches. Clear the query to see what you can do here.",
            Style::default().fg(theme.muted()),
        )))]
    } else {
        projection
            .rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| match row {
                Row::Header(section) => SelectableRow::from_line(Line::from(Span::styled(
                    section.title(),
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                ))),
                Row::Entry(index) => SelectableRow::from_line(entry_line(
                    &projection.entries[*index],
                    Some(row_index) == selected_row,
                    width,
                    theme,
                )),
            })
            .collect()
    };
    render_selectable_body(
        frame,
        list_rect,
        &rows,
        scroll,
        selected_row,
        theme,
        selection,
        layout_map,
    );

    if show_detail {
        separator(frame, line(body.height - 2));
        let detail = projection
            .selected(selected)
            .map(|ranked| match ranked.entry.availability {
                Availability::Available => Span::styled(
                    ranked.entry.detail.to_string(),
                    Style::default().fg(theme.dim()),
                ),
                Availability::Unavailable(reason) => Span::styled(
                    format!("Unavailable: {reason}"),
                    Style::default().fg(theme.warn()),
                ),
            })
            .unwrap_or_else(|| Span::raw(""));
        frame.render_widget(
            &mut Paragraph::new(Line::from(detail)),
            line(body.height - 1),
        );
    }

    if let Some(footer) = f.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::navigation("↑↓", "move"),
                FooterHint::navigation(": @ >", "filter"),
                FooterHint::primary("Enter", "run"),
                FooterHint::always("Esc", "close"),
            ],
            theme,
        );
    }
    outer
}

fn entry_line(ranked: &Ranked, is_selected: bool, width: usize, theme: &Theme) -> Line<'static> {
    let entry = &ranked.entry;
    let rs = row_style(is_selected, theme);
    let available = entry.is_available();
    let bg = if is_selected { rs.bg } else { Color::Reset };
    let base = Style::default().bg(bg);
    let title_style = match (available, is_selected) {
        (false, true) => base.fg(rs.dim),
        (false, false) => base.fg(theme.muted()),
        (true, true) => base.fg(rs.fg).add_modifier(Modifier::BOLD),
        (true, false) => base.fg(theme.fg()),
    };
    let hit_style = title_style
        .fg(if is_selected { rs.fg } else { theme.brand() })
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINE);

    let mut spans = vec![Span::styled("  ", base)];
    // Group adjacent chars that share a style into one span.
    let mut run = String::new();
    let mut run_hit = false;
    for (i, ch) in entry.title.chars().enumerate() {
        let hit = ranked.positions.contains(&i);
        if hit != run_hit && !run.is_empty() {
            let style = if run_hit { hit_style } else { title_style };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_hit = hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(
            run,
            if run_hit { hit_style } else { title_style },
        ));
    }
    match entry.danger {
        DangerLevel::Dangerous => spans.push(Span::styled(
            "  danger",
            base.fg(if is_selected { rs.fg } else { theme.err() })
                .add_modifier(Modifier::BOLD),
        )),
        DangerLevel::Cautious => spans.push(Span::styled(
            "  caution",
            base.fg(if is_selected { rs.fg } else { theme.warn() }),
        )),
        DangerLevel::Safe => {}
    }

    let right = match entry.availability {
        Availability::Unavailable(reason) => reason.to_string(),
        Availability::Available => entry
            .keycap
            .map(|key| key.display().to_string())
            .or_else(|| entry.alias.map(str::to_string))
            .unwrap_or_default(),
    };
    let right_style = base.fg(if is_selected { rs.dim } else { theme.muted() });
    let left_width: usize = spans.iter().map(|s| s.content.width()).sum();
    let right_width = right.width();
    if width >= left_width + right_width + 2 {
        spans.push(Span::styled(
            " ".repeat(width - left_width - right_width),
            base,
        ));
        spans.push(Span::styled(right, right_style));
    } else if width > left_width {
        spans.push(Span::styled(" ".repeat(width - left_width), base));
    }

    let mut line = Line::from(spans);
    if is_selected && !theme.elevation.is_chromatic() {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(Modifier::REVERSE);
        }
    }
    line
}
