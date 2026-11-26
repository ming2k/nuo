//! Telemetry overlays: Thread Stats and Session Trace modal orchestrators.
//!
//! Separated into two distinct surfaces (ADR-0037, ADR-0263):
//! - `draw_thread_stats_modal` — Thread Stats (/stats)
//! - `draw_session_trace_modal` — Session Trace (/trace) with orthogonal
//!   `Enter` drill-in (`Rounds` -> `Turns`) and `i` inspection (`RoundInfo` / `AttemptInfo`)

use std::collections::HashMap;

use graticule::{Frame, Rect};
use protocol::TokenSourceReport;

use super::super::common::placeholder;
use super::attempt::build_attempt_inspector_body;
use super::model::*;
use super::overview::build_overview_body;
use super::round::build_round_inspector_body;
use super::tables::{build_rounds_table, build_turns_table};
use crate::shell::render::Theme;
use crate::shell::surfaces::TraceView;
use ui_primitives::design::MODAL_INNER_H_PADDING;
use ui_primitives::primitives::{
    BodyRenderOptions, ContentModalSpec, FooterHint, HeaderPart, breadcrumb_parts,
    content_modal_area, content_modal_probe, hierarchical_breadcrumb, keyvocab, modal_chrome_rows,
    modal_frame, modal_header_parts, render_body, render_modal_footer,
};
use view::components::selectable_body::{SelectableRow, render_selectable_body};
use view::model::document::TranscriptMessage;
use view::model::layout::LayoutMap;
use view::model::selection::SelectionState;

/// Draw the Thread Stats modal (context window utilization, token accounting).
#[allow(clippy::too_many_arguments)]
pub fn draw_thread_stats_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageProps,
    loading: bool,
    scroll: &mut usize,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let geometry = ContentModalSpec::THREAD_STATS;
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
            &[HeaderPart::title("Thread Stats")],
            theme,
        );
        let body = vec![placeholder(
            "Loading thread stats from server…",
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
                &[FooterHint::always(
                    crate::shell::keymap::Key::ESC.display(),
                    "close",
                )],
                theme,
            );
        }
        return area;
    }

    let rounds = extract_telemetry_rounds(report);
    let header = vec![HeaderPart::title("Thread Stats")];
    let rows = build_overview_body(report, &rounds, context, body_width, theme);
    let footer = [
        FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
        FooterHint::always(crate::shell::keymap::Key::plain('t').display(), "trace"),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
    ];

    let desired = rows.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme, true, true);
    modal_header_parts(frame, modal.header, &header, theme);

    render_selectable_body(
        frame, modal.body, &rows, scroll, None, theme, selection, layout_map,
    );

    if let Some(footer_area) = modal.footer {
        render_modal_footer(frame, footer_area, &footer, theme);
    }
    area
}

/// Draw the Session Trace modal (ADR-0263: `Enter` enters child containers,
/// `i` inspects the highlighted round or attempt, `Esc` unwinds one layer).
#[allow(clippy::too_many_arguments)]
pub fn draw_session_trace_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    round_summaries: &HashMap<u64, protocol::RoundSummary>,
    messages: &[TranscriptMessage],
    context: ContextUsageProps,
    selected: usize,
    view: &TraceView,
    turn_cursor: usize,
    submitted_at_ms: Option<u64>,
    loading: bool,
    scroll: &mut usize,
    info_scroll: &mut usize,
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
                &[FooterHint::always(
                    crate::shell::keymap::Key::ESC.display(),
                    "close",
                )],
                theme,
            );
        }
        return area;
    }

    let rounds = extract_telemetry_rounds(report);
    // Durable summaries carried on `TokenSourceReport` are the authoritative
    // baseline (surviving restarts/reattaches); live `ThreadInstance` summaries
    // overlay any in-session completion before the next ledger snapshot lands.
    let mut merged_summaries: HashMap<u64, protocol::RoundSummary> = report
        .round_summaries
        .iter()
        .copied()
        .map(|s| (s.round, s))
        .collect();
    for (&round, &summary) in round_summaries {
        merged_summaries.insert(round, summary);
    }
    let selected = selected.min(rounds.len().saturating_sub(1));
    let round_num = rounds.get(selected).map_or(0, |r| r.round_number);

    match view {
        TraceView::AttemptInfo(target) => {
            let round_label = format!("Round #{}", target.round);
            let turn_prefix = if protocol::token_ledger::is_root_actor(&target.actor_id) {
                "Turn"
            } else {
                "sub Turn"
            };
            let turn_label = if target.attempt > 1 {
                format!("{turn_prefix} #{}.{}", target.turn, target.attempt)
            } else {
                format!("{turn_prefix} #{}", target.turn)
            };
            let levels = ["Session Trace", round_label.as_str(), turn_label.as_str()];
            let header = hierarchical_breadcrumb(&levels, body_width);
            let body = build_attempt_inspector_body(
                &rounds,
                target,
                context,
                body_width,
                submitted_at_ms,
                theme,
            );
            let footer = [
                FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "turns"),
            ];

            let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
            let area = content_modal_area(frame, geometry, desired);
            let modal = modal_frame(frame, area, theme, true, true);
            modal_header_parts(frame, modal.header, &header, theme);

            let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();
            render_selectable_body(
                frame,
                modal.body,
                &rows,
                info_scroll,
                None,
                theme,
                selection,
                layout_map,
            );
            if let Some(footer_area) = modal.footer {
                render_modal_footer(frame, footer_area, &footer, theme);
            }
            area
        }
        TraceView::RoundInfo => {
            let round_label = format!("Round #{round_num}");
            let header = breadcrumb_parts("Session Trace", &round_label).to_vec();
            let rows = match rounds.get(selected) {
                Some(round) => {
                    let recap = extract_round_transcript_recap(messages, round.round_number);
                    let summary = merged_summaries.get(&round.round_number);
                    build_round_inspector_body(round, summary, &recap, body_width, theme)
                }
                None => vec![SelectableRow::from_line(placeholder(
                    "Round record not found.",
                    false,
                    theme.muted(),
                ))],
            };
            let footer = [
                FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "rounds"),
            ];

            let desired = rows.len() as u16 + modal_chrome_rows(geometry.modal_spec());
            let area = content_modal_area(frame, geometry, desired);
            let modal = modal_frame(frame, area, theme, true, true);
            modal_header_parts(frame, modal.header, &header, theme);

            render_selectable_body(
                frame,
                modal.body,
                &rows,
                info_scroll,
                None,
                theme,
                selection,
                layout_map,
            );
            if let Some(footer_area) = modal.footer {
                render_modal_footer(frame, footer_area, &footer, theme);
            }
            area
        }
        TraceView::Turns => {
            let round_child = format!("Round #{round_num} Turns");
            let header = breadcrumb_parts("Session Trace", &round_child).to_vec();
            let (table_header, rows, follow) =
                build_turns_table(&rounds, selected, turn_cursor, body_width, theme);
            let footer = [
                FooterHint::always(keyvocab::ARROWS_UD, "select"),
                FooterHint::always(crate::shell::keymap::Key::plain('i').display(), "inspect"),
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "rounds"),
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
        }
        TraceView::Rounds => {
            let header = vec![HeaderPart::title("Session Trace")];
            let (table_header, rows, follow) =
                build_rounds_table(&rounds, &merged_summaries, selected, body_width, theme);
            let footer = [
                FooterHint::always(keyvocab::ARROWS_UD, "select"),
                FooterHint::always(crate::shell::keymap::Key::ENTER.display(), "turns"),
                FooterHint::always(crate::shell::keymap::Key::plain('i').display(), "inspect"),
                FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
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
}
