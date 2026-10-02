//! Text measurement and wrapping: grapheme-cluster width and CJK-aware line
//! breaking.
//!
//! This is the *engine* half of the old `render/text_layout.rs`, stripped of
//! any ratatui or selection vocabulary. It answers two questions the grid and
//! the application both need:
//!
//! - How many columns does a grapheme occupy? ([`grapheme_width`])
//! - How does a string break into lines of at most `N` columns, honoring CJK
//!   kinsoku (line-start/line-end prohibition) rules? ([`wrap`])
//!
//! The kinsoku tables are lifted verbatim from the prior `text_layout.rs` so
//! wrapping behavior stays identical for any content the application routes
//! through the engine.

use unicode_segmentation::UnicodeSegmentation;

/// The display width of a string, summing each grapheme's width.
pub fn str_len(s: &str) -> usize {
    s.graphemes(true).map(|g| grapheme_width(g) as usize).sum()
}

/// Alias for [`str_len`] (display columns). Named to match callers that think
/// in terms of "string display width".
pub fn str_width(s: &str) -> usize {
    str_len(s)
}

/// A slightly different width measure that treats the string as a whole via
/// `UnicodeWidthStr`, used where callers previously called `UnicodeWidthStr`.
pub fn str_len_w(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// The column width of a single grapheme cluster.
///
/// Uses `UnicodeWidthStr::width` on the **full cluster string** so that
/// multi-codepoint sequences — emoji ZWJ families, skin-tone variants,
/// flag sequences — are measured as a whole (typically width 2) instead of
/// only looking at the first code point.  `unicode-width ≥ 0.2` handles
/// these correctly at the string level; per-char measurement cannot.
///
/// Control characters (including embedded newlines) report width 0 so the
/// application can handle them structurally rather than as visible glyphs.
pub fn grapheme_width(grapheme: &str) -> u8 {
    use unicode_width::UnicodeWidthStr;
    grapheme.width() as u8
}

/// A grapheme broken out with its precomputed width and byte range. Returned
/// by [`graphemes`] so callers that walk a string column-by-column (the
/// wrapper, the grid's string writer) never re-measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece<'a> {
    pub text: &'a str,
    pub width: u8,
}

/// Iterate a string as width-annotated grapheme clusters. Newlines are kept
/// as standalone zero-width pieces so the wrapper can treat them as hard
/// breaks; other control characters are passed through with width 0.
pub fn graphemes(s: &str) -> impl Iterator<Item = Piece<'_>> {
    s.graphemes(true).map(|g| Piece {
        text: g,
        width: if g == "\n" { 0 } else { grapheme_width(g) },
    })
}

/// The display column of the caret sitting at `byte_offset` within `text`.
///
/// This is the single authoritative "byte offset → screen column" mapping the
/// application uses to place the terminal caret. It walks the string once as
/// grapheme clusters, adding each cluster's [`grapheme_width`] for every
/// cluster that ends at or before `byte_offset`, and stops at the first cluster
/// the offset falls inside. Because the grid paints with the *same*
/// [`graphemes`] / [`grapheme_width`] primitives, the column this returns can
/// never disagree with where the text was actually drawn — there is no second
/// width measurement to drift.
///
/// `byte_offset` is implicitly floored to a grapheme boundary: a value landing
/// inside a multi-codepoint grapheme (an accented letter, a ZWJ emoji, or a
/// masked `•••` input paired with a byte cursor from the unmasked string) snaps
/// to the start of that grapheme. `text.len()` is a valid caret position (after
/// the last grapheme), so the caret lands flush against the final glyph instead
/// of one grapheme short — two columns off for CJK, one for ASCII.
pub fn cursor_column(text: &str, byte_offset: usize) -> usize {
    let target = byte_offset.min(text.len());
    let mut col = 0usize;
    for (idx, g) in text.grapheme_indices(true) {
        if idx + g.len() <= target {
            col += grapheme_width(g) as usize;
        } else {
            break;
        }
    }
    col
}

