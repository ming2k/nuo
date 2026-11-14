//! Diff and patch content renderer.
use nuotc::{Line, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use nuo_view::text_layout::{padded_tail, wrap_text};
use nuo_view::tools::{DiffHunk, DiffOp};

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
        if !trimmed.is_empty()
            && let Some((offset, _)) = content.match_indices(trimmed).next()
        {
            return Some(content[..offset].matches('\n').count() + 1);
        }
        if needle.contains('\r') || content.contains('\r') {
            let content_norm = content.replace("\r\n", "\n");
            let needle_norm = needle.replace("\r\n", "\n");
            if let Some((offset, _)) = content_norm.match_indices(&needle_norm).next() {
                return Some(content_norm[..offset].matches('\n').count() + 1);
            }
            let needle_norm_trimmed = needle_norm.trim_end();
            if !needle_norm_trimmed.is_empty()
                && let Some((offset, _)) = content_norm.match_indices(needle_norm_trimmed).next()
            {
                return Some(content_norm[..offset].matches('\n').count() + 1);
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
