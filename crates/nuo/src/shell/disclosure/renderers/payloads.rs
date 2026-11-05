//! Per-tool body content renderers (bash output, grep, find, diff, file write, code blocks).
use nuotc::{
    Color, Modifier, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::base::{
    MARKER_COLLAPSED, MARKER_EXPANDED, RenderCtx, nonempty_wrapped, truncate_to_width,
};
use crate::shell::render::{CODE_BAND_GUTTER_GAP, CODE_BAND_GUTTER_MIN_WIDTH, Theme};
use nuo_ui_primitives::components::inline_layout::SemanticLine;
use nuo_ui_primitives::design::{
    BASH_FOLD_INSET_COLS, BASH_FOLD_SIGNATURE_MIN_COLS, COPY_LOG_KEY, JOIN_CAUSE,
    QUESTION_ANSWER_GAP_COLS, QUESTION_ANSWER_GLYPH, VIEW_LOG_KEY,
};
use nuo_ui_primitives::theme::ListingClass;
use nuo_view::model::layout::{
    BlockRegion, LinkHit, TOOL_FOLD_BLOCK_IDX, TOOL_FOLD_COPY_BLOCK_IDX,
};
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{
    CodeGutterParams, WrappedLine, block_selection_range, clamp_selection_range, code_gutter_line,
    line_selection, line_spans, padded_tail, wrap_text,
};
use nuo_view::tools::shell_output::{BashFold, OutputCoverage, ShellOutput};
use nuo_view::tools::{DiffCache, DiffHunk, DiffOp, ResultKind};

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
/// `code_surface`. Used for `read_text` / `edit_text` results and as the
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
pub(crate) fn draw_code_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    start_line: usize,
    language: Option<&str>,
    syntax_lang: nuo_ui_primitives::syntax::Language,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let mut logical_lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical_lines.push((offset, line));
        offset += line.len() + 1;
    }
    // `0` (unknown) is indistinguishable from `1` for gutter purposes: both
    // render the first row as line 1. Normalize once so the math below is
    // uniform.
    let first_line = start_line.max(1);
    let last_line = first_line.saturating_add(logical_lines.len().saturating_sub(1));
    let gutter_width = last_line.to_string().len().max(CODE_BAND_GUTTER_MIN_WIDTH);
    let left_indent = indent;
    let gutter_gap = CODE_BAND_GUTTER_GAP;
    let gutter_indent = left_indent + 1 /* space */ + gutter_width + gutter_gap;
    let wrap_width = inner_w.saturating_sub(1 + gutter_width + gutter_gap);
    let sel_range = block_selection_range(selection, mi, block_idx);

    // Subtle language tag on its own dim line above the gutter — mirrors the
    // markdown `Block::Code` band so both block origins share one code-block
    // design.
    if let Some(lang) = language.filter(|l| !l.is_empty()) {
        let pad = Style::default().bg(code_bg);
        let used = left_indent + 1 + lang.len();
        let line = Line::from(vec![
            Span::styled(" ".repeat(left_indent), pad),
            Span::styled(" ", pad),
            Span::styled(
                lang.to_string(),
                Style::default().bg(code_bg).fg(ctx.theme.dim()),
            ),
            Span::styled(padded_tail(ctx.full_width, used), pad),
        ]);
        ctx.paint(line);
    }

    let parsed_lang = syntax_lang;

    for (line_idx, (line_start_byte, logical_line)) in logical_lines.iter().enumerate() {
        let wrapped = nonempty_wrapped(wrap_text(logical_line, wrap_width));
        let syntax_spans = if parsed_lang != nuo_ui_primitives::syntax::Language::Plain {
            nuo_ui_primitives::syntax::tokenize_line(logical_line, parsed_lang)
        } else {
            Vec::new()
        };

        for (wrap_idx, wl) in wrapped.iter().enumerate() {
            let gutter = if wrap_idx == 0 {
                format!("{:>width$}", first_line + line_idx, width = gutter_width)
            } else {
                " ".repeat(gutter_width)
            };

            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: line_start_byte + wl.start_byte,
                end_byte: line_start_byte + wl.end_byte,
            };

            let line = if parsed_lang == nuo_ui_primitives::syntax::Language::Plain {
                code_gutter_line(CodeGutterParams {
                    left_bar: Color::Reset,
                    left_indent,
                    gutter: &gutter,
                    gutter_gap,
                    code_bg,
                    gutter_fg: ctx.theme.dim(),
                    text: &wl.text,
                    selected: line_selection(sel_range, &block_wl),
                    code_fg: ctx.theme.code_text(),
                    selected_bg: ctx.theme.selected(),
                    full_width: ctx.full_width,
                })
            } else {
                code_gutter_line_syntax(
                    left_indent,
                    &gutter,
                    gutter_gap,
                    code_bg,
                    ctx.theme.dim(),
                    logical_line,
                    &syntax_spans,
                    wl.start_byte,
                    wl.end_byte,
                    line_selection(sel_range, &block_wl),
                    ctx.theme.selected(),
                    ctx.full_width,
                    ctx.theme,
                )
            };
            ctx.paint_text_row(line, mi, block_idx, &block_wl, gutter_indent as u16, &[]);
        }
    }
}

/// Draw a full-width decorative band row — a result heading (count band) or a
/// Draw a wrapped, selectable title band for a path heading, registering one
/// region per wrapped line anchored at `abs_start` in the raw tool output. A
/// title owns its own tier (the full inner width), not the gutter column the
/// content rows beneath it align to.
#[allow(clippy::too_many_arguments)]
fn draw_title_band(
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

/// Parse a listing block's leading header line into the label for its count
/// band. Recognizes the shape emitted by `find_files`: `Found N matching files:`
///
/// Returns `None` when the first line is not a header (a restored session may start
/// at the first entry), in which case no band is invented.
fn parse_listing_header(first: &str) -> Option<String> {
    let first = first.trim_end();
    let rest = first.strip_prefix("Found ")?;
    let n: usize = rest.strip_suffix("matching files:")?.trim().parse().ok()?;
    Some(format!("Found {} {}", n, plural(n, "file", "files")))
}

/// Split a listing path into its `(directory, leaf)` halves, where `directory`
/// keeps its trailing slash so it reads as a heading (`docs/adr/`). A path with
/// no separator (a root-level entry, or a trailing-slash directory such as
/// `src/`) yields no directory heading.
fn split_dir_leaf(raw: &str) -> (Option<&str>, &str) {
    match raw.rfind('/') {
        Some(idx) if idx + 1 < raw.len() => (Some(&raw[..=idx]), &raw[idx + 1..]),
        _ => (None, raw),
    }
}

/// Render a `find_files` result as a *layered* block, sharing the
/// three-tier contract of a search block (see [`draw_matches_content`]):
///
/// - directory paths are grouped with a per-directory **title band**
///   on [`Theme::match_title_surface`], omitting redundant "Found N files" count
///   bands so the hierarchy stays clean and transitions directly to file entries.
///
/// Rows carry no line-number gutter (a listing has no meaningful line index),
/// and every selectable row's byte range stays anchored in the raw tool output.
pub(crate) fn draw_listing_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let title_bg = ctx.theme.match_title_surface();
    let pad = Style::default().bg(code_bg);
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    let title_style = Style::default()
        .bg(title_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);

    let mut logical: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical.push((offset, line));
        offset += line.len() + 1;
    }

    let header_label = logical
        .first()
        .and_then(|(_, line)| parse_listing_header(line));
    let body_idx = if header_label.is_some() { 1 } else { 0 };

    let mut current_dir: Option<&str> = None;
    for (line_start_byte, raw) in &logical[body_idx..] {
        if raw.is_empty() {
            continue;
        }

        // Plain path line (`find_files`, or a trailing-slash directory): group
        // entries sharing a directory under one title band, then draw the leaf
        // name beneath it.
        let is_dir = raw.ends_with('/');
        let (dir, leaf) = split_dir_leaf(raw);
        match dir {
            Some(dir) => {
                if current_dir != Some(dir) {
                    current_dir = Some(dir);
                    let normalized = nuo_ui_primitives::components::path::PathView::from_str(dir)
                        .maybe_base_dir(ctx.workspace_root)
                        .format_text();
                    draw_title_band(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        &normalized,
                        *line_start_byte,
                        title_style,
                        title_bg,
                        sel_range,
                    );
                }
            }
            None => current_dir = None,
        }
        let display = match dir {
            Some(_) => leaf.to_string(),
            None => nuo_ui_primitives::components::path::PathView::from_str(raw)
                .maybe_base_dir(ctx.workspace_root)
                .format_text(),
        };
        let fg = if is_dir {
            ctx.theme.listing_color(ListingClass::Dir)
        } else {
            ctx.theme.code_text()
        };
        let base = Style::default().bg(code_bg).fg(fg);
        for wl in nonempty_wrapped(wrap_text(&display, wrap_w)) {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: *line_start_byte + wl.start_byte,
                end_byte: *line_start_byte + wl.end_byte,
            };
            let mut line = line_spans(
                &" ".repeat(indent),
                pad,
                &wl.text,
                line_selection(sel_range, &block_wl),
                base,
                ctx.theme.selected(),
            );
            let used = indent + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
        }
    }
}

/// Render a `write_todos` / checklist result: structured task list with status glyphs
/// [✓] completed, [•] in_progress, [☐] pending, [✕] cancelled on `code_bg`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_checklist_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    #[derive(serde::Deserialize)]
    struct RawItem {
        #[serde(default)]
        content: String,
        #[serde(default)]
        status: String,
    }

    #[derive(serde::Deserialize)]
    struct RawList {
        #[serde(default)]
        items: Vec<RawItem>,
    }

    let parsed_items: Vec<RawItem> = serde_json::from_str::<Vec<RawItem>>(output)
        .or_else(|_| serde_json::from_str::<RawList>(output).map(|l| l.items))
        .or_else(|_| serde_json::from_str::<RawList>(arguments).map(|l| l.items))
        .or_else(|_| {
            let v: Result<serde_json::Value, _> = serde_json::from_str(arguments);
            if let Ok(v) = v
                && let Some(arr) = v.get("items").and_then(|v| v.as_array())
            {
                let items = arr
                    .iter()
                    .map(|item| RawItem {
                        content: item
                            .get("content")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string(),
                        status: item
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("pending")
                            .to_string(),
                    })
                    .collect();
                return Ok(items);
            }
            Err(())
        })
        .unwrap_or_default();

    if parsed_items.is_empty() {
        draw_listing_content(ctx, mi, block_idx, output, selection, indent, inner_w);
        return;
    }

    let mut offset = 0usize;
    for item in &parsed_items {
        let (glyph, glyph_style, text_style) = match item.status.as_str() {
            "completed" | "done" => (
                "✓ ",
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.ok())
                    .add_modifier(Modifier::BOLD),
                Style::default().bg(code_bg).fg(ctx.theme.muted()),
            ),
            "in_progress" => (
                "• ",
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.info())
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.code_text())
                    .add_modifier(Modifier::BOLD),
            ),
            "cancelled" => (
                "✕ ",
                Style::default().bg(code_bg).fg(ctx.theme.err()),
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.dim())
                    .add_modifier(Modifier::STRIKETHROUGH),
            ),
            _ => (
                "☐ ",
                Style::default().bg(code_bg).fg(ctx.theme.dim()),
                Style::default().bg(code_bg).fg(ctx.theme.code_text()),
            ),
        };

        let logical_line = format!("{}{}", glyph, item.content);
        let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));

        for (idx, wl) in wrapped.iter().enumerate() {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: offset + wl.start_byte,
                end_byte: offset + wl.end_byte,
            };

            let prefix_cols = if idx == 0 { indent } else { indent + 2 };
            let mut line = if idx == 0 && wl.text.starts_with(glyph) {
                let rest_text = &wl.text[glyph.len()..];
                let glyph_len = glyph.len();
                let (glyph_bg_style, rest_sel) = match line_selection(sel_range, &block_wl) {
                    Some((lo, hi)) => {
                        let g_style = if lo < glyph_len {
                            glyph_style.bg(ctx.theme.selected())
                        } else {
                            glyph_style
                        };
                        let r_sel = if hi > glyph_len {
                            let r_lo = lo.saturating_sub(glyph_len);
                            let r_hi = hi - glyph_len;
                            (r_lo < r_hi).then_some((r_lo, r_hi))
                        } else {
                            None
                        };
                        (g_style, r_sel)
                    }
                    None => (glyph_style, None),
                };
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(glyph, glyph_bg_style),
                ];
                let rest_spans = line_spans(
                    "",
                    pad,
                    rest_text,
                    rest_sel,
                    text_style,
                    ctx.theme.selected(),
                );
                spans.extend(
                    rest_spans
                        .spans
                        .into_iter()
                        .filter(|s| !s.content.is_empty()),
                );
                Line::from(spans)
            } else {
                line_spans(
                    &" ".repeat(indent + 2),
                    pad,
                    &wl.text,
                    line_selection(sel_range, &block_wl),
                    text_style,
                    ctx.theme.selected(),
                )
            };

            let used = prefix_cols + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, prefix_cols as u16, &[]);
        }
        offset += logical_line.len() + 1;
    }
}

/// One question parsed out of an `ask_user` call's arguments, reduced to the
/// fields the expanded body paints. Mirrors the wire `UserQuestion` shape but is
/// parsed defensively from JSON so a restored / partially-persisted call still
/// renders (a missing field degrades, never panics).
struct QuestionSpec {
    header: Option<String>,
    question: String,
    option_count: usize,
    multi_select: bool,
}

