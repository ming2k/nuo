//! File/directory listing renderer.
use graticule_canvas::{Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use crate::disclosure::renderers::payloads::summary::{draw_title_band, plural};
use crate::model::selection::SelectionState;
use crate::text_layout::{
    WrappedLine, block_selection_range, line_selection, line_spans, padded_tail, wrap_text,
};
use nuo_ui_primitives::theme::ListingClass;

/// Parse a listing block's leading header line into the label for its count
/// band.
///
/// Recognizes the bracketed headers emitted by [`nuo_tool::ToolOutput::Listing`]:
/// `[N entry/entries]`, `[Showing K of N entries]`, and
/// `[Showing K entries (scan truncated)]`.
///
/// Returns `None` when the first line is not a header (a restored session may start
/// at the first entry), in which case no band is invented.
pub(crate) fn parse_listing_header(first: &str) -> Option<String> {
    let first = first.trim_end();
    let inner = first
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))?;
    if let Some(rest) = inner.strip_prefix("Showing ") {
        if let Some((shown_str, total_str)) = rest
            .strip_suffix(" entries")
            .and_then(|s| s.split_once(" of "))
        {
            let shown: usize = shown_str.trim().parse().ok()?;
            let total: usize = total_str.trim().parse().ok()?;
            return Some(format!("{shown} of {total} entries"));
        }
        if let Some(shown_str) = rest.strip_suffix(" entries (scan truncated)") {
            let shown: usize = shown_str.trim().parse().ok()?;
            return Some(format!("{shown} entries (truncated)"));
        }
    }
    let count = inner
        .strip_suffix(" entries")
        .or_else(|| inner.strip_suffix(" entry"))?;
    let n: usize = count.parse().ok()?;
    Some(format!("{n} {}", plural(n, "entry", "entries")))
}

/// Split a listing path into its `(directory, leaf)` halves, where `directory`
/// keeps its trailing slash so it reads as a heading (`docs/adr/`). A path with
/// no separator (a root-level entry, or a trailing-slash directory such as
/// `src/`) yields no directory heading.
pub(crate) fn split_dir_leaf(raw: &str) -> (Option<&str>, &str) {
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
    let body_start = if header_label.is_some() { 1 } else { 0 };
    let body_end = if header_label.is_some()
        && logical
            .last()
            .is_some_and(|(_, line)| line.starts_with('[') && line.ends_with(']'))
        && logical.len() > body_start
    {
        logical.len() - 1
    } else {
        logical.len()
    };

    let mut current_dir: Option<&str> = None;
    for (line_start_byte, raw) in &logical[body_start..body_end] {
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

#[cfg(test)]
mod tests {
    use super::parse_listing_header;

    #[test]
    fn parses_complete_and_truncated_listing_headers() {
        assert_eq!(
            parse_listing_header("[3 entries]").as_deref(),
            Some("3 entries")
        );
        assert_eq!(
            parse_listing_header("[1 entry]").as_deref(),
            Some("1 entry")
        );
        assert_eq!(
            parse_listing_header("[Showing 200 of 350 entries]").as_deref(),
            Some("200 of 350 entries")
        );
        assert_eq!(
            parse_listing_header("[Showing 200 entries (scan truncated)]").as_deref(),
            Some("200 entries (truncated)")
        );
    }

    #[test]
    fn a_plain_entry_is_not_a_header() {
        assert!(parse_listing_header("src/main.rs").is_none());
    }
}
