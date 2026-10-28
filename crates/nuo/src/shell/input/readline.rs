//! The shared readline engine: intra-line caret motion, word boundaries and
//! grapheme deletion used by the composer and the inline prompts. Operates
//! natively on byte offsets and grapheme boundaries (aligned with `nuotc`).
//!
//! Vertical caret motion is *not* here: ↑/↓ walk the composer's wrapped visual
//! rows, which is a grid question — see
//! [`crate::shell::composer_grid::TextGrid`].

/// accept free-text input. Used by the Alt+Enter and Ctrl+J multi-line
/// entry bindings (plain Enter sends the message).
pub(crate) fn insert_newline(
    input: &mut String,
    cursor_position: &mut usize,
    allow_multiline: bool,
) {
    if allow_multiline {
        let byte_pos = normalized_cursor_byte(input, *cursor_position);
        input.insert(byte_pos, '\n');
        *cursor_position = byte_pos + 1;
    }
}

/// Normalize cursor byte offset to a valid grapheme boundary within `input`.
pub(crate) fn normalized_cursor_byte(input: &str, cursor_position: usize) -> usize {
    nuotc::floor_grapheme_boundary(input, cursor_position)
}

/// Move backward one extended grapheme cluster boundary (byte offset).
pub(crate) fn previous_grapheme_boundary(input: &str, cursor_position: usize) -> usize {
    let cursor_byte = normalized_cursor_byte(input, cursor_position);
    if cursor_byte == 0 {
        return 0;
    }
    nuotc::floor_grapheme_boundary(input, cursor_byte - 1)
}

/// Move forward one extended grapheme cluster boundary (byte offset).
pub(crate) fn next_grapheme_boundary(input: &str, cursor_position: usize) -> usize {
    let cursor_byte = normalized_cursor_byte(input, cursor_position);
    nuotc::inclusive_grapheme_end(input, cursor_byte)
}

pub(crate) fn delete_previous_grapheme(input: &mut String, cursor_position: &mut usize) -> bool {
    let end = normalized_cursor_byte(input, *cursor_position);
    if end == 0 {
        *cursor_position = 0;
        return false;
    }
    let start = nuotc::floor_grapheme_boundary(input, end - 1);
    input.replace_range(start..end, "");
    *cursor_position = start;
    true
}

pub(crate) fn delete_next_grapheme(input: &mut String, cursor_position: &mut usize) -> bool {
    let start = normalized_cursor_byte(input, *cursor_position);
    let end = nuotc::inclusive_grapheme_end(input, start);
    if end <= start {
        *cursor_position = start;
        return false;
    }
    input.replace_range(start..end, "");
    *cursor_position = start;
    true
}

/// Move the caret to the start of the current logical line.
pub(crate) fn cursor_line_start(input: &str, cursor_position: &mut usize) {
    let byte_pos = normalized_cursor_byte(input, *cursor_position);
    let before = &input[..byte_pos];
    if let Some(rel) = before.rfind('\n') {
        *cursor_position = rel + 1;
    } else {
        *cursor_position = 0;
    }
}

/// Move the caret to the end of the current logical line.
pub(crate) fn cursor_line_end(input: &str, cursor_position: &mut usize) {
    let byte_pos = normalized_cursor_byte(input, *cursor_position);
    let after = &input[byte_pos..];
    if let Some(rel) = after.find('\n') {
        *cursor_position = byte_pos + rel;
    } else {
        *cursor_position = input.len();
    }
}

/// Find the start byte offset of the previous whitespace-delimited word.
pub(crate) fn prev_word_start(input: &str, cursor_position: usize) -> usize {
    let pos = normalized_cursor_byte(input, cursor_position);
    let before = &input[..pos];
    let trimmed = before.trim_end();
    if trimmed.is_empty() {
        return 0;
    }
    trimmed.rfind(char::is_whitespace).map_or(0, |idx| {
        let after_ws = &trimmed[idx..];
        after_ws.chars().next().map_or(0, |c| idx + c.len_utf8())
    })
}

/// Find the end byte offset of the next whitespace-delimited word.
pub(crate) fn next_word_end(input: &str, cursor_position: usize) -> usize {
    let pos = normalized_cursor_byte(input, cursor_position);
    let after = &input[pos..];
    let leading_ws = after.len() - after.trim_start().len();
    let word = &after[leading_ws..];
    if word.is_empty() {
        return input.len();
    }
    let word_len = word.find(char::is_whitespace).unwrap_or(word.len());
    pos + leading_ws + word_len
}

// probe
