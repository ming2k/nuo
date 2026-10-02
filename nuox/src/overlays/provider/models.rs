//! The flat Models picker modal (selecting the active model/instance).

use nuotc::{
    Frame, Rect, {Line, Span}, {Modifier, Style},
};

use unicode_width::UnicodeWidthStr;

use super::super::common::truncate_ellipsis;
use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, search_empty_body,
    split_search_bottom,
};
use crate::components::options::{ChoiceTone, choice_style};
use crate::components::row::{ListRow, RowGroup};
use crate::primitives::{
    BodyRenderOptions, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    keyvocab, modal_area, modal_frame, modal_header, render_body, render_centered_body,
    render_modal_footer_with_extra,
};
use crate::providers::{ModelBodyLine, RankedModel, models_body_lines};
use crate::render::Theme;

/// The minimum columns the secondary **wire-id** column must have available
/// before it renders at all. Below this a wire id degenerates into an
/// unreadable fragment, and a fragment of the only string the user can type is
/// worse than no string: the row then leads with the label alone, the mirror of
/// a row whose provider published no label at all. The floor is deliberately
/// above a bare fragment — `deep…` would read as a different model rather than
/// as an abbreviation of `deepseek-flash`. (Restores the pre-multi-field-search
/// `MIN_SUFFIX_BUDGET` floor, which the id-first column layout dropped.)
const MIN_ID_COLUMN_WIDTH: usize = 10;

/// Properties for rendering the Models modal.
pub struct ModelsModalProps<'a> {
    pub models: &'a [RankedModel],
    pub current_provider: &'a str,
    pub current_model: &'a str,
    pub modal_index: usize,
    pub query: &'a str,
    pub cursor_position: usize,
    pub scroll: &'a mut usize,
    pub follow_selection: bool,
    pub search: bool,
    /// Whether this surface currently owns the terminal cursor
    /// (`App::caret_owner() == CaretOwner::Overlay`, ADR-0205). The picker
    /// never places the physical cursor on its own authority — the frame-level
    /// arbiter decides that, and this flag is its verdict threaded down.
    pub show_caret: bool,
    pub refreshing: bool,
    pub spinner_phase: usize,
}

/// Draw the **Models** flat model picker modal (`/models`).
pub fn draw_models_modal(
    frame: &mut Frame,
    props: ModelsModalProps<'_>,
    theme: &Theme,
) -> nuotc::Rect {
    let ModelsModalProps {
        models,
        current_provider,
        current_model,
        modal_index,
        query,
        cursor_position,
        scroll,
        follow_selection,
        search,
        show_caret,
        refreshing,
        spinner_phase,
    } = props;
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme, true, true);

    let header_rect = f.header;

    let refresh_label = if refreshing {
        "refreshing…"
    } else {
        "refresh"
    };
    let browse_hints: [FooterHint; 8] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("x", "block"),
        FooterHint::secondary("e", "settings"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let search_hints: [FooterHint; 4] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::key_always(crate::keymap::Key::ESC, "clear search"),
    ];
    let empty_hints: [FooterHint; 3] = [
        FooterHint::primary("a", "add connection"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else if models.is_empty() {
        (&empty_hints, &[])
    } else {
        (&browse_hints, &[])
    };

    if refreshing {
        let spin = theme.glyphs.spinner_frame(spinner_phase);
        let header = [
            crate::elevation::HeaderPart::title("Models"),
            crate::elevation::HeaderPart::Text {
                text: "  ",
                accent: false,
            },
            crate::elevation::HeaderPart::Text {
                text: spin,
                accent: true,
            },
            crate::elevation::HeaderPart::Text {
                text: " refreshing…",
                accent: false,
            },
        ];
        crate::elevation::modal_header_parts(frame, header_rect, &header, theme);
    } else if search && !query.is_empty() {
        let title = format!("Models ({})", models.len());
        modal_header(frame, header_rect, &title, theme);
    } else {
        modal_header(frame, header_rect, "Models", theme);
    }

    let (body_rect, search_rect) = split_search_bottom(f.body, search);
    if let Some(search_rect) = search_rect {
        if search_rect.y > f.body.y {
            let sep_rect = Rect {
                x: search_rect.x,
                y: search_rect.y - 1,
                width: search_rect.width,
                height: 1,
            };
            let sep_line = "─".repeat(sep_rect.width as usize);
            frame.render_widget(
                nuotc::Paragraph::new(Line::from(Span::styled(
                    sep_line,
                    Style::default().fg(theme.muted()),
                ))),
                sep_rect,
            );
        }
        draw_picker_search_row(frame, search_rect, query, cursor_position, theme);
    }

    if models.is_empty() && !search {
        let body = if refreshing {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                    Span::styled("Refreshing models…", Style::default().fg(theme.muted())),
                ]),
            ]
        } else {
            models_empty_body(theme)
        };
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        return area;
    }

    if models.is_empty() && search {
        let body = search_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        if show_caret && let Some(sr) = search_rect {
            place_picker_search_cursor(frame, sr, query, cursor_position);
        }
        return area;
    }

    let (body, row_line) = model_list_body(
        models,
        current_provider,
        current_model,
        modal_index,
        theme,
        body_rect.width as usize,
    );

    let follow = if follow_selection {
        row_line.get(modal_index).copied()
    } else {
        None
    };

    render_body(
        frame,
        body_rect,
        body,
        scroll,
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_extra(frame, fo, hints, extra, theme);
    }

    if show_caret
        && search
        && let Some(sr) = search_rect
    {
        place_picker_search_cursor(frame, sr, query, cursor_position);
    }
    area
}

