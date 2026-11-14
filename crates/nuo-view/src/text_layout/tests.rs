//! Unit tests for [`line_spans_rich`]. Copy fidelity is now guaranteed by
//! the block model (copy resolves against the block's raw `content`, which
//! still holds the original backticks), so these tests focus on the visible
//! layout instead: backtick delimiters are hidden as `code_bg` padding
//! spaces, only the inner content carries `code_fg` + `code_bg`, and
//! selection overrides backgrounds uniformly.

use super::{RichLineParams, RichTextColors, RichTextRanges, line_spans_rich};
use nuotc::{Color, Modifier, Style};

#[test]
fn block_selection_range_is_empty_for_collapsed_selection() {
    // A collapsed selection (anchor == head) is a caret and must cover no
    // bytes — otherwise `inclusive_grapheme_end` would expand the single
    // point to a whole glyph and every click in the input or transcript
    // would flash one character. This is the shared gate for both surfaces.
    use super::block_selection_range;
    use crate::model::layout::SemanticCursor;
    use crate::model::selection::SelectionState;

    let at = SemanticCursor::new(0, 0, 3);
    let collapsed = SelectionState::Range {
        anchor: at,
        head: at,
    };
    assert_eq!(block_selection_range(&collapsed, 0, 0), None);

    // A real (non-collapsed) range on the same block still resolves.
    let real = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 0),
        head: SemanticCursor::new(0, 0, 6),
    };
    assert_eq!(block_selection_range(&real, 0, 0), Some((0, Some(6))));
}

/// Rebuild the *visible* text from a line's spans, skipping the prefix.
fn rendered_content(line: &nuotc::Line<'_>) -> String {
    line.spans
        .iter()
        .skip(1)
        .map(|s| s.content.as_ref())
        .collect()
}

#[test]
fn markup_visible_width_excludes_delimiters() {
    use super::{markup_hidden_ranges, markup_visible_width};
    // `code` → backticks elided, only "code" (4) counts.
    assert_eq!(markup_visible_width("`code`", 0, &[(0, 6)], &[], &[]), 4);
    // **bold** → `**` elided, only "bold" (4) counts.
    assert_eq!(markup_visible_width("**bold**", 0, &[], &[(0, 8)], &[]), 4);
    // `a` is 1 visible col, `**b**` is 1 visible col → 2 total. Raw is 8.
    assert_eq!(
        markup_visible_width("`a`**b**", 0, &[(0, 3)], &[(3, 8)], &[]),
        2
    );
    // Hidden ranges are local byte offsets into the window.
    assert_eq!(
        markup_hidden_ranges("`ab`", 0, &[(0, 4)], &[], &[]),
        vec![(0, 1), (3, 4)]
    );
}

#[test]
fn wrap_text_markup_keeps_delimiters_zero_width() {
    use super::{markup_visible_width, wrap_text_markup};
    // Width 4: "**bold**" is 4 visible cols, so it fits on one line with
    // the `**` markers tagging along at zero cost. Raw width (8) would have
    // split it under plain `wrap_text`.
    let lines = wrap_text_markup("**bold**", 4, &[(0, 2), (6, 8)]);
    assert_eq!(lines.len(), 1, "should not wrap, got {} lines", lines.len());
    assert_eq!(lines[0].text, "**bold**");
    assert_eq!(lines[0].start_byte, 0);
    assert_eq!(
        markup_visible_width(&lines[0].text, 0, &[], &[(0, 8)], &[]),
        4,
        "rendered width must respect the 4-col budget"
    );
}

fn test_colors() -> RichTextColors {
    RichTextColors {
        code_fg: Color::Green,
        code_bg: Color::Black,
        math_fg: Color::Magenta,
        link_fg: Color::Blue,
        selected_bg: Color::Red,
    }
}

#[test]
fn no_code_defers_to_plain_builder() {
    // A line with no code range must produce a single content span (the
    // fast path), identical to the plain `line_spans`.
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text: "plain text",
        line_start_byte: 0,
        selected: None,
        ranges: RichTextRanges::default(),
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });
    // prefix (empty) + one content span
    assert_eq!(line.spans.len(), 2);
    assert_eq!(rendered_content(&line), "plain text");
}

