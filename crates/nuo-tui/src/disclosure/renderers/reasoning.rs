//! Collapsible reasoning / thinking trace renderer.

use nuotc::{
    Color, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::super::{Disclosure, Interaction, summary_text_color};
use super::base::{
    MARKER_COLLAPSED, MARKER_EXPANDED, RenderCtx, advance_plain_blank_rows, truncate_to_width,
};
use super::sticky::StickyStep;
use crate::message_body::draw_message_body;
use crate::model::document::{Block, Inline, TranscriptMessage};
use crate::model::layout::{BlockRegion, REASONING_BLOCK_IDX};
use crate::model::selection::{CellDragInfo, SelectionState};
use crate::render::{
    REASONING_TRACE_BLOCK_GAP_ROWS, REASONING_TRACE_BODY_TOP_GAP_ROWS,
    TRANSCRIPT_BODY_LEADING_INDENT,
};
use crate::text_layout::{
    RichLineParams, RichTextColors, RichTextRanges, WrappedLine, block_selection_range,
    line_selection, line_spans_rich, padded_tail,
};

#[allow(clippy::too_many_arguments)]
pub fn draw_reasoning_trace(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    sticky_steps: &mut Vec<StickyStep>,
    hovered: bool,
    focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(summary) = msg.reasoning_summary() else {
        return;
    };
    let expanded = msg.reasoning_expanded() == Some(true);
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

    let summary_line_idx = {
        draw_reasoning_summary(
            ctx,
            mi,
            expanded,
            // Always use the disclosure marker (`+`/`-`), never a streaming
            // `●`. With the activity bar as the single breathing anchor
            // (ADR 0008), nothing about the marker needs to change between
            // streaming and finished — the lifecycle reads from the summary
            // text (duration omitted while streaming) and the steady hue
            // alone. The marker color now follows the disclosure ×
            // interaction weight, so it tracks the highlight like the
            // summary text and like tool-step markers (no fixed hue).
            None,
            &summary,
            hovered,
            focused,
            REASONING_BLOCK_IDX,
        )
    };

    if expanded {
        // The leading indent is all that remains now that the horizontal
        // gutter is applied once at the stream entry point.
        let body_prefix = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
        let body_wrap_width = ctx
            .area
            .width
            .saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT) as usize;

        advance_plain_blank_rows(
            ctx.area,
            REASONING_TRACE_BODY_TOP_GAP_ROWS,
            &mut *ctx.skip_rows,
            ctx.y,
            &mut *ctx.content_lines,
        );
        let mut emitted_any_block = false;
        for (bi, block) in msg.blocks.iter().enumerate() {
            if let Block::Text(inline) = block {
                let Inline {
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                } = inline;
                if emitted_any_block {
                    advance_plain_blank_rows(
                        ctx.area,
                        REASONING_TRACE_BLOCK_GAP_ROWS,
                        &mut *ctx.skip_rows,
                        ctx.y,
                        &mut *ctx.content_lines,
                    );
                }
                emitted_any_block = true;
                let lines = ctx.wrap.wrap_text(content, body_wrap_width);
                let sel_range = block_selection_range(selection, mi, bi);
                for wl in lines.iter() {
                    let block_wl = WrappedLine {
                        text: wl.text.clone(),
                        start_byte: wl.start_byte,
                        end_byte: wl.end_byte,
                    };
                    let line = line_spans_rich(RichLineParams {
                        prefix: &body_prefix,
                        prefix_style: Style::default(),
                        text: &wl.text,
                        line_start_byte: wl.start_byte,
                        selected: line_selection(sel_range, &block_wl),
                        ranges: RichTextRanges {
                            code: code_ranges,
                            bold: bold_ranges,
                            math: math_ranges,
                            links: link_ranges,
                        },
                        base: Style::default().fg(ctx.theme.muted()),
                        colors: RichTextColors::from_theme(ctx.theme),
                    });
                    let used = TRANSCRIPT_BODY_LEADING_INDENT as usize + wl.text.width();
                    let mut line = line;
                    line.spans.push(Span::styled(
                        padded_tail(ctx.full_width, used),
                        Style::default(),
                    ));
                    ctx.paint_text_row(
                        line,
                        mi,
                        bi,
                        &block_wl,
                        TRANSCRIPT_BODY_LEADING_INDENT,
                        &[],
                    );
                }
            }
        }
        // No trailing bottom gap here: the layout resolves the semantic
        // boundary to the next component segment.
    }

    if expanded {
        sticky_steps.push(StickyStep {
            message_idx: mi,
            summary: summary.to_string(),
            color: ctx.theme.muted(),
            background: None,
            summary_line: summary_line_idx,
            body_end_line: *ctx.content_lines,
        });
    }
}

pub(crate) fn reasoning_summary_line(
    marker: &str,
    summary: &str,
    marker_color: Color,
    summary_color: Color,
    full_width: usize,
) -> Line<'static> {
    let marker_text = format!("{} ", marker);
    let mut used = marker_text.width();
    let budget = full_width.saturating_sub(used);
    let summary_text = truncate_to_width(summary, budget);
    used += summary_text.width();
    Line::from(vec![
        Span::styled(
            marker_text,
            Style::default()
                .fg(marker_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            summary_text,
            Style::default()
                .fg(summary_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(padded_tail(full_width, used), Style::default()),
    ])
}

#[allow(clippy::too_many_arguments)]
fn draw_reasoning_summary(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    expanded: bool,
    marker_override: Option<&str>,
    summary: &str,
    hovered: bool,
    focused: bool,
    block_idx: usize,
) -> usize {
    let marker = marker_override.unwrap_or(if expanded {
        MARKER_EXPANDED
    } else {
        MARKER_COLLAPSED
    });
    let summary_line_idx = *ctx.content_lines;
    let summary_color = summary_text_color(
        None,
        Disclosure::from_expanded(expanded),
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    let line = reasoning_summary_line(
        marker,
        summary,
        summary_color,
        summary_color,
        ctx.full_width,
    );
    if let Some(rect) = ctx.paint(line.clone()) {
        ctx.layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    summary_line_idx
}
