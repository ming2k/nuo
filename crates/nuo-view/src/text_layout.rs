//! Low-level text helpers: wrapping, byte-range → span splitting, code-line
//! gutter rendering, and selection arithmetic shared by every renderer that
//! lays out character-addressable content.

use nuotc::{
    floor_grapheme_boundary, inclusive_grapheme_end, {Color, Modifier, Style}, {Line, Span},
};

use crate::model::document::LinkRange;
use crate::model::selection::SelectionState;

pub use nuo_ui_primitives::text_layout::padded_tail;

/// The byte range of a block covered by the selection.
/// `(start, None)` means "from start to the end of the block".
pub fn block_selection_range(
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
pub fn line_selection(
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
pub fn clamp_selection_range(
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
pub fn line_spans(
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
pub fn bold_local_regions(
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
        let left_stars = bytes[lo..hi].iter().take_while(|&&b| b == b'*').count();
        let right_stars = bytes[lo..hi]
            .iter()
            .rev()
            .take_while(|&&b| b == b'*')
            .count();
        let left_delim = left_stars >= 2;
        let right_delim = right_stars >= 2 && hi - right_stars > lo;
        let content_lo = if left_delim { lo + left_stars } else { lo };
        let content_hi = if right_delim { hi - right_stars } else { hi };
        out.push((lo, content_lo, content_hi, hi));
    }
    out
}

/// The byte ranges within `text` that are the `**` bold *delimiter* markers
/// (the leading/trailing `**`), rendered as zero-width so the bold content
/// sits flush with its neighbours. Callers store these in
/// `BlockRegion::hidden_ranges` so hit-testing maps screen columns to byte
/// offsets the same way the user sees them.
pub fn bold_delim_local_ranges(
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

pub fn link_delim_local_ranges(
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
pub fn visible_width(text: &str, hidden_ranges: &[(usize, usize)]) -> usize {
    let hidden: usize = hidden_ranges
        .iter()
        .map(|&(lo, hi)| hi.saturating_sub(lo))
        .sum();
    nuotc::str_len(text).saturating_sub(hidden)
}

/// Visible (markup-elided) display width of a text window. `text` is a slice of
/// some original cell content beginning at byte `line_start_byte`; `code_ranges`
/// and `bold_ranges` are absolute byte ranges into that original content. Inline
/// code backticks and `**` bold markers are rendered at zero width by
/// [`line_spans_rich`], so the returned width excludes exactly the bytes it
/// elides — keeping column sizing / padding in sync with what is painted.
pub fn markup_visible_width(
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
pub fn block_hidden_ranges(
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
pub fn strip_inline_markup_delimiters(
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

pub fn markup_hidden_ranges(
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
/// exact `` `read` `` source.
#[derive(Debug, Clone, Copy, Default)]
pub struct RichTextRanges<'a> {
    pub code: &'a [(usize, usize)],
    pub bold: &'a [(usize, usize)],
    pub math: &'a [(usize, usize)],
    pub links: &'a [LinkRange],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichTextColors {
    pub code_fg: Color,
    pub code_bg: Color,
    pub math_fg: Color,
    pub link_fg: Color,
    pub selected_bg: Color,
}

impl RichTextColors {
    pub fn from_theme(theme: &nuo_ui_primitives::Theme) -> Self {
        Self {
            code_fg: theme.code_text(),
            code_bg: theme.code_surface(),
            math_fg: theme.info(),
            link_fg: theme.info(),
            selected_bg: theme.selected(),
        }
    }
}

pub struct RichLineParams<'a> {
    pub prefix: &'a str,
    pub prefix_style: Style,
    pub text: &'a str,
    pub line_start_byte: usize,
    pub selected: Option<(usize, usize)>,
    pub ranges: RichTextRanges<'a>,
    pub base: Style,
    pub colors: RichTextColors,
}

pub fn line_spans_rich(params: RichLineParams<'_>) -> Line<'static> {
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
                        style = style.fg(link_fg).add_modifier(Modifier::UNDERLINE);
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
                    style = style.fg(link_fg).add_modifier(Modifier::UNDERLINE);
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
pub struct CodeGutterParams<'a> {
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

pub fn code_gutter_line(params: CodeGutterParams<'_>) -> Line<'static> {
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
    let used = indent + nuotc::str_len(text);
    spans.push(Span::styled(
        padded_tail(full_width, used),
        Style::default().bg(code_bg),
    ));
    Line::from(spans)
}

pub use nuo_ui_primitives::text_layout::{
    WrappedLine, is_cjk_breakable, wrap_text, wrap_text_markup,
};

/// Content-addressed wrap cache.
#[derive(Default)]
pub struct BlockWrapCache {
    lines: std::collections::HashMap<WrapKey, std::sync::Arc<CachedWrap>>,
    code: std::collections::HashMap<WrapKey, std::sync::Arc<PreparedCode>>,
    order: std::collections::VecDeque<(WrapKind, WrapKey)>,
}

const WRAP_CACHE_LINES_CAP: usize = 2048;
const WRAP_CACHE_CODE_CAP: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum WrapKind {
    Lines,
    Code,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct WrapKey {
    hash: u64,
    len: u32,
    width: u32,
    markup: bool,
}

struct CachedWrap {
    content: String,
    lines: std::sync::Arc<Vec<WrappedLine>>,
}

pub struct PreparedCode {
    pub content: String,
    pub logical: Vec<(usize, Vec<WrappedLine>)>,
}

fn wrap_key(content: &str, width: usize) -> WrapKey {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(content.as_bytes());
    WrapKey {
        hash: hasher.finish(),
        len: content.len() as u32,
        width: width as u32,
        markup: false,
    }
}

fn wrap_key_markup(content: &str, width: usize) -> WrapKey {
    WrapKey {
        markup: true,
        ..wrap_key(content, width)
    }
}

impl BlockWrapCache {
    pub fn wrap_text(&mut self, content: &str, width: usize) -> std::sync::Arc<Vec<WrappedLine>> {
        let key = wrap_key(content, width);
        if let Some(entry) = self.lines.get(&key)
            && entry.content == content
        {
            return std::sync::Arc::clone(&entry.lines);
        }
        let lines = wrap_text(content, width);
        let shared = std::sync::Arc::new(lines);
        self.lines.insert(
            key,
            std::sync::Arc::new(CachedWrap {
                content: content.to_string(),
                lines: std::sync::Arc::clone(&shared),
            }),
        );
        self.touch(WrapKind::Lines, key);
        shared
    }

    pub fn wrap_text_markup(
        &mut self,
        content: &str,
        width: usize,
        hidden_ranges: &[(usize, usize)],
    ) -> std::sync::Arc<Vec<WrappedLine>> {
        let key = wrap_key_markup(content, width);
        if let Some(entry) = self.lines.get(&key)
            && entry.content == content
        {
            return std::sync::Arc::clone(&entry.lines);
        }
        let lines = wrap_text_markup(content, width, hidden_ranges);
        let shared = std::sync::Arc::new(lines);
        self.lines.insert(
            key,
            std::sync::Arc::new(CachedWrap {
                content: content.to_string(),
                lines: std::sync::Arc::clone(&shared),
            }),
        );
        self.touch(WrapKind::Lines, key);
        shared
    }

    pub fn prepare_code(&mut self, content: &str, width: usize) -> std::sync::Arc<PreparedCode> {
        let key = wrap_key(content, width);
        if let Some(entry) = self.code.get(&key)
            && entry.content == content
        {
            return std::sync::Arc::clone(entry);
        }
        let mut logical: Vec<(usize, Vec<WrappedLine>)> = Vec::new();
        let mut offset = 0usize;
        for line in content.split('\n') {
            let wrapped = wrap_text(line, width);
            let wrapped = if wrapped.is_empty() {
                vec![WrappedLine {
                    text: String::new(),
                    start_byte: 0,
                    end_byte: 0,
                }]
            } else {
                wrapped
            };
            logical.push((offset, wrapped));
            offset += line.len() + 1; // +1 for the '\n'
        }
        let prepared = std::sync::Arc::new(PreparedCode {
            content: content.to_string(),
            logical,
        });
        self.code.insert(key, std::sync::Arc::clone(&prepared));
        self.touch(WrapKind::Code, key);
        prepared
    }

    fn touch(&mut self, kind: WrapKind, key: WrapKey) {
        self.order.push_back((kind, key));
        while self.lines.len() > WRAP_CACHE_LINES_CAP || self.code.len() > WRAP_CACHE_CODE_CAP {
            let Some((kind, key)) = self.order.pop_front() else {
                break;
            };
            match kind {
                WrapKind::Lines => {
                    self.lines.remove(&key);
                }
                WrapKind::Code => {
                    self.code.remove(&key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