/// Build the **Models** flat model list body via the shared [`crate::components::row::ListRow`]
/// standard, **sectioned into three labeled groups** — Favorites, Recent, All models.
pub(crate) fn model_list_body(
    models: &[RankedModel],
    _current_provider: &str,
    _current_model: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    if models.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (geometry, row_line) = models_body_lines(models);
    let mut body: Vec<Line> = Vec::with_capacity(geometry.len() + 3);

    let spacer = || {
        Line::from(Span::styled(
            " ".repeat(body_width.max(1)),
            Style::default().bg(theme.panel()),
        ))
    };

    // Identity column width, shared by every row so the list stays tabular.
    // Sizing keys on the *rendered label* — the provider's own name when it
    // publishes one, else the wire id — because that is what leads the row.
    // The label owns this region; the wire id it names is anchored in the
    // column beside it, and that column drops for the whole list when the
    // region cannot fund a readable id (docs/reference/tui/modals.md:
    // name-first, id-fallback).
    let identity_budget = (body_width * 3 / 5).max(1);
    let label_width = |m: &RankedModel| m.name.as_deref().unwrap_or(m.model.as_str()).width();
    let max_label_len = models.iter().map(label_width).max().unwrap_or(20);
    // Never below 18 (so a short-label list still reads as a column) and never
    // above the region (so `clamp` cannot invert its bounds on a tiny body).
    let label_col_cap = identity_budget.max(18);
    let label_col_width = max_label_len.clamp(18, label_col_cap);
    let id_col = label_col_width + 2;
    let id_budget = identity_budget.saturating_sub(id_col);
    let show_id_column = id_budget >= MIN_ID_COLUMN_WIDTH;

    for line in geometry {
        match line {
            ModelBodyLine::Section(section) => {
                if !body.is_empty() {
                    body.push(spacer());
                }
                body.push(Line::from(Span::styled(
                    format!(" {}", section.label()),
                    Style::default().fg(theme.muted()),
                )));
            }
            ModelBodyLine::Row(row) => {
                let rm = &models[row];
                let is_selected = row == modal_index;
                let locked = !rm.usable;
                let style = choice_style(ChoiceTone::Filled, is_selected && !locked, theme);

                let tag = match (rm.thinking, rm.effort.as_deref()) {
                    (Some(true), Some(effort)) => format!("think on {effort}"),
                    (Some(true), None) => "think on".to_string(),
                    (None, Some(effort)) => effort.to_string(),
                    _ => String::new(),
                };
                // The provider's own declaration leads the tag row so why a row
                // is inert reads first (official `/model` parity). When the
                // provider stated a reason it is shown verbatim; when it stated
                // none the tag stays the bare word, and the refusal toast says
                // only that. A model the user overrode keeps its upstream
                // verdict visible rather than looking natively runnable.
                let tag = if locked {
                    let verdict = rm.locked_reason.as_deref().map_or_else(
                        || "locked".to_string(),
                        |reason| format!("locked: {reason}"),
                    );
                    if tag.is_empty() {
                        verdict
                    } else {
                        format!("{verdict} · {tag}")
                    }
                } else if rm.availability_overridden {
                    if tag.is_empty() {
                        "locked upstream · overridden by you".to_string()
                    } else {
                        format!("locked upstream · overridden by you · {tag}")
                    }
                } else {
                    tag
                };
                // A verdict observed before a failed refresh is marked as such:
                // it is still enforced, but it is not freshly confirmed.
                let tag = if rm.availability_stale {
                    if tag.is_empty() {
                        "as last observed · re-check failed".to_string()
                    } else {
                        format!("{tag} · as last observed · re-check failed")
                    }
                } else {
                    tag
                };

                // 1. Provider label — the row's PRIMARY column, always bold.
                // The provider's own name for the model (`Qwen3.8-Flash`) is
                // what the row reads as; it falls back to the wire id when the
                // catalog publishes no label (the stock OpenAI/Gemini case), so
                // the column is never empty. Identity itself is unaffected by
                // what is drawn: activation, favorites, hiding and every config
                // surface still key on `rm.model` (docs/reference/tui/modals.md
                // "name-first, id-fallback"; ADR-0131 — the display name is
                // presentation, never the wire value).
                let display = rm.name.as_deref().unwrap_or(rm.model.as_str());
                let distinct_name = rm.name.as_deref().filter(|name| *name != rm.model.as_str());
                let label_text = truncate_ellipsis(display, label_col_width);
                // Highlighting indexes what is drawn, per column: a labelled row
                // highlights its name via `match_name`, an unlabelled one its id
                // via `match_id`. A row kept alive by the *other* field (or by
                // the connection name) renders unhighlighted, because its
                // `match_*` is then `None` — never a mis-indexed `m`, whose
                // positions belong to whichever field matched first.
                let label_matched = match_set(if distinct_name.is_some() {
                    rm.match_name.as_ref()
                } else {
                    rm.match_id.as_ref()
                });
                let label_group = RowGroup::fixed().matched_text(
                    &label_text,
                    Style::default()
                        .bg(style.bg)
                        .fg(style.fg)
                        .add_modifier(Modifier::BOLD),
                    Style::default()
                        .bg(style.bg)
                        .fg(if is_selected { style.fg } else { theme.brand() })
                        .add_modifier(Modifier::BOLD),
                    &label_matched,
                    0,
                );

                let mut list_row = ListRow::new(style, body_width).group(label_group);

                // 2. Wire ID — the SECONDARY column, dimmed, anchored at
                // (label_col_width + 2), and only when a name label leads: an
                // unlabelled row already draws the id in column 1. The id must
                // never hide behind the name — it is the string that goes on the
                // wire and into `hidden_models`/favorites/route settings — but
                // it drops entirely when the identity column cannot fund a
                // readable width, since a useless mid-word truncation of the
                // only typeable handle is worse than its absence.
                if distinct_name.is_some() && show_id_column {
                    let id_text = truncate_ellipsis(&rm.model, id_budget);
                    let id_matched = match_set(rm.match_id.as_ref());
                    let id_group = RowGroup::column(id_col).matched_text(
                        &id_text,
                        Style::default().bg(style.bg).fg(style.dim),
                        Style::default()
                            .bg(style.bg)
                            .fg(if is_selected { style.fg } else { theme.brand() })
                            .add_modifier(Modifier::BOLD),
                        &id_matched,
                        0,
                    );
                    list_row = list_row.group(id_group);
                }

                // 3. Connection Name: Starts at ratio(3, 5), dimmed, with connection match highlighting.
                let tag_len = if tag.is_empty() { 0 } else { tag.width() + 2 };
                let conn_budget = body_width
                    .saturating_sub((body_width * 3) / 5 + tag_len + 1)
                    .max(1);
                let conn_text = truncate_ellipsis(&rm.provider_label, conn_budget);
                let conn_matched = match_set(rm.match_connection.as_ref());
                let conn_group = RowGroup::ratio(3, 5).matched_text(
                    &conn_text,
                    Style::default().bg(style.bg).fg(style.dim),
                    Style::default()
                        .bg(style.bg)
                        .fg(if is_selected { style.fg } else { theme.brand() })
                        .add_modifier(Modifier::BOLD),
                    &conn_matched,
                    0,
                );
                list_row = list_row.group(conn_group);

                if !tag.is_empty() {
                    let tag_fg = if is_selected {
                        list_row.fill_fg()
                    } else {
                        theme.info()
                    };
                    list_row = list_row.group(RowGroup::trailing().text(tag, tag_fg, 0));
                }
                body.push(list_row.finish());
            }
        }
    }
    (body, row_line)
}

/// The Models empty-state body: shown when no model exists (no provider configured
/// or no models returned).
pub(crate) fn models_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "No models available",
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Add a connection via ", Style::default().fg(theme.muted())),
            Span::styled(
                "/connections",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (or press ", Style::default().fg(theme.muted())),
            Span::styled(
                "a",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(")", Style::default().fg(theme.muted())),
        ]),
        Line::from(Span::styled(
            "Configured models will appear here",
            Style::default().fg(theme.muted()),
        )),
    ]
}