/// The largest grapheme boundary `<= offset`, clamped to `text.len()`.
///
/// A grapheme boundary is the end of any grapheme, which includes `0` (the
/// start of the string) and `text.len()` (the caret's resting spot after the
/// last grapheme). This therefore returns `offset` unchanged when it already
/// sits on a boundary — including `text.len()` — and only moves left when
/// `offset` lands inside a multi-byte grapheme. Use it to keep `text[..n]`
/// slices valid before measuring or rendering them, and pair it with
/// [`inclusive_grapheme_end`] for inclusive selection heads.
pub fn floor_grapheme_boundary(text: &str, offset: usize) -> usize {
    let clamped = offset.min(text.len());
    let mut floor = 0;
    for (idx, g) in text.grapheme_indices(true) {
        if idx + g.len() <= clamped {
            floor = idx + g.len();
        } else {
            break;
        }
    }
    floor
}

/// The end (exclusive) of the grapheme cluster that contains `offset`, so the
/// character under a selection head is included. `text.len()` (a caret resting
/// past the last grapheme) returns `text.len()`.
pub fn inclusive_grapheme_end(text: &str, offset: usize) -> usize {
    let clamped = offset.min(text.len());
    for (idx, g) in text.grapheme_indices(true) {
        let end = idx + g.len();
        if end > clamped {
            return end;
        }
    }
    text.len()
}

/// A wrapped line: the text it contains and its byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Wrap `text` into lines that each fit within `max_width` display columns.
///
/// Hard newlines start a new line. When a grapheme would overflow the column
/// budget, the line breaks before it — but CJK kinsoku rules pull a
/// line-start-prohibited glyph (e.g. `，`) back onto the previous line, and a
/// line-end-prohibited glyph (e.g. `（`) forward onto the next, so punctuation
/// never strands. The tables and the break math are lifted from the prior
/// `text_layout.rs::wrap_text` so behavior is identical.
pub fn wrap(text: &str, max_width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut line_start_byte = 0usize;

    // Iterate by *grapheme* so multi-codepoint sequences (like emojis with ZWJ,
    // or base characters with combining marks) are never split across lines.
    // Kinsoku rules are tested against the first and last char of the grapheme.
    for (byte_idx, grapheme) in text.grapheme_indices(true) {
        let g_width = if grapheme == "\n" {
            0
        } else {
            grapheme_width(grapheme) as usize
        };

        if grapheme == "\n" {
            lines.push(Line {
                text: std::mem::take(&mut current),
                start_byte: line_start_byte,
                end_byte: byte_idx,
            });
            line_start_byte = byte_idx + grapheme.len();
            current_width = 0;
            continue;
        }

        if current_width + g_width > max_width && !current.is_empty() {
            // Kinsoku: avoid starting the next line with a closing punct, or
            // ending it with an opening punct. Pull one grapheme across the break
            // to keep the pair intact.
            #[allow(clippy::unwrap_used)] // grapheme cluster always contains >=1 char
            let first_char = grapheme.chars().next().unwrap();
            #[allow(clippy::unwrap_used)] // current is non-empty in this branch
            let last_char = current.chars().last().unwrap();

            let move_previous = prohibited_line_start(first_char) || prohibited_line_end(last_char);

            let mut moved_grapheme = None;
            if move_previous
                && let Some((offset, last_g)) = current.grapheme_indices(true).next_back()
            {
                // Only move if there is more than 1 grapheme in current,
                // so we don't strand an empty line.
                if offset > 0 {
                    moved_grapheme = Some(last_g.to_string());
                    current.truncate(offset);
                }
            }

            if let Some(moved) = moved_grapheme {
                let moved_start = byte_idx - moved.len();
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    start_byte: line_start_byte,
                    end_byte: moved_start,
                });
                current.push_str(&moved);
                current_width = grapheme_width(&moved) as usize;
                line_start_byte = moved_start;
            } else {
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    start_byte: line_start_byte,
                    end_byte: byte_idx,
                });
                line_start_byte = byte_idx;
                current_width = 0;
            }
        }

        current.push_str(grapheme);
        current_width += g_width;
    }

    if !current.is_empty() || line_start_byte < text.len() {
        lines.push(Line {
            text: current,
            start_byte: line_start_byte,
            end_byte: text.len(),
        });
    }

    lines
}

/// Characters that must not start a line (CJK closing punctuation, and the
/// ASCII closers so mixed CJK/Latin text also breaks cleanly).
///
/// Shared with `mutx`'s markup-aware wrapper so both wrap paths apply
/// the exact same kinsoku rules.
pub const PROHIBITED_LINE_START: &[char] = &[
    '，', '。', '、', '！', '？', '：', '；', '）', '】', '》', '〉', '」', '』', '〕', '”', '’',
    ',', '.', '!', '?', ':', ';', ')', ']', '}',
];