/// Parse the `questions` array out of an `ask_user` call's raw arguments.
/// Entries without a `question` string are dropped; malformed JSON yields an
/// empty list (the caller then falls back to the raw result text).
fn parse_question_specs(arguments: &str) -> Vec<QuestionSpec> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return Vec::new();
    };
    let Some(questions) = value.get("questions").and_then(|q| q.as_array()) else {
        return Vec::new();
    };
    questions
        .iter()
        .filter_map(|q| {
            let question = q.get("question").and_then(|v| v.as_str())?;
            Some(QuestionSpec {
                header: q
                    .get("header")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string),
                question: question.to_string(),
                option_count: q
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|o| o.len())
                    .unwrap_or(0),
                multi_select: q
                    .get("multi_select")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// Recover the recorded selection from the tool's result text, or `None` when
/// the request was cancelled (or the result is not an answer payload at all).
///
/// The harness returns the answers as a pretty JSON array-of-arrays, either
/// bare or behind prose ("User answered the question(s). Selected option
/// labels:", or the autonomous "[answered by policy, not by user]" framing).
/// Try the whole string first, then the trailing JSON array — so an answered
/// step renders its selections regardless of which framing produced it.
fn parse_question_answers(output: &str) -> Option<Vec<Vec<String>>> {
    let trimmed = output.trim();
    if let Ok(answers) = serde_json::from_str::<Vec<Vec<String>>>(trimmed) {
        return Some(answers);
    }
    let idx = trimmed.find('[')?;
    serde_json::from_str::<Vec<Vec<String>>>(trimmed[idx..].trim()).ok()
}

/// Emit one logical (already-parsed) row of the question/answer list at
/// `indent` under `style`, wrapping text to `wrap_w` columns (the caller passes
/// the row's own content width — `inner_w` for a flush row, narrowed for an
/// indented continuation answer). When `meta` is provided it is appended,
/// dimmed, to the last wrapped row if it fits within the full row width;
/// otherwise it drops to its own dim row so the tag is never clipped at the
/// right edge.
///
/// Each painted row records a [`BlockRegion`] anchored in the block's *logical*
/// text via `*offset` (advanced by every row's text so a selection spanning the
/// list copies the questions and answers in reading order). The `meta`
/// decoration stays outside the recorded range, like the bash `$ command`
/// prompt line.
#[allow(clippy::too_many_arguments)]
fn emit_question_text(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    text: &str,
    style: Style,
    meta: Option<&str>,
    meta_style: Style,
    sel_range: Option<(usize, Option<usize>)>,
    offset: &mut usize,
) {
    let bg = style.bg;
    let pad = Style::default().bg(bg);
    let wrapped = nonempty_wrapped(wrap_text(text, wrap_w.max(1)));
    let last = wrapped.len().saturating_sub(1);
    let mut deferred_meta: Option<String> = None;
    for (i, wl) in wrapped.iter().enumerate() {
        let block_wl = WrappedLine {
            text: wl.text.clone(),
            start_byte: *offset + wl.start_byte,
            end_byte: *offset + wl.end_byte,
        };
        let mut line = line_spans(
            &" ".repeat(indent),
            pad,
            &wl.text,
            line_selection(sel_range, &block_wl),
            style,
            ctx.theme.selected(),
        );
        let mut used = indent + wl.text.width();
        if i == last
            && let Some(m) = meta
        {
            if used + 1 + m.width() <= ctx.full_width {
                line.spans.push(Span::styled(" ".to_string(), meta_style));
                line.spans.push(Span::styled(m.to_string(), meta_style));
                used += 1 + m.width();
            } else {
                deferred_meta = Some(m.to_string());
            }
        }
        line.spans
            .push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
    }
    *offset += text.len() + 1;

    if let Some(m) = deferred_meta {
        let mw = m.width();
        let wl = WrappedLine {
            text: m.clone(),
            start_byte: *offset,
            end_byte: *offset,
        };
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(m, meta_style),
            Span::styled(padded_tail(ctx.full_width, indent + mw), pad),
        ]);
        ctx.paint_text_row(line, mi, block_idx, &wl, indent as u16, &[]);
    }
}

/// Render an expanded `ask_user` step as a question→answer list.
///
/// The old path let the step fall through to the generic code renderer, which
/// dumped the harness's answer JSON as an unreadable line-numbered blob and
/// never showed the questions at all. Here the questions are recovered from the
/// call's `arguments` (header chip, text, option count, multi-select flag) and
/// re-paired with the recorded selection from the result (`output`), so the
/// reader sees *what was asked* and *what was chosen* — and a cancelled request
/// reads as `↳ cancelled — no answer` rather than an empty array.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_questions_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let sel_range = block_selection_range(selection, mi, block_idx);
    // Rows at `indent` wrap to `inner_w` (the caller's content width); an
    // indented continuation answer wraps to whatever that indent leaves.
    let content_w = inner_w.max(1);

    let questions = parse_question_specs(arguments);
    let answers = parse_question_answers(output);

    // Legacy / truncated arguments (no parseable questions): render the raw
    // result text so the step is never blank instead of silently empty.
    if questions.is_empty() {
        let fallback = Style::default().bg(code_bg).fg(ctx.theme.muted());
        let text = output.trim();
        let text = if text.is_empty() {
            "(no question recorded)"
        } else {
            text
        };
        let mut offset = 0usize;
        emit_question_text(
            ctx,
            mi,
            block_idx,
            indent,
            content_w,
            text,
            fallback,
            None,
            fallback,
            sel_range,
            &mut offset,
        );
        return;
    }

    let header_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.info())
        .add_modifier(Modifier::BOLD);
    let question_style = Style::default().bg(code_bg).fg(ctx.theme.fg());
    let meta_style = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let answer_style = Style::default().bg(code_bg).fg(ctx.theme.muted());
    let cancel_style = Style::default().bg(code_bg).fg(ctx.theme.warn());

    // A continuation answer (the 2nd+ option of a multi-select) aligns under the
    // first answer's label: the glyph width plus its gap.
    let label_indent = indent + QUESTION_ANSWER_GLYPH.width() + QUESTION_ANSWER_GAP_COLS;
    let label_w = content_w
        .saturating_sub(label_indent.saturating_sub(indent))
        .max(1);

    let policy_answered = output.contains("answered by policy");
    let total = questions.len();
    let mut offset = 0usize;

    for (q_idx, q) in questions.iter().enumerate() {
        if q_idx > 0 {
            ctx.paint(Line::from(Span::styled(
                padded_tail(ctx.full_width, 0),
                pad,
            )));
        }

        if let Some(header) = &q.header {
            emit_question_text(
                ctx,
                mi,
                block_idx,
                indent,
                content_w,
                header,
                header_style,
                None,
                meta_style,
                sel_range,
                &mut offset,
            );
        }

        let meta = format!(
            "({} option{}{})",
            q.option_count,
            if q.option_count == 1 { "" } else { "s" },
            if q.multi_select { ", multi-select" } else { "" }
        );
        emit_question_text(
            ctx,
            mi,
            block_idx,
            indent,
            content_w,
            &q.question,
            question_style,
            Some(&meta),
            meta_style,
            sel_range,
            &mut offset,
        );

        match &answers {
            Some(all) => {
                let labels = all.get(q_idx).map(Vec::as_slice).unwrap_or(&[]);
                if labels.is_empty() {
                    emit_question_text(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        content_w,
                        "<no option selected>",
                        meta_style,
                        None,
                        meta_style,
                        sel_range,
                        &mut offset,
                    );
                    continue;
                }
                for (i, label) in labels.iter().enumerate() {
                    let (row_indent, row_w, text) = if i == 0 {
                        (
                            indent,
                            content_w,
                            format!("{QUESTION_ANSWER_GLYPH} {label}"),
                        )
                    } else {
                        (label_indent, label_w, label.clone())
                    };
                    emit_question_text(
                        ctx,
                        mi,
                        block_idx,
                        row_indent,
                        row_w,
                        &text,
                        answer_style,
                        None,
                        meta_style,
                        sel_range,
                        &mut offset,
                    );
                }
            }
            None => {
                // Cancelled: one step-level status, shown once after the last
                // question so it reads as "the whole request was dropped".
                if q_idx + 1 == total {
                    emit_question_text(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        content_w,
                        &format!("{QUESTION_ANSWER_GLYPH} cancelled — no answer"),
                        cancel_style,
                        None,
                        meta_style,
                        sel_range,
                        &mut offset,
                    );
                }
            }
        }
    }

    if policy_answered {
        // Autonomous sessions settle by policy, not by a human — label it so the
        // answers are never mistaken for a real user decision.
        emit_question_text(
            ctx,
            mi,
            block_idx,
            indent,
            content_w,
            "[answered by policy, not by the user]",
            meta_style,
            None,
            meta_style,
            sel_range,
            &mut offset,
        );
    }
}

/// Render an interactive web search result stream with cards, domain pills, and clickable URLs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_web_search_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    structured: Option<&nuo_protocol::ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let _sel_range = block_selection_range(selection, mi, block_idx);

    let (query, _provider, hits, truncated) = match structured {
        Some(nuo_protocol::ToolOutput::WebSearch {
            query,
            provider,
            results,
            truncated,
        }) => (query.clone(), provider.clone(), results.clone(), *truncated),
        _ => parse_fallback_web_search(output, arguments),
    };

    if hits.is_empty() {
        let text = if query.is_empty() {
            "No web search results found.".to_string()
        } else {
            format!("No web search results found for '{}'.", query)
        };
        let wrapped = nonempty_wrapped(wrap_text(&text, inner_w.max(1)));
        for wl in &wrapped {
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(
                    wl.text.clone(),
                    Style::default().bg(code_bg).fg(ctx.theme.muted()),
                ),
            ];
            let used = indent + wl.text.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
        }
        return;
    }

    let index_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.brand())
        .add_modifier(Modifier::BOLD);
    let title_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let domain_style = Style::default().bg(code_bg).fg(ctx.theme.muted());
    let url_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.info())
        .add_modifier(Modifier::UNDERLINE);
    let snippet_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());

    for (idx, hit) in hits.iter().enumerate() {
        if idx > 0 {
            let line = Line::from(vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(padded_tail(ctx.full_width, indent), pad),
            ]);
            ctx.paint(line);
        }

        // Line 1: [idx + 1] Title
        let prefix = format!("[{}] ", idx + 1);
        let title_avail_w = inner_w.saturating_sub(prefix.width()).max(1);
        let wrapped_title = nonempty_wrapped(wrap_text(&hit.title, title_avail_w));
        for (t_idx, wl) in wrapped_title.iter().enumerate() {
            let mut spans = vec![Span::styled(" ".repeat(indent), pad)];
            let used = if t_idx == 0 {
                spans.push(Span::styled(prefix.clone(), index_style));
                spans.push(Span::styled(wl.text.clone(), title_style));
                indent + prefix.width() + wl.text.width()
            } else {
                let sub_indent = prefix.width();
                spans.push(Span::styled(" ".repeat(sub_indent), pad));
                spans.push(Span::styled(wl.text.clone(), title_style));
                indent + sub_indent + wl.text.width()
            };
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
        }

        // Line 2: 🌐 domain · url
        if !hit.url.is_empty() {
            let globe = "  🌐 ";
            let domain_pill = if hit.domain.is_empty() {
                String::new()
            } else {
                format!("{} · ", hit.domain)
            };
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(globe, domain_style),
                Span::styled(domain_pill.clone(), domain_style),
                Span::styled(hit.url.clone(), url_style),
            ];
            let used = indent + globe.width() + domain_pill.width() + hit.url.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            if let Some(rect) = ctx.paint(Line::from(spans)) {
                let col_start = indent + globe.width() + domain_pill.width();
                let url_w = hit.url.width();
                let max_w = (ctx.area.width as usize).saturating_sub(col_start);
                ctx.layout_map.push_link_hit(LinkHit {
                    message_idx: mi,
                    block_idx,
                    range: (0, 0),
                    url: hit.url.clone(),
                    rect: Rect::new(
                        rect.x + (col_start as u16),
                        rect.y,
                        (url_w.min(max_w) as u16).max(1),
                        1,
                    ),
                });
            }
        }

        // Line 3: Snippet
        if !hit.snippet.is_empty() {
            let snip_indent = indent + 4;
            let snip_w = inner_w.saturating_sub(4).max(1);
            let wrapped_snip = nonempty_wrapped(wrap_text(&hit.snippet, snip_w));
            for wl in &wrapped_snip {
                let mut spans = vec![
                    Span::styled(" ".repeat(snip_indent), pad),
                    Span::styled(wl.text.clone(), snippet_style),
                ];
                let used = snip_indent + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        }
    }

    if truncated {
        let note = "[... more search results omitted to fit context budget ...]";
        let mut spans = vec![
            Span::styled(" ".repeat(indent + 2), pad),
            Span::styled(note, Style::default().bg(code_bg).fg(ctx.theme.warn())),
        ];
        let used = indent + 2 + note.len();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint(Line::from(spans));
    }
}

/// Render a structured article reader view for fetched web pages without code gutters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_web_article_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    structured: Option<&nuo_protocol::ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let _sel_range = block_selection_range(selection, mi, block_idx);

    let (url, title, _domain, markdown, reader, tokens, truncated) = match structured {
        Some(nuo_protocol::ToolOutput::WebArticle {
            url,
            title,
            domain,
            markdown,
            reader,
            tokens,
            truncated,
        }) => (
            url.clone(),
            title.clone(),
            domain.clone(),
            markdown.clone(),
            reader.clone(),
            *tokens,
            *truncated,
        ),
        _ => parse_fallback_web_article(output, arguments),
    };

    // Header line 1: 🔗 URL (clickable link)
    if !url.is_empty() {
        let prefix = "🔗 ";
        let mut spans = vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(prefix, Style::default().bg(code_bg).fg(ctx.theme.muted())),
            Span::styled(
                url.clone(),
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.info())
                    .add_modifier(Modifier::UNDERLINE),
            ),
        ];
        let used = indent + prefix.width() + url.width();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        if let Some(rect) = ctx.paint(Line::from(spans)) {
            let col_start = indent + prefix.width();
            let url_w = url.width();
            let max_w = (ctx.area.width as usize).saturating_sub(col_start);
            ctx.layout_map.push_link_hit(LinkHit {
                message_idx: mi,
                block_idx,
                range: (0, 0),
                url: url.clone(),
                rect: Rect::new(
                    rect.x + (col_start as u16),
                    rect.y,
                    (url_w.min(max_w) as u16).max(1),
                    1,
                ),
            });
        }
    }

    // Header line 2: Security & Reader provenance banner
    {
        let shield = "🛡️  ";
        let provenance = format!(
            "Untrusted External Content · Reader: {} · ~{} tokens",
            reader, tokens
        );
        let mut spans = vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(shield, Style::default().bg(code_bg).fg(ctx.theme.warn())),
            Span::styled(
                provenance.clone(),
                Style::default().bg(code_bg).fg(ctx.theme.muted()),
            ),
        ];
        let used = indent + shield.width() + provenance.width();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint(Line::from(spans));
    }

    // Blank separator row
    {
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(padded_tail(ctx.full_width, indent), pad),
        ]);
        ctx.paint(line);
    }

    // Title (if available and not already first heading in markdown)
    let has_first_heading = markdown
        .lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.trim().starts_with('#'));
    if let Some(ref t) = title
        && !has_first_heading
    {
        let wrapped_title = nonempty_wrapped(wrap_text(t, inner_w.max(1)));
        for wl in &wrapped_title {
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(
                    wl.text.clone(),
                    Style::default()
                        .bg(code_bg)
                        .fg(ctx.theme.heading())
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            let used = indent + wl.text.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
        }
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), pad),
            Span::styled(padded_tail(ctx.full_width, indent), pad),
        ]);
        ctx.paint(line);
    }

    // Render Article Markdown body without code gutters
    let heading_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let quote_style = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let quote_bar_style = Style::default().bg(code_bg).fg(ctx.theme.info());
    let bullet_style = Style::default().bg(code_bg).fg(ctx.theme.brand());
    let text_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());

    let mut in_code_block = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            let mut spans = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(
                    line.to_string(),
                    Style::default().bg(code_bg).fg(ctx.theme.dim()),
                ),
            ];
            let used = indent + line.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
            continue;
        }

        if in_code_block {
            let snip_indent = indent + 2;
            let mut spans = vec![
                Span::styled(" ".repeat(snip_indent), pad),
                Span::styled(line.to_string(), text_style),
            ];
            let used = snip_indent + line.width();
            spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint(Line::from(spans));
            continue;
        }

        if trimmed.is_empty() {
            let line = Line::from(vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(padded_tail(ctx.full_width, indent), pad),
            ]);
            ctx.paint(line);
            continue;
        }

        if trimmed.starts_with('#') {
            let wrapped = nonempty_wrapped(wrap_text(trimmed, inner_w.max(1)));
            for wl in &wrapped {
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), heading_style),
                ];
                let used = indent + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        } else if let Some(quote_content) = trimmed.strip_prefix('>') {
            let quote_trimmed = quote_content.trim();
            let avail_w = inner_w.saturating_sub(4).max(1);
            let wrapped = nonempty_wrapped(wrap_text(quote_trimmed, avail_w));
            for wl in &wrapped {
                // Share the transcript's blockquote gutter glyph so a quoted
                // run reads identically in an assistant turn and in a rendered
                // web-article payload.
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(
                        format!("{} ", nuo_ui_primitives::design::QUOTE_GUTTER_GLYPH),
                        quote_bar_style,
                    ),
                    Span::styled(wl.text.clone(), quote_style),
                ];
                let used = indent + 2 + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let bullet_item = &trimmed[2..];
            let avail_w = inner_w.saturating_sub(3).max(1);
            let wrapped = nonempty_wrapped(wrap_text(bullet_item, avail_w));
            for (i, wl) in wrapped.iter().enumerate() {
                let mut spans = vec![Span::styled(" ".repeat(indent), pad)];
                let used = if i == 0 {
                    spans.push(Span::styled("• ", bullet_style));
                    spans.push(Span::styled(wl.text.clone(), text_style));
                    indent + 2 + wl.text.width()
                } else {
                    spans.push(Span::styled("  ", pad));
                    spans.push(Span::styled(wl.text.clone(), text_style));
                    indent + 2 + wl.text.width()
                };
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        } else {
            let wrapped = nonempty_wrapped(wrap_text(trimmed, inner_w.max(1)));
            for wl in &wrapped {
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), text_style),
                ];
                let used = indent + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        }
    }

    if truncated {
        let note = "[... article body truncated to fit context budget — request specific sections if needed ...]";
        let mut spans = vec![
            Span::styled(" ".repeat(indent + 2), pad),
            Span::styled(note, Style::default().bg(code_bg).fg(ctx.theme.warn())),
        ];
        let used = indent + 2 + note.len();
        spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint(Line::from(spans));
    }
}

