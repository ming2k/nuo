//! Shared helper routines and search components for provider modals.

use nuotc::{
    Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

use super::super::common::{caret_column, field_viewport};
use crate::render::Theme;

pub(crate) const PICKER_SEARCH_PREFIX: &str = " Search  › ";

pub(crate) fn split_search_body(body: Rect, search: bool) -> (Option<Rect>, Rect) {
    if !search || body.height == 0 {
        return (None, body);
    }

    let search_rect = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: 1,
    };
    let consumed = if body.height > 1 { 2 } else { 1 };
    let list_rect = Rect {
        x: body.x,
        y: body.y.saturating_add(consumed),
        width: body.width,
        height: body.height.saturating_sub(consumed),
    };
    (Some(search_rect), list_rect)
}

/// Split the modal body by placing the search row at the **bottom**, leaving the
/// top of the list completely motionless to avoid layout shift when search toggles.
pub(crate) fn split_search_bottom(body: Rect, search: bool) -> (Rect, Option<Rect>) {
    if !search || body.height == 0 {
        return (body, None);
    }

    let consumed = if body.height > 1 { 2 } else { 1 };
    let list_rect = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height.saturating_sub(consumed),
    };
    let search_rect = Rect {
        x: body.x,
        y: body.bottom().saturating_sub(1),
        width: body.width,
        height: 1,
    };
    (list_rect, Some(search_rect))
}

pub(crate) fn draw_picker_search_row(
    frame: &mut Frame,
    rect: Rect,
    query: &str,
    cursor_position: usize,
    theme: &Theme,
) {
    let prefix_width = PICKER_SEARCH_PREFIX.width();
    let field_width = (rect.width as usize).saturating_sub(prefix_width);
    let visible_query = field_viewport(query, cursor_position, field_width).1;
    let value_style = Style::default()
        .fg(if query.is_empty() {
            theme.muted()
        } else {
            theme.fg()
        })
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(PICKER_SEARCH_PREFIX, Style::default().fg(theme.muted())),
            Span::styled(
                if query.is_empty() {
                    "type to fuzzy-filter".to_string()
                } else {
                    visible_query
                },
                value_style,
            ),
        ])),
        rect,
    );
}

pub(crate) fn place_picker_search_cursor(
    frame: &mut Frame,
    rect: Rect,
    query: &str,
    cursor_position: usize,
) {
    let prefix_width = PICKER_SEARCH_PREFIX.width() as u16;
    let field_width = rect.width.saturating_sub(prefix_width);
    if rect.height == 0 || field_width == 0 {
        return;
    }
    let (offset, _) = field_viewport(query, cursor_position, field_width as usize);
    let caret = caret_column(query, cursor_position);
    let local = caret
        .saturating_sub(offset.min(u16::MAX as usize) as u16)
        .min(field_width.saturating_sub(1));
    let x = rect.x.saturating_add(prefix_width).saturating_add(local);
    frame.set_cursor_position((x, rect.y));
}

pub(crate) fn search_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled(
        "(no matches — try a shorter or different query)",
        Style::default().fg(theme.muted()),
    ))]
}

pub(crate) fn match_set(m: Option<&crate::fuzzy::FuzzyMatch>) -> std::collections::HashSet<usize> {
    m.map(|fm| fm.positions.iter().copied().collect())
        .unwrap_or_default()
}
