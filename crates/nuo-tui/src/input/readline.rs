//! The shared readline engine: caret motion, word boundaries, grapheme
//! deletion, and line walking used by the composer and the inline prompts.

/// accept free-text input. Used by the Alt+Enter and Ctrl+J multi-line
/// entry bindings (plain Enter sends the message).
pub(crate) fn insert_newline(
    input: &mut String,
    cursor_position: &mut usize,
    allow_multiline: bool,
) {
    if allow_multiline {
        let byte_pos = normalized_cursor_byte(input, *cursor_position);
        *cursor_position = input[..byte_pos].chars().count();
        input.insert(byte_pos, '\n');
        *cursor_position += 1;
    }
}

/// Convert the application's char-index cursor to a grapheme boundary. The
/// logical model remains a char index for compatibility with selection and
/// word-navigation code, but every visible edit/motion lands only between
/// grapheme clusters.
pub(crate) fn normalized_cursor_byte(input: &str, cursor_position: usize) -> usize {
    let raw = input
        .char_indices()
        .nth(cursor_position.min(input.chars().count()))
        .map(|(byte, _)| byte)
        .unwrap_or(input.len());
    nuotc::text::floor_grapheme_boundary(input, raw)
}

pub(crate) fn char_index_at_byte(input: &str, byte: usize) -> usize {
    input[..byte.min(input.len())].chars().count()
}

pub(crate) fn normalize_cursor_char_index(input: &str, cursor_position: usize) -> usize {
    char_index_at_byte(input, normalized_cursor_byte(input, cursor_position))
}

pub(crate) fn previous_grapheme_char_index(input: &str, cursor_position: usize) -> usize {
    let cursor_byte = normalized_cursor_byte(input, cursor_position);
    if cursor_byte == 0 {
        return 0;
    }
    let previous = nuotc::text::floor_grapheme_boundary(input, cursor_byte - 1);
    char_index_at_byte(input, previous)
}

pub(crate) fn next_grapheme_char_index(input: &str, cursor_position: usize) -> usize {
    let cursor_byte = normalized_cursor_byte(input, cursor_position);
    let next = nuotc::text::inclusive_grapheme_end(input, cursor_byte);
    char_index_at_byte(input, next)
}

pub(crate) fn delete_previous_grapheme(input: &mut String, cursor_position: &mut usize) -> bool {
    let end = normalized_cursor_byte(input, *cursor_position);
    if end == 0 {
        *cursor_position = 0;
        return false;
    }
    let start = nuotc::text::floor_grapheme_boundary(input, end - 1);
    input.replace_range(start..end, "");
    *cursor_position = char_index_at_byte(input, start);
    true
}

pub(crate) fn delete_next_grapheme(input: &mut String, cursor_position: &mut usize) -> bool {
    let start = normalized_cursor_byte(input, *cursor_position);
    let end = nuotc::text::inclusive_grapheme_end(input, start);
    if end <= start {
        *cursor_position = char_index_at_byte(input, start);
        return false;
    }
    input.replace_range(start..end, "");
    *cursor_position = char_index_at_byte(input, start);
    true
}

/// Move the caret to the start of the current logical line.
///
/// Used by the `Home` key and `Ctrl+A` (readline convention). For a
/// single-line buffer this is the very start; for a multi-line buffer it
/// stops just past the nearest preceding newline. `cursor_position` is a
/// char index, so the newline search is translated back to chars.
pub(crate) fn cursor_line_start(input: &str, cursor_position: &mut usize) {
    let char_count = input.chars().count();
    let char_pos = (*cursor_position).min(char_count);
    let byte_offset = input
        .char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    let before = &input[..byte_offset];
    if let Some(rel) = before.rfind('\n') {
        let after_newline = rel + '\n'.len_utf8();
        *cursor_position = before[..after_newline].chars().count();
    } else {
        *cursor_position = 0;
    }
}

/// Move the caret to the end of the current logical line.
///
/// Used by the `End` key and `Ctrl+E`. For a multi-line buffer the caret
/// stops just before the next newline rather than at the end of the whole
/// buffer, matching the readline/standard-editor behaviour users expect.
pub(crate) fn cursor_line_end(input: &str, cursor_position: &mut usize) {
    let char_count = input.chars().count();
    let char_pos = (*cursor_position).min(char_count);
    let byte_offset = input
        .char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    let after = &input[byte_offset..];
    if let Some(rel) = after.find('\n') {
        let end_byte = byte_offset + rel;
        *cursor_position = input[..end_byte].chars().count();
    } else {
        *cursor_position = char_count;
    }
}

