//! Low-level text helpers: wrapping, byte-range → span splitting, code-line
//! gutter rendering, and selection arithmetic shared by every renderer that
//! lays out character-addressable content.

use nuotc::{
    text::{
        floor_grapheme_boundary, inclusive_grapheme_end, prohibited_line_end, prohibited_line_start,
    },
    {Color, Modifier, Style}, {Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::model::document::LinkRange;
use crate::model::selection::SelectionState;

/// Produce a run of spaces that fills the rest of a full-width line so a
/// region reads as a solid colored band (the caller attaches the bg style).
pub(crate) fn padded_tail(full_width: usize, used: usize) -> String {
    " ".repeat(full_width.saturating_sub(used))
}

/// The byte range of a block covered by the selection.
/// `(start, None)` means "from start to the end of the block".
pub(crate) fn block_selection_range(
    selection: &SelectionState,
    message_idx: usize,
    block_idx: usize,
) -> Option<(usize, Option<usize>)> {
    match selection {
        SelectionState::None => None,
        SelectionState::TableCell { .. } => None,
        SelectionState::Block {
            message_idx: mi,
            block_idx: bi,
        } => (*mi == message_idx && *bi == block_idx).then_some((0, None)),
        SelectionState::InputRange {
            anchor_byte,
            head_byte,
        } => {
            if message_idx == crate::model::layout::INPUT_MSG_IDX && block_idx == 0 {
                let s = (*anchor_byte).min(*head_byte);
                let e = (*anchor_byte).max(*head_byte);
                (s != e).then_some((s, Some(e)))
            } else {
                None
            }
        }
        SelectionState::Range { .. } => {
            let (start, end) = selection.active_normalized_range()?;
            let here = (message_idx, block_idx);
            if here < (start.message_idx, start.block_idx)
                || here > (end.message_idx, end.block_idx)
            {
                return None;
            }
            let s = if here == (start.message_idx, start.block_idx) {
                start.byte_offset
            } else {
                0
            };
            let e = if here == (end.message_idx, end.block_idx) {
                Some(end.byte_offset)
            } else {
                None
            };
            Some((s, e))
        }
    }
}

/// Intersect a block selection range with one wrapped line, producing the
/// selected byte range *relative to the line text*. The selection head
/// character is included.
pub(crate) fn line_selection(
    range: Option<(usize, Option<usize>)>,
    wl: &WrappedLine,
) -> Option<(usize, usize)> {
    let (s, e) = range?;
    if let Some(e) = e
        && e <= wl.start_byte
        && !(e == wl.start_byte && wl.text.is_empty())
    {
        return None;
    }
    if s >= wl.end_byte && !(s == wl.start_byte && wl.text.is_empty()) {
        return None;
    }
    let lo = floor_grapheme_boundary(&wl.text, s.saturating_sub(wl.start_byte));
    let hi = match e {
        Some(e) if e < wl.end_byte => inclusive_grapheme_end(&wl.text, e - wl.start_byte),
        _ => wl.text.len(),
    };
    (lo < hi).then_some((lo, hi))
}

/// Clamp a selection byte range `(lo, hi)` to `text.len()`, ensuring valid
/// character boundaries and `lo < hi`. Returns `None` if the range is invalid
/// or collapsed.
pub(crate) fn clamp_selection_range(
    selected: Option<(usize, usize)>,
    text: &str,
) -> Option<(usize, usize)> {
    let (lo, hi) = selected?;
    let text_len = text.len();
    let lo = lo.min(text_len);
    let hi = hi.min(text_len);
    if lo < hi && text.is_char_boundary(lo) && text.is_char_boundary(hi) {
        Some((lo, hi))
    } else {
        None
    }
}

/// Build a rendered line: decoration prefix plus the text split into
/// unselected / selected / unselected spans.
pub(crate) fn line_spans(
    prefix: &str,
    prefix_style: Style,
    text: &str,
    selected: Option<(usize, usize)>,
    base: Style,
    selected_bg: Color,
) -> Line<'static> {
    let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
    let selected = clamp_selection_range(selected, text);
    match selected {
        None => spans.push(Span::styled(text.to_string(), base)),
        Some((lo, hi)) => {
            if lo > 0 {
                spans.push(Span::styled(text[..lo].to_string(), base));
            }
            spans.push(Span::styled(text[lo..hi].to_string(), base.bg(selected_bg)));
            if hi < text.len() {
                spans.push(Span::styled(text[hi..].to_string(), base));
            }
        }
    }
    Line::from(spans)
}

/// Resolve bold ranges (absolute byte offsets into a block's `content`) into
/// the local `(range_lo, content_lo, content_hi, range_hi)` 4-tuples relative
/// to `text` (a single wrapped line starting at `line_start_byte`).
///
/// Each tuple covers the full `**…**` span: `[range_lo, content_lo)` is the
/// leading `**` delimiter (rendered zero-width), `[content_lo, content_hi)` is
/// the inner content that carries `BOLD`, and `[content_hi, range_hi)` is the
/// trailing `**` delimiter. Ranges outside the line are dropped; ranges
/// straddling a wrap boundary are clamped to the line.
pub(crate) fn bold_local_regions(
    text: &str,
    line_start_byte: usize,
    bold_ranges: &[(usize, usize)],
) -> Vec<(usize, usize, usize, usize)> {
    let line_end_byte = line_start_byte + text.len();
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for &(cs, ce) in bold_ranges {
        if ce <= line_start_byte || cs >= line_end_byte {
            continue;
        }
        let lo = cs.saturating_sub(line_start_byte);
        let hi = (ce - line_start_byte).min(text.len());
        if lo >= hi {
            continue;
        }
        let left_delim = bytes.get(lo) == Some(&b'*') && bytes.get(lo + 1) == Some(&b'*');
        let right_delim = hi > 1
            && bytes.get(hi - 1) == Some(&b'*')
            && bytes.get(hi - 2) == Some(&b'*')
            && hi - 2 > lo;
        let content_lo = if left_delim { lo + 2 } else { lo };
        let content_hi = if right_delim { hi - 2 } else { hi };
        out.push((lo, content_lo, content_hi, hi));
    }
    out
}

/// The byte ranges within `text` that are the `**` bold *delimiter* markers
/// (the leading/trailing `**`), rendered as zero-width so the bold content
/// sits flush with its neighbours. Callers store these in
/// `BlockRegion::hidden_ranges` so hit-testing maps screen columns to byte
/// offsets the same way the user sees them.
pub(crate) fn bold_delim_local_ranges(
    text: &str,
    line_start_byte: usize,
    bold_ranges: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for &(lo, content_lo, content_hi, hi) in &bold_local_regions(text, line_start_byte, bold_ranges)
    {
        if lo < content_lo {
            out.push((lo, content_lo));
        }
        if content_hi < hi {
            out.push((content_hi, hi));
        }
    }
    out
}

pub(crate) fn link_delim_local_ranges(
    text: &str,
    line_start_byte: usize,
    link_ranges: &[LinkRange],
) -> Vec<(usize, usize)> {
    let line_end_byte = line_start_byte + text.len();
    let mut out = Vec::new();
    for link in link_ranges {
        let (rs, re) = link.range;
        let (ls, le) = link.label_range;
        if re <= line_start_byte || rs >= line_end_byte {
            continue;
        }
        let pieces = [(rs, ls), (le, re)];
        for (lo, hi) in pieces {
            let clipped_lo = lo.max(line_start_byte);
            let clipped_hi = hi.min(line_end_byte);
            if clipped_lo < clipped_hi {
                out.push((clipped_lo - line_start_byte, clipped_hi - line_start_byte));
            }
        }
    }
    out
}

/// Display width of `text` with the given zero-width (`hidden`) ranges
/// excluded. Hidden ranges are ASCII markup delimiters, so their byte length
/// equals their display width and a simple subtraction is exact.
pub(crate) fn visible_width(text: &str, hidden_ranges: &[(usize, usize)]) -> usize {
    let hidden: usize = hidden_ranges
        .iter()
        .map(|&(lo, hi)| hi.saturating_sub(lo))
        .sum();
    text.width().saturating_sub(hidden)
}

/// Visible (markup-elided) display width of a text window. `text` is a slice of
/// some original cell content beginning at byte `line_start_byte`; `code_ranges`
/// and `bold_ranges` are absolute byte ranges into that original content. Inline
/// code backticks and `**` bold markers are rendered at zero width by
/// [`line_spans_rich`], so the returned width excludes exactly the bytes it
/// elides — keeping column sizing / padding in sync with what is painted.
pub(crate) fn markup_visible_width(
    text: &str,
    line_start_byte: usize,
    code_ranges: &[(usize, usize)],
    bold_ranges: &[(usize, usize)],
    math_ranges: &[(usize, usize)],
) -> usize {
    let hidden = markup_hidden_ranges(text, line_start_byte, code_ranges, bold_ranges, math_ranges);
    visible_width(text, &hidden)
}

/// Byte ranges within the `text` window (local offsets) that the renderer
/// elides to zero width: `` ` `` code delimiters and `**` bold markers. `text`
/// is a slice of an original cell beginning at `line_start_byte`; `code_ranges`
/// and `bold_ranges` are absolute byte offsets into that original content. Used
/// to budget display columns for both width measurement and markup-aware
/// wrapping, so neither reserves space for delimiters the renderer hides.
pub(crate) fn block_hidden_ranges(
    content: &str,
    code_ranges: &[(usize, usize)],
    bold_ranges: &[(usize, usize)],
    math_ranges: &[(usize, usize)],
    link_ranges: &[crate::model::document::LinkRange],
) -> Vec<(usize, usize)> {
    let mut hidden = markup_hidden_ranges(content, 0, code_ranges, bold_ranges, math_ranges);
    hidden.extend(link_delim_local_ranges(content, 0, link_ranges));
    hidden.sort_unstable_by_key(|&(lo, _)| lo);
    hidden
}

/// Strip zero-width markdown formatting delimiters from a selected slice of a block's content.
///
/// `byte_start` and `byte_end` are byte offsets within `full_content` representing
/// the inclusive-head selection range. Any delimiter ranges that intersect `[byte_start, byte_end)`
/// are excluded, ensuring copied text matches the clean rendered characters the user saw on screen.
pub(crate) fn strip_inline_markup_delimiters(
    full_content: &str,
    byte_start: usize,
    byte_end: usize,
    hidden_ranges: &[(usize, usize)],
) -> String {
    if byte_start >= byte_end || byte_start >= full_content.len() {
        return String::new();
    }
    let byte_end = byte_end.min(full_content.len());

    let mut result = String::with_capacity(byte_end - byte_start);
    let mut curr = byte_start;

    for &(h_start, h_end) in hidden_ranges {
        if h_end <= curr {
            continue;
        }
        if h_start >= byte_end {
            break;
        }
        // Segment before this hidden range
        if curr < h_start {
            let seg_end = h_start.min(byte_end);
            if curr < seg_end {
                result.push_str(&full_content[curr..seg_end]);
            }
        }
        // Advance past hidden range
        curr = curr.max(h_end);
    }

    if curr < byte_end {
        result.push_str(&full_content[curr..byte_end]);
    }

    result
}

pub(crate) fn markup_hidden_ranges(
    text: &str,
    line_start_byte: usize,
    code_ranges: &[(usize, usize)],
    bold_ranges: &[(usize, usize)],
    math_ranges: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    let mut hidden = bold_delim_local_ranges(text, line_start_byte, bold_ranges);
    let bytes = text.as_bytes();
    let line_end_byte = line_start_byte + text.len();
    for &(cs, ce) in code_ranges {
        if ce <= line_start_byte || cs >= line_end_byte {
            continue;
        }
        let lo = cs.saturating_sub(line_start_byte);
        let hi = (ce - line_start_byte).min(text.len());
        // Trim one backtick per side, mirroring `line_spans_rich`'s elision of
        // the leading/trailing delimiter byte.
        if bytes.get(lo) == Some(&b'`') && hi > lo {
            hidden.push((lo, lo + 1));
        }
        if hi > 0 && bytes.get(hi - 1) == Some(&b'`') && hi - 1 > lo {
            hidden.push((hi - 1, hi));
        }
    }
    for &(cs, ce) in math_ranges {
        if ce <= line_start_byte || cs >= line_end_byte {
            continue;
        }
        let lo = cs.saturating_sub(line_start_byte);
        let hi = (ce - line_start_byte).min(text.len());
        if bytes.get(lo) == Some(&b'$') && hi > lo {
            hidden.push((lo, lo + 1));
        }
        if hi > 0 && bytes.get(hi - 1) == Some(&b'$') && hi - 1 > lo {
            hidden.push((hi - 1, hi));
        }
        if bytes.get(lo..lo + 2) == Some(b"\\(") {
            hidden.push((lo, lo + 2));
        }
        if hi >= 2 && bytes.get(hi - 2..hi) == Some(b"\\)") && hi - 2 > lo {
            hidden.push((hi - 2, hi));
        }
    }
    hidden
}

/// Build a rendered line that, in addition to the prefix / selection behaviour
/// of [`line_spans`], paints inline-code runs on a distinct `code_bg` band.
///
/// `text` is the flattened text for this wrapped line; `line_start_byte` is its
/// byte offset within the block's raw `content` (so that `code_ranges`, which
/// are absolute byte ranges into `content`, can be intersected with this line).
/// `selected` is relative to `text`, as in [`line_spans`].
///
/// The line is split into runs that are each uniform in the selection state,
/// the code state, and the bold state. The recorded `code_ranges` cover the
/// full `` `…` `` span *including* both backtick delimiters, and `bold_ranges`
/// cover the full `**…**` span *including* both `**` markers, so the underlying
/// text — and thus copy, which resolves against the block's raw `content` —
/// keeps them. Visually, though, only the inner content is painted on the code
/// surface (`code_fg` + `code_bg`); the backtick delimiter bytes are visually
/// elided (zero-width) — same as bold `**` markers — while copy still yields the
/// exact `` `read_text` `` source.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RichTextRanges<'a> {
    pub code: &'a [(usize, usize)],
    pub bold: &'a [(usize, usize)],
    pub math: &'a [(usize, usize)],
    pub links: &'a [LinkRange],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RichTextColors {
    pub code_fg: Color,
    pub code_bg: Color,
    pub math_fg: Color,
    pub link_fg: Color,
    pub selected_bg: Color,
}