pub(crate) fn parse_fallback_web_search(
    output: &str,
    arguments: &str,
) -> (String, String, Vec<nuo_protocol::WebSearchHit>, bool) {
    let query = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| {
            v.get("query")
                .and_then(|q| q.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let provider = if let Some(idx) = output.find("(via ") {
        let rest = &output[idx + 5..];
        rest.split(')').next().unwrap_or("Web").to_string()
    } else {
        "Web".to_string()
    };

    let mut hits = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_url: Option<String> = None;
    let mut current_snippet = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Search results for") || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("[... ") && trimmed.ends_with("...]") {
            continue;
        }
        let is_numbered = trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
            && trimmed.find(". ").is_some();
        if is_numbered {
            if let (Some(title), Some(url)) = (current_title.take(), current_url.take()) {
                let domain = extract_url_host(&url);
                hits.push(nuo_protocol::WebSearchHit {
                    title,
                    url,
                    domain,
                    snippet: current_snippet.join(" "),
                });
                current_snippet.clear();
            }
            let title = trimmed
                .split_once(". ")
                .map(|x| x.1)
                .unwrap_or(trimmed)
                .to_string();
            current_title = Some(title);
        } else if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && current_url.is_none()
        {
            current_url = Some(trimmed.to_string());
        } else if current_title.is_some() {
            current_snippet.push(trimmed);
        }
    }

    if let (Some(title), Some(url)) = (current_title, current_url) {
        let domain = extract_url_host(&url);
        hits.push(nuo_protocol::WebSearchHit {
            title,
            url,
            domain,
            snippet: current_snippet.join(" "),
        });
    }

    let truncated = output.contains("more results omitted to fit");
    (query, provider, hits, truncated)
}

pub(crate) fn parse_fallback_web_article(
    output: &str,
    arguments: &str,
) -> (String, Option<String>, String, String, String, usize, bool) {
    let url = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let domain = extract_url_host(&url);

    let mut cleaned = output;
    if let Some(idx) = cleaned.find("[BEGIN UNTRUSTED WEB CONTENT")
        && let Some(nl) = cleaned[idx..].find('\n')
    {
        cleaned = &cleaned[idx + nl + 1..];
    }
    if let Some(idx) = cleaned.rfind("[END UNTRUSTED WEB CONTENT]") {
        cleaned = &cleaned[..idx];
    }
    let trimmed = cleaned.trim();
    let tokens = nuo_protocol::tokenizer::count_tokens(trimmed);
    let truncated = output.contains("kept the first") || output.contains("truncated to fit");
    (
        url,
        None,
        domain,
        trimmed.to_string(),
        "Reader".to_string(),
        tokens,
        truncated,
    )
}

fn extract_url_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = authority.split('@').next_back().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        "web".to_string()
    } else {
        host.to_string()
    }
}

/// A single logical line parsed out of `search_text`'s `path:linenum:content` format.
struct MatchLine<'a> {
    path: &'a str,
    lineno: &'a str,
    content: &'a str,
    /// Byte offset of `content` within the original ripgrep output line.
    content_offset: usize,
}

/// Check if a leading line is the filesystem tool's `Found N match(es):` count header.
fn is_matches_header(first: &str) -> bool {
    let first = first.trim();
    if let Some(rest) = first.strip_prefix("Found ") {
        if let Some(num) = rest.strip_suffix("match(es):") {
            return num.trim().parse::<usize>().is_ok();
        }
    }
    false
}

/// Recover the literal search query from a `search_text` call's arguments.
/// Returns `None` when the query is absent/empty or was run as a regex — only a
/// plain literal is safe to bold inside a `path:line:content` line, since those
/// lines carry no per-match column ranges and a regex would make substring
/// matching misleading.
fn literal_query(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    if value
        .get("regex")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let query = value.get("query").and_then(serde_json::Value::as_str)?;
    (!query.is_empty()).then(|| query.to_string())
}

/// Segment `text` into non-overlapping `(lo, hi, is_match)` byte ranges: every
/// case-sensitive occurrence of the literal `query` is marked `true` (adjacent
/// occurrences merged), everything between them `false`. When `query` is
/// `None`/empty the whole `text` is one plain range. The ranges tile `text`
/// exactly, so callers can style each slice and reassemble the line verbatim.
/// A literal `str::find` always lands on char boundaries, so the byte ranges are
/// always safe to slice.
fn segment_matches(text: &str, query: Option<&str>) -> Vec<(usize, usize, bool)> {
    let Some(q) = query.filter(|q| !q.is_empty()) else {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![(0, text.len(), false)]
        };
    };
    let mut spans: Vec<(usize, usize, bool)> = Vec::new();
    let mut cursor = 0usize;
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(q) {
        let start = from + rel;
        let end = start + q.len();
        if start > cursor {
            spans.push((cursor, start, false));
        }
        // Merge into the previous span when it was itself an occurrence ending
        // exactly at this one (adjacent matches read as one bold run).
        match spans.last_mut() {
            Some(last) if last.2 && last.1 == start => last.1 = end,
            _ => spans.push((start, end, true)),
        }
        cursor = end;
        from = end;
    }
    if cursor < text.len() {
        spans.push((cursor, text.len(), false));
    }
    spans
}

/// Split one wrapped match row into styled spans: base text in `base`, any
/// literal-query occurrence(s) in bold (`highlight_fg`) so the matched text
/// stands out, and the selected byte range on the selection background. Match
/// and selection are independent layers, so a bold match inside the selection
/// keeps both (bold foreground *and* selection band).
fn match_content_spans(
    text: &str,
    query: Option<&str>,
    base: Style,
    selected: Option<(usize, usize)>,
    highlight_fg: Color,
    selected_bg: Color,
) -> Vec<Span<'static>> {
    let selected = clamp_selection_range(selected, text);
    let mut spans = Vec::new();
    for (lo, hi, is_match) in segment_matches(text, query) {
        if lo >= hi {
            continue;
        }
        let style = if is_match {
            base.fg(highlight_fg).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        match selected {
            None => spans.push(Span::styled(text[lo..hi].to_string(), style)),
            Some((s_lo, s_hi)) => {
                let lo_c = lo.max(s_lo);
                let hi_c = hi.min(s_hi);
                if lo_c >= hi_c {
                    spans.push(Span::styled(text[lo..hi].to_string(), style));
                } else {
                    if lo_c > lo {
                        spans.push(Span::styled(text[lo..lo_c].to_string(), style));
                    }
                    spans.push(Span::styled(
                        text[lo_c..hi_c].to_string(),
                        style.bg(selected_bg),
                    ));
                    if hi_c < hi {
                        spans.push(Span::styled(text[hi_c..hi].to_string(), style));
                    }
                }
            }
        }
    }
    spans
}

/// `"{count} {noun}"` with a naive `-s` plural — the labels here are fixed
/// (`match`/`file`), so a full pluralization pass would be overkill.
fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        one.to_string()
    } else {
        many.to_string()
    }
}

/// Parse `path:linenum:content` (ripgrep's default with `-n`). Paths may
/// contain `:` (e.g. Windows `C:\foo`), so the scan accepts the first colon
/// that is followed by an all-digit run and another colon as the
/// line-number separator. Returns `None` for blank separators or any line
/// that doesn't match the ripgrep shape.
fn parse_match_line(line: &str) -> Option<MatchLine<'_>> {
    for (idx, ch) in line.char_indices() {
        if ch != ':' {
            continue;
        }
        let after = &line[idx + 1..];
        let digits_end = after
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_digit())
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if digits_end > 0 && after.as_bytes().get(digits_end) == Some(&b':') {
            let path = &line[..idx];
            if path.is_empty() {
                continue;
            }
            let lineno = &after[..digits_end];
            let content = &after[digits_end + 1..];
            let content_offset = idx + 1 + digits_end + 1;
            return Some(MatchLine {
                path,
                lineno,
                content,
                content_offset,
            });
        }
    }
    None
}

/// Emit `text` as one or more wrapped rows at column `indent`, all styled
/// with `style` on `pad`'s background, recording a selectable [`BlockRegion`]
/// per row whose byte range is anchored at `abs_start` within the tool
/// output. Used for ripgrep separator rows and any other "simple" result row
/// that doesn't need a line-number gutter.
#[allow(clippy::too_many_arguments)]
fn emit_simple_rows(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    text: &str,
    abs_start: usize,
    pad: Style,
    style: Style,
    sel_range: Option<(usize, Option<usize>)>,
) {
    let wrap_w = ctx.full_width.saturating_sub(indent).max(1);
    let wrapped = nonempty_wrapped(wrap_text(text, wrap_w));
    for wl in &wrapped {
        let block_wl = WrappedLine {
            text: wl.text.clone(),
            start_byte: abs_start + wl.start_byte,
            end_byte: abs_start + wl.end_byte,
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

/// Render a `search_text` result directly as file groups separated by
/// distinct file title bands, with line numbers aligned in a gutter column.
///
/// Each file begins with a full-width title row showing the workspace-relative
/// path. Matches within the file show the line number right-aligned in the
/// gutter, a fixed 2-character gap, and the wrapped line content with any
/// literal query occurrence rendered in bold. The file titles sit on
/// `match_title_surface` above the match content on `code_surface`.
///
/// Any legacy `Found N match(es):` count header from the tool is stripped
/// so the view transitions directly from the action header to the file groups
/// without an intermediate metadata band.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_matches_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    arguments: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let title_bg = ctx.theme.match_title_surface();
    let pad = Style::default().bg(code_bg);
    let title_style = Style::default()
        .bg(title_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let match_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());
    let sel_range = block_selection_range(selection, mi, block_idx);
    // Only a plain literal query is safe to bold in `path:line:content` lines.
    let query = literal_query(arguments);

    // Walk logical lines with their byte offsets in `content`.
    let mut logical: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical.push((offset, line));
        offset += line.len() + 1;
    }

    // Skip the legacy `Found N match(es):` count header emitted by the filesystem
    // tool so it does not leak into match rows as raw unparsed text.
    let header_idx = if logical
        .first()
        .is_some_and(|(_, line)| is_matches_header(line))
    {
        1
    } else {
        0
    };

    // Width of the line-number column: the widest lineno across all matches, so
    // the content column stays aligned within and across files.
    let mut lineno_width = 1usize;
    for (_, line) in &logical[header_idx..] {
        if let Some(p) = parse_match_line(line) {
            lineno_width = lineno_width.max(p.lineno.len());
        }
    }
    let gap = 2usize;
    let content_cols = indent + lineno_width + gap;
    let content_wrap_w = inner_w.saturating_sub(lineno_width + gap).max(1);

    let mut current_path: Option<&str> = None;

    for (line_start_byte, logical_line) in &logical[header_idx..] {
        match parse_match_line(logical_line) {
            Some(parsed) => {
                if current_path != Some(parsed.path) {
                    current_path = Some(parsed.path);
                    let normalized =
                        nuo_ui_primitives::components::path::PathView::from_str(parsed.path)
                            .maybe_base_dir(ctx.workspace_root)
                            .format_text();
                    // File title band: distinct background + bold, wrapped at the
                    // full inner width (a title owns its own tier, not the gutter
                    // column the match rows align to).
                    draw_title_band(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        &normalized,
                        *line_start_byte,
                        title_style,
                        title_bg,
                        sel_range,
                    );
                }
                // Absolute byte offset of `content` within the tool output.
                let content_abs = line_start_byte + parsed.content_offset;
                let wrapped = nonempty_wrapped(wrap_text(parsed.content, content_wrap_w));
                for (wrap_idx, wl) in wrapped.iter().enumerate() {
                    let lineno_span = if wrap_idx == 0 {
                        let lpad = lineno_width.saturating_sub(parsed.lineno.len());
                        Span::styled(format!("{}{}", " ".repeat(lpad), parsed.lineno), dim)
                    } else {
                        Span::styled(" ".repeat(lineno_width), dim)
                    };
                    let block_wl = WrappedLine {
                        text: wl.text.clone(),
                        start_byte: content_abs + wl.start_byte,
                        end_byte: content_abs + wl.end_byte,
                    };
                    let selected =
                        clamp_selection_range(line_selection(sel_range, &block_wl), &wl.text);
                    let mut spans = vec![
                        Span::styled(" ".repeat(indent), pad),
                        lineno_span,
                        Span::styled(" ".repeat(gap), pad),
                    ];
                    spans.extend(match_content_spans(
                        &wl.text,
                        query.as_deref(),
                        match_style,
                        selected,
                        ctx.theme.heading(),
                        ctx.theme.selected(),
                    ));
                    let used = content_cols + wl.text.width();
                    spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                    ctx.paint_text_row(
                        Line::from(spans),
                        mi,
                        block_idx,
                        &block_wl,
                        content_cols as u16,
                        &[],
                    );
                }
            }
            None => {
                emit_simple_rows(
                    ctx,
                    mi,
                    block_idx,
                    indent,
                    logical_line,
                    *line_start_byte,
                    pad,
                    dim,
                    sel_range,
                );
            }
        }
    }
}

/// Resolve a shell step's `ShellTermination` into a themed footer
/// `(text, style)`, or `None` for a healthy `Exited` run (which carries no
/// extra marker beyond its optional `exit N` line).
///
/// The footer answers the one question the output cannot: *the process did not
/// end on its own — the harness stopped it, and here is why*. Each variant is
/// `state — cause`, the join form `docs/reference/tui/visual-language.md`
/// assigns to a cause that explains a state (the same form as `Edit — <name>`).
///
/// The vocabulary is deliberately plain (`output limit`, not `stream cap`) and
/// free of subsystem names: the reader is a human debugging a command, not the
/// component that named the budget (ADR-0063 §Concise, Professional Footers).
/// Every supervisor-interrupted variant states `terminated` explicitly in the
/// `state — cause` join form, because harness termination is the authentic fact
/// that cannot be recovered from the child output stream alone.
fn termination_footer(
    term: nuo_protocol::tool_output::ShellTermination,
    theme: &Theme,
    bg: Color,
) -> Option<(String, Style)> {
    use nuo_protocol::tool_output::ShellTermination as T;
    let warn_style = Style::default().bg(bg).fg(theme.warn());
    let err_style = Style::default().bg(bg).fg(theme.err());
    // `state — cause` via the shared join token, so the separator/spacing can
    // never drift from `docs/reference/tui/visual-language.md`.
    let cause = |state: &str, why: &str| format!("{state}{JOIN_CAUSE}{why}");
    match term {
        T::Exited => None,
        T::IdleBlocked => Some((cause("terminated", "idle timeout"), warn_style)),
        T::InteractiveBlocked => Some((cause("not started", "command asks for input"), warn_style)),
        T::InputUnanswered => Some((cause("terminated", "input prompt unanswered"), warn_style)),
        T::Timeout => Some((cause("terminated", "timed out"), warn_style)),
        T::Cancelled => Some((cause("terminated", "cancelled"), err_style)),
        T::StreamGuard => Some((cause("terminated", "output limit reached"), warn_style)),
        T::Detached => Some((cause("moved to background", "still running"), warn_style)),
    }
}

/// Dynamic supervisor surface tint for Part 3 Harness Governance band (ADR-0070 → ADR-0093).
fn harness_intervention_surface(
    term: nuo_protocol::tool_output::ShellTermination,
    theme: &Theme,
) -> Color {
    use nuo_protocol::tool_output::ShellTermination as T;
    let tint = match term {
        T::Cancelled => theme.err(),
        _ => theme.warn(),
    };
    match theme.code_surface() {
        Color::Rgb(..) => nuo_ui_primitives::theme::mix(theme.code_surface(), tint, 0.12),
        _ => theme.code_surface(),
    }
}

/// Map POSIX/standard shell exit codes to canonical explanatory descriptions (ADR-0070 → ADR-0093).
fn exit_code_explanation(code: i32) -> &'static str {
    match code {
        1 => "general error",
        2 => "misuse of shell builtins",
        126 => "permission denied / cannot execute",
        127 => "command not found",
        128 => "invalid exit argument",
        130 => "interrupted (SIGINT)",
        131 => "core dumped (SIGQUIT)",
        134 => "aborted (SIGABRT)",
        137 => "killed (SIGKILL / OOM)",
        139 => "segmentation fault (SIGSEGV)",
        143 => "terminated (SIGTERM)",
        _ => "error",
    }
}

