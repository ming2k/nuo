//! Code content renderer and normalization.
use nuotc::{Color, Line, Modifier, Span, Style};

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use crate::shell::disclosure::renderers::payloads::diff::project_syntax_slice;
use crate::shell::render::{CODE_BAND_GUTTER_GAP, CODE_BAND_GUTTER_MIN_WIDTH};
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{
    CodeGutterParams, WrappedLine, block_selection_range, code_gutter_line, line_selection,
    padded_tail, wrap_text,
};

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

    // 1. Strip leading tool-result envelope marker (e.g. `[read result]:`) if present
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
