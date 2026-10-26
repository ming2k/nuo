//! Shared helper routines and search components for provider modals.

use nuotc::{
    Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

use super::super::common::{caret_column, field_viewport};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::detail::{detail_kv_row, detail_section_row};
use nuo_view::components::selectable_body::SelectableRow;

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
        &mut Paragraph::new(Line::from(vec![
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

pub(crate) fn match_set(
    m: Option<&nuo_ui_primitives::fuzzy::FuzzyMatch>,
) -> std::collections::HashSet<usize> {
    m.map(|fm| fm.positions.iter().copied().collect())
        .unwrap_or_default()
}

/// Renders standard model pricing and rates section for inspector/detail sub-views.
///
/// Shared SSOT between Connection Details (for active model pricing) and Model Info Details.
pub(crate) fn render_pricing_detail_rows(
    provider_id: &str,
    model: &str,
    model_prefix: Option<&str>,
    section_title: &str,
    theme: &Theme,
) -> Option<Vec<SelectableRow>> {
    let pricing = nuo_wire::pricing::resolve_model_pricing(provider_id, model)?;
    let mut rows = Vec::new();
    rows.push(SelectableRow::empty());
    rows.push(detail_section_row(section_title, theme));

    if pricing
        .tiers
        .iter()
        .all(|t| t.input_standard_nano_per_m == 0 && t.output_standard_nano_per_m == 0)
    {
        let msg = match model_prefix {
            Some(prefix) => format!("{prefix} is plan-included (zero token surcharge)"),
            None => "Plan included (zero token surcharge)".to_string(),
        };
        rows.push(detail_kv_row(
            if model_prefix.is_some() {
                "Billing"
            } else {
                "Rate"
            },
            &msg,
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            theme,
        ));
    } else {
        let base_tier = &pricing.tiers[0];
        let in_str = nuo_wire::pricing::NanoUsd(base_tier.input_standard_nano_per_m).format_rate();
        let out_str =
            nuo_wire::pricing::NanoUsd(base_tier.output_standard_nano_per_m).format_rate();
        let tiered_suffix = if pricing.tiers.len() > 1 {
            ", tiered"
        } else {
            ""
        };
        let base_desc = match model_prefix {
            Some(prefix) => {
                format!("{prefix}: {in_str} in · {out_str} out (per 1M tokens{tiered_suffix})")
            }
            None => format!("{in_str} in · {out_str} out (per 1M tokens{tiered_suffix})"),
        };
        rows.push(detail_kv_row(
            "Standard Rates",
            &base_desc,
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            theme,
        ));

        if base_tier.cache_read_nano_per_m > 0 {
            let hit_str = nuo_wire::pricing::NanoUsd(base_tier.cache_read_nano_per_m).format_rate();
            let discount_pct = if base_tier.input_standard_nano_per_m > 0 {
                let saved = base_tier
                    .input_standard_nano_per_m
                    .saturating_sub(base_tier.cache_read_nano_per_m);
                (saved as f64 / base_tier.input_standard_nano_per_m as f64 * 100.0).round() as u32
            } else {
                0
            };
            let hit_val = if discount_pct > 0 {
                format!("{hit_str} / 1M ({discount_pct}% discount)")
            } else {
                format!("{hit_str} / 1M")
            };
            rows.push(detail_kv_row(
                "Prompt Cache Hit",
                &hit_val,
                Style::default().fg(theme.fg()),
                theme,
            ));
        }

        if let Some(off_peak) = &pricing.off_peak {
            let op_tier = &off_peak.tiers[0];
            let op_in = nuo_wire::pricing::NanoUsd(op_tier.input_standard_nano_per_m).format_rate();
            let op_out =
                nuo_wire::pricing::NanoUsd(op_tier.output_standard_nano_per_m).format_rate();
            rows.push(detail_kv_row(
                "Off-Peak Rates",
                &format!("50% off ({op_in} in · {op_out} out)"),
                Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD),
                theme,
            ));
            rows.push(detail_kv_row(
                "Off-Peak Window",
                "Mon–Fri outside 09:00–12:00, 14:00–18:00 (UTC+8); weekends & holidays all day",
                Style::default().fg(theme.muted()),
                theme,
            ));
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let window = off_peak
                .schedule
                .window_at(nuo_wire::pricing::RatingInstant::from_epoch_ms(now_ms));
            let (win_label, win_style) = if window.is_off_peak() {
                (
                    "Off-Peak (Active)",
                    Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD),
                )
            } else {
                ("Peak", Style::default().fg(theme.dim()))
            };
            rows.push(detail_kv_row("Current State", win_label, win_style, theme));
        }
    }

    Some(rows)
}