/// Render a command step as a terminal-like `code_bg` block: a `$ command`
/// prompt line first, then the output's head/tail window (ADR-0115), then the
/// exit and coverage footers. Output rows have no line-number gutter.
///
/// Everything below the prompt is derived by
/// [`nuo_view::tools::shell_output::ShellOutput`]: in-band markers
/// (`STDOUT:`, `Exit N`, the harness notes, offload envelopes) are stripped and
/// the facts they carried are re-materialized in the footers, so a restored
/// flat text and a live structured payload paint the same shape. `revealed`
/// turns the fold off for this step — the rows are the same either way, which
/// is what lets the disclosure ladder promise a reveal that always lands.
///
/// The command line is not selectable (it is derived from the call, not the
/// output stream); output rows are.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_command_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    structured: Option<&nuo_protocol::ToolOutput>,
    command: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
    revealed: bool,
) {
    let result_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(result_bg);
    let base = Style::default().bg(result_bg).fg(ctx.theme.code_text());
    let sel_range = block_selection_range(selection, mi, block_idx);
    // The body is a flat, flush-left block: every row starts at `indent`, and
    // the only per-row mark is the `$ ` on the invocation. There is no left
    // track — a column of `│` down 30 rows of stdout reads as a frame the
    // content does not have, and, because the terminal's own shift-drag
    // selection cannot tell chrome from content, every line a user copied out
    // of the terminal arrived with a border glued to its front. The block is
    // identified by its surface, its `$`, and its indentation instead
    // (ADR-0150).
    let body = CommandBodyCtx::new(indent, ctx.theme);

    // The invocation row. `$ ` is drawn in the brand hue because it names the
    // *act* ("this ran"), which is the one cell of a long block that has to be
    // findable at a glance. It is painted, never registered: the region's text
    // is the command itself, so selecting the row and copying yields
    // `cargo build` rather than `$ cargo build`, which a shell would refuse.
    if !command.is_empty() {
        let cmd_style = Style::default().bg(result_bg).fg(ctx.theme.fg());
        let prompt_wrap = inner_w.saturating_sub(PROMPT_COLS as usize).max(1);
        for cont in command.split('\n') {
            let _ = body.emit(
                ctx,
                mi,
                COMMAND_INVOCATION_BLOCK,
                prompt_wrap,
                pad,
                sel_range,
                cont,
                cmd_style,
                PROMPT_COLS as usize,
                0,
            );
        }
    }

    // ADR-0115: one derivation for the rows, the outcome, and what each
    // audience holds — the same `nuo-view` derivation the activation ladder
    // consults, so the fold can only ever hide what activation can reveal.
    let view = ShellOutput::derive(content, structured);

    let wrap_w = inner_w.max(1);
    let mut byte_offset = 0usize;
    if !view.rows.is_empty() {
        // Revealing is a *viewport* choice, never a data choice: the whole log
        // is in `view.rows` either way.
        let fold = if revealed { None } else { view.fold() };
        byte_offset = emit_command_output(
            ctx,
            mi,
            // The output body is block 1: the model stores the invocation as
            // block 0 and the rows as block 1, so a region registered here
            // indexes the string a selection will slice.
            COMMAND_OUTPUT_BLOCK,
            &body,
            wrap_w,
            pad,
            base,
            sel_range,
            &view.rows,
            fold,
            byte_offset,
        );
    }

    // Part 2: Process Exit Outcome Strip (flush under the output body).
    // Only non-zero exits paint a distinct marker with explanation (ADR-0070 → ADR-0093).
    if let Some(code) = view.exit {
        if code != 0 {
            let expl = exit_code_explanation(code);
            let m = format!("exit {} ({})", code, expl);
            let footer_marker_style = Style::default().bg(result_bg).fg(ctx.theme.warn());
            let _ = body.emit(
                ctx,
                mi,
                block_idx,
                wrap_w,
                pad,
                sel_range,
                &m,
                footer_marker_style,
                0,
                byte_offset,
            );
        }
    }

    // Part 3: Harness Governance Strip (flush beneath Part 2).
    // Only rendered when supervisor intervention occurred (timeout, output
    // limit, cancellation) or when a copy of this log was cut.
    let harness_bg = harness_intervention_surface(view.termination, ctx.theme);
    let harness_pad = Style::default().bg(harness_bg);
    let governance_entry =
        termination_footer(view.termination, ctx.theme, harness_bg).or_else(|| {
            coverage_footer(view.coverage).map(|text| {
                let warn_style = Style::default().bg(harness_bg).fg(ctx.theme.warn());
                (text.to_string(), warn_style)
            })
        });

    if let Some((text, style)) = governance_entry {
        let _ = body.emit(
            ctx,
            mi,
            block_idx,
            wrap_w,
            harness_pad,
            sel_range,
            &text,
            style,
            0,
            byte_offset,
        );
    }
}

/// Columns the `$ ` prompt adds in front of the command text.
const PROMPT_COLS: u16 = 2;
/// The command block's **invocation** block index. Block 1 is the output body —
/// the same two-block shape
/// [`TranscriptMessage::refresh_tool_step`](nuo_view::model::document::TranscriptMessage)
/// stores, so a region's byte offsets index the string a selection slices.
const COMMAND_INVOCATION_BLOCK: usize = 0;
/// The command block's **output body** block index (the rows below the prompt).
const COMMAND_OUTPUT_BLOCK: usize = 1;

/// The two facts a command body row needs to paint itself: where the block
/// starts, and which foreground each of its parts takes.
///
/// It exists so the prompt row, the output rows, the exit strip and the
/// governance strip share one definition of the body's geometry instead of four
/// copies of `indent`. It deliberately carries **no glyph**: a command body has
/// no per-row mark other than the `$` the invocation row owns (ADR-0150).
struct CommandBodyCtx<'t> {
    /// Column the block starts at — the same column every row of the body,
    /// including the folded block's own inset, resolves against.
    indent: usize,
    /// The surface every body row is painted on, so the fold block's own rows
    /// (and its blank rows) sit on the same tone as the output they interrupt.
    surface: Color,
    prompt: Color,
    dim: Color,
    theme: &'t Theme,
}

impl<'t> CommandBodyCtx<'t> {
    fn new(indent: usize, theme: &'t Theme) -> Self {
        Self {
            indent,
            surface: theme.code_surface(),
            prompt: theme.command_prompt(),
            dim: theme.dim(),
            theme,
        }
    }

    /// Paint one body row and register it as addressable text.
    ///
    /// The region's text is the row's **content**, not the painted line: the
    /// block model stores the content, so the byte offsets a region records must
    /// index that same string or every selection in a command body would slice
    /// the wrong bytes. The `$ ` prompt is therefore painted but not registered —
    /// `prefix_cols` moves the hit-test past it, and a drag that starts on it
    /// resolves to the first content byte. Copying `$ cargo build` into a shell
    /// would not run.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        ctx: &mut RenderCtx<'_, '_>,
        mi: usize,
        block_idx: usize,
        wrap_w: usize,
        pad: Style,
        sel_range: Option<(usize, Option<usize>)>,
        text: &str,
        style: Style,
        prompt_cols: usize,
        mut byte_offset: usize,
    ) -> usize {
        let text = text.trim_end_matches(&['\r', '\n'][..]);
        let prompt_gap = " ".repeat(prompt_cols);
        for logical_line in text.split('\n') {
            let logical_line = nuo_protocol::tool_output::normalize_carriage_returns(logical_line);
            let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));
            for (wrap_idx, wl) in wrapped.iter().enumerate() {
                let block_wl = WrappedLine {
                    text: wl.text.clone(),
                    start_byte: byte_offset + wl.start_byte,
                    end_byte: byte_offset + wl.end_byte,
                };
                // Continuation rows of a wrapped line never repeat the prompt: a
                // `$` on every row would claim each line ran separately.
                let prompt = if wrap_idx == 0 && prompt_cols > 0 {
                    "$ "
                } else {
                    &prompt_gap[..]
                };
                let mut spans: Vec<Span<'static>> = vec![
                    Span::styled(" ".repeat(self.indent), pad),
                    Span::styled(
                        prompt.to_string(),
                        pad.fg(if prompt.starts_with('$') {
                            self.prompt
                        } else {
                            self.dim
                        }),
                    ),
                ];
                spans.extend(
                    line_spans(
                        "",
                        pad,
                        &wl.text,
                        line_selection(sel_range, &block_wl),
                        style,
                        self.theme.selected(),
                    )
                    .spans,
                );
                let used = self.indent + prompt_cols + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint_text_row(
                    Line::from(spans),
                    mi,
                    block_idx,
                    &block_wl,
                    (self.indent + prompt_cols) as u16,
                    &[],
                );
            }
            byte_offset += logical_line.len() + 1;
        }
        byte_offset
    }
}

/// The footer line for a command step whose log was cut *somewhere*.
///
/// ADR-0115: state which copy is short, not just that something is. The old
/// single "output truncated · partial kept" claimed the screen held a partial
/// log even when the screen held every byte — the fold was hiding it, and the
/// fold can be revealed. A statement the user can check is the whole point.
fn coverage_footer(coverage: OutputCoverage) -> Option<&'static str> {
    match coverage {
        OutputCoverage::Full => None,
        // The screen is complete; the model's view was cut at the context
        // watermark, so the fold may hide rows it never saw either.
        OutputCoverage::ModelTruncated => Some("output truncated for model context"),
        // The payload arrived already cut: the middle is gone for everyone.
        OutputCoverage::CutBeforeUi => Some("output truncated · partial kept"),
    }
}

/// Paint `rows`, folding the middle when `fold` is `Some`.
#[allow(clippy::too_many_arguments)]
fn emit_command_output(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    body: &CommandBodyCtx<'_>,
    wrap_w: usize,
    pad: Style,
    row_style: Style,
    sel_range: Option<(usize, Option<usize>)>,
    rows: &[String],
    fold: Option<BashFold>,
    mut byte_offset: usize,
) -> usize {
    let emit = |ctx: &mut RenderCtx<'_, '_>, row: &str, byte_offset: usize| {
        body.emit(
            ctx,
            mi,
            block_idx,
            wrap_w,
            pad,
            sel_range,
            row,
            row_style,
            0,
            byte_offset,
        )
    };

    let Some(fold) = fold else {
        for row in rows {
            byte_offset = emit(ctx, row, byte_offset);
        }
        return byte_offset;
    };

    for row in &rows[..fold.head_end] {
        byte_offset = emit(ctx, row, byte_offset);
    }
    // Advance past the hidden rows so every visible row keeps anchoring at its
    // true `output`-space byte offset — a selection spanning the fold must copy
    // the rows around it exactly as if the middle were on screen.
    for row in fold.hidden_rows(rows) {
        byte_offset += row.len() + 1;
    }
    emit_fold_block(ctx, mi, body, &fold, fold.hidden_rows(rows));
    for row in &rows[fold.tail_start..] {
        byte_offset = emit(ctx, row, byte_offset);
    }
    byte_offset
}

