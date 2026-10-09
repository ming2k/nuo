//! Telemetry overlays: Session Stats and Session Trace modal orchestrators.
//!
//! Separated into two distinct surfaces (ADR-0037):
//! - `draw_session_stats_modal` — Session Stats (/stats)
//! - `draw_session_trace_modal` — Session Trace (/trace) with hierarchical drill-in (L1/L2/L3)

use nuo_wire::TokenSourceReport;
use nuotc::{Frame, Rect};

use super::attempt::build_attempt_inspector_body;
use super::model::*;
use super::overview::build_overview_body;
use super::tables::{build_rounds_table, build_turns_table};
use super::super::common::placeholder;
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

/// Draw the Session Stats modal (context window utilization, token accounting).
#[allow(clippy::too_many_arguments)]
pub fn draw_session_stats_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageProps,
    loading: bool,
    scroll: &mut usize,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let geometry = ContentModalSpec::SESSION_STATS;
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
    let header = vec![HeaderPart::title("Session Stats")];
    let overview = build_overview_body(report, &rounds, context, body_width, theme);
    let footer = [
        FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
        FooterHint::key_always(crate::keymap::Key::plain('t'), "trace"),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];

    let desired = overview.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme, true, true);
    modal_header_parts(frame, modal.header, &header, theme);

    let rows: Vec<SelectableRow> = overview
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

/// Draw the Session Trace modal (hierarchical rounds -> turns -> attempt waterfall profiler).
#[allow(clippy::too_many_arguments)]
pub fn draw_session_trace_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageProps,
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
    let geometry = ContentModalSpec::SESSION_TRACE;
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
            &[HeaderPart::title("Session Trace")],
            theme,
        );
        let body = vec![placeholder(
            "Loading session trace from server…",
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
        let levels = ["Session Trace", round_child.as_str(), turn_child.as_str()];
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
        let header = breadcrumb_parts("Session Trace", &round_child).to_vec();
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
        // L1: Rounds Table with Sticky Header (Zero tab strip!)
        let header = vec![HeaderPart::title("Session Trace")];
        let (table_header, rows, follow) =
            build_rounds_table(&rounds, selected, body_width, theme);
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "select"),
            FooterHint::key_always(crate::keymap::Key::ENTER, "turns"),
            FooterHint::key_always(crate::keymap::Key::ESC, "close"),
        ];

        let desired = (rows.len() + 1) as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme, true, true);
        modal_header_parts(frame, modal.header, &header, theme);

        // 1. Sticky table header
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

        // 2. Scrollable table body
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
    }
}