/// Characters that must not end a line (CJK opening punctuation, and the
/// ASCII openers).
pub const PROHIBITED_LINE_END: &[char] = &[
    '（', '【', '《', '〈', '「', '『', '〔', '“', '‘', '(', '[', '{',
];

/// Whether `ch` must not start a line (see [`PROHIBITED_LINE_START`]).
pub fn prohibited_line_start(ch: char) -> bool {
    PROHIBITED_LINE_START.contains(&ch)
}

/// Whether `ch` must not end a line (see [`PROHIBITED_LINE_END`]).
pub fn prohibited_line_end(ch: char) -> bool {
    PROHIBITED_LINE_END.contains(&ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_column_lands_flush_at_end_of_text() {
        // The caret resting after the last grapheme must measure the *full*
        // width — not one grapheme short (regression: the old floor returned
        // the last grapheme start, leaving the caret 2 cols left for CJK).
        assert_eq!(cursor_column("中文", "中文".len()), 4);
        assert_eq!(cursor_column("ab", 2), 2);
        assert_eq!(cursor_column("😀", "😀".len()), 2);
        assert_eq!(cursor_column("", 0), 0);
    }

    #[test]
    fn cursor_column_mid_string_and_mid_grapheme() {
        // Between glyphs: exact.
        assert_eq!(cursor_column("中文", 3), 2);
        assert_eq!(cursor_column("ab", 1), 1);
        // A byte offset inside a multi-byte grapheme floors to its start.
        assert_eq!(cursor_column("中文", 1), 0);
        assert_eq!(cursor_column("中文", 4), 2);
        assert_eq!(cursor_column("😀", 2), 0);
    }

    #[test]
    fn cursor_column_matches_paint_width_exactly() {
        // The column the caret computes must equal what the grid paints for the
        // same prefix — i.e. str_len of the floored prefix. This is the
        // invariant that makes the caret line up with the rendered glyphs.
        for (text, off) in [("中文测试", 6), ("abc", 2), ("a😀b", 2), ("中", 3)] {
            let col = cursor_column(text, off);
            let floored = floor_grapheme_boundary(text, off);
            assert_eq!(
                col,
                str_len(&text[..floored]),
                "mismatch for {text:?}@{off}"
            );
        }
    }

    #[test]
    fn floor_and_inclusive_boundaries_are_grapheme_aware() {
        // floor includes text.len(); inclusive returns the containing grapheme.
        assert_eq!(floor_grapheme_boundary("中文", "中文".len()), 6);
        assert_eq!(floor_grapheme_boundary("中文", 4), 3);
        assert_eq!(inclusive_grapheme_end("中文", 3), 6);
        assert_eq!(inclusive_grapheme_end("中文", 6), 6);
    }

    #[test]
    fn ascii_wraps_on_word_overflow() {
        let lines = wrap("hello world", 5);
        // "hello" then " worl" then "d" — matches the prior implementation's
        // greedy break (it does not do word splitting, just column budget).
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, " worl");
        assert_eq!(lines[2].text, "d");
    }

    #[test]
    fn newline_is_a_hard_break() {
        let lines = wrap("hi\nthere", 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "hi");
        assert_eq!(lines[1].text, "there");
        // Byte ranges cover the source exactly, including the newline.
        assert_eq!(lines[0].start_byte, 0);
        assert_eq!(lines[0].end_byte, 2);
        assert_eq!(lines[1].start_byte, 3);
        assert_eq!(lines[1].end_byte, 8);
    }

    #[test]
    fn cjk_avoids_prohibited_line_start() {
        // Comma must not start a line; the preceding char comes with it.
        let lines = wrap("人生需要坚持，才能前进。", 12);
        assert!(lines.len() > 1);
        assert!(lines.iter().skip(1).all(|l| {
            l.text
                .chars()
                .next()
                .is_none_or(|ch| !prohibited_line_start(ch))
        }));
        assert!(lines.iter().all(|l| {
            l.text
                .chars()
                .last()
                .is_none_or(|ch| !prohibited_line_end(ch))
        }));
    }

    #[test]
    fn wide_grapheme_has_width_two() {
        assert_eq!(grapheme_width("😀"), 2);
        assert_eq!(grapheme_width("a"), 1);
        // Combining mark: base width only.
        assert_eq!(grapheme_width("e\u{0301}"), 1);
    }

    #[test]
    fn empty_string_yields_no_lines() {
        assert!(wrap("", 10).is_empty());
    }
}