/// The fold's **block**: a blank row, two annotated rows, a blank row.
///
/// # Why a block and not a divider
///
/// The fold is the one region of a command body that is not process output: it
/// states a count, names the reachable verbs, and nothing it contains was printed
/// by the command. A single row wedged between two output rows had to pretend to
/// be output, so it either stretched a fact and an affordance across the full
/// width (a desert of whitespace between them) or stacked them into one crowded
/// line. Padding it above and below and insetting it makes the distinction
/// *structural*: the reader sees a small object floating inside the log, not
/// another log line.
///
/// # What each row says
///
/// ```text
///   ▎ 246 lines hidden (7.1 KB)
///   ▎ press v view full log, press y copy hidden
/// ```
///
/// The first row is the fact (how much is missing, and how big it was). The second
/// is how to act on it, written as a sentence so the two keys read as words — `v`
/// for *view*, `y` for *yank*. The keys are the only bold, high-contrast cells,
/// which is what makes them the thing the eye lands on while scanning.
///
/// # Why the gutter is a mark, not a track
///
/// Both rows carry the same one-column mark so they read as one object, and the
/// mark stops where the block stops. Nothing in the body carries a track: the
/// terminal's own shift-drag selection cannot tell chrome from content, so a `│`
/// in front of every stdout row meant every copied line arrived with a border
/// glued to it (ADR-0150).
///
/// # Degradation
///
/// Both rows degrade by *choosing a shorter whole rendering*, never by cutting a
/// fragment. A count is the longest form that fits (`246 lines hidden (7.1 KB)` →
/// `246 lines hidden` → `246 hidden` → nothing); the verb row walks a ladder from
/// the full two-clause sentence down to the bare key, and a clause is never cut
/// mid-word. A row that printed `12 lines hidde` or `press v view f` would be
/// asserting something false about the log or the key, which is the class of
/// defect this whole component was rebuilt to remove.
///
/// # Hit targets
///
/// Each verb owns its own region (see [`TOOL_FOLD_BLOCK_IDX`] /
/// [`TOOL_FOLD_COPY_BLOCK_IDX`]): one region per affordance, because a row-wide
/// target could not tell "copy hidden" from "view full log". The region covers
/// the clause the reader perceives as the button, not just its key letter.
fn emit_fold_block(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    body: &CommandBodyCtx<'_>,
    fold: &BashFold,
    hidden_rows: &[String],
) {
    let inset = body.indent + BASH_FOLD_INSET_COLS;
    let glyph = ctx.theme.command_fold_gutter_glyph();
    // No band, no frame: the block sits on the same code surface as the rows it
    // interrupts, and is told apart by three things only — the blank rows above
    // and below, the inset, and the gutter mark. A second background tier here
    // would be decoration competing with the transcript's own surface ladder.
    let fill = Style::default().bg(body.surface);
    let gutter_style = fill.fg(ctx.theme.command_fold_gutter());
    let count_style = fill.fg(ctx.theme.code_text());
    let hint_style = fill.fg(ctx.theme.muted());
    let key_style = fill
        .fg(ctx.theme.keycap_accent())
        .add_modifier(Modifier::BOLD);
    // `gutter + gap` on both text rows: the block's text starts one column past
    // its mark, so the two rows and the blank rows above/below share one edge.
    let text_col = inset + glyph.width() + 1;
    let space = ctx.full_width.saturating_sub(text_col);

    // Blank row above: real estate, not a region. It separates the fold from the
    // head rows so a shift-drag over the head can never include it.
    draw_blank_rows(ctx, fill, 1);

    // ---- Fact row -----------------------------------------------------------
    let omitted_kb = fold.omitted_bytes as f64 / 1024.0;
    let count = fold_count_for_budget(fold.hidden, omitted_kb, space);
    let mut stats_row = vec![
        Span::styled(" ".repeat(inset), fill),
        Span::styled(glyph.to_string(), gutter_style),
        Span::styled(" ", fill),
        Span::styled(count.clone(), count_style),
    ];
    let mut used = text_col + count.width();
    // `[INV-FOLD-01]`: a fold that hides a failure site must say so, or the
    // diagnosis becomes unreachable behind a bare count. The signature rides on
    // the fact row — never the verb row — so it never competes with the keys.
    if let Some(signature) = first_salient_signature(hidden_rows) {
        let budget = ctx
            .full_width
            .saturating_sub(used)
            .saturating_sub(JOIN_CAUSE.width());
        if let Some(shown) = fold_signature_for_budget(&signature, budget) {
            used += JOIN_CAUSE.width() + shown.width();
            stats_row.push(Span::styled(JOIN_CAUSE, gutter_style));
            stats_row.push(Span::styled(shown, fill.fg(ctx.theme.warn())));
        }
    }
    stats_row.push(Span::styled(padded_tail(ctx.full_width, used), fill));
    // The fact row is chrome: it registers nothing, so no selection can capture a
    // sentence the process never printed.
    ctx.paint(Line::from(stats_row));

    // ---- Verb row -----------------------------------------------------------
    let clauses = FOLD_VERB_CLAUSES;
    // The ladder, most informative first. Every rung is a whole rendering, so a
    // clause can never be painted as `press v view f` — a promise of a verb the
    // row then fails to name. It bottoms out at the bare key, which is still the
    // one cell the sentence exists to make findable.
    let ladder = [
        (VerbTerse::Sentence, 2),
        (VerbTerse::Compact, 2),
        (VerbTerse::Sentence, 1),
        (VerbTerse::Compact, 1),
        (VerbTerse::KeyOnly, 2),
        (VerbTerse::KeyOnly, 1),
    ];
    let room = ctx.full_width.saturating_sub(text_col);
    let chosen = ladder
        .into_iter()
        .find(|&(terse, keep)| fold_verbs_width(&clauses[..keep], terse) <= room);

    let mut spans = vec![
        Span::styled(" ".repeat(inset), fill),
        Span::styled(glyph.to_string(), gutter_style),
        Span::styled(" ", fill),
    ];
    used = text_col;
    let mut hit_spans: Vec<(usize, (u16, u16))> = Vec::with_capacity(2);
    if let Some((terse, keep)) = chosen {
        for (idx, clause) in clauses[..keep].iter().enumerate() {
            if idx > 0 {
                spans.push(Span::styled(VERB_JOIN.to_string(), hint_style));
                used += VERB_JOIN.width();
            }
            let (before, after) = clause.connectives(terse);
            let start = used;
            if !before.is_empty() {
                spans.push(Span::styled(before.to_string(), hint_style));
                used += before.width();
            }
            spans.push(Span::styled(clause.key.to_string(), key_style));
            used += clause.key.width();
            spans.push(Span::styled(after.to_string(), hint_style));
            used += after.width();
            hit_spans.push((clause.sentinel(), (start as u16, used as u16)));
        }
    }
    spans.push(Span::styled(padded_tail(ctx.full_width, used), fill));
    if let Some(rect) = ctx.paint(Line::from(spans)) {
        // One region per clause, pushed in reverse so `region_at`'s first-match
        // walk still resolves each clause at any column inside it.
        for (sentinel, (start, end)) in hit_spans.into_iter().rev() {
            ctx.layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx: sentinel,
                start_byte: 0,
                end_byte: 0,
                text: String::new(),
                prefix_cols: 0,
                rect: Rect::new(rect.x + start, rect.y, end.saturating_sub(start), 1),
                hidden_ranges: Vec::new(),
            });
        }
    }

    // Blank row below: symmetric with the row above, so the block is inset on all
    // four sides and the tail rows resume on a clean edge.
    draw_blank_rows(ctx, fill, 1);
    // Both text rows are laid out whole, so the block's painted height is exactly
    // `BASH_FOLD_CHROME_ROWS` and the compile-time assertions on the fold geometry
    // keep holding.
}

/// The folded count, in the most specific form that fits `space` columns.
///
/// Chosen rather than truncated: `12 lines hidde` is a wrong number, and a
/// half-drawn `(` is a syntax error on screen. When nothing fits the row is left
/// with its gutter mark alone, which still says "something is here" without
/// asserting a figure it cannot show.
fn fold_count_for_budget(hidden: usize, omitted_kb: f64, space: usize) -> String {
    for candidate in [
        format!("{hidden} lines hidden ({omitted_kb:.1} KB)"),
        format!("{hidden} lines hidden"),
        format!("{hidden} hidden"),
        format!("{hidden}"),
    ] {
        if candidate.width() <= space {
            return candidate;
        }
    }
    String::new()
}

/// One "press `key` …" clause of the fold block's verb sentence.
///
/// The pieces are kept apart because they are painted in two tones: the
/// connectives are muted, the key is the bold cell the eye is meant to catch.
/// Spelling the sentence as data is also what lets the row drop whole clauses
/// under width pressure instead of cutting mid-word.
///
/// `compact_*` is the same clause with the polite connective removed, used only
/// when the full sentence cannot fit — so a narrow block still *names* its verb
/// rather than being cut to `press v view f`.
#[derive(Clone, Copy)]
struct VerbClause {
    key: &'static str,
    before: &'static str,
    after: &'static str,
    compact_after: &'static str,
}

/// How much of a clause is painted. Ordered from most to least informative, and
/// every level is a *complete* rendering — which is why this is an enum and not a
/// width budget for `truncate_to_width`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VerbTerse {
    /// `press v view full log`
    Sentence,
    /// `v view full log`
    Compact,
    /// `v`
    KeyOnly,
}

impl VerbClause {
    fn connectives(self, terse: VerbTerse) -> (&'static str, &'static str) {
        match terse {
            VerbTerse::Sentence => (self.before, self.after),
            VerbTerse::Compact => ("", self.compact_after),
            VerbTerse::KeyOnly => ("", ""),
        }
    }
    /// Which click sentinel this clause owns. One place, so the sentence and the
    /// hit target cannot disagree about which verb a clause names.
    fn sentinel(self) -> usize {
        if self.key == VIEW_LOG_KEY {
            TOOL_FOLD_BLOCK_IDX
        } else {
            TOOL_FOLD_COPY_BLOCK_IDX
        }
    }
}

/// Columns `clauses` occupy when painted as one row.
fn fold_verbs_width(clauses: &[VerbClause], terse: VerbTerse) -> usize {
    clauses
        .iter()
        .map(|c| {
            let (before, after) = c.connectives(terse);
            before.width() + c.key.width() + after.width()
        })
        .sum::<usize>()
        + VERB_JOIN.width() * clauses.len().saturating_sub(1)
}

/// Separator between two verb clauses: a comma and one space, because the row
/// reads as a sentence ("press v …, press y …") rather than as a legend.
const VERB_JOIN: &str = ", ";

/// The fold block's verbs, in priority order.
///
/// `v` (read the whole log) leads because it is the reason the block exists; `y`
/// is the shortcut and is the one that yields under pressure.
const FOLD_VERB_CLAUSES: [VerbClause; 2] = [
    VerbClause {
        key: VIEW_LOG_KEY,
        before: "press ",
        after: " view full log",
        compact_after: " view full log",
    },
    VerbClause {
        key: COPY_LOG_KEY,
        before: "press ",
        after: " copy hidden",
        compact_after: " copy hidden",
    },
];

/// The failure signature as it may be shown in `budget` columns, or `None` when
/// it cannot be shown honestly.
///
/// A signature is a machine token followed by prose (`error[E0425]: cannot find
/// value …`, `panicked at src/main.rs:9`). Cutting it at a *column* boundary
/// produces a different machine token — `error[E042…` is not an error code, and
/// `panicked at src/ma` names no file — so the only faithful renderings are
/// whole-token ones:
///
/// - the whole signature, when it fits;
/// - as many whole tokens as fit, then `…` (`error[E0425]: cannot find…`);
/// - nothing at all, when not even the first token fits.
///
/// The first token is the floor because it is the part a reader greps for. When
/// it cannot fit, the caller prints the count alone — honest — and the verb one
/// row below reaches the full text.
///
/// Working in tokens rather than columns is the whole point: `unicode-width` can
/// tell us where a glyph ends but not where an identifier ends, and only the
/// latter is safe to cut.
fn fold_signature_for_budget(signature: &str, budget: usize) -> Option<String> {
    if budget < BASH_FOLD_SIGNATURE_MIN_COLS {
        return None;
    }
    if signature.width() <= budget {
        return Some(signature.to_string());
    }
    let marker = '…';
    let mut tokens = signature.split_whitespace();
    // One column is reserved for the marker on every truncated rendering.
    let mut shown = tokens.next()?.to_string();
    if shown.width() + 1 > budget {
        return None;
    }
    for token in tokens {
        if shown.width() + 1 + token.width() + 1 > budget {
            break;
        }
        shown.push(' ');
        shown.push_str(token);
    }
    shown.push(marker);
    Some(shown)
}

/// First salient failure signature among the folded rows, if any.
///
/// Shares `nuo-tool`'s scanner with the model-facing fold so the human and the
/// model are pointed at the same line.
fn first_salient_signature(hidden_rows: &[String]) -> Option<String> {
    let omitted = hidden_rows.join("\n");
    nuo_tool::vfs::extract_salient_signatures(&omitted, 1)
        .into_iter()
        .next()
        .map(|(_, signature)| signature)
}

/// Emit a (possibly multi-line) bash body section at `indent`, wrapping to
/// `wrap_w`, all rows in `style`, anchoring selection byte ranges at
/// `*byte_offset` (advanced past the section).
///
/// Used by the renderers whose body is a plain text run rather than a derived
/// output view (tool errors, patch warnings). Command bodies go through
/// [`CommandBodyCtx`], which also owns the prompt row; both share the same
/// flush, unmarked row shape.
#[allow(clippy::too_many_arguments)]
fn emit_command_lines(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    pad: Style,
    sel_range: Option<(usize, Option<usize>)>,
    text: &str,
    style: Style,
    mut byte_offset: usize,
) -> usize {
    let text = text.trim_end_matches(&['\r', '\n'][..]);
    for logical_line in text.split('\n') {
        let logical_line = nuo_protocol::tool_output::normalize_carriage_returns(logical_line);
        let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));
        for wl in &wrapped {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: byte_offset + wl.start_byte,
                end_byte: byte_offset + wl.end_byte,
            };
            let mut line = line_spans(
                "",
                pad,
                &wl.text,
                line_selection(sel_range, &block_wl),
                style,
                ctx.theme.selected(),
            );
            line.spans.insert(0, Span::styled(" ".repeat(indent), pad));
            let used = indent + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
        }
        byte_offset += logical_line.len() + 1;
    }
    byte_offset
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedCode {
    pub content: String,
    pub start_line: usize,
    pub detected_path: Option<String>,
}

fn parse_numbered_line(line: &str, delim: char) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let digits_end = trimmed.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    let num: usize = trimmed[..digits_end].parse().ok()?;
    let rest = trimmed[digits_end..].trim_start();
    if delim == '\t' {
        if !rest.starts_with('\t') {
            return None;
        }
        return Some((num, &rest[1..]));
    }
    if !rest.starts_with(delim) {
        return None;
    }
    let after_delim = &rest[delim.len_utf8()..];
    // Reject operator repetitions like `||` or `::`
    if after_delim.starts_with(delim) {
        return None;
    }
    let content = if let Some(stripped) = after_delim.strip_prefix(' ') {
        stripped
    } else if after_delim.is_empty() {
        ""
    } else if delim == ':' {
        // For ':' delimiter, must be followed by space or empty string
        return None;
    } else {
        after_delim
    };
    Some((num, content))
}