#[test]
fn code_delimiters_hidden_as_padding_content_on_code_surface() {
    // "use `foo` now" with the code range covering `` `foo` ``.
    let text = "use `foo` now";
    let code_range = (4, 9); // "`foo`"
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text,
        line_start_byte: 0,
        selected: None,
        ranges: RichTextRanges {
            code: &[code_range],
            ..Default::default()
        },
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });

    // Backticks are visually elided (zero-width); only the inner code content
    // carries code_fg on code_bg. Copy still yields the original `` `foo` ``
    // via the block's raw `content`.
    assert_eq!(rendered_content(&line), "use foo now");

    // Three content spans: "use " | "foo" | " now".
    let content: Vec<_> = line.spans.iter().skip(1).collect();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0].content, "use ");
    assert_eq!(content[1].content, "foo");
    assert_eq!(content[2].content, " now");

    // Backgrounds: plain spans carry base (Reset), the code content span
    // carries code_bg (Black).
    assert_eq!(content[0].style.bg, Color::Reset); // "use "
    assert_eq!(content[1].style.bg, Color::Black); // code content
    assert_eq!(content[1].style.fg, Color::Green); // code_fg
    assert_eq!(content[2].style.bg, Color::Reset); // " now"
}

#[test]
fn selection_overrides_background() {
    // Selecting the whole line paints every span with `selected_bg`,
    // including the code content and the delimiter padding.
    let text = "a `b` c";
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text,
        line_start_byte: 0,
        selected: Some((0, text.len())),
        ranges: RichTextRanges {
            code: &[(2, 5)],
            ..Default::default()
        },
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });
    let content: Vec<_> = line.spans.iter().skip(1).collect();
    // Every span carries the selected background, including the code content.
    assert!(content.iter().all(|s| s.style.bg == Color::Red));
    // Backticks are zero-width; the code content sits flush with neighbours.
    assert_eq!(rendered_content(&line), "a b c");
}

#[test]
fn code_range_outside_line_is_ignored() {
    // `line_start_byte` positions this line at byte 100; a code range
    // living in [0,6) is entirely before it and must be ignored.
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text: "no code here",
        line_start_byte: 100,
        selected: None,
        ranges: RichTextRanges {
            code: &[(0, 6)],
            ..Default::default()
        },
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });
    assert_eq!(line.spans.len(), 2); // fast path
    assert_eq!(rendered_content(&line), "no code here");
}

#[test]
fn bold_delimiters_hidden_as_padding_inner_content_bold() {
    // "**foo** bar" with the bold range covering the full `**foo**`.
    let text = "**foo** bar";
    let bold_range = (0, 7); // "**foo**"
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text,
        line_start_byte: 0,
        selected: None,
        ranges: RichTextRanges {
            bold: &[bold_range],
            ..Default::default()
        },
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });

    // The `**` delimiters are visually elided (zero-width); only the
    // inner content carries BOLD and the literal inter-word space remains.
    assert_eq!(rendered_content(&line), "foo bar");

    // Two content spans: "foo" | " bar".
    let content: Vec<_> = line.spans.iter().skip(1).collect();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0].content, "foo"); // bold content
    assert_eq!(content[1].content, " bar"); // trailing prose

    // Only the inner content carries the BOLD modifier.
    assert!(content[0].style.add.contains(Modifier::BOLD));
    assert!(!content[1].style.add.contains(Modifier::BOLD));
}

#[test]
fn bold_delimiters_hidden_under_selection() {
    // Selecting the whole line paints every span with `selected_bg`,
    // and the inner content keeps BOLD. The `**` markers are zero-width
    // so there is no padding to highlight.
    let text = "a **b** c";
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text,
        line_start_byte: 0,
        selected: Some((0, text.len())),
        ranges: RichTextRanges {
            bold: &[(2, 7)],
            ..Default::default()
        },
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });
    let content: Vec<_> = line.spans.iter().skip(1).collect();
    // Every span carries the selected background.
    assert!(content.iter().all(|s| s.style.bg == Color::Red));
    // The `**` markers are zero-width: "a b c".
    assert_eq!(rendered_content(&line), "a b c");
    // Find the "b" span and confirm it is bold.
    let b_span = content
        .iter()
        .find(|s| s.content == "b")
        .expect("bold content span present");
    assert!(b_span.style.add.contains(Modifier::BOLD));
}

