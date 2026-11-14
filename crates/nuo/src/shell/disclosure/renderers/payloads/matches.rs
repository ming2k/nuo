//! Search matches renderer (grep / find pattern matches).
use nuotc::{Color, Line, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use crate::shell::disclosure::renderers::payloads::summary::draw_title_band;
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{
    WrappedLine, block_selection_range, clamp_selection_range, line_selection, line_spans,
    padded_tail, wrap_text,
};

/// A single logical line parsed out of `search_text`'s `path:linenum:content` format.
struct MatchLine<'a> {
    path: &'a str,
    lineno: &'a str,
    content: &'a str,
    /// Byte offset of `content` within the original ripgrep output line.
    content_offset: usize,
}

/// Check if a leading line is the filesystem tool's `Found N match(es):` count header.
pub(crate) fn is_matches_header(first: &str) -> bool {
    let first = first.trim();
    if let Some(rest) = first.strip_prefix("Found ")
        && let Some(num) = rest.strip_suffix("match(es):")
    {
        return num.trim().parse::<usize>().is_ok();
    }
    false
}

/// Recover the literal search query from a `search_text` call's arguments.
/// Returns `None` when the query is absent/empty or was run as a regex — only a
/// plain literal is safe to bold inside a `path:line:content` line, since those
/// lines carry no per-match column ranges and a regex would make substring
/// matching misleading.
pub(crate) fn literal_query(arguments: &str) -> Option<String> {
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
pub(crate) fn segment_matches(text: &str, query: Option<&str>) -> Vec<(usize, usize, bool)> {
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
