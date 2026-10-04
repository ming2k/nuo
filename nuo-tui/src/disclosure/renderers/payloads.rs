//! Per-tool body content renderers (bash output, grep, find, diff, file write, code blocks).

use nuotc::{
    Color, Modifier, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::base::{
    MARKER_COLLAPSED, MARKER_EXPANDED, RenderCtx, nonempty_wrapped, truncate_to_width,
};
use crate::components::inline_layout::SemanticLine;
use crate::model::layout::{BlockRegion, LinkHit};
use crate::model::selection::SelectionState;
use crate::render::{
    BASH_FOLD_HEAD_ROWS, BASH_FOLD_TAIL_ROWS, CODE_BAND_GUTTER_GAP, CODE_BAND_GUTTER_MIN_WIDTH,
    Theme,
};
use crate::text_layout::{
    CodeGutterParams, WrappedLine, block_selection_range, clamp_selection_range, code_gutter_line,
    line_selection, line_spans, padded_tail, wrap_text,
};
use crate::tools::{DiffCache, DiffHunk, DiffOp, ResultKind};

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

/// Build the summary line for a tool/subagent step using semantic inline flex layout (ADR-0206).
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

/// Render the shared summary of an expandable step with semantic inline flex layout (ADR-0206).
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
    syntax_lang: crate::syntax::Language,
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
        let syntax_spans = if parsed_lang != crate::syntax::Language::Plain {
            crate::syntax::tokenize_line(logical_line, parsed_lang)
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

            let line = if parsed_lang == crate::syntax::Language::Plain {
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

/// Render a `list_dir` / `find_files` result: one entry per row on `code_bg`,
/// directories (entries ending in `/`) in `info`, files in `code_fg`. No
/// line-number gutter since listing rows have no meaningful line index.
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
    let pad = Style::default().bg(code_bg);
    let dir_fg = ctx.theme.info();
    let file_fg = ctx.theme.code_text();
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    let mut logical_lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical_lines.push((offset, line));
        offset += line.len() + 1;
    }

    for (line_start_byte, logical_line) in logical_lines.iter() {
        let is_dir = logical_line.ends_with('/');
        let fg = if is_dir { dir_fg } else { file_fg };
        let base = Style::default().bg(code_bg).fg(fg);
        let normalized = crate::components::path::PathView::from_str(logical_line)
            .maybe_base_dir(ctx.workspace_root)
            .format_text();
        let wrapped = nonempty_wrapped(wrap_text(&normalized, wrap_w));
        for wl in &wrapped {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: line_start_byte + wl.start_byte,
                end_byte: line_start_byte + wl.end_byte,
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

/// Render an interactive web search result stream with cards, domain pills, and clickable URLs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_web_search_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    structured: Option<&nuo_wire::ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let _sel_range = block_selection_range(selection, mi, block_idx);

    let (query, _provider, hits, truncated) = match structured {
        Some(nuo_wire::ToolOutput::WebSearch {
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
        .add_modifier(Modifier::UNDERLINED);
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
    structured: Option<&nuo_wire::ToolOutput>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let _sel_range = block_selection_range(selection, mi, block_idx);

    let (url, title, _domain, markdown, reader, tokens, truncated) = match structured {
        Some(nuo_wire::ToolOutput::WebArticle {
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
                    .add_modifier(Modifier::UNDERLINED),
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
                        format!("{} ", crate::design::QUOTE_GUTTER_GLYPH),
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
) -> (String, String, Vec<nuo_wire::WebSearchHit>, bool) {
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
                hits.push(nuo_wire::WebSearchHit {
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
        hits.push(nuo_wire::WebSearchHit {
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
    let tokens = nuo_wire::tokenizer::count_tokens(trimmed);
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
/// output. Used for search path headers, ripgrep separator rows, and any
/// other "simple" result row that doesn't need a line-number gutter.
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

/// Render a `search_text` result by grouping matches under their file path. Each
/// new path is printed once as a bold `heading_fg` header row; each match
/// is shown as `{lineno}  {content}` with the line number dimmed and the
/// line-number column aligned across the whole result. Non-match lines
/// (ripgrep block separators, etc.) fall back to a dimmed plain row.
/// Selection byte ranges are anchored in the original tool output so
/// copy/cut works across the visible match content.
pub(crate) fn draw_matches_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let header_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let match_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());
    let sel_range = block_selection_range(selection, mi, block_idx);

    // Walk logical lines with their byte offsets in `content`.
    let mut logical: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical.push((offset, line));
        offset += line.len() + 1;
    }

    // Width of the line-number column: the widest lineno across all matches,
    // so the content column stays aligned within and across files.
    let mut lineno_width = 1usize;
    for (_, line) in &logical {
        if let Some(p) = parse_match_line(line) {
            lineno_width = lineno_width.max(p.lineno.len());
        }
    }
    let gap = 2usize;
    let content_cols = indent + lineno_width + gap;
    let content_wrap_w = inner_w.saturating_sub(lineno_width + gap).max(1);

    let mut current_path: Option<&str> = None;

    for (line_start_byte, logical_line) in &logical {
        match parse_match_line(logical_line) {
            Some(parsed) => {
                if current_path != Some(parsed.path) {
                    current_path = Some(parsed.path);
                    let normalized = crate::components::path::PathView::from_str(parsed.path)
                        .maybe_base_dir(ctx.workspace_root)
                        .format_text();
                    emit_simple_rows(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        &normalized,
                        *line_start_byte,
                        pad,
                        header_style,
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
                    match selected {
                        None => spans.push(Span::styled(wl.text.clone(), match_style)),
                        Some((lo, hi)) => {
                            if lo > 0 {
                                spans.push(Span::styled(wl.text[..lo].to_string(), match_style));
                            }
                            spans.push(Span::styled(
                                wl.text[lo..hi].to_string(),
                                match_style.bg(ctx.theme.selected()),
                            ));
                            if hi < wl.text.len() {
                                spans.push(Span::styled(wl.text[hi..].to_string(), match_style));
                            }
                        }
                    }
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
/// extra marker beyond its optional `exit N` line). The footer explains *why*
/// the command ended and — for the blocked variants — how to retry
/// non-interactively, closing the loop the agent can't close itself. Colors
/// reuse the block-level design contract: `warn()` for the blocked/timeout
/// family, `err()` for cancellation, all on the code surface.
fn termination_footer(
    term: nuo_wire::tool_output::ShellTermination,
    theme: &Theme,
) -> Option<(String, Style)> {
    use nuo_wire::tool_output::ShellTermination as T;
    let bg = theme.code_surface();
    let warn_style = Style::default()
        .bg(bg)
        .fg(theme.warn())
        .add_modifier(Modifier::BOLD);
    let err_style = Style::default()
        .bg(bg)
        .fg(theme.err())
        .add_modifier(Modifier::BOLD);
    match term {
        T::Exited => None,
        T::IdleBlocked => Some((
            "[process killed]   idle timeout detected".to_string(),
            warn_style,
        )),
        T::InteractiveBlocked => Some((
            "[process blocked]  interactive prompt detected".to_string(),
            warn_style,
        )),
        T::InputUnanswered => Some((
            "[process killed]   unanswered input prompt".to_string(),
            warn_style,
        )),
        T::Timeout => Some((
            "[process killed]   overall timeout reached".to_string(),
            warn_style,
        )),
        T::Cancelled => Some(("[process killed]   cancelled by operator".to_string(), err_style)),
        T::StreamGuard => Some((
            "[process killed]   runaway stream detected".to_string(),
            warn_style,
        )),
        T::Detached => Some((
            "[process detached] adopted by background fabric".to_string(),
            warn_style,
        )),
    }
}

/// Render a `bash` step as a terminal-like `code_bg` block: a `$ command`
/// prompt line first, then stdout / stderr / an exit or truncation footer.
/// Output rows have no line-number gutter. Legacy section markers (`Exit N`,
/// `STDOUT:`, …) are highlighted in `warning` for sessions restored without a
/// structured payload. The command line is not selectable (it's derived from
/// the call, not the output stream); output rows are.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_command_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    structured: Option<&nuo_wire::ToolOutput>,
    command: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let result_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(result_bg);
    let base = Style::default().bg(result_bg).fg(ctx.theme.code_text());
    let marker_style = Style::default()
        .bg(result_bg)
        .fg(ctx.theme.warn())
        .add_modifier(Modifier::BOLD);
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    // `$ command` prompt line(s) — the command may span multiple lines; only
    // the first rendered row carries the `$ ` prompt.
    if !command.is_empty() {
        let cmd_style = Style::default().bg(result_bg).fg(ctx.theme.fg());
        let mut rows = command.split('\n');
        if let Some(first) = rows.next() {
            let prompt = format!("$ {}", first);
            for wl in nonempty_wrapped(wrap_text(&prompt, wrap_w)) {
                let used = indent + wl.text.width();
                let line = Line::from(vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), cmd_style),
                    Span::styled(padded_tail(ctx.full_width, used), pad),
                ]);
                let _ = ctx.paint(line);
            }
        }
        for cont in rows {
            for wl in nonempty_wrapped(wrap_text(cont, wrap_w)) {
                let used = indent + wl.text.width();
                let line = Line::from(vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), cmd_style),
                    Span::styled(padded_tail(ctx.full_width, used), pad),
                ]);
                let _ = ctx.paint(line);
            }
        }
    }

    if let Some(nuo_wire::ToolOutput::Shell {
        stdout,
        stderr,
        lines,
        exit,
        truncated,
        termination,
        detached_job_id: None,
        ..
    }) = structured
    {
        // Materialize the output stream once, then emit it through the folding
        // emitter: a head of leading context, a `⋯ N lines hidden` row for the
        // verbose middle, and a tail of trailing context. Only output goes
        // through the fold — the exit / truncated / termination footers below
        // stay outside it, so the trailing "events" are always visible even for
        // a huge log. Short output (≤ HEAD + TAIL + 1 lines) renders verbatim;
        // `emit_command_lines_folded` is a no-op on an empty stream.
        let mut byte_offset = 0usize;
        let output_rows = command_structured_lines(lines, stdout, stderr, base);
        if !output_rows.is_empty() {
            byte_offset = emit_command_lines_folded(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                &output_rows,
                byte_offset,
            );
        }
        if *truncated {
            byte_offset = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                "[stream truncated] output cap reached",
                marker_style,
                byte_offset,
            );
        }
        // Exit-code footer: always painted when the code is known, so an
        // expanded step closes with a diagnostic fact even on success
        // ("did it actually exit 0?"). A clean `exit 0` is dimmed to stay
        // quiet; any non-zero code keeps the loud warn marker.
        if let Some(code) = exit {
            let m = format!("exit {}", code);
            let style = if *code == 0 {
                Style::default().bg(result_bg).fg(ctx.theme.dim())
            } else {
                marker_style
            };
            let _ = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                &m,
                style,
                byte_offset,
            );
        }

        // Themed termination footer (L6)
        // Every non-trivial termination renders a themed footer so the user
        // and the model see *why* the command ended, not just that it did.
        // A healthy `Exited` run is silent here (its exit code is above);
        // every other variant paints a coloured marker + a
        // remediation hint. All colors flow through the shared theme tokens,
        // reusing the block-level design contract (diff tokens, warn/err)
        // so the footer reads as part of the same surface language.
        if let Some(footer) = termination_footer(*termination, ctx.theme) {
            let (text, style) = footer;
            let _ = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                &text,
                style,
                byte_offset,
            );
        }
        return;
    }

    // Legacy fallback for non-Shell results (e.g. restored sessions whose
    // structured payload was not persisted): render the composed `content`
    // string, highlighting the conventional section markers. This path does
    // *not* middle-fold: the legacy `content` string inlines its markers
    // (`Exit N`, `STDOUT:`, `[Output truncated` …) at arbitrary positions, so a
    // head/tail window could hide a trailing event marker. Restored sessions
    // are also the rare case — the live structured `Shell` path is what every
    // fresh bash call takes, and it folds above. Folding here is low value and
    // high risk, so the full content is rendered verbatim.
    let content = content.trim_end_matches(&['\r', '\n'][..]);
    if content.is_empty() {
        return;
    }
    let mut logical_lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical_lines.push((offset, line));
        offset += line.len() + 1;
    }
    for (line_start_byte, logical_line) in logical_lines.iter() {
        let trimmed = logical_line.trim_end();
        let is_marker = trimmed.starts_with("Exit ")
            || trimmed == "STDOUT:"
            || trimmed == "STDERR:"
            || trimmed.starts_with("(success, stderr):")
            || trimmed.starts_with("[Output truncated")
            || trimmed.starts_with("[output truncated")
            || trimmed.starts_with("[stream truncated")
            || trimmed.starts_with("[process killed")
            || trimmed.starts_with("[process blocked")
            || trimmed.starts_with("[process detached")
            || trimmed.starts_with("[Output was large")
            || trimmed.starts_with("[killed by harness")
            || trimmed.starts_with("[not executed");
        let style = if is_marker { marker_style } else { base };
        let _ = emit_command_lines(
            ctx,
            mi,
            block_idx,
            indent,
            wrap_w,
            pad,
            sel_range,
            logical_line,
            style,
            *line_start_byte,
        );
    }
}

/// Emit a (possibly multi-line) bash body section at `indent`, wrapping to
/// `wrap_w`, all rows in `style`, anchoring selection byte ranges at
/// `*byte_offset` (advanced past the section). Shared by the structured and
/// legacy bash renderers.
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
    // Shell capture appends a `\n` after every emitted line, so a payload like
    // `date`'s stdout (`"Fri … 2026\n"`) would otherwise split into
    // `["Fri … 2026", ""]` and paint a phantom trailing blank row (padded with
    // spaces). Trim trailing newlines first; internal blank lines are
    // preserved. This is a no-op for the single-line marker/legacy callers,
    // whose strings never carry a trailing newline.
    let text = text.trim_end_matches(&['\r', '\n'][..]);
    for logical_line in text.split('\n') {
        // Carriage-return / backspace normalization: capture already resolves
        // these (a `\r`-refreshed progress bar collapses to its final frame),
        // but the legacy / restored-session flat-string path can still carry
        // raw `\r`s, so resolve them here too — with the *same* function the
        // capture layer uses, so both paths agree instead of the renderer
        // doing a cruder "keep only the last segment" approximation.
        let logical_line = nuo_wire::tool_output::normalize_carriage_returns(logical_line);
        let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));
        for wl in &wrapped {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: byte_offset + wl.start_byte,
                end_byte: byte_offset + wl.end_byte,
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
        byte_offset += logical_line.len() + 1;
    }
    byte_offset
}

/// Materialize a structured `Shell` result's output stream into an ordered
/// list of `(text, style)` logical lines, in the same byte-offset layout
/// [`emit_command_lines`] uses (one logical line per entry; the caller anchors
/// them sequentially). Output lines are styled using terminal text style `base`.
///
/// Prefers the arrival-ordered `lines` (the TUI-authoritative interleaved
/// view), falling back to the all-stdout-then-all-stderr flat strings for the
/// legacy / live-seed / restored-session path. Empty bands contribute nothing.
fn command_structured_lines(
    lines: &[nuo_wire::tool_output::ShellLine],
    stdout: &str,
    stderr: &str,
    base: Style,
) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    if !lines.is_empty() {
        for l in lines {
            // `emit_command_lines` normalizes CR/BS itself, so pass the raw text.
            out.push((l.text.clone(), base));
        }
        return out;
    }
    // Legacy / live-seed fallback: all-stdout band then all-stderr band.
    for text in [stdout, stderr] {
        let text = text.trim_end_matches(&['\r', '\n'][..]);
        if text.is_empty() {
            continue;
        }
        for line in text.split('\n') {
            out.push((line.to_string(), base));
        }
    }
    out
}

/// Render `rows` (logical `(text, style)` lines) at `indent`, folding the
/// verbose middle into a single dim `⋯ N lines hidden` row when there are
/// more than `BASH_FOLD_HEAD_ROWS + BASH_FOLD_TAIL_ROWS + 1` lines. Visible
/// rows are registered for selection at their true `output`-space byte
/// offsets: `byte_offset` advances past *every* logical line (including the
/// hidden ones) so the tail rows anchor correctly, exactly as the unfolded
/// path would. The synthesized ellipsis row is not selectable (it is a
/// summary, not real content).
///
/// This preserves the contract that a selection spanning the fold copies the
/// visible head and tail text only — the hidden middle is neither painted nor
/// selectable, matching "you can't select what isn't on screen."
#[allow(clippy::too_many_arguments)]
fn emit_command_lines_folded(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    pad: Style,
    sel_range: Option<(usize, Option<usize>)>,
    rows: &[(String, Style)],
    mut byte_offset: usize,
) -> usize {
    let total = rows.len();
    let fold_after = BASH_FOLD_HEAD_ROWS + BASH_FOLD_TAIL_ROWS + 1;
    if total <= fold_after {
        for (text, style) in rows {
            byte_offset = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                text,
                *style,
                byte_offset,
            );
        }
        return byte_offset;
    }

    // Window: first HEAD rows, one ellipsis row, last TAIL rows.
    let head_end = BASH_FOLD_HEAD_ROWS;
    let tail_start = total - BASH_FOLD_TAIL_ROWS;
    let hidden = tail_start - head_end;

    // head
    for (text, style) in rows[..head_end].iter() {
        byte_offset = emit_command_lines(
            ctx,
            mi,
            block_idx,
            indent,
            wrap_w,
            pad,
            sel_range,
            text,
            *style,
            byte_offset,
        );
    }

    // ellipsis
    // Advance `byte_offset` past every hidden logical line so the tail rows
    // anchor at their true `output`-space positions. Each hidden line occupies
    // `text.len() + 1` bytes in the flat stream (the `+1` is the `\n`
    // separator `emit_command_lines` counts). `normalize_carriage_returns` can
    // only shrink a line, so this upper bound keeps offsets monotonic; the
    // tail offsets stay past the head, which is all selection anchoring needs.
    for (text, _style) in rows[head_end..tail_start].iter() {
        byte_offset += text.len() + 1;
    }
    let ellipsis_text = format!("⋯ {} lines hidden", hidden);
    let used = indent + ellipsis_text.width();
    let ellipsis_line = Line::from(vec![
        Span::styled(" ".repeat(indent), pad),
        Span::styled(ellipsis_text, pad.fg(ctx.theme.dim())),
        Span::styled(padded_tail(ctx.full_width, used), pad),
    ]);
    let _ = ctx.paint(ellipsis_line); // summary row: not registered for selection

    // tail
    for (text, style) in rows[tail_start..].iter() {
        byte_offset = emit_command_lines(
            ctx,
            mi,
            block_idx,
            indent,
            wrap_w,
            pad,
            sel_range,
            text,
            *style,
            byte_offset,
        );
    }

    byte_offset
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
    structured: Option<&nuo_wire::ToolOutput>,
    diff_cache: &mut DiffCache,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let block_idx = 1usize;
    // Explicit tool errors are terminal results, not the successful shape the
    // presenter normally renders. In particular, a failed edit has no Patch;
    // deriving a diff from its call arguments would show an intended change
    // that never reached disk and hide the actionable failure message.
    if matches!(
        structured,
        Some(
            nuo_wire::ToolOutput::Error { .. }
                | nuo_wire::ToolOutput::PermissionDenied { .. }
        )
    ) {
        draw_tool_error(ctx, mi, block_idx, output, selection, indent, inner_w);
        return;
    }

    let kind = crate::tools::presenter_for(name).result_kind();
    match kind {
        ResultKind::Listing => {
            draw_listing_content(ctx, mi, block_idx, output, selection, indent, inner_w)
        }
        ResultKind::Matches => {
            draw_matches_content(ctx, mi, block_idx, output, selection, indent, inner_w)
        }
        ResultKind::Command => {
            let command = command_for(structured, arguments);
            draw_command_content(
                ctx, mi, block_idx, output, structured, &command, selection, indent, inner_w,
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
            let (content, start_line, explicit_lang) = match structured {
                Some(nuo_wire::ToolOutput::Code {
                    text,
                    start_line,
                    lang,
                    ..
                }) => (text.as_str(), *start_line, lang.as_deref()),
                Some(nuo_wire::ToolOutput::Patch {
                    new, start_line, ..
                }) => (new.as_str(), *start_line, None),
                _ => (output, 0, None),
            };
            let syntax_lang = explicit_lang
                .map(crate::syntax::Language::from_path)
                .or_else(|| {
                    serde_json::from_str::<serde_json::Value>(arguments)
                        .ok()
                        .and_then(|v| {
                            v.get("path")
                                .and_then(|p| p.as_str())
                                .map(crate::syntax::Language::from_path)
                        })
                })
                .unwrap_or(crate::syntax::Language::Plain);

            draw_code_content(
                ctx,
                mi,
                block_idx,
                content,
                start_line,
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
                Some(nuo_wire::ToolOutput::Patch { .. }) => None,
                _ => serde_json::from_str::<serde_json::Value>(arguments)
                    .ok()
                    .and_then(|v| {
                        v.get("path")
                            .and_then(|p| p.as_str())
                            .map(|s| s.to_string())
                    }),
            };
            let path_ref = match structured {
                Some(nuo_wire::ToolOutput::Patch { path, .. }) => Some(path.as_str()),
                _ => path_buf.as_deref(),
            };
            let lang = path_ref
                .map(crate::syntax::Language::from_path)
                .unwrap_or(crate::syntax::Language::Plain);

            let hunks = match structured {
                Some(nuo_wire::ToolOutput::Patch {
                    old,
                    new,
                    start_line,
                    ..
                }) => diff_cache.patch(message_id, old, new, *start_line),
                _ => diff_cache.legacy_arguments(message_id, name, arguments),
            };
            if let Some(nuo_wire::ToolOutput::Patch { warnings, .. }) = structured {
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
            draw_diff_content(ctx, hunks.as_ref(), indent, inner_w, lang);
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
/// [`ToolOutput::Shell`](nuo_wire::ToolOutput) payload (set as soon as the
/// call starts, so it is available even while streaming), falling back to
/// parsing the JSON arguments for legacy / restored sessions without a
/// structured payload.
fn command_for(structured: Option<&nuo_wire::ToolOutput>, arguments: &str) -> String {
    if let Some(nuo_wire::ToolOutput::Shell { command, .. }) = structured
        && !command.is_empty()
    {
        return command.clone();
    }
    crate::model::document::parse_arguments_kv(arguments)
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
    lang: crate::syntax::Language,
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
            let syntax_spans = crate::syntax::tokenize_line(&full, lang);

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
                        let token_fg = if lang == crate::syntax::Language::Plain {
                            base_fg
                        } else {
                            ctx.theme.syntax_color(slice.kind)
                        };
                        let bg = if slice.changed { hi_bg } else { row_bg };
                        let mut style = Style::default().bg(bg).fg(token_fg);
                        if slice.changed {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if slice.kind == crate::syntax::SyntaxKind::Comment {
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
    pub kind: crate::syntax::SyntaxKind,
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
    frags: &[crate::tools::DiffFrag],
    syntax_spans: &[crate::syntax::SyntaxSpan],
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
            .unwrap_or(crate::syntax::SyntaxKind::Plain);

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
    frags: &'a [crate::tools::DiffFrag],
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
    syntax_spans: &[crate::syntax::SyntaxSpan],
    start_byte: usize,
    end_byte: usize,
) -> Vec<(&'a str, crate::syntax::SyntaxKind)> {
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
            .unwrap_or(crate::syntax::SyntaxKind::Plain);

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
    syntax_spans: &[crate::syntax::SyntaxSpan],
    start_byte: usize,
    end_byte: usize,
    _selected: Option<(usize, usize)>,
    _selected_bg: Color,
    full_width: usize,
    theme: &crate::Theme,
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
        if kind == crate::syntax::SyntaxKind::Comment {
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
    use nuo_wire::tool_output::ShellTermination;

    #[test]
    fn termination_footer_renders_stream_guard_warning() {
        let theme = crate::Theme::default();
        let footer = termination_footer(ShellTermination::StreamGuard, &theme);
        assert!(footer.is_some());
        let (text, style) = footer.unwrap();
        assert_eq!(text, "[process killed]   runaway stream detected");
        assert_eq!(style.fg, theme.warn());
    }
}