#[test]
fn bold_and_code_coexist_without_interference() {
    // "use `foo` and **bar** now" — a code chip and a bold run on one line.
    let text = "use `foo` and **bar** now";
    let line = line_spans_rich(RichLineParams {
        prefix: "",
        prefix_style: Style::default(),
        text,
        line_start_byte: 0,
        selected: None,
        ranges: RichTextRanges {
            code: &[(4, 9)],
            bold: &[(14, 21)],
            ..Default::default()
        },
        base: Style::default().fg(Color::White),
        colors: test_colors(),
    });
    // Both backtick and `**` delimiters are zero-width: "use foo and bar now".
    assert_eq!(rendered_content(&line), "use foo and bar now");
    // The code content carries code_fg/code_bg; the bold content carries
    // BOLD on the base style.
    let content: Vec<_> = line.spans.iter().skip(1).collect();
    let foo_span = content
        .iter()
        .find(|s| s.content == "foo")
        .expect("code content span present");
    assert_eq!(foo_span.style.fg, Color::Green);
    assert_eq!(foo_span.style.bg, Color::Black);
    let bar_span = content
        .iter()
        .find(|s| s.content == "bar")
        .expect("bold content span present");
    assert!(bar_span.style.add.contains(Modifier::BOLD));
    assert_eq!(bar_span.style.fg, Color::White); // base, not code_fg
}

#[test]
fn line_spans_clamps_out_of_bounds_selection_without_panic() {
    use super::{clamp_selection_range, line_spans};

    let text = "a".repeat(60);
    // 64 is out of bounds for string of length 60: must not panic.
    let line = line_spans(
        "",
        Style::default(),
        &text,
        Some((0, 64)),
        Style::default(),
        Color::Red,
    );
    let selected_spans: Vec<_> = line
        .spans
        .iter()
        .filter(|s| s.style.bg == Color::Red)
        .collect();
    assert_eq!(selected_spans.len(), 1);
    assert_eq!(selected_spans[0].content, text);

    // Clamping directly:
    assert_eq!(clamp_selection_range(Some((0, 64)), &text), Some((0, 60)));
    assert_eq!(clamp_selection_range(Some((60, 64)), &text), None);
    assert_eq!(clamp_selection_range(Some((10, 5)), &text), None);

    // UTF-8 multi-byte boundary validation:
    let utf8_text = "你好"; // 6 bytes: [0..3], [3..6]
    assert_eq!(clamp_selection_range(Some((0, 3)), utf8_text), Some((0, 3)));
    assert_eq!(clamp_selection_range(Some((1, 3)), utf8_text), None); // byte 1 is not a char boundary
}

#[test]
fn strip_inline_markup_delimiters_boundary_edge_cases() {
    use super::strip_inline_markup_delimiters;

    let content = "**bold**";
    let hidden = vec![(0, 2), (6, 8)];

    // Whole span
    assert_eq!(
        strip_inline_markup_delimiters(content, 0, 8, &hidden),
        "bold"
    );

    // Exactly the inner content
    assert_eq!(
        strip_inline_markup_delimiters(content, 2, 6, &hidden),
        "bold"
    );

    // Starting inside leading delimiter
    assert_eq!(
        strip_inline_markup_delimiters(content, 1, 6, &hidden),
        "bold"
    );

    // Ending inside trailing delimiter
    assert_eq!(
        strip_inline_markup_delimiters(content, 2, 7, &hidden),
        "bold"
    );

    // Enclosing both boundaries partially
    assert_eq!(
        strip_inline_markup_delimiters(content, 1, 7, &hidden),
        "bold"
    );

    // Substring inside content
    assert_eq!(strip_inline_markup_delimiters(content, 2, 4, &hidden), "bo");
}

#[test]
fn wrap_text_zero_width_clamps_to_one() {
    use super::wrap_text;

    let lines = wrap_text("hello", 0);
    assert_eq!(lines.len(), 5);
    for line in lines {
        assert_eq!(line.text.len(), 1);
    }
}

#[test]
fn wrap_text_whitespace_and_empty_edge_cases() {
    use super::wrap_text;

    assert!(wrap_text("", 10).is_empty());

    let lines = wrap_text("   ", 2);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "  ");
    assert_eq!(lines[1].text, " ");

    let lines_nl = wrap_text("\n\n", 10);
    assert_eq!(lines_nl.len(), 2);
    assert_eq!(lines_nl[0].text, "");
    assert_eq!(lines_nl[1].text, "");
}

#[test]
fn wrap_text_multiple_spaces_absorbed_into_previous_line() {
    use super::wrap_text;

    // "hello   world" at width 10:
    // "hello   " has 3 spaces, width 8 <= 10.
    // "world" (5 cols) overflows (8 + 5 = 13 > 10).
    // Line 1 should be "hello   " (with spaces).
    // Line 2 should be "world" (no leading spaces).
    let lines = wrap_text("hello   world", 10);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "hello   ");
    assert_eq!(lines[1].text, "world");
    assert_eq!(lines[1].start_byte, 8);
}

