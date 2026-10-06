//! Tool step renderer (tool invocation summaries, expanded bodies, diffs, results).

use nuotc::{
    Color, Modifier, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::super::{Disclosure, Interaction, summary_text_color};
use super::base::{RenderCtx, nonempty_wrapped};
use super::payloads::{
    draw_blank_rows, draw_semantic_step_summary, draw_step_summary, draw_tool_result,
};
use super::sticky::StickyStep;
use crate::message_body::draw_message_body;
use crate::model::document::TranscriptMessage;
use crate::model::selection::{CellDragInfo, SelectionState};
use crate::render::{
    STEP_MIN_WIDTH, TOOL_STEP_BODY_INDENT_COLS, TOOL_STEP_BODY_TOP_GAP_ROWS,
    TOOL_STEP_CHILDREN_GAP_ROWS,
};
use crate::text_layout::{padded_tail, wrap_text};
use crate::tools::{ArgLayout, DiffCache, ToolStatus};

#[allow(clippy::too_many_arguments)]
pub fn draw_tool_step(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    diff_cache: &mut DiffCache,
    sticky_steps: &mut Vec<StickyStep>,
    hovered: bool,
    focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(summary) = msg.tool_step_summary() else {
        return;
    };
    let expanded = msg.tool_step_expanded() == Some(true);

    // Run state is conveyed by color alone: muted while running, red on
    // failure, dim when cancelled, and weight-only on success.
    // There is no status glyph or per-tool icon in the summary. The summary
    // text color is resolved through the shared state machine: a non-completed
    // lifecycle supplies an accent that supplies the hue while the disclosure ×
    // interaction weight channel modulates its brightness; the completed case
    // yields no accent and falls fully through to the weight ladder so a
    // finished call reads as calm when idle — bright (primary foreground) while
    // its body is open, the hover tone while focused or under the pointer, and
    // muted when collapsed and idle.
    //
    // The activity bar is the single breathing anchor (ADR 0008); per-step
    // liveness rides on hue alone so a transcript full of running steps does
    // not flash in unison and steal attention from the content the user is
    // reading.
    let status = msg
        .tool_step_status()
        .map(ToolStatus::from_status)
        .unwrap_or(ToolStatus::Running);
    // Tool steps render flat on the app background (no band) — like
    // reasoning traces, only the optional content block carries a `code_bg`.
    let summary_bg = ctx.theme.surface();
    let status_color = status.color(ctx.theme);
    let accent = match status {
        ToolStatus::Ok => None,
        _ => Some(status_color),
    };
    let summary_color = summary_text_color(
        accent,
        Disclosure::from_expanded(expanded),
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    // `ctx.area` arrives already inset by `draw_transcript` (the
    // uniform horizontal gutters are applied once at the stream entry point),
    // so all helpers below read `ctx.area.x` / `.width` directly.
    let full_width = ctx.area.width as usize;
    if full_width < STEP_MIN_WIDTH {
        // Too narrow to draw; fall back to plain block rendering.
        draw_message_body(
            &mut *ctx.frame,
            ctx.area,
            msg,
            mi,
            selection,
            cell_selection,
            ctx.theme,
            &mut *ctx.layout_map,
            &mut *ctx.skip_rows,
            ctx.y,
            &mut *ctx.content_lines,
            true,
            ctx.wrap,
        );
        return;
    }

    let inner_width = ctx.area.width as usize;
    let summary_line_idx = {
        if let Some((semantic_line, suffix)) = msg.tool_step_semantic_summary(ctx.workspace_root) {
            let suffix_style = Style::default()
                .fg(summary_color)
                .add_modifier(Modifier::BOLD);
            let suffix_pair = suffix.as_deref().map(|s| (s, suffix_style));
            draw_semantic_step_summary(
                ctx,
                mi,
                usize::MAX,
                expanded,
                &semantic_line,
                suffix_pair,
                summary_color,
                summary_bg,
            )
        } else {
            draw_step_summary(
                ctx,
                mi,
                usize::MAX,
                expanded,
                &summary,
                summary_color,
                summary_bg,
            )
        }
    };

    // Body region (only when expanded). Tool steps are flat — no band, no
    // Tool/Arguments/Result labels — so an expanded step reads like a log entry:
    // the tool-specific content directly under the summary (bash → `$ cmd` +
    // output; list/search → entries; edit/write → diff; read → code), indented to
    // align with prose. Only content blocks carry a `code_bg`; everything else
    // sits on the app background.
    if expanded {
        let surface = ctx.theme.surface();
        let pad = Style::default().bg(surface);
        let indent = TOOL_STEP_BODY_INDENT_COLS;
        let inner_w = inner_width.saturating_sub(indent);

        {
            draw_blank_rows(ctx, pad, TOOL_STEP_BODY_TOP_GAP_ROWS);

            if let crate::model::document::MessageKind::ToolStep {
                name,
                arguments,
                output,
                structured,
                ..
            } = &msg.kind
            {
                // Unknown / MCP tools spell out their arguments as `key: value`
                // rows (the summary only carries the primary one). No label — the
                // key names are self-describing, and the result block below
                // carries its own `code_bg` so the two stay visually distinct.
                if matches!(
                    crate::tools::presenter_for(name).arg_layout(),
                    ArgLayout::KeyValue
                ) {
                    let kv = crate::model::document::parse_arguments_kv(arguments);
                    if !kv.is_empty() {
                        let kv_style = Style::default().bg(surface).fg(ctx.theme.muted());
                        let wrap_w = inner_w.max(1);
                        for (k, v) in &kv {
                            let row = format!("{}: {}", k, v);
                            for wl in nonempty_wrapped(wrap_text(&row, wrap_w)) {
                                let used = indent + wl.text.width();
                                let line = Line::from(vec![
                                    Span::styled(" ".repeat(indent), pad),
                                    Span::styled(wl.text.clone(), kv_style),
                                    Span::styled(padded_tail(ctx.full_width, used), pad),
                                ]);
                                let _ = ctx.paint(line);
                            }
                        }
                    }
                }

                // Tool-specific content (label-free). bash renders `$ cmd` +
                // output; others their block. A streaming or freshly-spawned command
                // step renders its `$ cmd` and live streaming output.
                let has_output = output.as_deref().is_some_and(|s| !s.is_empty());
                let is_command = name.as_str() == "execute_command";
                let has_structured = structured.is_some();
                if has_output || is_command || has_structured {
                    draw_tool_result(
                        ctx,
                        mi,
                        msg.id,
                        name,
                        arguments,
                        output.as_deref().unwrap_or(""),
                        structured.as_deref(),
                        diff_cache,
                        selection,
                        indent,
                        inner_w,
                    );
                }
            }
        }

        // Nested subagent children
        if let crate::model::document::MessageKind::ToolStep { children, .. } = &msg.kind {
            if !children.is_empty() {
                draw_blank_rows(ctx, pad, TOOL_STEP_CHILDREN_GAP_ROWS);
            }
            for child in children {
                if child.is_tool_step() {
                    draw_child_tool_step(ctx, child, status_color);
                } else {
                    let remaining_height = ctx
                        .area
                        .y
                        .saturating_add(ctx.area.height)
                        .saturating_sub(*ctx.y);
                    let child_area = Rect::new(
                        ctx.area.x + 6,
                        *ctx.y,
                        ctx.area.width.saturating_sub(12),
                        remaining_height,
                    );
                    draw_message_body(
                        &mut *ctx.frame,
                        child_area,
                        child,
                        usize::MAX,
                        selection,
                        cell_selection,
                        ctx.theme,
                        &mut *ctx.layout_map,
                        &mut *ctx.skip_rows,
                        ctx.y,
                        &mut *ctx.content_lines,
                        false,
                        ctx.wrap,
                    );
                }
            }
        }

        // No trailing bottom gap here: the layout resolves the semantic
        // boundary to the next component. Same-turn tool siblings use zero
        // rows; every other segment uses one, independent of disclosure state.
    }

    if expanded {
        sticky_steps.push(StickyStep {
            message_idx: mi,
            summary: summary.to_string(),
            color: status_color,
            background: Some(ctx.theme.surface()),
            summary_line: summary_line_idx,
            body_end_line: *ctx.content_lines,
        });
    }
}

/// Render a nested child tool step as a compact summary line plus its output.
fn draw_child_tool_step(
    ctx: &mut RenderCtx<'_, '_>,
    child: &TranscriptMessage,
    status_color: Color,
) {
    let Some(summary) = child.tool_step_summary() else {
        return;
    };
    let surface = ctx.theme.surface();
    let full_width = ctx.full_width;
    let indent = 6usize;
    let bg_style = Style::default().bg(surface);

    let summary_text = summary.to_string();
    let summary_lines = wrap_text(&summary_text, full_width.saturating_sub(indent));
    for wl in &summary_lines {
        let used = indent + wl.text.width();
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), bg_style),
            Span::styled(wl.text.clone(), bg_style.fg(status_color)),
            Span::styled(padded_tail(full_width, used), bg_style),
        ]);
        let _ = ctx.paint(line);
    }

    if let crate::model::document::MessageKind::ToolStep {
        output: Some(output),
        ..
    } = &child.kind
    {
        let output_lines = wrap_text(output, full_width.saturating_sub(indent + 1));
        for wl in &output_lines {
            let used = indent + wl.text.width();
            let line = Line::from(vec![
                Span::styled(" ".repeat(indent), bg_style),
                Span::styled(wl.text.clone(), bg_style.fg(ctx.theme.fg())),
                Span::styled(padded_tail(full_width, used), bg_style),
            ]);
            let _ = ctx.paint(line);
        }
    }
}