/// Find the start char index of the previous whitespace-delimited word.
/// Skips trailing whitespace (including newlines), then removes the
/// contiguous run of non-whitespace before the caret.  Returns 0 when
/// the caret is at the very start of the buffer; otherwise the returned
/// position can cross newline boundaries.
///
/// Matches readline's `unix-word-rubout` (Ctrl+W) and the
/// `backward-word` / `backward-kill-word` motions users expect from
/// shells and editors.
pub(crate) fn prev_word_start(input: &str, cursor_position: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut i = cursor_position.min(chars.len());
    // Skip whitespace between caret and the previous word (includes \n).
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    // Skip the contiguous run of non-whitespace that forms the word.
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// Find the end char index of the next whitespace-delimited word.
/// Skips leading whitespace (including newlines), then skips the
/// contiguous run of non-whitespace.  Returns `input.len()` when the
/// caret is at the very end; otherwise the returned position can cross
/// newline boundaries.
///
/// Matches readline's `kill-word` (Alt+D) and `forward-word` motions.
pub(crate) fn next_word_end(input: &str, cursor_position: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut i = cursor_position.min(chars.len());
    // Skip whitespace between caret and the next word (includes \n).
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    // Skip the contiguous run of non-whitespace that forms the word.
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Char index of the start of the current logical line, mirroring
/// [`cursor_line_start`] but operating on a borrowed char slice so the
/// word-boundary helpers can call it without re-allocating.
pub(crate) fn cursor_line_start_char(chars: &[char], cursor_position: usize) -> usize {
    let char_pos = cursor_position.min(chars.len());
    if let Some(rel) = chars[..char_pos].iter().rposition(|&c| c == '\n') {
        rel + 1
    } else {
        0
    }
}

/// Char index of the end of the current logical line, mirroring
/// [`cursor_line_end`] on a borrowed char slice.
pub(crate) fn cursor_line_end_char(chars: &[char], cursor_position: usize) -> usize {
    let char_pos = cursor_position.min(chars.len());
    if let Some(rel) = chars[char_pos..].iter().position(|&c| c == '\n') {
        char_pos + rel
    } else {
        chars.len()
    }
}

/// Try to move the caret up one logical line in a multi-line buffer,
/// preserving the column (char offset within the line) clamped to the
/// previous line's length. Returns `true` and updates `cursor_position`
/// when there is a line above; returns `false` (without moving) when the
/// caret is already on the first line, so the caller can fall through to
/// history navigation.
///
/// This is what lets `↑` walk lines inside a multi-line draft instead of
/// always jumping to the previous history entry — only at the top line
/// does it hand off to input history.
pub(crate) fn cursor_line_up(input: &str, cursor_position: &mut usize) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let pos = (*cursor_position).min(chars.len());
    let line_start = cursor_line_start_char(&chars, pos);
    if line_start == 0 {
        return false;
    }
    let col = pos - line_start;
    // The char just before `line_start` is the newline that ends the
    // previous line; the previous line's text lives in [prev_start, prev_end).
    let prev_end = line_start - 1;
    let prev_start = if let Some(rel) = chars[..prev_end].iter().rposition(|&c| c == '\n') {
        rel + 1
    } else {
        0
    };
    let target = prev_start + col.min(prev_end - prev_start);
    *cursor_position = normalize_cursor_char_index(input, target);
    true
}

/// Try to move the caret down one logical line, mirroring
/// [`cursor_line_up`]. Returns `false` (without moving) when the caret is
/// already on the last line, so `↓` hands off to history navigation there.
pub(crate) fn cursor_line_down(input: &str, cursor_position: &mut usize) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let pos = (*cursor_position).min(chars.len());
    let line_end = cursor_line_end_char(&chars, pos);
    if line_end >= chars.len() {
        return false;
    }
    let line_start = cursor_line_start_char(&chars, pos);
    let col = pos - line_start;
    // `line_end` is the index of the newline; the next line starts after it.
    let next_start = line_end + 1;
    let next_end = if let Some(rel) = chars[next_start..].iter().position(|&c| c == '\n') {
        next_start + rel
    } else {
        chars.len()
    };
    let target = next_start + col.min(next_end - next_start);
    *cursor_position = normalize_cursor_char_index(input, target);
    true
}