#[test]
fn wrap_text_preserves_whole_words() {
    use super::wrap_text;

    // "hello world" at max_width 8: should not break "world" across lines.
    let lines = wrap_text("hello world", 8);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "hello ");
    assert_eq!(lines[0].start_byte, 0);
    assert_eq!(lines[0].end_byte, 6);
    assert_eq!(lines[1].text, "world");
    assert_eq!(lines[1].start_byte, 6);
    assert_eq!(lines[1].end_byte, 11);
}

#[test]
fn wrap_text_monolithic_word_falls_back_to_force_break() {
    use super::wrap_text;

    // Single word longer than max_width: must force-break without dropping chars.
    let lines = wrap_text("supercalifragilistic", 8);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text, "supercal");
    assert_eq!(lines[0].start_byte, 0);
    assert_eq!(lines[0].end_byte, 8);
    assert_eq!(lines[1].text, "ifragili");
    assert_eq!(lines[1].start_byte, 8);
    assert_eq!(lines[1].end_byte, 16);
    assert_eq!(lines[2].text, "stic");
    assert_eq!(lines[2].start_byte, 16);
    assert_eq!(lines[2].end_byte, 20);
}

#[test]
fn wrap_text_cjk_ideographs_break_cleanly() {
    use super::wrap_text;

    // CJK text at width 8 (4 Hanzi per line):
    let lines = wrap_text("这是一个测试文本", 8);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "这是一个");
    assert_eq!(lines[1].text, "测试文本");
}

#[test]
fn wrap_text_cjk_prohibited_line_start_moves_previous_char() {
    use super::wrap_text;

    // "天气，" at width 4:
    // "天" (2 cols), "气" (2 cols) = 4 cols.
    // "，" (2 cols) overflows to next line, but "，" is prohibited line start.
    // So "气" moves down with "，" -> line 1: "天", line 2: "气，"
    let lines = wrap_text("天气，", 4);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "天");
    assert_eq!(lines[1].text, "气，");
}

#[test]
fn wrap_text_mixed_cjk_and_western_words() {
    use super::wrap_text;

    let lines = wrap_text("测试word wrapping", 10);
    // "测试word " is 4 + 4 + 1 = 9 cols <= 10.
    // "wrapping" (8 cols) wraps onto line 2.
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "测试word ");
    assert_eq!(lines[1].text, "wrapping");
}

#[test]
fn triple_asterisk_bold_italic_delimiters_stripped_cleanly() {
    use super::{bold_delim_local_ranges, strip_inline_markup_delimiters};

    let text = "***important note***";
    let hidden = bold_delim_local_ranges(text, 0, &[(0, text.len())]);
    assert_eq!(hidden, vec![(0, 3), (17, 20)]);

    let clean = strip_inline_markup_delimiters(text, 0, text.len(), &hidden);
    assert_eq!(clean, "important note");
}

#[test]
fn warning_symbol_and_cjk_wrap_and_width_remain_synchronized() {
    use super::wrap_text_markup;
    use crate::model::document::scan_inline;
    use crate::text_layout::block_hidden_ranges;

    let s = "⚠️ 一个未经证实的前提：wright 组装多 source 时是真实解压目录平级、`../optics` 可解析（ccache 的 `source/xxhash-src` 是嵌套的，但本方案是平级，ccache/dbus-broker 均未直接演示平级二源码树）。";
    let scan = scan_inline(s);
    let hidden = block_hidden_ranges(
        s,
        &scan.code_ranges,
        &scan.bold_ranges,
        &scan.math_ranges,
        &scan.link_ranges,
    );

    // The invariant under test is *synchronization*, not one substrate's idea of
    // how wide `⚠️` is: `str_len` must equal the sum of per-grapheme widths, and
    // the wrap must agree with that same measure. Pinning the literal column
    // count here instead encodes whichever `nuotc` the snapshot was recorded
    // against — a variation selector's width is the substrate's call (VS16 is
    // zero-width on POSIX terminals, TR51-emoji under `unicode-width`), and a
    // release must not fail because that judgement moved.
    let prefix = "⚠️ 一个未经证实的前提：";
    let mut grapheme_sum = 0usize;
    let mut rest = prefix;
    while !rest.is_empty() {
        let end = nuotc::inclusive_grapheme_end(rest, 0);
        grapheme_sum += nuotc::grapheme_width(&rest[..end]) as usize;
        rest = &rest[end..];
    }
    assert_eq!(nuotc::str_len(prefix), grapheme_sum);
    assert!(nuotc::grapheme_width("⚠️") >= 1);

    let lines = wrap_text_markup(s, 76, &hidden);
    assert!(!lines.is_empty());
    assert!(lines[0].text.starts_with("⚠️ 一个未经证实的前提："));
}
