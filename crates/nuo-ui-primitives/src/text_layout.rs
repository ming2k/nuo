//! Pure text layout and wrapping primitives for terminal UI.

use nuotc::{grapheme_width, prohibited_line_end, prohibited_line_start};
use unicode_segmentation::UnicodeSegmentation;

/// Produce a run of spaces that fills the rest of a full-width line so a
/// region reads as a solid colored band (the caller attaches the bg style).
pub fn padded_tail(full_width: usize, used: usize) -> String {
    " ".repeat(full_width.saturating_sub(used))
}

/// A wrapped line with byte-offset bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedLine {
    pub text: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Wrap text into lines that fit within `max_width` display columns.
/// Returns each line along with the byte range it covers in the original text.
pub fn wrap_text(text: &str, max_width: usize) -> Vec<WrappedLine> {
    wrap_impl(text, max_width, &[])
}

/// Like [`wrap_text`], but characters whose byte offsets fall inside
/// `hidden_ranges` count as zero display width.
pub fn wrap_text_markup(
    text: &str,
    max_width: usize,
    hidden_ranges: &[(usize, usize)],
) -> Vec<WrappedLine> {
    wrap_impl(text, max_width, hidden_ranges)
}

pub fn is_cjk_breakable(ch: char) -> bool {
    matches!(ch,
        '\u{2E80}'..='\u{2FD5}'
        | '\u{3000}'..='\u{303F}'
        | '\u{3040}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF00}'..='\u{FFEF}'
        | '\u{20000}'..='\u{2FA1F}'
    )
}

fn substring_visible_width(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    hidden_ranges: &[(usize, usize)],
) -> usize {
    if start_byte >= end_byte || start_byte >= text.len() {
        return 0;
    }
    let end_byte = end_byte.min(text.len());
    let slice = &text[start_byte..end_byte];
    let mut w = 0;
    for (rel, g) in slice.grapheme_indices(true) {
        let abs = start_byte + rel;
        let is_hidden = !hidden_ranges.is_empty()
            && hidden_ranges.iter().any(|&(lo, hi)| abs >= lo && abs < hi);
        if !is_hidden && g != "\n" {
            w += grapheme_width(g) as usize;
        }
    }
    w
}

fn wrap_impl(text: &str, max_width: usize, hidden_ranges: &[(usize, usize)]) -> Vec<WrappedLine> {
    let max_width = max_width.max(1);
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;
    let mut line_start_byte = 0;
    let mut last_breakpoint: Option<usize> = None;
    let mut prev_char: Option<char> = None;

    for (byte_idx, grapheme) in text.grapheme_indices(true) {
        let curr_first = grapheme.chars().next();
        let curr_last = grapheme.chars().last();

        if let (Some(prev), Some(first)) = (prev_char, curr_first) {
            if prev.is_whitespace()
                || ((is_cjk_breakable(prev) || is_cjk_breakable(first))
                    && !prohibited_line_end(prev)
                    && !prohibited_line_start(first))
            {
                last_breakpoint = Some(byte_idx);
            }
        }

        let is_hidden = !hidden_ranges.is_empty()
            && hidden_ranges
                .iter()
                .any(|&(lo, hi)| byte_idx >= lo && byte_idx < hi);

        let g_width = if is_hidden || grapheme == "\n" {
            0
        } else {
            grapheme_width(grapheme) as usize
        };

        if grapheme == "\n" {
            lines.push(WrappedLine {
                text: std::mem::take(&mut current_line),
                start_byte: line_start_byte,
                end_byte: byte_idx,
            });
            line_start_byte = byte_idx + grapheme.len();
            current_width = 0;
            last_breakpoint = None;
            prev_char = None;
            continue;
        }

        if current_width + g_width > max_width && !current_line.is_empty() {
            let last_char = current_line.chars().last();
            let move_previous = match (curr_first, last_char) {
                (Some(f), Some(l)) => prohibited_line_start(f) || prohibited_line_end(l),
                _ => false,
            };

            let mut moved_grapheme = None;
            if move_previous
                && let Some((offset, last_g)) = current_line.grapheme_indices(true).next_back()
                && offset > 0
            {
                moved_grapheme = Some(last_g.to_string());
                current_line.truncate(offset);
            }

            if let Some(moved) = moved_grapheme {
                let moved_start = byte_idx - moved.len();
                lines.push(WrappedLine {
                    text: std::mem::take(&mut current_line),
                    start_byte: line_start_byte,
                    end_byte: moved_start,
                });
                current_line.push_str(&moved);
                current_width = substring_visible_width(text, moved_start, byte_idx, hidden_ranges);
                line_start_byte = moved_start;
                last_breakpoint = None;
            } else if let Some(cand_byte) = last_breakpoint.filter(|&b| b > line_start_byte) {
                lines.push(WrappedLine {
                    text: text[line_start_byte..cand_byte].to_string(),
                    start_byte: line_start_byte,
                    end_byte: cand_byte,
                });
                line_start_byte = cand_byte;
                current_line = text[cand_byte..byte_idx].to_string();
                current_width = substring_visible_width(text, cand_byte, byte_idx, hidden_ranges);
                last_breakpoint = None;
            } else {
                lines.push(WrappedLine {
                    text: std::mem::take(&mut current_line),
                    start_byte: line_start_byte,
                    end_byte: byte_idx,
                });
                line_start_byte = byte_idx;
                current_width = 0;
                last_breakpoint = None;

                if curr_first.is_some_and(|c| c.is_whitespace() && c != '\n') {
                    line_start_byte = byte_idx + grapheme.len();
                    prev_char = None;
                    continue;
                }
            }
        }

        current_line.push_str(grapheme);
        current_width += g_width;
        prev_char = curr_last;
    }

    if !current_line.is_empty() || line_start_byte < text.len() {
        lines.push(WrappedLine {
            text: current_line,
            start_byte: line_start_byte,
            end_byte: text.len(),
        });
    }

    lines
}
