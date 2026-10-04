//! Slash-command execution results and durable ack bodies.

use nuotc::{
    Color, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::base::RenderCtx;
use super::super::{Disclosure, Interaction, summary_text_color};
use crate::design::TURN_HEADER_BODY_GAP_ROWS;
use crate::message_body::draw_message_body;
use crate::model::document::{CommandPhase, TranscriptMessage};
use crate::model::layout::{BlockRegion, COMMAND_RESULT_BLOCK_IDX};
use crate::model::selection::{CellDragInfo, SelectionState};
use crate::render::TRANSCRIPT_BODY_LEADING_INDENT;
use crate::text_layout::wrap_text;

pub fn draw_command_result(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    hovered: bool,
    focused: bool,
) {
    let Some(invocation) = msg.command_result_summary() else {
        return;
    };
    let phase = msg
        .command_result_phase()
        .unwrap_or(CommandPhase::Completed);
    let full_width = ctx.area.width as usize;

    if full_width < (TRANSCRIPT_BODY_LEADING_INDENT as usize + 1) {
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

    let time_label = msg.sent_at_ms.map(crate::time::sent_time_label);

    // One component in two lifecycle states (ADR-0108/ADR-0111), deliberately
    // *not* an expandable step: the invocation and its reply are a single
    // entry, so it carries no disclosure marker and no folding state. The
    // interaction channel tints the header so a hovered or focused command is
    // visibly the component under attention (ADR-0174).
    let header_tone = summary_text_color(
        Some((*ctx.theme).info()),
        Disclosure::Expanded,
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );
    let header_line = command_header_line(
        "command",
        header_tone,
        time_label.as_deref(),
        ctx.theme.muted(),
        full_width,
    );
    {
        if let Some(rect) = ctx.paint(header_line) {
            ctx.layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx: COMMAND_RESULT_BLOCK_IDX,
                start_byte: 0,
                end_byte: 0,
                text: String::new(),
                prefix_cols: 0,
                rect,
                hidden_ranges: Vec::new(),
            });
        }
    }

    // 1-row blank gap between entry header and body
    ctx.advance_blank_rows(TURN_HEADER_BODY_GAP_ROWS);

    // Concrete command invocation inside the entry body. It is indented by
    // `TRANSCRIPT_BODY_LEADING_INDENT` so the invocation's first column lines
    // up with the result body drawn below it by `draw_message_body` — the
    // entry reads as one aligned block instead of a hanging head.
    let body_indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
    let invocation_style = if phase == CommandPhase::Pending {
        Style::default()
            .fg(ctx.theme.muted())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg((*ctx.theme).fg())
            .add_modifier(Modifier::BOLD)
    };

    let invocation_line = Line::from(vec![
        Span::styled(body_indent, invocation_style),
        Span::styled(invocation, invocation_style),
    ]);
    {
        ctx.paint(invocation_line);
    }

    // Result body blocks
    if phase == CommandPhase::Completed && msg.command_result_text().is_some() {
        ctx.advance_blank_rows(1);
        // An ack with detail renders in its own two-tone scheme (ADR-0106):
        // the headline in the entry's foreground tone — the part the eye
        // should land on first — and each explanation line muted below it.
        // The generic body path stays for every other result shape.
        if let Some((title, detail)) = msg.command_ack_split() {
            draw_ack_body(ctx, mi, title, detail, (*ctx.theme).info());
        } else {
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
        }
    }
}

/// Draw an ack reply body: headline first in the foreground tone, then each
/// detail line muted — never one `•`-joined row (the layout bug this fixes).
pub fn draw_ack_body(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    title: &str,
    detail: &[String],
    family_tone: Color,
) {
    let body_indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
    let wrap_width = (ctx.area.width as usize)
        .saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT as usize)
        .max(1);

    // Headline: prominent (the command family tone, bold) so "Delegated mode ON"
    // reads as the outcome, not as detail noise.
    let title_style = Style::default()
        .fg(family_tone)
        .add_modifier(Modifier::BOLD);
    let mut regions: Vec<(String, Style)> = vec![(title.to_string(), title_style)];
    // Detail: muted prose, one line each, wrapping preserved.
    let muted_style = Style::default().fg(ctx.theme.muted());
    for line in detail {
        regions.push((line.clone(), muted_style));
    }

    for (text, style) in regions {
        for wl in wrap_text(&text, wrap_width) {
            let line = Line::from(vec![
                Span::styled(body_indent.clone(), style),
                Span::styled(wl.text.clone(), style),
            ]);
            ctx.paint_text_row(
                line,
                mi,
                COMMAND_RESULT_BLOCK_IDX,
                &wl,
                TRANSCRIPT_BODY_LEADING_INDENT,
                &[],
            );
        }
    }
}

fn command_header_line(
    category_label: &str,
    family_tone: Color,
    time_label: Option<&str>,
    muted: Color,
    full_width: usize,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);
    let mut used = 0usize;

    let tag = format!("⌘ {category_label}");
    used += tag.width();
    spans.push(Span::styled(
        tag,
        Style::default()
            .fg(family_tone)
            .add_modifier(Modifier::BOLD),
    ));

    if let Some(time) = time_label {
        let time_width = time.width();
        if used + 1 + time_width <= full_width {
            spans.push(Span::styled(
                " ".repeat(full_width - used - time_width),
                Style::default(),
            ));
            spans.push(Span::styled(time.to_string(), Style::default().fg(muted)));
        }
    }

    Line::from(spans)
}