impl RichTextColors {
    pub fn from_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            code_fg: theme.code_text(),
            code_bg: theme.code_surface(),
            math_fg: theme.info(),
            link_fg: theme.info(),
            selected_bg: theme.selected(),
        }
    }
}

pub(crate) struct RichLineParams<'a> {
    pub prefix: &'a str,
    pub prefix_style: Style,
    pub text: &'a str,
    pub line_start_byte: usize,
    pub selected: Option<(usize, usize)>,
    pub ranges: RichTextRanges<'a>,
    pub base: Style,
    pub colors: RichTextColors,
}

pub(crate) fn line_spans_rich(params: RichLineParams<'_>) -> Line<'static> {
    let RichLineParams {
        prefix,
        prefix_style,
        text,
        line_start_byte,
        selected,
        ranges,
        base,
        colors,
    } = params;
    let RichTextRanges {
        code: code_ranges,
        bold: bold_ranges,
        math: math_ranges,
        links: link_ranges,
    } = ranges;
    let RichTextColors {
        code_fg,
        code_bg,
        math_fg,
        link_fg,
        selected_bg,
    } = colors;
    let selected = clamp_selection_range(selected, text);

    let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
    if text.is_empty() {
        return Line::from(spans);
    }

    // Code ranges translated into offsets relative to this line's text, clamped
    // to the line's own [0, len] window.
    let line_end_byte = line_start_byte + text.len();
    let mut local_code: Vec<(usize, usize)> = Vec::new();
    for &(cs, ce) in code_ranges {
        if ce <= line_start_byte || cs >= line_end_byte {
            continue;
        }
        let lo = cs.saturating_sub(line_start_byte);
        let hi = (ce - line_start_byte).min(text.len());
        if lo < hi {
            local_code.push((lo, hi));
        }
    }
    // Bold ranges translated into offsets relative to this line's text
    // (trimmed to the line's own [0, len] window), then resolved into
    // `(range_lo, content_lo, content_hi, range_hi)` 4-tuples where the
    // inner `[content_lo, content_hi)` carries `BOLD` and the surrounding
    // `**` delimiters are visually elided (zero-width).
    let bold_regions = bold_local_regions(text, line_start_byte, bold_ranges);
    let mut local_math: Vec<(usize, usize)> = Vec::new();
    for &(cs, ce) in math_ranges {
        if ce <= line_start_byte || cs >= line_end_byte {
            continue;
        }
        let lo = cs.saturating_sub(line_start_byte);
        let hi = (ce - line_start_byte).min(text.len());
        if lo < hi {
            local_math.push((lo, hi));
        }
    }
    let mut local_links: Vec<(usize, usize, usize, usize)> = Vec::new();
    for link in link_ranges {
        let (rs, re) = link.range;
        let (ls, le) = link.label_range;
        if re <= line_start_byte || rs >= line_end_byte {
            continue;
        }
        let lo = rs.saturating_sub(line_start_byte);
        let hi = (re - line_start_byte).min(text.len());
        let label_lo = ls.max(line_start_byte).saturating_sub(line_start_byte);
        let label_hi = le.min(line_end_byte).saturating_sub(line_start_byte);
        if lo < hi {
            local_links.push((lo, label_lo, label_hi, hi));
        }
    }

    // Fast path: no rich inline markup on this line → defer to the plain builder.
    if local_code.is_empty()
        && bold_regions.is_empty()
        && local_math.is_empty()
        && local_links.is_empty()
    {
        return line_spans(prefix, prefix_style, text, selected, base, selected_bg);
    }

    // For each local code range, compute the inner content sub-range by
    // trimming the backtick delimiters. The delimiters are visually elided
    // (zero-width), while only the inner content carries `code_fg` on the
    // code surface. Copy is unaffected because it resolves against the
    // block's raw `content`, which still holds the original `` `…` `` source.
    let bytes = text.as_bytes();
    // (range_start, content_start, content_end, range_end)
    let mut regions: Vec<(usize, usize, usize, usize)> = Vec::new();
    for &(lo, hi) in &local_code {
        let left_delim = bytes.get(lo) == Some(&b'`');
        let right_delim = hi > 0 && bytes.get(hi - 1) == Some(&b'`') && hi - 1 > lo;
        let content_lo = if left_delim { lo + 1 } else { lo };
        let content_hi = if right_delim { hi - 1 } else { hi };
        regions.push((lo, content_lo, content_hi, hi));
    }

    // Collect every boundary that can change the run style: line start/end,
    // the selection edges, and each region's four edges. Sort + dedupe, then
    // walk adjacent pairs emitting one span per uniform segment.
    let mut points: Vec<usize> = vec![0, text.len()];
    if let Some((lo, hi)) = selected {
        points.push(lo);
        points.push(hi);
    }
    for &(lo, content_lo, content_hi, hi) in &regions {
        points.push(lo);
        points.push(content_lo);
        points.push(content_hi);
        points.push(hi);
    }
    for &(lo, content_lo, content_hi, hi) in &bold_regions {
        points.push(lo);
        points.push(content_lo);
        points.push(content_hi);
        points.push(hi);
    }
    for &(lo, hi) in &local_math {
        points.push(lo);
        points.push(hi);
        let bytes = text.as_bytes();
        if bytes.get(lo) == Some(&b'$') {
            points.push(lo + 1);
        } else if bytes.get(lo..lo + 2) == Some(b"\\(") {
            points.push(lo + 2);
        }
        if hi > 0 && bytes.get(hi - 1) == Some(&b'$') {
            points.push(hi - 1);
        } else if hi >= 2 && bytes.get(hi - 2..hi) == Some(b"\\)") {
            points.push(hi - 2);
        }
    }
    for &(lo, label_lo, label_hi, hi) in &local_links {
        points.push(lo);
        points.push(label_lo);
        points.push(label_hi);
        points.push(hi);
    }
    points.sort_unstable();
    points.dedup();

    // Region of a point: `None` = plain, `Some(true)` = code content,
    // `Some(false)` = delimiter (padding).
    let region = |p: usize| -> Option<bool> {
        regions.iter().find_map(|&(lo, clo, chi, hi)| {
            if p >= lo && p < hi {
                Some(p >= clo && p < chi)
            } else {
                None
            }
        })
    };
    let is_sel = |p: usize| matches!(selected, Some((lo, hi)) if p >= lo && p < hi);
    // Whether `p` lies in bold *content* (the inner bytes that carry `BOLD`).
    let is_bold = |p: usize| {
        bold_regions
            .iter()
            .any(|&(_, content_lo, content_hi, _)| p >= content_lo && p < content_hi)
    };
    // Whether `p` lies in a bold *delimiter* (`**`) zone — those bytes are
    // visually elided (zero-width), so the bold content sits flush with its
    // neighbours. Copy is unaffected because it resolves against the block's
    // raw `content`, which still holds the original `**…**` source.
    let bold_delim = |p: usize| {
        bold_regions
            .iter()
            .any(|&(lo, content_lo, content_hi, hi)| {
                (p >= lo && p < content_lo) || (p >= content_hi && p < hi)
            })
    };
    let math_state = |p: usize| -> Option<bool> {
        local_math.iter().find_map(|&(lo, hi)| {
            if p < lo || p >= hi {
                return None;
            }
            let bytes = text.as_bytes();
            let content_lo = if bytes.get(lo) == Some(&b'$') {
                lo + 1
            } else if bytes.get(lo..lo + 2) == Some(b"\\(") {
                lo + 2
            } else {
                lo
            };
            let content_hi = if hi > 0 && bytes.get(hi - 1) == Some(&b'$') {
                hi - 1
            } else if hi >= 2 && bytes.get(hi - 2..hi) == Some(b"\\)") {
                hi - 2
            } else {
                hi
            };
            Some(p >= content_lo && p < content_hi)
        })
    };
    let link_state = |p: usize| -> Option<bool> {
        local_links
            .iter()
            .find_map(|&(lo, label_lo, label_hi, hi)| {
                if p >= lo && p < hi {
                    Some(p >= label_lo && p < label_hi)
                } else {
                    None
                }
            })
    };

    let mut i = 0;
    while i + 1 < points.len() {
        let seg_lo = points[i];
        let seg_hi = points[i + 1];
        i += 1;
        if seg_lo >= seg_hi {
            continue;
        }
        let sel = is_sel(seg_lo);
        let bold = is_bold(seg_lo);
        let math = math_state(seg_lo);
        let link = link_state(seg_lo);
        match region(seg_lo) {
            None => {
                if bold_delim(seg_lo) || matches!(math, Some(false)) || matches!(link, Some(false))
                {
                    // Formatting/link/math delimiters are visually elided
                    // (zero-width). The bytes remain in `text` so the layout
                    // keeps byte-accurate offsets and copy can map through
                    // them (stripping the delimiters to yield clean text).
                    continue;
                } else {
                    let mut style = if sel { base.bg(selected_bg) } else { base };
                    if bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if math == Some(true) {
                        style = style.fg(math_fg).add_modifier(Modifier::ITALIC);
                    }
                    if link == Some(true) {
                        style = style.fg(link_fg).add_modifier(Modifier::UNDERLINED);
                    }
                    spans.push(Span::styled(text[seg_lo..seg_hi].to_string(), style));
                }
            }
            Some(true) => {
                let mut style = if sel {
                    Style::default().fg(code_fg).bg(selected_bg)
                } else {
                    Style::default().fg(code_fg).bg(code_bg)
                };
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if math == Some(true) {
                    style = style.fg(math_fg).add_modifier(Modifier::ITALIC);
                }
                if link == Some(true) {
                    style = style.fg(link_fg).add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(text[seg_lo..seg_hi].to_string(), style));
            }
            Some(false) => {
                // Delimiter byte(s): visually elided (zero-width), same as
                // bold markers. The underlying text (and thus copy) keeps the
                // backtick; they just occupy no screen columns.
                continue;
            }
        }
    }
    Line::from(spans)
}

/// Build a rendered code line with a line-number gutter, a uniform `code_bg`
/// band that fills the full width, and character-level selection highlighting.
/// The optional left bar is retained for callers that want it; code blocks
/// themselves render borderless with only the gutter as ornament.
pub(crate) struct CodeGutterParams<'a> {
    pub left_bar: Color,
    pub left_indent: usize,
    pub gutter: &'a str,
    pub gutter_gap: usize,
    pub code_bg: Color,
    pub gutter_fg: Color,
    pub text: &'a str,
    pub selected: Option<(usize, usize)>,
    pub code_fg: Color,
    pub selected_bg: Color,
    pub full_width: usize,
}

