//! Inline step renderer for Tier-2 Subagent tasks.

use nuotc::{
    Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::super::{Disclosure, Interaction, summary_text_color};
use super::base::{RenderCtx, truncate_to_width};
use crate::model::document::TranscriptMessage;
use crate::model::layout::BlockRegion;
use crate::render::STEP_MIN_WIDTH;
use crate::text_layout::{padded_tail, wrap_text};
use crate::tools::ToolStatus;

pub fn draw_subagent_inline_step(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    hovered: bool,
    focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(summary) = msg.tool_step_summary() else {
        return;
    };

    let status = msg
        .tool_step_status()
        .map(ToolStatus::from_status)
        .unwrap_or(ToolStatus::Running);

    // `ctx.area` arrives already inset by `draw_transcript`, so no
    // re-clip is needed here.
    let full_width = ctx.area.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return;
    }

    let bg = ctx.theme.surface();

    // Summary color flows through the shared state machine exactly like a
    // tool step (steady lifecycle accent while non-terminal, weight ladder
    // once completed); the badge borrows the brand hue so the role reads as
    // identity rather than as run state.
    let status_color = status.color(ctx.theme);
    let accent = match status {
        ToolStatus::Ok => None,
        _ => Some(status_color),
    };
    let summary_color = summary_text_color(
        accent,
        Disclosure::Collapsed,
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    // Row 1: `[badge]  summary`. The badge is a plain bracketed token in the
    // brand color (no inverse pill) so it sits quietly next to the summary
    // and survives narrow terminals. Two plain spaces separate badge and
    // summary (R2 same-rank peers on the join ladder).
    let badge = msg
        .subagent_profile()
        .map(|role| format!("[{}]", role.to_uppercase()))
        .unwrap_or_else(|| "[SUBAGENT]".to_string());
    let summary_row = {
        let base = Style::default().bg(bg);
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(4);
        let mut used = 0usize;
        let badge_text = format!("  {}", badge);
        used += badge_text.width();
        spans.push(Span::styled(badge_text, base.fg((*ctx.theme).brand())));
        let sep = "  ";
        used += sep.len();
        spans.push(Span::styled(sep, base));
        let summary_budget = full_width.saturating_sub(used);
        let clamped = truncate_to_width(&summary, summary_budget);
        used += clamped.width();
        spans.push(Span::styled(
            clamped,
            base.fg(summary_color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(padded_tail(full_width, used), base));
        Line::from(spans)
    };
    if let Some(rect) = ctx.paint(summary_row) {
        ctx.layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: usize::MAX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    // Row 2: the `└`-edged live row. While running it carries the peek
    // (current activity + elapsed); once the subagent terminates it is replaced
    // in place by the one-line conclusion. The `└` is constant — the step is
    // always exactly two rows, so the second row is always the leaf — and the
    // running/terminated distinction is carried by color and content, not by
    // the glyph.
    let (row_text, row_color) = if let Some(peek) = msg.subagent_status_line() {
        (peek, (*ctx.theme).info())
    } else if let Some(outcome) = msg.subagent_outcome_line() {
        (outcome, ctx.theme.muted())
    } else {
        (String::new(), ctx.theme.muted())
    };
    if !row_text.is_empty() {
        let bg_style = Style::default().bg(bg);
        let inner_width = ctx.full_width.saturating_sub(4);
        let wrapped = wrap_text(&row_text, inner_width.max(1));
        for (i, wl) in wrapped.iter().enumerate() {
            // The `└` edge marks the first wrapped row; continuation rows fall
            // back to a plain indent so the tree edge reads as a single branch.
            let (prefix, prefix_w) = if i == 0 { ("  └ ", 4) } else { ("    ", 4) };
            let used = prefix_w + wl.text.width();
            let line = Line::from(vec![
                Span::styled(prefix, bg_style.fg(ctx.theme.muted())),
                Span::styled(wl.text.clone(), bg_style.fg(row_color)),
                Span::styled(padded_tail(ctx.full_width, used), bg_style),
            ]);
            // Make the whole second row part of the same clickable summary so
            // clicking anywhere on the step enters the subagent view.
            if let Some(rect) = ctx.paint(line) {
                ctx.layout_map.push(BlockRegion {
                    message_idx: mi,
                    block_idx: usize::MAX,
                    start_byte: 0,
                    end_byte: 0,
                    text: String::new(),
                    prefix_cols: 0,
                    rect,
                    hidden_ranges: Vec::new(),
                });
            }
        }
    }
}
