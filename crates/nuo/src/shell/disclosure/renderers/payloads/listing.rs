//! File/directory listing renderer.
use nuotc::{Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::shell::disclosure::renderers::base::{RenderCtx, nonempty_wrapped};
use crate::shell::disclosure::renderers::payloads::summary::{draw_title_band, plural};
use nuo_ui_primitives::theme::ListingClass;
use nuo_view::model::selection::SelectionState;
use nuo_view::text_layout::{
    WrappedLine, block_selection_range, line_selection, line_spans, padded_tail, wrap_text,
};

/// Parse a listing block's leading header line into the label for its count
/// band. Recognizes the shape emitted by `find_files`: `Found N matching files:`
///
/// Returns `None` when the first line is not a header (a restored session may start
/// at the first entry), in which case no band is invented.
pub(crate) fn parse_listing_header(first: &str) -> Option<String> {
    let first = first.trim_end();
    if let Some(inner) = first
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let inner = inner.split(',').next().unwrap_or(inner).trim();
        let count = inner
            .strip_suffix(" entries")
            .or_else(|| inner.strip_suffix(" entry"))?;
        let n: usize = count.parse().ok()?;
        return Some(format!("{n} {}", plural(n, "entry", "entries")));
    }
    let rest = first.strip_prefix("Found ")?;
    let n: usize = rest.strip_suffix("matching files:")?.trim().parse().ok()?;
    Some(format!("Found {} {}", n, plural(n, "file", "files")))
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

#[cfg(test)]
mod tests {
    use super::parse_listing_header;

    #[test]
    fn parses_the_unified_read_listing_header() {
        assert_eq!(
            parse_listing_header("[3 entries]").as_deref(),
            Some("3 entries")
        );
        assert_eq!(
            parse_listing_header("[5 entries, showing 3-4]").as_deref(),
            Some("5 entries")
        );
        assert_eq!(
            parse_listing_header("[1 entry]").as_deref(),
            Some("1 entry")
        );
    }

    #[test]
    fn keeps_the_find_files_header() {
        assert_eq!(
            parse_listing_header("Found 2 matching files:").as_deref(),
            Some("Found 2 files")
        );
    }

    #[test]
    fn a_plain_entry_is_not_a_header() {
        assert!(parse_listing_header("src/main.rs").is_none());
    }
}
