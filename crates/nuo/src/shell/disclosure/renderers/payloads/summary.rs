//! Summary line and title band renderers.
use nuotc::{Color, Line, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{
    MARKER_COLLAPSED, MARKER_EXPANDED, RenderCtx, nonempty_wrapped, truncate_to_width,
};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::inline_layout::SemanticLine;
use nuo_view::model::layout::BlockRegion;
use nuo_view::text_layout::{WrappedLine, line_selection, line_spans, padded_tail, wrap_text};

/// Build the summary line for a tool/subagent step: an optional expand marker
/// followed by the summary text, padded to `full_width`.
pub(crate) fn tool_summary_line(
    expand: &str,
    summary: &str,
    fg: Color,
    bg: Color,
    full_width: usize,
) -> Line<'static> {
    let base = Style::default().bg(bg);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    let mut used = 0usize;

    if !expand.is_empty() {
        let s = format!("{} ", expand);
        used += s.width();
        spans.push(Span::styled(s, base.fg(fg).add_modifier(Modifier::BOLD)));
    }

    // Clamp the summary to the columns that remain inside the band so the
    // trailing `padded_tail` has at least its right gutter to fill; without
    // this a header wider than `full_width` drives `padded_tail` to zero and
    // the text spills past the right edge.
    let summary_budget = full_width.saturating_sub(used);
    let clamped = truncate_to_width(summary, summary_budget);
    used += clamped.width();
    spans.push(Span::styled(
        clamped,
        base.fg(fg).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(padded_tail(full_width, used), base));
    Line::from(spans)
}

/// Render the shared summary of an expandable step and record its rect in the
/// layout map so clicks / `Enter` on it can toggle the step. Returns the
/// content-line index of the summary (used for sticky-pin tracking).
///
/// `block_idx` is the sentinel recorded in [`BlockRegion`] so the click handler
/// can tell step/trace kinds apart: `usize::MAX` for tool steps and
/// `usize::MAX - 1` for reasoning traces.
///
/// `bg` is the row's surface — the page tone. Keyboard ownership is **not**
/// expressed here (ADR-0145, amended): the pane wash and the entry marker are
/// painted once each by `nuo_view::focus_plane`, so a summary line is the same
/// line whether or not the keyboard is on it. That is what keeps the disclosure
/// and lifecycle tones the caller resolved intact.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_step_summary(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    expanded: bool,
    summary: &str,
    summary_color: Color,
    bg: Color,
) -> usize {
    let expand = if expanded {
        MARKER_EXPANDED
    } else {
        MARKER_COLLAPSED
    };
    let summary_line_idx = *ctx.content_lines;

    let line = tool_summary_line(expand, summary, summary_color, bg, ctx.full_width);
    if let Some(rect) = ctx.paint(line) {
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

/// Build the summary line for a tool/subagent step using semantic inline flex layout.
pub(crate) fn semantic_tool_summary_line(
    expand: &str,
    semantic_line: &SemanticLine<'_>,
    suffix: Option<(&str, Style)>,
    fg: Color,
    bg: Color,
    full_width: usize,
    theme: &Theme,
) -> Line<'static> {
    let base = Style::default().bg(bg);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    if !expand.is_empty() {
        let s = format!("{} ", expand);
        used += s.width();
        spans.push(Span::styled(s, base.fg(fg).add_modifier(Modifier::BOLD)));
    }

    let summary_budget = full_width.saturating_sub(used);
    let base_style = base.fg(fg).add_modifier(Modifier::BOLD);
    let resolved_spans = semantic_line.resolve(summary_budget, theme, base_style, suffix);
    for span in resolved_spans {
        used += span.content.width();
        spans.push(span);
    }
    spans.push(Span::styled(padded_tail(full_width, used), base));
    Line::from(spans)
}

/// Render the shared summary of an expandable step with semantic inline flex
/// layout. See [`draw_step_summary`] for why keyboard ownership is not a
/// parameter.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_semantic_step_summary(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    expanded: bool,
    semantic_line: &SemanticLine<'_>,
    suffix: Option<(&str, Style)>,
    summary_color: Color,
    bg: Color,
) -> usize {
    let expand = if expanded {
        MARKER_EXPANDED
    } else {
        MARKER_COLLAPSED
    };
    let summary_line_idx = *ctx.content_lines;

    let line = semantic_tool_summary_line(
        expand,
        semantic_line,
        suffix,
        summary_color,
        bg,
        ctx.full_width,
        ctx.theme,
    );
    if let Some(rect) = ctx.paint(line) {
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

/// Draw blank rows padded to `full_width` with `style`'s background. The row
/// count is supplied by component spacing tokens in `design.rs`.
pub(crate) fn draw_blank_rows(ctx: &mut RenderCtx<'_, '_>, style: Style, rows: usize) {
    for _ in 0..rows {
        let _ = ctx.paint(Line::from(Span::styled(
            padded_tail(ctx.full_width, 0),
            style,
        )));
    }
}

/// Render text content as a code block with a line-number gutter on
/// `code_surface`. Used for `read` / `edit_text` results and as the
/// fallback for unrecognized tools. The gutter starts at column `indent`
/// so the code aligns with the rest of the step body.
///
/// When `language` is `Some`, a subtle language tag is drawn on its own dim
/// line above the gutter — matching the markdown `Block::Code` band, so a
/// code block reads identically whether it sits in assistant prose or inside
/// an expanded tool step (the block-level design contract).
///
/// `start_line` is the 1-based file line of the first row of `content`
/// (carried by `ToolOutput::Code::start_line`). `0` means "unknown" — the
/// renderer then numbers the slice 1, 2, 3… The gutter width is derived from
/// the *highest* displayed line number (not the line *count*) so an offset
/// snippet like 100..104 still gets a 3-wide column instead of overflowing.
#[allow(clippy::too_many_arguments)]

/// Draw a full-width decorative band row — a result heading (count band) or a
/// Draw a wrapped, selectable title band for a path heading, registering one
/// region per wrapped line anchored at `abs_start` in the raw tool output. A
/// title owns its own tier (the full inner width), not the gutter column the
/// content rows beneath it align to.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_title_band(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    text: &str,
    abs_start: usize,
    style: Style,
    band_bg: Color,
    sel_range: Option<(usize, Option<usize>)>,
) {
    let pad = Style::default().bg(band_bg);
    let wrap_w = ctx.full_width.saturating_sub(indent).max(1);
    for wl in nonempty_wrapped(wrap_text(text, wrap_w)) {
        let block_wl = WrappedLine {
            text: wl.text.clone(),
            start_byte: abs_start + wl.start_byte,
            end_byte: abs_start + text.len(),
        };
        let mut line = line_spans(
            &" ".repeat(indent),
            pad,
            &wl.text,
            line_selection(sel_range, &block_wl),
            style,
            ctx.theme.selected(),
        );
        let used = indent + wl.text.width();
        line.spans
            .push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
    }
}

pub(crate) fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        one.to_string()
    } else {
        many.to_string()
    }
}
