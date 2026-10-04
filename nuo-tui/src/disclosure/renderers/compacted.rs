//! Collapsible, inspectable compaction checkpoint card renderer (ADR-0296).

use nuotc::{Modifier, Style, {Line, Span}};

use super::super::{Disclosure, Interaction, summary_text_color};
use super::base::{MARKER_COLLAPSED, MARKER_EXPANDED, RenderCtx};
use crate::message_body::draw_message_body;
use crate::model::document::TranscriptMessage;
use crate::model::layout::{BlockRegion, COMPACTED_CARD_BLOCK_IDX};
use crate::model::selection::{CellDragInfo, SelectionState};
use crate::render::{REASONING_TRACE_BLOCK_GAP_ROWS, TRANSCRIPT_BODY_LEADING_INDENT};

#[allow(clippy::too_many_arguments)]
pub fn draw_compacted_card(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    hovered: bool,
    focused: bool,
) {
    let Some((archived_messages, tokens_before, tokens_after, summary, tracked_files, expanded)) =
        msg.compacted_card_data()
    else {
        return;
    };
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

    ctx.advance_blank_rows(REASONING_TRACE_BLOCK_GAP_ROWS);

    let marker = if expanded {
        MARKER_EXPANDED
    } else {
        MARKER_COLLAPSED
    };

    let reclaim_pct = if tokens_before > 0 && tokens_before >= tokens_after {
        ((tokens_before - tokens_after) as f64 / tokens_before as f64 * 100.0) as usize
    } else {
        0
    };

    let stat_label = format!(
        "Folded {} turns  •  {} → {} tokens (-{}%)",
        archived_messages, tokens_before, tokens_after, reclaim_pct
    );

    // Activation is `Enter` or `Space` (`InteractiveEntry::handle_focused_key`
    // accepts both), matching the dialog toggle convention.
    let hint = if expanded {
        "[Enter / Space to collapse]"
    } else {
        "[Enter / Space to inspect]"
    };

    let lead_icon = "✦ ";
    let title = "Context Compacted";

    // The card is an activatable entry like every other step summary, so it
    // composes the same three channels: disclosure luminance, lifecycle hue
    // (none — compaction is not a failure), and the transient
    // hover/focus affordance hue (ADR-0174).
    let summary_color = summary_text_color(
        None,
        Disclosure::from_expanded(expanded),
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    let mut spans = Vec::new();
    // Disclosure marker
    spans.push(Span::styled(
        format!("{} ", marker),
        Style::default().fg(ctx.theme.brand()),
    ));
    // Header icon & title. The title and stat line carry the composed summary
    // tone so a collapsed card rests muted, an open one reads at full
    // foreground, and hovering or focusing it lights the affordance hue — the
    // same visual contract as every other step summary.
    spans.push(Span::styled(
        lead_icon,
        Style::default()
            .fg(ctx.theme.brand())
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        title,
        Style::default()
            .fg(summary_color)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        "  •  ",
        Style::default().fg(ctx.theme.muted()),
    ));
    spans.push(Span::styled(
        stat_label,
        Style::default().fg(summary_color),
    ));
    spans.push(Span::styled(
        "  •  ",
        Style::default().fg(ctx.theme.muted()),
    ));
    spans.push(Span::styled(hint, Style::default().fg(ctx.theme.muted())));

    let header_line = Line::from(spans);
    if let Some(rect) = ctx.paint(header_line) {
        ctx.layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: COMPACTED_CARD_BLOCK_IDX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    // Expanded view
    if expanded {
        // If there are tracked files, display them
        if !tracked_files.is_empty() {
            ctx.advance_blank_rows(1);

            let file_header = format!("  📁 Touched Files ({})", tracked_files.len());
            ctx.paint(Line::from(vec![Span::styled(
                file_header,
                Style::default()
                    .fg(ctx.theme.warn())
                    .add_modifier(Modifier::BOLD),
            )]));

            for file in tracked_files {
                let spans = vec![
                    Span::styled("    • ", Style::default().fg(ctx.theme.muted())),
                    Span::styled(file.clone(), Style::default().fg(ctx.theme.fg())),
                ];
                ctx.paint(Line::from(spans));
            }
        }

        // Draw summary blocks
        if summary.is_some() {
            ctx.advance_blank_rows(1);

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

    ctx.advance_blank_rows(REASONING_TRACE_BLOCK_GAP_ROWS);
}
