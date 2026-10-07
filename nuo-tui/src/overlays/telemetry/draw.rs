//! Telemetry modal orchestrator: chrome, breadcrumbs, and level routing.
//!
//! Rendering is split by level:
//! - `overview` — Overview tab (L1)
//! - `tables`   — Activity tab rounds/turns tables (L1/L2)
//! - `attempt`  — Attempt inspector with the latency timeline (L3)

use nuo_wire::TokenSourceReport;
use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use super::super::common::placeholder;
use super::TelemetryTab;
use super::attempt::build_attempt_inspector_body;
use super::model::*;
use super::overview::build_overview_body;
use super::tables::{build_rounds_table, build_turns_table};
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    BodyRenderOptions, ContentModalSpec, FooterHint, HeaderPart, breadcrumb_parts,
    content_modal_area, content_modal_probe, hierarchical_breadcrumb, keyvocab, modal_chrome_rows,
    modal_frame, modal_header_parts, render_body, render_modal_footer,
};
use crate::render::Theme;

#[allow(clippy::too_many_arguments)]
pub fn draw_telemetry_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageProps,
    tab: TelemetryTab,
    selected: usize,
    detail: bool,
    turn: Option<(u32, u32)>,
    turn_cursor: usize,
    submitted_at_ms: Option<u64>,
    loading: bool,
    scroll: &mut usize,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let geometry = ContentModalSpec::TELEMETRY;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    if loading {
        let area = content_modal_area(frame, geometry, 7);
        let modal = modal_frame(frame, area, theme, true, true);
        modal_header_parts(
            frame,
            modal.header,
            &[HeaderPart::title("Session Stats")],
            theme,
        );
        let body = vec![placeholder(
            "Loading session stats from server…",
            true,
            theme.muted(),
        )];
        render_body(
            frame,
            modal.body,
            body,
            scroll,
            BodyRenderOptions::follow(None),
            theme,
        );
        if let Some(footer_area) = modal.footer {
            render_modal_footer(
                frame,
                footer_area,
                &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
                theme,
            );
        }
        return area;
    }

    let rounds = extract_telemetry_rounds(report);
    let round_num = rounds.get(selected).map_or(0, |r| r.round_number);
    let round_child = if detail || turn.is_some() {
        if let Some((target_round, _)) = turn {
            format!("Round #{target_round}")
        } else {
            format!("Round #{round_num} Turns")
        }
    } else {
        String::new()
    };
    let turn_child = if let Some((_, target_attempt)) = turn {
        format!("Attempt #{target_attempt}")
    } else {
        String::new()
    };

    if let Some((target_round, target_attempt)) = turn {
        // L3: Attempt Inspector
        let levels = ["Session Stats", round_child.as_str(), turn_child.as_str()];
        let header = hierarchical_breadcrumb(&levels, body_width);
        let body = build_attempt_inspector_body(
            &rounds,
            target_round,
            target_attempt,
            context,
            body_width,
            submitted_at_ms,
            theme,
        );
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
            FooterHint::key_always(crate::keymap::Key::ESC, "turns"),
        ];

        let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme, true, true);
        modal_header_parts(frame, modal.header, &header, theme);

        let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();
        render_selectable_body(
            frame, modal.body, &rows, scroll, None, theme, selection, layout_map,
        );
        if let Some(footer_area) = modal.footer {
            render_modal_footer(frame, footer_area, &footer, theme);
        }
        area
    } else if detail {
        // L2: Turn List with Sticky Header
        let header = breadcrumb_parts("Session Stats", &round_child).to_vec();
        let (table_header, rows, follow) =
            build_turns_table(&rounds, selected, turn_cursor, body_width, theme);
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "select"),
            FooterHint::key_always(crate::keymap::Key::ENTER, "inspect"),
            FooterHint::key_always(crate::keymap::Key::ESC, "rounds"),
        ];

        let desired = (rows.len() + 1) as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme, true, true);
        modal_header_parts(frame, modal.header, &header, theme);

        let header_h = 1.min(modal.body.height);
        let header_rect = Rect {
            x: modal.body.x,
            y: modal.body.y,
            width: modal.body.width,
            height: header_h,
        };
        let mut header_scroll = 0;
        render_body(
            frame,
            header_rect,
            table_header,
            &mut header_scroll,
            BodyRenderOptions::follow(None),
            theme,
        );

        let table_rect = Rect {
            x: modal.body.x,
            y: modal.body.y.saturating_add(header_h),
            width: modal.body.width,
            height: modal.body.height.saturating_sub(header_h),
        };
        render_body(
            frame,
            table_rect,
            rows,
            scroll,
            BodyRenderOptions::follow(follow),
            theme,
        );

        if let Some(footer_area) = modal.footer {
            render_modal_footer(frame, footer_area, &footer, theme);
        }
        area
    } else {
        // L1: Top Level Tabs (Overview vs Activity)
        let header = vec![HeaderPart::title("Session Stats")];

        match tab {
            TelemetryTab::Overview => {
                let tab_strip = vec![tab_strip_line(tab, rounds.len(), theme), Line::from("")];
                let overview = build_overview_body(report, &rounds, context, body_width, theme);
                let body_lines: Vec<Line<'static>> =
                    tab_strip.into_iter().chain(overview).collect();
                let footer = [
                    FooterHint::key_always(crate::keymap::Key::TAB, "2 Activity"),
                    FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                    FooterHint::key_always(crate::keymap::Key::ENTER, "activity"),
                    FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                ];

                let desired = body_lines.len() as u16 + modal_chrome_rows(geometry.modal_spec());
                let area = content_modal_area(frame, geometry, desired);
                let modal = modal_frame(frame, area, theme, true, true);
                modal_header_parts(frame, modal.header, &header, theme);

                let rows: Vec<SelectableRow> = body_lines
                    .into_iter()
                    .map(SelectableRow::from_line)
                    .collect();
                render_selectable_body(
                    frame, modal.body, &rows, scroll, None, theme, selection, layout_map,
                );

                if let Some(footer_area) = modal.footer {
                    render_modal_footer(frame, footer_area, &footer, theme);
                }
                area
            }
            TelemetryTab::Activity => {
                let tab_strip = vec![tab_strip_line(tab, rounds.len(), theme), Line::from("")];
                let (table_header, rows, follow) =
                    build_rounds_table(&rounds, selected, body_width, theme);
                let footer = [
                    FooterHint::key_always(crate::keymap::Key::TAB, "1 Overview"),
                    FooterHint::always(keyvocab::ARROWS_UD, "select"),
                    FooterHint::key_always(crate::keymap::Key::ENTER, "turns"),
                    FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                ];

                let desired = (rows.len() + 3) as u16 + modal_chrome_rows(geometry.modal_spec());
                let area = content_modal_area(frame, geometry, desired);
                let modal = modal_frame(frame, area, theme, true, true);
                modal_header_parts(frame, modal.header, &header, theme);

                // 1. Tab strip (fixed at top)
                let tab_h = 2.min(modal.body.height);
                let tab_rect = Rect {
                    x: modal.body.x,
                    y: modal.body.y,
                    width: modal.body.width,
                    height: tab_h,
                };
                let mut tab_scroll = 0;
                render_body(
                    frame,
                    tab_rect,
                    tab_strip,
                    &mut tab_scroll,
                    BodyRenderOptions::follow(None),
                    theme,
                );

                // 2. Sticky table header (fixed right below tab strip)
                let header_h = 1.min(modal.body.height.saturating_sub(tab_h));
                let header_rect = Rect {
                    x: modal.body.x,
                    y: modal.body.y.saturating_add(tab_h),
                    width: modal.body.width,
                    height: header_h,
                };
                let mut header_scroll = 0;
                render_body(
                    frame,
                    header_rect,
                    table_header,
                    &mut header_scroll,
                    BodyRenderOptions::follow(None),
                    theme,
                );

                // 3. Scrollable table body
                let fixed_h = tab_h.saturating_add(header_h);
                let table_rect = Rect {
                    x: modal.body.x,
                    y: modal.body.y.saturating_add(fixed_h),
                    width: modal.body.width,
                    height: modal.body.height.saturating_sub(fixed_h),
                };
                render_body(
                    frame,
                    table_rect,
                    rows,
                    scroll,
                    BodyRenderOptions::follow(follow),
                    theme,
                );

                if let Some(footer_area) = modal.footer {
                    render_modal_footer(frame, footer_area, &footer, theme);
                }
                area
            }
        }
    }
}

pub(crate) fn tab_strip_line(
    active_tab: TelemetryTab,
    rounds_count: usize,
    theme: &Theme,
) -> Line<'static> {
    let (ov_style, act_style) = match active_tab {
        TelemetryTab::Overview => (
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.muted()),
        ),
        TelemetryTab::Activity => (
            Style::default().fg(theme.muted()),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
    };
    let rounds_suffix = if rounds_count > 0 {
        format!(" ({rounds_count})")
    } else {
        String::new()
    };
    Line::from(vec![
        Span::styled("  ", Style::default()),
        if active_tab == TelemetryTab::Overview {
            Span::styled("[ 1 Overview ]", ov_style)
        } else {
            Span::styled("  1 Overview  ", ov_style)
        },
        Span::styled("    ", Style::default()),
        if active_tab == TelemetryTab::Activity {
            Span::styled(format!("[ 2 Activity{rounds_suffix} ]"), act_style)
        } else {
            Span::styled(format!("  2 Activity{rounds_suffix}  "), act_style)
        },
    ])
}