pub(crate) fn code_gutter_line(params: CodeGutterParams<'_>) -> Line<'static> {
    let CodeGutterParams {
        left_bar,
        left_indent,
        gutter,
        gutter_gap,
        code_bg,
        gutter_fg,
        text,
        selected,
        code_fg,
        selected_bg,
        full_width,
    } = params;
    let mut spans = Vec::new();
    let mut prefix = left_indent;

    spans.push(Span::styled(
        " ".repeat(left_indent),
        Style::default().bg(code_bg),
    ));

    if left_bar != Color::Reset {
        spans.push(Span::styled("┃", Style::default().bg(code_bg).fg(left_bar)));
        prefix += 1;
    }

    spans.push(Span::styled(" ", Style::default().bg(code_bg)));
    prefix += 1;

    spans.push(Span::styled(
        gutter.to_string(),
        Style::default().bg(code_bg).fg(gutter_fg),
    ));
    spans.push(Span::styled(
        " ".repeat(gutter_gap),
        Style::default().bg(code_bg),
    ));

    let indent = prefix + gutter.len() + gutter_gap;
    let selected = clamp_selection_range(selected, text);
    match selected {
        None => {
            spans.push(Span::styled(
                text.to_string(),
                Style::default().fg(code_fg).bg(code_bg),
            ));
        }
        Some((lo, hi)) => {
            if lo > 0 {
                spans.push(Span::styled(
                    text[..lo].to_string(),
                    Style::default().fg(code_fg).bg(code_bg),
                ));
            }
            spans.push(Span::styled(
                text[lo..hi].to_string(),
                Style::default().fg(code_fg).bg(selected_bg),
            ));
            if hi < text.len() {
                spans.push(Span::styled(
                    text[hi..].to_string(),
                    Style::default().fg(code_fg).bg(code_bg),
                ));
            }
        }
    }
    let used = indent + text.width();
    spans.push(Span::styled(
        padded_tail(full_width, used),
        Style::default().bg(code_bg),
    ));
    Line::from(spans)
}

/// A wrapped line with byte-offset bookkeeping.
pub(crate) struct WrappedLine {
    pub text: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Wrap text into lines that fit within `max_width` display columns.
/// Returns each line along with the byte range it covers in the original text.
pub(crate) fn wrap_text(text: &str, max_width: usize) -> Vec<WrappedLine> {
    wrap_impl(text, max_width, &[])
}

/// Like [`wrap_text`], but characters whose byte offsets fall inside
/// `hidden_ranges` count as zero display width. Inline code / bold delimiters
/// are rendered zero-width, so wrapping against the raw width would let them
/// eat into the column budget and even split a `` `…` ``/`**…**` pair across
/// lines. Wrapping on the visible width keeps markup intact on one line and
/// in sync with the rendered width.
pub(crate) fn wrap_text_markup(
    text: &str,
    max_width: usize,
    hidden_ranges: &[(usize, usize)],
) -> Vec<WrappedLine> {
    wrap_impl(text, max_width, hidden_ranges)
}

fn wrap_impl(text: &str, max_width: usize, hidden_ranges: &[(usize, usize)]) -> Vec<WrappedLine> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;
    let mut line_start_byte = 0;

    for (byte_idx, grapheme) in text.grapheme_indices(true) {
        // Markup delimiter bytes are rendered zero-width, so they neither count
        // against the column budget nor trigger a wrap.
        let is_hidden = !hidden_ranges.is_empty()
            && hidden_ranges
                .iter()
                .any(|&(lo, hi)| byte_idx >= lo && byte_idx < hi);

        let g_width = if is_hidden || grapheme == "\n" {
            0
        } else {
            nuotc::text::grapheme_width(grapheme) as usize
        };

        if grapheme == "\n" {
            lines.push(WrappedLine {
                text: std::mem::take(&mut current_line),
                start_byte: line_start_byte,
                end_byte: byte_idx,
            });
            line_start_byte = byte_idx + grapheme.len();
            current_width = 0;
            continue;
        }

        // Keep closing CJK punctuation with the preceding character. If it
        // would start the next line, move the preceding character with it.
        if current_width + g_width > max_width && !current_line.is_empty() {
            let (Some(first_char), Some(last_char)) =
                (grapheme.chars().next(), current_line.chars().last())
            else {
                continue;
            };

            let move_previous = prohibited_line_start(first_char) || prohibited_line_end(last_char);

            let mut moved_grapheme = None;
            if move_previous
                && let Some((offset, last_g)) = current_line.grapheme_indices(true).next_back()
            {
                // Only pop when we know a character will move down with the wrap
                // leaving at least one on the current line.
                if offset > 0 {
                    moved_grapheme = Some(last_g.to_string());
                    current_line.truncate(offset);
                }
            }

            if let Some(moved) = moved_grapheme {
                let moved_start = byte_idx - moved.len();
                lines.push(WrappedLine {
                    text: std::mem::take(&mut current_line),
                    start_byte: line_start_byte,
                    end_byte: moved_start,
                });
                current_line.push_str(&moved);
                let moved_is_hidden = !hidden_ranges.is_empty()
                    && hidden_ranges
                        .iter()
                        .any(|&(lo, hi)| moved_start >= lo && moved_start < hi);
                current_width = if moved_is_hidden {
                    0
                } else {
                    nuotc::text::grapheme_width(&moved) as usize
                };
                line_start_byte = moved_start;
            } else {
                lines.push(WrappedLine {
                    text: std::mem::take(&mut current_line),
                    start_byte: line_start_byte,
                    end_byte: byte_idx,
                });
                line_start_byte = byte_idx;
                current_width = 0;
            }
        }

        current_line.push_str(grapheme);
        current_width += g_width;
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

#[cfg(test)]
mod tests {
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

        // Three content spans: "use " · "foo" · " now".
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

        // Two content spans: "foo" · " bar".
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
}