pub(crate) fn normalize_code_content(
    raw: &str,
    raw_start_line: usize,
    arguments: &str,
) -> NormalizedCode {
    let (arg_offset, arg_path) = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .map(|v| {
            let offset = v
                .get("offset")
                .or_else(|| v.get("start_line"))
                .and_then(|val| val.as_u64())
                .map(|o| o as usize);
            let path = v
                .get("path")
                .or_else(|| v.get("file_path"))
                .or_else(|| v.get("filename"))
                .or_else(|| v.get("file"))
                .and_then(|p| p.as_str())
                .map(|s| s.to_string());
            (offset, path)
        })
        .unwrap_or((None, None));

    if raw.is_empty() {
        return NormalizedCode {
            content: String::new(),
            start_line: raw_start_line.max(arg_offset.unwrap_or(1)).max(1),
            detected_path: arg_path,
        };
    }

    let mut lines: Vec<&str> = raw.lines().collect();
    let mut header_start_line: Option<usize> = None;
    let mut detected_path: Option<String> = arg_path;

    // 1. Strip leading tool-result envelope marker (e.g. `[read_text result]:`) if present
    if let Some(first_idx) = lines.iter().position(|l| !l.trim().is_empty()) {
        let first = lines[first_idx].trim();
        if first.starts_with('[') && first.ends_with("result]:") {
            lines.drain(..=first_idx);
        }
    }

    // 2. Strip leading `[Lines <start>-<end> of <total> from <path>]` model-facing framing
    if let Some(first_idx) = lines.iter().position(|l| !l.trim().is_empty()) {
        let first = lines[first_idx].trim();
        if first.starts_with("[Lines ") && first.ends_with(']') {
            let after_prefix = &first["[Lines ".len()..];
            let num_end = after_prefix
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_prefix.len());
            if let Ok(num) = after_prefix[..num_end].parse::<usize>() {
                header_start_line = Some(num);
            }
            if let Some(from_pos) = first.find("from ") {
                let after_from = &first[from_pos + "from ".len()..first.len() - 1];
                let cleaned_path = after_from
                    .trim()
                    .trim_matches(|c| c == '`' || c == '"' || c == '\'');
                if !cleaned_path.is_empty() && detected_path.is_none() {
                    detected_path = Some(cleaned_path.to_string());
                }
            }
            lines.drain(..=first_idx);
        }
    }

    // 3. Strip trailing continuation hint like `[398 more lines — read with offset=102]`
    if let Some(last_idx) = lines.iter().rposition(|l| !l.trim().is_empty()) {
        let last = lines[last_idx].trim();
        if last.starts_with('[')
            && last.ends_with(']')
            && (last.contains("more line") || last.contains("more lines"))
        {
            lines.drain(last_idx..);
        }
    }

    // 4. Detect and strip embedded line numbers (e.g. `   1 | ---` or `1: ---` or `1\t---`)
    let mut matched_delim: Option<char> = None;
    let mut first_matched_num: Option<usize> = None;

    if !lines.is_empty() {
        for &delim in &['|', '│', ':', '\t'] {
            let mut non_empty_count = 0;
            let mut matched_count = 0;
            let mut prev_num: Option<usize> = None;
            let mut strictly_increasing = true;
            let mut first_num = None;

            for line in &lines {
                if line.trim().is_empty() {
                    continue;
                }
                non_empty_count += 1;
                if let Some((num, _)) = parse_numbered_line(line, delim) {
                    matched_count += 1;
                    if first_num.is_none() {
                        first_num = Some(num);
                    }
                    if let Some(prev) = prev_num {
                        if num <= prev {
                            strictly_increasing = false;
                            break;
                        }
                    }
                    prev_num = Some(num);
                } else {
                    break;
                }
            }

            if non_empty_count > 0 && matched_count == non_empty_count && strictly_increasing {
                if delim == ':' && non_empty_count == 1 {
                    let matches_context = header_start_line.is_some_and(|h| Some(h) == first_num)
                        || arg_offset.is_some_and(|o| Some(o) == first_num);
                    if !matches_context {
                        continue;
                    }
                }
                matched_delim = Some(delim);
                first_matched_num = first_num;
                break;
            }
        }
    }

    let stripped_embedded = matched_delim.is_some();
    let clean_lines: Vec<String> = if let Some(delim) = matched_delim {
        lines
            .iter()
            .map(|line| {
                if let Some((_, content)) = parse_numbered_line(line, delim) {
                    content.to_string()
                } else {
                    line.to_string()
                }
            })
            .collect()
    } else {
        lines.iter().map(|l| l.to_string()).collect()
    };

    let resolved_start_line = if let Some(first) = first_matched_num.filter(|&n| n > 1) {
        first
    } else if raw_start_line > 0 {
        raw_start_line
    } else if let Some(header_start) = header_start_line.filter(|&h| h > 1) {
        header_start
    } else if let Some(offset) = arg_offset.filter(|&o| o > 1) {
        offset
    } else if let Some(header_start) = header_start_line {
        header_start
    } else if let Some(offset) = arg_offset {
        offset
    } else {
        first_matched_num.unwrap_or(1)
    };

    let mut content = clean_lines.join("\n");
    if !stripped_embedded && raw.ends_with('\n') {
        content.push('\n');
    } else if stripped_embedded && clean_lines.last().is_some_and(String::is_empty) {
        // A framed slice whose final line is blank (e.g. `  10 | `) must keep
        // that blank line. `join` cannot represent a trailing empty element on
        // its own because `str::lines` drops a single trailing newline, so the
        // blank line needs one extra terminator to survive round-tripping.
        content.push('\n');
    }

    NormalizedCode {
        content,
        start_line: resolved_start_line.max(1),
        detected_path,
    }
}

/// Render an expanded tool step's content — no `Result`/`Diff` label, no
/// separator; just the tool-specific block dispatched by `result_kind`. Known
/// tools with structured output get a specialized renderer; everything else
/// falls back to a line-numbered code block via [`draw_code_content`]. `bash`
/// additionally prefixes the block with a `$ command` line so the whole step
/// reads like a terminal session.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_tool_result(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    message_id: u64,
    name: &str,
    arguments: &str,
    output: &str,
    structured: Option<&nuo_protocol::ToolOutput>,
    diff_cache: &mut DiffCache,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
    revealed: bool,
) {
    let block_idx = 1usize;
    // Explicit tool errors are terminal results, not the successful shape the
    // presenter normally renders. In particular, a failed edit has no Patch;
    // deriving a diff from its call arguments would show an intended change
    // that never reached disk and hide the actionable failure message.
    if matches!(
        structured,
        Some(
            nuo_protocol::ToolOutput::Error { .. }
                | nuo_protocol::ToolOutput::PermissionDenied { .. }
        )
    ) {
        draw_tool_error(ctx, mi, block_idx, output, selection, indent, inner_w);
        return;
    }

    let kind = nuo_view::tools::presenter_for(name).result_kind();
    match kind {
        ResultKind::Listing => {
            draw_listing_content(ctx, mi, block_idx, output, selection, indent, inner_w)
        }
        ResultKind::Matches => draw_matches_content(
            ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
        ),
        ResultKind::Command => {
            let command = command_for(structured, arguments);
            draw_command_content(
                ctx, mi, block_idx, output, structured, &command, selection, indent, inner_w,
                revealed,
            );
        }
        ResultKind::Code => {
            // Prefer the structured payload: `Code::text` is pure file content
            // (the model-facing `prefix`/`suffix` framing is ignored here) and
            // `start_line` carries the read `offset` so an offset snippet
            // numbers from its true file line. `lang` is surfaced as a
            // language-tag line so the block matches the markdown code band.
            // Legacy/restored steps without a payload fall back to the
            // flattened `output` string with `start_line = 0` (slice-relative
            // 1-based numbering).
            let (raw_content, raw_start_line, explicit_lang) = match structured {
                Some(nuo_protocol::ToolOutput::Code {
                    text,
                    start_line,
                    lang,
                    ..
                }) => (text.as_str(), *start_line, lang.as_deref()),
                Some(nuo_protocol::ToolOutput::Patch {
                    new, start_line, ..
                }) => (new.as_str(), *start_line, None),
                _ => (output, 0, None),
            };

            let normalized = normalize_code_content(raw_content, raw_start_line, arguments);

            // The file path is authoritative for file content; `lang` is a
            // narrow extension hint (e.g. `"rs"`) whose whole job is the
            // language-tag line. Preferring the path keeps syntax highlighting
            // when the hint is a bare extension that `from_path` can't resolve.
            let path_lang = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    v.get("path")
                        .or_else(|| v.get("file_path"))
                        .or_else(|| v.get("filename"))
                        .or_else(|| v.get("file"))
                        .and_then(|p| p.as_str())
                        .map(nuo_ui_primitives::syntax::Language::from_path)
                })
                .or_else(|| {
                    normalized
                        .detected_path
                        .as_deref()
                        .map(nuo_ui_primitives::syntax::Language::from_path)
                });
            let syntax_lang = path_lang
                .or_else(|| explicit_lang.map(nuo_ui_primitives::syntax::Language::from_path))
                .unwrap_or(nuo_ui_primitives::syntax::Language::Plain);

            draw_code_content(
                ctx,
                mi,
                block_idx,
                &normalized.content,
                normalized.start_line,
                explicit_lang,
                syntax_lang,
                selection,
                indent,
                inner_w,
            )
        }
        ResultKind::Diff => {
            // Prefer the structured Patch payload (old/new from the result);
            // fall back to argument-derived rows only for legacy/restored
            // completed steps. Both paths are cached by stable message id and
            // exact source, so animation frames never repeat Myers/word diffing.
            let path_buf: Option<String> = match structured {
                Some(nuo_protocol::ToolOutput::Patch { .. }) => None,
                _ => {
                    let map = if let Ok(serde_json::Value::Object(obj)) =
                        serde_json::from_str::<serde_json::Value>(arguments)
                    {
                        obj
                    } else {
                        nuo_view::tools::parse_streaming_arguments(arguments)
                    };
                    map.get("path")
                        .or_else(|| map.get("file_path"))
                        .or_else(|| map.get("filename"))
                        .or_else(|| map.get("file"))
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                }
            };
            let path_ref = match structured {
                Some(nuo_protocol::ToolOutput::Patch { path, .. }) => Some(path.as_str()),
                _ => path_buf.as_deref(),
            };
            let lang = path_ref
                .map(nuo_ui_primitives::syntax::Language::from_path)
                .unwrap_or(nuo_ui_primitives::syntax::Language::Plain);

            let hunks = match structured {
                Some(nuo_protocol::ToolOutput::Patch {
                    old,
                    new,
                    start_line,
                    ..
                }) => {
                    let mut start = *start_line;
                    if start == 0
                        && let Some(path) = path_ref
                    {
                        if let Some(resolved) =
                            find_start_line_in_file(path, ctx.workspace_root, old, new)
                        {
                            start = resolved;
                        }
                    }
                    diff_cache.patch(message_id, old, new, start)
                }
                _ => {
                    let (old_buf, new_buf) = extract_diff_strings_for_tool(name, arguments);
                    if !old_buf.is_empty() || !new_buf.is_empty() {
                        let start = if let Some(path) = path_ref {
                            find_start_line_in_file(path, ctx.workspace_root, &old_buf, &new_buf)
                                .unwrap_or(0)
                        } else {
                            0
                        };
                        diff_cache.patch(message_id, &old_buf, &new_buf, start)
                    } else {
                        diff_cache.legacy_arguments(message_id, name, arguments)
                    }
                }
            };
            if let Some(nuo_protocol::ToolOutput::Patch { warnings, .. }) = structured {
                for warning in warnings {
                    let text = format!("Warning: {warning}");
                    let bg = ctx.theme.code_surface();
                    let style = Style::default().bg(bg).fg(ctx.theme.warn());
                    let _ = emit_command_lines(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        inner_w.max(1),
                        Style::default().bg(bg),
                        block_selection_range(selection, mi, block_idx),
                        &text,
                        style,
                        0,
                    );
                }
            }
            if hunks.is_empty() && !output.trim().is_empty() {
                draw_code_content(
                    ctx,
                    mi,
                    block_idx,
                    output,
                    1,
                    None,
                    nuo_ui_primitives::syntax::Language::Plain,
                    selection,
                    indent,
                    inner_w,
                );
            } else {
                draw_diff_content(ctx, hunks.as_ref(), indent, inner_w, lang);
            }
        }
        ResultKind::Checklist => {
            draw_checklist_content(
                ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
            );
        }
        ResultKind::WebSearch => {
            draw_web_search_content(
                ctx, mi, block_idx, output, arguments, structured, selection, indent, inner_w,
            );
        }
        ResultKind::WebArticle => {
            draw_web_article_content(
                ctx, mi, block_idx, output, arguments, structured, selection, indent, inner_w,
            );
        }
        ResultKind::Questions => {
            draw_questions_content(
                ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
            );
        }
    }
}

/// Locate the 1-based start line of an edit (`old` or `new` snippet) within the specific file.
pub(crate) fn find_start_line_in_file(
    path: &str,
    workspace_root: Option<&std::path::Path>,
    old: &str,
    new: &str,
) -> Option<usize> {
    let p = std::path::Path::new(path);
    let full_path = if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(root) = workspace_root {
        root.join(p)
    } else {
        p.to_path_buf()
    };

    let content = std::fs::read_to_string(&full_path).ok()?;

    let find_in_content = |needle: &str| -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        if let Some((offset, _)) = content.match_indices(needle).next() {
            return Some(content[..offset].matches('\n').count() + 1);
        }
        let trimmed = needle.trim_end();
        if !trimmed.is_empty() {
            if let Some((offset, _)) = content.match_indices(trimmed).next() {
                return Some(content[..offset].matches('\n').count() + 1);
            }
        }
        if needle.contains('\r') || content.contains('\r') {
            let content_norm = content.replace("\r\n", "\n");
            let needle_norm = needle.replace("\r\n", "\n");
            if let Some((offset, _)) = content_norm.match_indices(&needle_norm).next() {
                return Some(content_norm[..offset].matches('\n').count() + 1);
            }
            let needle_norm_trimmed = needle_norm.trim_end();
            if !needle_norm_trimmed.is_empty() {
                if let Some((offset, _)) = content_norm.match_indices(needle_norm_trimmed).next() {
                    return Some(content_norm[..offset].matches('\n').count() + 1);
                }
            }
        }
        None
    };

    let find_by_lines = |needle: &str| -> Option<usize> {
        let needle_lines: Vec<&str> = needle
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect();
        if needle_lines.is_empty() {
            return None;
        }
        let file_lines: Vec<&str> = content.lines().map(str::trim_end).collect();
        if needle_lines.len() > file_lines.len() {
            return None;
        }
        for (i, window) in file_lines.windows(needle_lines.len()).enumerate() {
            if window == needle_lines.as_slice() {
                return Some(i + 1);
            }
        }
        None
    };

    find_in_content(old)
        .or_else(|| find_in_content(new))
        .or_else(|| find_by_lines(old))
        .or_else(|| find_by_lines(new))
}

pub(crate) fn extract_diff_strings_for_tool(name: &str, arguments: &str) -> (String, String) {
    let map = if let Ok(serde_json::Value::Object(obj)) =
        serde_json::from_str::<serde_json::Value>(arguments)
    {
        obj
    } else {
        nuo_view::tools::parse_streaming_arguments(arguments)
    };
    if map.is_empty() {
        return (String::new(), String::new());
    }
    let get_any = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| map.get(*k).and_then(serde_json::Value::as_str))
            .map(|s| s.to_string())
    };
    let base = name.rsplit_once(':').map(|(_, b)| b).unwrap_or(name);
    match base {
        "edit_text" | "edit" | "edit_file" => {
            let old = get_any(&["old_string", "old_str", "old_text", "old"]).unwrap_or_default();
            let new = get_any(&["new_string", "new_str", "new_text", "new"]).unwrap_or_default();
            (old, new)
        }
        "write_file" | "write" => {
            let content = get_any(&["content", "new_string", "text"]).unwrap_or_default();
            (String::new(), content)
        }
        _ => (String::new(), String::new()),
    }
}

