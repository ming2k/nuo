//! The flat Models picker modal (selecting the active model/instance).
use nuotc::{
    Frame, Rect, {Line, Span}, {Modifier, Style},
};

use unicode_width::UnicodeWidthStr;

use super::super::common::truncate_ellipsis;
use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, render_pricing_detail_rows,
    search_empty_body, split_search_bottom,
};
use crate::components::detail::DetailLayout;
use crate::components::options::{ChoiceTone, choice_style};
use crate::components::row::{GROUP_GAP, GUTTER, ListRow, RowGroup};
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
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
    /// (`App::caret_owner == CaretOwner::Overlay`). The picker
    /// never places the physical cursor on its own authority — the frame-level
    /// arbiter decides that, and this flag is its verdict threaded down.
    pub show_caret: bool,
    pub refreshing: bool,
    pub spinner_phase: usize,
    pub info_detail: bool,
    pub info_scroll: &'a mut usize,
}

/// Draw the **Models** flat model picker modal (`/models`).
pub fn draw_models_modal(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    props: ModelsModalProps<'_>,
    theme: &Theme,
    selection: &SelectionState,
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
        info_detail,
        info_scroll,
    } = props;
    let area = modal_area(frame, FixedModalSpec::MODELS);
    let f = modal_frame(frame, area, theme, true, true);

    let header_rect = f.header;

    if info_detail && let Some(rm) = models.get(modal_index.min(models.len().saturating_sub(1))) {
        let display = rm.name.as_deref().unwrap_or(rm.model.as_str());
        let breadcrumbs = crate::primitives::breadcrumb_parts("Models", display);
        crate::elevation::modal_header_parts(frame, header_rect, &breadcrumbs, theme);

        let rows = model_detail_body(rm, f.body.width as usize, theme);

        let refresh_label = if refreshing {
            "refreshing…"
        } else {
            "refresh"
        };
        let mut detail_hints: Vec<FooterHint> = Vec::with_capacity(5);
        if rm.usable {
            detail_hints.push(FooterHint::key_primary(
                crate::keymap::Key::ENTER,
                "activate",
            ));
            detail_hints.push(FooterHint::secondary("e", "edit"));
        }
        detail_hints.push(FooterHint::secondary("r", refresh_label));
        detail_hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "list"));

        render_selectable_body(
            frame,
            f.body,
            &rows,
            info_scroll,
            None,
            theme,
            selection,
            layout_map,
        );

        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, &detail_hints, &[], theme);
        }
        return area;
    }

    let refresh_label = if refreshing {
        "refreshing…"
    } else {
        "refresh"
    };
    let browse_hints: [FooterHint; 9] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::secondary("i", "details"),
        FooterHint::secondary("e", "edit"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("x", "block"),
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
    // Connection column positioning: allocate ~67% (2/3) to the primary
    // identity region (label + wire id) so modern descriptive model names
    // (25-35 columns) have ample room, while ensuring the right side funds
    // the connection name and trailing status badge without overflowing.
    let max_conn_len = models
        .iter()
        .map(|m| m.provider_label.width())
        .max()
        .unwrap_or(8);
    let conn_width = max_conn_len.clamp(8, 24);

    let max_tag_len = models
        .iter()
        .map(|rm| {
            let tag = match (rm.thinking, rm.effort.as_deref()) {
                (Some(true), Some(effort)) => format!("think on {effort}"),
                (Some(true), None) => "think on".to_string(),
                (None, Some(effort)) => effort.to_string(),
                _ => String::new(),
            };
            let tag = if !rm.usable {
                if tag.is_empty() {
                    "locked".to_string()
                } else {
                    format!("locked · {tag}")
                }
            } else if rm.availability_overridden {
                if tag.is_empty() {
                    "overridden".to_string()
                } else {
                    format!("overridden · {tag}")
                }
            } else {
                tag
            };
            tag.width()
        })
        .max()
        .unwrap_or(0);

    let tag_demand = if max_tag_len > 0 {
        max_tag_len + GROUP_GAP
    } else {
        0
    };
    let right_demand = conn_width + tag_demand + GROUP_GAP;
    let min_conn_col = (GUTTER + 18 + GROUP_GAP).min(body_width);
    let max_conn_col = body_width.saturating_sub(right_demand);
    let target_conn_col = (body_width * 2) / 3;
    let conn_col = target_conn_col.clamp(min_conn_col, max_conn_col.max(min_conn_col));

    // Usable width for the identity region (before conn_col, minus gutter and gap).
    let usable_identity = conn_col.saturating_sub(GUTTER + GROUP_GAP);
    let label_width = |m: &RankedModel| m.name.as_deref().unwrap_or(m.model.as_str()).width();
    let max_label_len = models.iter().map(label_width).max().unwrap_or(20);
    // Never below 18 (so a short-label list still reads as a column) and never
    // above usable_identity (so the label cannot push or touch the connection column).
    let label_col_width = if usable_identity < 18 {
        usable_identity
    } else {
        max_label_len.clamp(18, usable_identity)
    };

    // Wire ID sits in the secondary column, anchored after the label column + gap.
    let id_col = GUTTER + label_col_width + GROUP_GAP;
    let id_budget = conn_col.saturating_sub(id_col + GROUP_GAP);
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
                // Clean status token leads the tag row so the row stays readable
                // and never overflows into the identity/connection columns.
                // Full verbose reasons are inspected via `i` (model detail).
                let tag = if locked {
                    let verdict = "locked".to_string();
                    if tag.is_empty() {
                        verdict
                    } else {
                        format!("{verdict} · {tag}")
                    }
                } else if rm.availability_overridden {
                    if tag.is_empty() {
                        "overridden".to_string()
                    } else {
                        format!("overridden · {tag}")
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
                // "name-first, id-fallback"; — the display name is
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
                let label_fg = if locked { theme.dim() } else { style.fg };
                let label_match_fg = if locked {
                    theme.dim()
                } else if is_selected {
                    style.fg
                } else {
                    theme.brand()
                };
                let label_modifier = if locked {
                    Modifier::empty()
                } else {
                    Modifier::BOLD
                };
                let label_group = RowGroup::fixed().matched_text(
                    &label_text,
                    Style::default()
                        .bg(style.bg)
                        .fg(label_fg)
                        .add_modifier(label_modifier),
                    Style::default()
                        .bg(style.bg)
                        .fg(label_match_fg)
                        .add_modifier(label_modifier),
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
                    let id_fg = if locked { theme.dim() } else { style.dim };
                    let id_match_fg = if locked {
                        theme.dim()
                    } else if is_selected {
                        style.fg
                    } else {
                        theme.brand()
                    };
                    let id_modifier = if locked {
                        Modifier::empty()
                    } else {
                        Modifier::BOLD
                    };
                    let id_group = RowGroup::column(id_col).matched_text(
                        &id_text,
                        Style::default().bg(style.bg).fg(id_fg),
                        Style::default()
                            .bg(style.bg)
                            .fg(id_match_fg)
                            .add_modifier(id_modifier),
                        &id_matched,
                        0,
                    );
                    list_row = list_row.group(id_group);
                }

                // 3. Connection Name: Starts at conn_col, dimmed, with connection match highlighting.
                let tag_len = if tag.is_empty() {
                    0
                } else {
                    tag.width() + GROUP_GAP
                };
                let conn_budget = body_width
                    .saturating_sub(conn_col + tag_len + GROUP_GAP)
                    .max(1);
                let conn_text = truncate_ellipsis(&rm.provider_label, conn_budget);
                let conn_matched = match_set(rm.match_connection.as_ref());
                let conn_fg = if locked { theme.dim() } else { style.dim };
                let conn_match_fg = if locked {
                    theme.dim()
                } else if is_selected {
                    style.fg
                } else {
                    theme.brand()
                };
                let conn_modifier = if locked {
                    Modifier::empty()
                } else {
                    Modifier::BOLD
                };
                let conn_group = RowGroup::column(conn_col).matched_text(
                    &conn_text,
                    Style::default().bg(style.bg).fg(conn_fg),
                    Style::default()
                        .bg(style.bg)
                        .fg(conn_match_fg)
                        .add_modifier(conn_modifier),
                    &conn_matched,
                    0,
                );
                list_row = list_row.group(conn_group);

                if !tag.is_empty() {
                    let tag_fg = if locked {
                        theme.dim()
                    } else if is_selected {
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

/// The model detail sub-view body: renders identity, capabilities, pricing, and live configuration or availability reason.
pub(crate) fn model_detail_body(
    rm: &RankedModel,
    _body_width: usize,
    theme: &Theme,
) -> Vec<SelectableRow> {
    let mut layout = DetailLayout::new(theme);
    let value_style = Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD);

    // 1. Identity section
    layout = layout.section("Identity");
    if let Some(name) = &rm.name {
        layout = layout.kv_styled("Display Name", name, value_style);
    }
    layout = layout.kv_styled("Wire ID", &rm.model, value_style);
    layout = layout.kv(
        "Connection",
        format!("{} ({})", rm.provider_label, rm.provider_id),
    );

    // 2. Capabilities section
    layout = layout.section("Capabilities");
    layout = layout.kv("Context Window", format!("{} tokens", rm.context_window));
    if !rm.effort_levels.is_empty() {
        layout = layout.kv("Reasoning Tiers", rm.effort_levels.join(" · "));
    }

    // 3. Pricing section (if declared or recognized)
    if let Some(pricing_rows) =
        render_pricing_detail_rows(&rm.provider_id, &rm.model, None, "Pricing", theme)
    {
        layout = layout.raw_rows(pricing_rows);
    }

    // 4. Status or Configuration section
    if rm.usable {
        layout = layout.section("Configuration");
        if !rm.effort_levels.is_empty() || rm.effort.is_some() {
            let current_effort = rm.effort.as_deref().unwrap_or("medium");
            layout = layout.kv_styled("Reasoning Effort", current_effort, value_style);
        }
        if let Some(thinking) = rm.thinking {
            let check = if thinking { "on" } else { "off" };
            let style = if thinking {
                Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.dim())
            };
            layout = layout.kv_styled("Thinking", check, style);
        }
        if rm.effort_levels.is_empty() && rm.effort.is_none() && rm.thinking.is_none() {
            layout = layout.kv("Mode", "Standard");
        }
    } else {
        layout = layout.section("Availability");
        layout = layout.kv_styled(
            "Status",
            "Locked",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        );
        let reason_text = rm
            .locked_reason
            .as_deref()
            .unwrap_or("Model requires subscription or account entitlement.");
        layout = layout.kv_muted("Reason", reason_text);
        if rm.availability_overridden {
            layout = layout.kv_styled(
                "Scope Override",
                "Allowed by user scope",
                Style::default().fg(theme.ok()),
            );
        }
        if rm.availability_stale {
            layout = layout.kv_styled(
                "Notice",
                "Verdict observed before catalog refresh failure; may be stale upstream.",
                Style::default().fg(theme.dim()),
            );
        }
    }

    layout.build()
}