/// Render an explicit tool failure as error text on the shared code surface.
/// It deliberately has no line-number gutter: these rows describe the failed
/// operation and are not source-file content.
pub(crate) fn draw_tool_error(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let bg = ctx.theme.code_surface();
    let pad = Style::default().bg(bg);
    let error = Style::default()
        .bg(bg)
        .fg(ctx.theme.err())
        .add_modifier(Modifier::BOLD);
    let selection = block_selection_range(selection, mi, block_idx);
    let _ = emit_command_lines(
        ctx,
        mi,
        block_idx,
        indent,
        inner_w.max(1),
        pad,
        selection,
        output,
        error,
        0,
    );
}

/// Resolve the shell command for a `bash` step: prefer the structured
/// [`ToolOutput::Shell`](nuo_protocol::ToolOutput) payload (set as soon as the
/// call starts, so it is available even while streaming), falling back to
/// parsing the JSON arguments for legacy / restored sessions without a
/// structured payload.
pub(crate) fn command_for(
    structured: Option<&nuo_protocol::ToolOutput>,
    arguments: &str,
) -> String {
    if let Some(nuo_protocol::ToolOutput::Shell { command, .. }) = structured
        && !command.is_empty()
    {
        return command.clone();
    }
    nuo_view::model::document::parse_arguments_kv(arguments)
        .iter()
        .find(|(k, _)| k == "command")
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// Render explicit Git-style hunks inside an expanded edit step. Every hunk
/// starts with its authoritative `@@ -N,M +P,Q @@` header, followed by rows
/// with dual old/new line-number gutters and colored change signs. Hunk
/// grouping and ranges are derived before rendering; this function only
/// paints them and never infers source semantics from presentation rows.
///
/// Every logical row goes through [`RenderCtx::paint`], which counts it in
/// `content_lines` even when scroll-skip or viewport clip keeps it off
/// screen. Undercounting here would make the measured step height (and thus
/// `max_scroll`) depend on the scroll position, feeding back as jumpy scroll
/// and flicker while the transcript animates.
pub(crate) fn draw_diff_content(
    ctx: &mut RenderCtx<'_, '_>,
    hunks: &[DiffHunk],
    indent: usize,
    inner_w: usize,
    lang: nuo_ui_primitives::syntax::Language,
) {
    if hunks.is_empty() {
        return;
    }
    let code_bg = ctx.theme.code_surface();
    let gutter_fg = ctx.theme.muted();
    // Each number column is at least 2 chars wide so single-digit files
    // align cleanly (GitHub-style: right-aligned old_no | new_no).
    let max_no = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| line.old_no.or(line.new_no))
        .max()
        .unwrap_or(0);
    let gutter_w = max_no.to_string().len().max(2);
    let sign_w = 2usize; // "+ " / "- " / "  "
    // Dual gutter: old_no(right, gutter_w) + " " + new_no(right, gutter_w).
    let gutter_cols = 2 * gutter_w + 1;
    let text_w = inner_w.saturating_sub(gutter_cols + sign_w).max(1);
    // opencode-style banding: the whole row carries a low-chroma tint so
    // added/removed blocks read at a glance, and the exact edited word sits
    // on a brighter tint on top of the row band. Context rows stay on the
    // neutral code surface so they recede. All four tints are first-class
    // theme tokens (the block-level design contract) — no inline literals —
    // so retuning the palette here retunes every block-level surface.
    let add_row_bg = ctx.theme.diff_add_bg();
    let del_row_bg = ctx.theme.diff_del_bg();
    let add_hi_bg = ctx.theme.diff_add_hl();
    let del_hi_bg = ctx.theme.diff_del_hl();
    let info_fg = ctx.theme.info();

    for hunk in hunks {
        let range_header = hunk.range_header();
        {
            let pad = Style::default().bg(code_bg);
            let mut spans: Vec<Span<'static>> = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(" ".repeat(gutter_cols), pad),
                Span::styled("  ", Style::default().bg(code_bg)),
                Span::styled(
                    range_header.clone(),
                    Style::default().bg(code_bg).fg(info_fg),
                ),
            ];
            let mut used = indent + gutter_cols + sign_w + range_header.len();
            if let Some(hint) = &hunk.hint {
                spans.push(Span::styled(" ", Style::default().bg(code_bg)));
                spans.push(Span::styled(
                    hint.clone(),
                    Style::default().bg(code_bg).fg(ctx.theme.muted()),
                ));
                used += 1 + hint.chars().count();
            }
            spans.push(Span::styled(
                padded_tail(ctx.full_width, used),
                Style::default().bg(code_bg),
            ));

            ctx.paint(Line::from(spans));
        }

        for line in &hunk.lines {
            let (sign, row_bg, base_fg, hi_bg) = match line.op {
                DiffOp::Add => ('+', add_row_bg, ctx.theme.ok(), add_hi_bg),
                DiffOp::Remove => ('-', del_row_bg, ctx.theme.err(), del_hi_bg),
                DiffOp::Context => (' ', code_bg, ctx.theme.muted(), code_bg),
            };
            let pad = Style::default().bg(row_bg);

            let full = line.text();
            let wrapped = nonempty_wrapped(wrap_text(&full, text_w));
            let syntax_spans = nuo_ui_primitives::syntax::tokenize_line(&full, lang);

            let (first_old, first_new) = match line.op {
                DiffOp::Context => (fmt_no(line.old_no, gutter_w), fmt_no(line.new_no, gutter_w)),
                DiffOp::Remove => (fmt_no(line.old_no, gutter_w), fmt_no(None, gutter_w)),
                DiffOp::Add => (fmt_no(None, gutter_w), fmt_no(line.new_no, gutter_w)),
            };
            let blank_col = fmt_no(None, gutter_w);

            for (i, wl) in wrapped.iter().enumerate() {
                let is_cont = i > 0;
                let (old_col, new_col) = if is_cont {
                    (&blank_col, &blank_col)
                } else {
                    (&first_old, &first_new)
                };
                let sign_text = if is_cont {
                    "  "
                } else {
                    match sign {
                        '+' => "+ ",
                        '-' => "- ",
                        _ => "  ",
                    }
                };
                let mut spans: Vec<Span<'static>> = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(old_col.clone(), Style::default().bg(row_bg).fg(gutter_fg)),
                    Span::styled(" ", Style::default().bg(row_bg)),
                    Span::styled(new_col.clone(), Style::default().bg(row_bg).fg(gutter_fg)),
                    Span::styled(
                        sign_text,
                        Style::default()
                            .bg(row_bg)
                            .fg(base_fg)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                let projected = project_syntax_diff_frags(
                    &full,
                    &line.frags,
                    &syntax_spans,
                    wl.start_byte,
                    wl.end_byte,
                );
                if !projected.is_empty() {
                    for slice in projected {
                        let token_fg = if lang == nuo_ui_primitives::syntax::Language::Plain {
                            base_fg
                        } else {
                            ctx.theme.syntax_color(slice.kind)
                        };
                        let bg = if slice.changed { hi_bg } else { row_bg };
                        let mut style = Style::default().bg(bg).fg(token_fg);
                        if slice.changed {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if slice.kind == nuo_ui_primitives::syntax::SyntaxKind::Comment {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        spans.push(Span::styled(slice.text.to_string(), style));
                    }
                } else {
                    spans.push(Span::styled(
                        wl.text.clone(),
                        Style::default().bg(row_bg).fg(base_fg),
                    ));
                }
                let used = indent + gutter_cols + sign_w + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        }
    }
}

/// Format an optional line number as a right-aligned, `width`-wide string.
/// `None` yields `width` spaces.
fn fmt_no(no: Option<usize>, width: usize) -> String {
    match no {
        Some(n) => format!("{:>width$}", n, width = width),
        None => format!("{:>width$}", "", width = width),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HighlightedSlice<'a> {
    pub text: &'a str,
    pub kind: nuo_ui_primitives::syntax::SyntaxKind,
    pub changed: bool,
}

/// Project syntax tokens and word-diff frags onto a wrapped slice covering
/// byte range `[start_byte..end_byte)`.
///
/// Implements orthogonal two-layer rendering:
/// - Layer 1 (AST / Lexical Syntax): Token foreground color (Keyword, Type, Function, etc.)
/// - Layer 2 (Diff Topology): Delta background tint (Add, Remove, Word-level highlight)
///
/// Ensures both layers blend seamlessly without degradation across wrapped continuation rows.
pub(crate) fn project_syntax_diff_frags<'a>(
    full: &'a str,
    frags: &[nuo_view::tools::DiffFrag],
    syntax_spans: &[nuo_ui_primitives::syntax::SyntaxSpan],
    start_byte: usize,
    end_byte: usize,
) -> Vec<HighlightedSlice<'a>> {
    let mut result = Vec::new();
    if start_byte >= end_byte || full.is_empty() {
        return result;
    }

    let mut points = Vec::with_capacity(frags.len() * 2 + syntax_spans.len() * 2 + 2);
    points.push(start_byte);
    points.push(end_byte);

    let mut curr_frag_byte = 0;
    let mut frag_ranges = Vec::with_capacity(frags.len());
    for frag in frags {
        let f_start = curr_frag_byte;
        let f_end = curr_frag_byte + frag.text.len();
        curr_frag_byte = f_end;
        frag_ranges.push((f_start, f_end, frag.changed));

        if f_start > start_byte && f_start < end_byte {
            points.push(f_start);
        }
        if f_end > start_byte && f_end < end_byte {
            points.push(f_end);
        }
    }

    for span in syntax_spans {
        if span.start_byte > start_byte && span.start_byte < end_byte {
            points.push(span.start_byte);
        }
        if span.end_byte > start_byte && span.end_byte < end_byte {
            points.push(span.end_byte);
        }
    }

    points.sort_unstable();
    points.dedup();

    for pair in points.windows(2) {
        let seg_start = pair[0];
        let seg_end = pair[1];
        if seg_start >= seg_end {
            continue;
        }

        if !full.is_char_boundary(seg_start) || !full.is_char_boundary(seg_end) {
            continue;
        }

        let slice = &full[seg_start..seg_end];
        let clean = slice.trim_end_matches('\n').trim_end_matches('\r');
        if clean.is_empty() {
            continue;
        }

        let changed = frag_ranges
            .iter()
            .find(|(fs, fe, _)| *fs <= seg_start && seg_start < *fe)
            .map(|(_, _, ch)| *ch)
            .unwrap_or(false);

        let kind = syntax_spans
            .iter()
            .find(|s| s.start_byte <= seg_start && seg_start < s.end_byte)
            .map(|s| s.kind)
            .unwrap_or(nuo_ui_primitives::syntax::SyntaxKind::Plain);

        result.push(HighlightedSlice {
            text: clean,
            kind,
            changed,
        });
    }

    result
}

/// Project a diff line's word-level fragments onto a wrapped slice covering
/// byte range `[start_byte..end_byte)`.
#[cfg(test)]
pub(crate) fn project_frags_to_wrapped<'a>(
    full: &'a str,
    frags: &'a [nuo_view::tools::DiffFrag],
    start_byte: usize,
    end_byte: usize,
) -> Vec<(&'a str, bool)> {
    let empty_syntax = Vec::new();
    project_syntax_diff_frags(full, frags, &empty_syntax, start_byte, end_byte)
        .into_iter()
        .map(|s| (s.text, s.changed))
        .collect()
}

/// Project syntax tokens onto a wrapped slice of text covering `start_byte..end_byte`.
pub(crate) fn project_syntax_slice<'a>(
    full: &'a str,
    syntax_spans: &[nuo_ui_primitives::syntax::SyntaxSpan],
    start_byte: usize,
    end_byte: usize,
) -> Vec<(&'a str, nuo_ui_primitives::syntax::SyntaxKind)> {
    let mut result = Vec::new();
    if start_byte >= end_byte || full.is_empty() {
        return result;
    }

    let mut points = Vec::with_capacity(syntax_spans.len() * 2 + 2);
    points.push(start_byte);
    points.push(end_byte);

    for span in syntax_spans {
        if span.start_byte > start_byte && span.start_byte < end_byte {
            points.push(span.start_byte);
        }
        if span.end_byte > start_byte && span.end_byte < end_byte {
            points.push(span.end_byte);
        }
    }

    points.sort_unstable();
    points.dedup();

    for pair in points.windows(2) {
        let seg_start = pair[0];
        let seg_end = pair[1];
        if seg_start >= seg_end
            || !full.is_char_boundary(seg_start)
            || !full.is_char_boundary(seg_end)
        {
            continue;
        }

        let slice = &full[seg_start..seg_end];
        let clean = slice.trim_end_matches('\n').trim_end_matches('\r');
        if clean.is_empty() {
            continue;
        }

        let kind = syntax_spans
            .iter()
            .find(|s| s.start_byte <= seg_start && seg_start < s.end_byte)
            .map(|s| s.kind)
            .unwrap_or(nuo_ui_primitives::syntax::SyntaxKind::Plain);

        result.push((clean, kind));
    }

    result
}

#[allow(clippy::too_many_arguments)]
fn code_gutter_line_syntax(
    left_indent: usize,
    gutter: &str,
    gutter_gap: usize,
    code_bg: Color,
    gutter_fg: Color,
    full_line: &str,
    syntax_spans: &[nuo_ui_primitives::syntax::SyntaxSpan],
    start_byte: usize,
    end_byte: usize,
    _selected: Option<(usize, usize)>,
    _selected_bg: Color,
    full_width: usize,
    theme: &nuo_view::Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    let prefix = left_indent + 1 + gutter.len() + gutter_gap;

    spans.push(Span::styled(
        " ".repeat(left_indent),
        Style::default().bg(code_bg),
    ));
    spans.push(Span::styled(" ", Style::default().bg(code_bg)));
    spans.push(Span::styled(
        gutter.to_string(),
        Style::default().bg(code_bg).fg(gutter_fg),
    ));
    spans.push(Span::styled(
        " ".repeat(gutter_gap),
        Style::default().bg(code_bg),
    ));

    let projected = project_syntax_slice(full_line, syntax_spans, start_byte, end_byte);

    for (token_text, kind) in projected {
        let token_fg = theme.syntax_color(kind);
        let mut style = Style::default().bg(code_bg).fg(token_fg);
        if kind == nuo_ui_primitives::syntax::SyntaxKind::Comment {
            style = style.add_modifier(Modifier::ITALIC);
        }
        spans.push(Span::styled(token_text.to_string(), style));
    }

    let row_slice_len = if end_byte >= start_byte
        && full_line.is_char_boundary(start_byte)
        && full_line.is_char_boundary(end_byte)
    {
        full_line[start_byte..end_byte]
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .len()
    } else {
        0
    };
    let used = prefix + row_slice_len;
    spans.push(Span::styled(
        padded_tail(full_width, used),
        Style::default().bg(code_bg),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::tool_output::ShellTermination;

    #[test]
    fn termination_footer_renders_stream_guard_warning() {
        let theme = nuo_view::Theme::default();
        let footer = termination_footer(
            ShellTermination::StreamGuard,
            &theme,
            theme.match_title_surface(),
        );
        assert!(footer.is_some());
        let (text, style) = footer.unwrap();
        assert_eq!(text, "terminated — output limit reached");
        assert_eq!(style.fg, theme.warn());
        assert_eq!(style.bg, theme.match_title_surface());
    }

    #[test]
    fn termination_footer_renders_professional_supervisor_verdicts() {
        let theme = nuo_view::Theme::default();
        let surface = theme.match_title_surface();

        let timeout = termination_footer(ShellTermination::Timeout, &theme, surface).unwrap();
        assert_eq!(timeout.0, "terminated — timed out");
        assert_eq!(timeout.1.fg, theme.warn());

        let cancelled = termination_footer(ShellTermination::Cancelled, &theme, surface).unwrap();
        assert_eq!(cancelled.0, "terminated — cancelled");
        assert_eq!(cancelled.1.fg, theme.err());

        let idle = termination_footer(ShellTermination::IdleBlocked, &theme, surface).unwrap();
        assert_eq!(idle.0, "terminated — idle timeout");

        let input = termination_footer(ShellTermination::InputUnanswered, &theme, surface).unwrap();
        assert_eq!(input.0, "terminated — input prompt unanswered");

        let interactive =
            termination_footer(ShellTermination::InteractiveBlocked, &theme, surface).unwrap();
        assert_eq!(interactive.0, "not started — command asks for input");

        let detached = termination_footer(ShellTermination::Detached, &theme, surface).unwrap();
        assert_eq!(detached.0, "moved to background — still running");

        assert!(termination_footer(ShellTermination::Exited, &theme, surface).is_none());
    }

    /// ADR-0115: the footer must name *which* copy is short. A step whose
    /// structured payload crossed the model watermark still holds every row on
    /// screen, so it may not claim the screen kept a partial log.
    #[test]
    fn coverage_footer_names_the_short_copy() {
        assert_eq!(coverage_footer(OutputCoverage::Full), None);
        assert_eq!(
            coverage_footer(OutputCoverage::ModelTruncated),
            Some("output truncated for model context"),
            "the screen holds the whole log; only the model's copy was cut",
        );
        assert_eq!(
            coverage_footer(OutputCoverage::CutBeforeUi),
            Some("output truncated · partial kept"),
            "a payload cut before the UI keeps the older, graver wording",
        );
    }

    /// The fold label must disclose a hidden failure site the user cannot see
    /// (`[INV-FOLD-01]`): drawing only a count leaves the diagnosis unreachable.
    #[test]
    fn fold_label_surfaces_the_first_hidden_failure_signature() {
        let hidden = vec![
            "compiling foo".to_string(),
            "error[E0425]: cannot find value `config` in this scope".to_string(),
            "panicked at src/main.rs:9".to_string(),
        ];
        assert_eq!(
            first_salient_signature(&hidden).as_deref(),
            Some("error[E0425]: cannot find value `config` in this scope"),
        );
        assert_eq!(first_salient_signature(&["quiet line".to_string()]), None);
    }

    /// A signature may only be shown in full, at a word boundary, or not at all.
    ///
    /// `[INV-FOLD-01]` wants the hidden failure site named; correctness wants the
    /// name to stay *the same name*. A column-boundary cut turned
    /// `error[E0425]: …` into `error[E042…` — an identifier for an error that
    /// does not exist — at exactly the widths where a reader most needs the code.
    #[test]
    fn fold_signature_truncates_at_word_boundaries_or_not_at_all() {
        let signature = "error[E0425]: cannot find value `config` in this scope";

        // Wide enough: verbatim, unchanged.
        assert_eq!(
            fold_signature_for_budget(signature, 200).as_deref(),
            Some(signature)
        );
        // Exactly its own width: still verbatim (no gratuitous ellipsis).
        assert_eq!(
            fold_signature_for_budget(signature, signature.width()).as_deref(),
            Some(signature)
        );

        // Every intermediate budget: the leading token always survives whole, and
        // the fragment always ends on a word plus the marker.
        for budget in 14..signature.len() {
            let Some(shown) = fold_signature_for_budget(signature, budget) else {
                continue;
            };
            assert!(
                shown.width() <= budget,
                "`{shown}` overflows its {budget}-column budget"
            );
            assert!(
                shown.starts_with("error[E0425]"),
                "the error code is the part a reader greps for, budget {budget} gave `{shown}`"
            );
            assert!(
                shown.ends_with('…'),
                "a truncated signature says so, budget {budget} gave `{shown}`"
            );
            // The text before the marker is whole-token prefix of the source.
            let body = shown.trim_end_matches('…');
            assert!(
                signature.starts_with(body),
                "`{body}` must be a prefix of the signature"
            );
            let boundary = &signature[body.len()..];
            assert!(
                boundary.is_empty() || boundary.starts_with(' '),
                "`{body}` must end on a token boundary, not mid-token"
            );
        }

        // Below the floor: nothing, rather than a token that does not exist.
        assert_eq!(fold_signature_for_budget(signature, 4), None);
        // A single unbreakable token cannot be truncated honestly either.
        assert_eq!(fold_signature_for_budget("error[E0425]", 8), None);
        // A path is one token, so it is dropped whole rather than cut: naming
        // `src/` would assert a location that does not exist.
        assert_eq!(
            fold_signature_for_budget("panicked at src/main.rs:9", 20).as_deref(),
            Some("panicked at…")
        );
        assert_eq!(
            fold_signature_for_budget("panicked at src/main.rs:9", 30).as_deref(),
            Some("panicked at src/main.rs:9")
        );
    }

    #[test]
    fn is_matches_header_detects_tool_count_line() {
        assert!(is_matches_header("Found 3 match(es):"));
        assert!(is_matches_header("Found 1 match(es):"));
        assert!(!is_matches_header("src/a.rs:10:let foo = 1;"));
        assert!(!is_matches_header("No matching lines found."));
    }

    #[test]
    fn literal_query_rejects_regex_and_empty() {
        assert_eq!(
            literal_query(r#"{"query":"foo","path":"src"}"#).as_deref(),
            Some("foo")
        );
        assert!(literal_query(r#"{"query":"f.o","regex":true}"#).is_none());
        assert!(literal_query(r#"{"query":""}"#).is_none());
        assert!(literal_query(r#"{"path":"src"}"#).is_none());
    }

    #[test]
    fn parse_listing_header_reads_find_files_shape() {
        // `find_files`: no scope path, so the band is just the file count.
        assert_eq!(
            parse_listing_header("Found 27 matching files:").as_deref(),
            Some("Found 27 files")
        );
        assert_eq!(
            parse_listing_header("Found 1 matching files:").as_deref(),
            Some("Found 1 file")
        );
    }

    #[test]
    fn parse_listing_header_is_none_without_the_tool_header() {
        // A restored session may start at the first entry — no band is invented.
        assert!(parse_listing_header("src/main.rs").is_none());
        assert!(parse_listing_header("src/a.rs\nsrc/b.rs").is_none());
        assert!(parse_listing_header("").is_none());
    }

    #[test]
    fn split_dir_leaf_keeps_the_trailing_slash_on_the_directory() {
        assert_eq!(
            split_dir_leaf("docs/adr/0001-x.md"),
            (Some("docs/adr/"), "0001-x.md")
        );
        assert_eq!(split_dir_leaf("src/"), (None, "src/"));
        assert_eq!(split_dir_leaf("Cargo.toml"), (None, "Cargo.toml"));
    }

    #[test]
    fn segment_matches_tiles_text_and_marks_every_occurrence() {
        let segments = segment_matches("let foo = foo;", Some("foo"));
        // Ranges must tile the whole string with no gaps and no overlap.
        let mut cursor = 0usize;
        for &(lo, hi, _) in &segments {
            assert_eq!(lo, cursor, "ranges must be contiguous");
            assert!(hi > lo, "no empty ranges");
            cursor = hi;
        }
        assert_eq!(cursor, "let foo = foo;".len());
        let matched: Vec<&str> = segments
            .iter()
            .filter(|(_, _, m)| *m)
            .map(|&(lo, hi, _)| &"let foo = foo;"[lo..hi])
            .collect();
        assert_eq!(matched, vec!["foo", "foo"]);
    }

    #[test]
    fn segment_matches_none_or_empty_query_is_one_plain_range() {
        assert_eq!(segment_matches("plain text", None), vec![(0, 10, false)]);
        assert_eq!(
            segment_matches("plain text", Some("")),
            vec![(0, 10, false)]
        );
        assert!(segment_matches("", Some("foo")).is_empty());
    }

    #[test]
    fn normalize_code_content_strips_lines_framing_and_embedded_numbers() {
        let raw = "\
[Lines 1-120 of 205 from `docs/adr/0024-two-row-head-band-session-identity-and-scene-row.md`]
   1 | ---
   2 | id: ADR-0024
   3 | title: \"Two-Row Head Band\"
   4 | status: accepted
   5 | date: 2026-10-06
   6 | scope: tui/nuo-tui
   7 | superseded_by: null
   8 | negative_knowledge: true
   9 | ---
  10 | ";
        let norm = normalize_code_content(raw, 0, "{}");
        assert_eq!(norm.start_line, 1);
        assert_eq!(
            norm.detected_path.as_deref(),
            Some("docs/adr/0024-two-row-head-band-session-identity-and-scene-row.md")
        );
        assert!(!norm.content.contains("[Lines"));
        assert!(!norm.content.contains("1 |"));
        let lines: Vec<&str> = norm.content.lines().collect();
        assert_eq!(lines[0], "---");
        assert_eq!(lines[1], "id: ADR-0024");
        assert_eq!(lines[8], "---");
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[9], "");
    }

    #[test]
    fn normalize_code_content_handles_offset_read_with_continuation_hint() {
        let raw = "\
[Lines 100-102 of 500 from `src/lib.rs`]
 100 | fn a() {}
 101 | fn b() {}
 102 | fn c() {}
[398 more lines — read with offset=103]";
        let norm = normalize_code_content(raw, 0, "{}");
        assert_eq!(norm.start_line, 100);
        assert_eq!(norm.content, "fn a() {}\nfn b() {}\nfn c() {}");
        assert_eq!(norm.detected_path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn normalize_code_content_handles_colon_delimited_output() {
        let raw = "10: fn hello() {}\n11: fn world() {}";
        let norm = normalize_code_content(raw, 0, "{}");
        assert_eq!(norm.start_line, 10);
        assert_eq!(norm.content, "fn hello() {}\nfn world() {}");
    }

    #[test]
    fn normalize_code_content_preserves_plain_unformatted_code() {
        let raw = "fn main() {\n    let x = 1;\n}\n";
        let norm = normalize_code_content(raw, 1, r#"{"path":"src/main.rs"}"#);
        assert_eq!(norm.start_line, 1);
        assert_eq!(norm.content, raw);
    }

    #[test]
    fn normalize_code_content_uses_arg_offset_when_slice_relative_numbered() {
        let raw = "1 | fn a() {}\n2 | fn b() {}";
        let norm = normalize_code_content(raw, 0, r#"{"path":"src/lib.rs","offset":50}"#);
        assert_eq!(norm.start_line, 50);
        assert_eq!(norm.content, "fn a() {}\nfn b() {}");
    }

    #[test]
    fn find_start_line_in_file_locates_exact_line_number() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.rs");
        let content = "line 1\nline 2\nline 3\nfn target() {\n    return 42;\n}\nline 7\n";
        std::fs::write(&file_path, content).unwrap();

        // Finding old string returns 1-based start line (line 4)
        let found = find_start_line_in_file(
            "test.rs",
            Some(dir.path()),
            "fn target() {\n    return 42;\n}",
            "fn target() {\n    return 100;\n}",
        );
        assert_eq!(found, Some(4));

        // Finding inside a sub-line snippet
        let found_inner =
            find_start_line_in_file("test.rs", Some(dir.path()), "return 42;", "return 100;");
        assert_eq!(found_inner, Some(5));

        // Finding when old is already replaced (matches new_string)
        let found_new =
            find_start_line_in_file("test.rs", Some(dir.path()), "nonexistent", "return 42;");
        assert_eq!(found_new, Some(5));

        // When neither matches
        let found_none = find_start_line_in_file(
            "test.rs",
            Some(dir.path()),
            "completely missing",
            "also missing",
        );
        assert_eq!(found_none, None);
    }

    #[test]
    fn edit_diff_renders_with_actual_file_line_number() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("a.rs");
        let mut content = String::new();
        for i in 1..=50 {
            content.push_str(&format!("line {i}\n"));
        }
        std::fs::write(&file_path, &content).unwrap();

        let mut terminal = nuotc::TestTerminal::new(80, 20);
        let mut layout_map = nuo_view::model::layout::LayoutMap::default();
        let theme = Theme::default();
        let mut skip_rows = 0;
        let mut y = 0;
        let mut content_lines = 0;
        let mut wrap = crate::shell::render::BlockWrapCache::default();
        let mut diff_cache = DiffCache::default();

        terminal.draw(|f| {
            let mut ctx = RenderCtx {
                frame: f,
                area: Rect::new(0, 0, 80, 20),
                full_width: 80,
                theme: &theme,
                layout_map: &mut layout_map,
                skip_rows: &mut skip_rows,
                y: &mut y,
                content_lines: &mut content_lines,
                wrap: &mut wrap,
                workspace_root: Some(dir.path()),
            };

            draw_tool_result(
                &mut ctx,
                0,
                1,
                "edit_text",
                r#"{"path":"a.rs","old_string":"line 42\n","new_string":"line 42 modified\n"}"#,
                "",
                None, // in-flight or argument-derived
                &mut diff_cache,
                &SelectionState::None,
                0,
                80,
                false,
            );
        });

        let rendered = terminal
            .buffer()
            .cells()
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();

        assert!(
            rendered.contains("@@ -42"),
            "Hunk header must indicate line 42:\n{rendered}"
        );
        assert!(
            rendered.contains("42"),
            "Gutter must contain line 42:\n{rendered}"
        );
        assert!(
            !rendered.contains("@@ -1 ") && !rendered.contains("@@ -1,"),
            "Hunk header must not start from line 1:\n{rendered}"
        );
    }

    #[test]
    fn extract_diff_strings_for_tool_extracts_from_all_forms() {
        // Standard edit_text
        let (old, new) = extract_diff_strings_for_tool(
            "edit_text",
            r#"{"path":"test.rs","old_string":"foo","new_string":"bar"}"#,
        );
        assert_eq!(old, "foo");
        assert_eq!(new, "bar");

        // Namespaced tool name
        let (old_ns, new_ns) = extract_diff_strings_for_tool(
            "provider:edit_text",
            r#"{"path":"test.rs","old_str":"foo","new_str":"bar"}"#,
        );
        assert_eq!(old_ns, "foo");
        assert_eq!(new_ns, "bar");

        // Write file with content
        let (old_wf, new_wf) = extract_diff_strings_for_tool(
            "write_file",
            r#"{"path":"test.rs","content":"hello world"}"#,
        );
        assert_eq!(old_wf, "");
        assert_eq!(new_wf, "hello world");

        // Streaming partial JSON
        let (old_str, new_str) = extract_diff_strings_for_tool(
            "edit_text",
            r#"{"path":"test.rs","old_string":"streaming"#,
        );
        assert_eq!(old_str, "streaming");
        assert_eq!(new_str, "");
    }
}
