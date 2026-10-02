//! Paragraph widget, text lines, spans, wrapping, and alignment.

use super::block::Block;
use crate::buffer::grid::Grid;
use crate::buffer::text::{graphemes, str_len, str_width, wrap};
use crate::buffer::Color;
use crate::buffer::Style;
use crate::layout::rect::Rect;

/// A styled string fragment.
#[derive(Debug, Clone)]
pub struct Span<'a> {
    pub content: std::borrow::Cow<'a, str>,
    pub style: Style,
}

impl<'a> Span<'a> {
    pub fn raw(content: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self {
            content: content.into(),
            style: Style::RESET,
        }
    }
    pub fn styled(content: impl Into<std::borrow::Cow<'a, str>>, style: Style) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }

    /// Display width of this span's content.
    pub fn width(&self) -> usize {
        str_width(&self.content)
    }
}

impl<'a> From<Vec<Span<'a>>> for Line<'a> {
    fn from(spans: Vec<Span<'a>>) -> Self {
        Self {
            spans,
            ..Default::default()
        }
    }
}
impl<'a> From<Span<'a>> for Line<'a> {
    fn from(span: Span<'a>) -> Self {
        Self {
            spans: vec![span],
            ..Default::default()
        }
    }
}
impl<'a> From<&'a str> for Line<'a> {
    fn from(s: &'a str) -> Self {
        Line::raw(s)
    }
}
impl<'a> From<&'a str> for Span<'a> {
    fn from(s: &'a str) -> Self {
        Span::raw(s)
    }
}
impl<'a> From<String> for Span<'a> {
    fn from(s: String) -> Self {
        Span::raw(s)
    }
}

/// A line of spans, with an optional overall style and alignment.
#[derive(Debug, Clone, Default)]
pub struct Line<'a> {
    pub spans: Vec<Span<'a>>,
    pub style: Style,
    pub alignment: Alignment,
}

impl<'a> Line<'a> {
    pub fn raw(content: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self {
            spans: vec![Span::raw(content)],
            ..Default::default()
        }
    }
    /// Construct from a single span (any type that can become a `Span`).
    /// Note: this is `Line::from_span` not `Line::from` to avoid shadowing
    /// the `From<Vec<Span>>` trait impl.
    pub fn from_span<T: Into<Span<'a>>>(span: T) -> Self {
        Self {
            spans: vec![span.into()],
            ..Default::default()
        }
    }
    pub fn from_spans(spans: Vec<Span<'a>>) -> Self {
        Self {
            spans,
            ..Default::default()
        }
    }
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    pub fn alignment(mut self, a: Alignment) -> Self {
        self.alignment = a;
        self
    }
    /// The total display width of this line (sum of span widths).
    pub fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|s| str_width(&s.content))
            .sum()
    }
}

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

/// Word-wrapping configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wrap {
    pub trim: bool,
}

/// A paragraph: one or more lines, with optional scroll, wrap, alignment,
/// and an enclosing block.
#[derive(Debug, Clone, Default)]
pub struct Paragraph<'a> {
    pub lines: Vec<Line<'a>>,
    pub scroll: (u16, u16),
    pub wrap: Option<Wrap>,
    pub alignment: Alignment,
    pub block: Option<Block<'a>>,
    pub style: Style,
}

impl<'a> Paragraph<'a> {
    pub fn new<T: Into<ParagraphLines<'a>>>(content: T) -> Self {
        let lines = content.into().0;
        Self {
            lines,
            ..Default::default()
        }
    }
    pub fn scroll(mut self, row: u16, col: u16) -> Self {
        self.scroll = (row, col);
        self
    }
    pub fn wrap(mut self, w: Wrap) -> Self {
        self.wrap = Some(w);
        self
    }
    pub fn alignment(mut self, a: Alignment) -> Self {
        self.alignment = a;
        self
    }
    pub fn block(mut self, b: Block<'a>) -> Self {
        self.block = Some(b);
        self
    }
    pub fn style(mut self, s: Style) -> Self {
        self.style = s;
        self
    }

    /// Render the paragraph into `grid` within `area`. Handles block chrome,
    /// scroll offset, line wrapping, and alignment.
    pub fn render(&self, area: Rect, grid: &mut Grid) {
        let inner = if let Some(b) = &self.block {
            b.render(area, grid);
            // No additional padding beyond borders (matches muta usage).
            let mut ix = area.x;
            let mut iw = area.width;
            if b.borders.0 & super::block::Borders::LEFT.0 != 0 {
                ix += 1;
                iw = iw.saturating_sub(1);
            }
            if b.borders.0 & super::block::Borders::RIGHT.0 != 0 {
                iw = iw.saturating_sub(1);
            }
            Rect::new(ix, area.y, iw, area.height)
        } else {
            area
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let max_width = inner.width as usize;
        let row_offset = self.scroll.0 as usize;
        let mut y = inner.y;
        let bottom = inner.y + inner.height;
        let mut emitted = 0usize;

        for line in &self.lines {
            // Combine line.style with paragraph style (line wins on conflict).
            let base = merge_style(self.style, line.style);
            // Wrap this line into display rows (or split on embedded newlines).
            let wrapped = if self.wrap.is_some() {
                wrap_line(line, max_width)
            } else {
                split_line_newlines(line)
            };
            for wl in &wrapped {
                if emitted < row_offset {
                    emitted += 1;
                    continue;
                }
                if y >= bottom {
                    return;
                }
                // Horizontal alignment within the inner width.
                let lw = line_display_width(wl);
                let x = match self.alignment {
                    Alignment::Left => inner.x + self.scroll.1,
                    Alignment::Center => inner.x + (inner.width.saturating_sub(lw as u16)) / 2,
                    Alignment::Right => inner.x + inner.width.saturating_sub(lw as u16),
                };
                // Clip each span to the rect's right edge. `grid.put` only
                // clips at the *terminal* edge, so without this a non-wrapped
                // line longer than `inner.width` (e.g. a modal footer hint)
                // would spill past the panel into the backdrop. Wrapped lines
                // already fit within `max_width`, so this is a no-op for them.
                let right = inner.x + inner.width;
                let mut cx = x;
                for span in &wl.spans {
                    if cx >= right {
                        break;
                    }
                    let s = merge_style(base, span.style);
                    let avail = (right - cx) as usize;
                    let content = clip_to_cols(&span.content, avail);
                    let end = grid.put(cx, y, crate::buffer::grid::Fit::Clip, s, content);
                    cx = end.x;
                }
                y += 1;
                emitted += 1;
            }
        }
    }
}

/// Helper for `Paragraph::new` accepting a single line or a vec.
pub struct ParagraphLines<'a>(pub Vec<Line<'a>>);

impl<'a> From<Line<'a>> for ParagraphLines<'a> {
    fn from(l: Line<'a>) -> Self {
        ParagraphLines(vec![l])
    }
}
impl<'a> From<&'a str> for ParagraphLines<'a> {
    fn from(s: &'a str) -> Self {
        // Split on newlines into separate lines.
        let lines = s.split('\n').map(Line::raw).collect();
        ParagraphLines(lines)
    }
}
impl<'a> From<String> for ParagraphLines<'a> {
    fn from(s: String) -> Self {
        let lines = s.split('\n').map(|l| Line::raw(l.to_string())).collect();
        ParagraphLines(lines)
    }
}
impl<'a> From<Vec<Line<'a>>> for ParagraphLines<'a> {
    fn from(v: Vec<Line<'a>>) -> Self {
        ParagraphLines(v)
    }
}

fn merge_style(base: Style, over: Style) -> Style {
    Style {
        fg: if over.fg != Color::Reset {
            over.fg
        } else {
            base.fg
        },
        bg: if over.bg != Color::Reset {
            over.bg
        } else {
            base.bg
        },
        add: base.add | over.add,
    }
}

fn line_display_width(l: &Line<'_>) -> usize {
    l.spans
        .iter()
        .map(|s| str_len(&s.content))
        .sum()
}

/// The longest whole-grapheme prefix of `s` that fits within `max_cols`
/// display columns on a single row. Returns a borrowed slice (the full string when it already
/// fits), so the common in-bounds case allocates nothing. A wide glyph that
/// would straddle the boundary is dropped rather than half-drawn.
///
/// Stops immediately if a newline (`\n` or `\r`) is encountered, ensuring
/// that row-level span rendering never leaks into subsequent lines.
fn clip_to_cols(s: &str, max_cols: usize) -> &str {
    let mut used = 0usize;
    let mut bytes = 0usize;
    for piece in graphemes(s) {
        if piece.text == "\n" || piece.text == "\r\n" || piece.text == "\r" {
            break;
        }
        let w = piece.width as usize;
        if used + w > max_cols {
            break;
        }
        used += w;
        bytes += piece.text.len();
    }
    &s[..bytes]
}

/// Split a `Line` containing embedded newlines into multiple independent `Line`s,
/// preserving each span's style and the parent line's alignment/style.
fn split_line_newlines<'a>(line: &Line<'a>) -> Vec<Line<'a>> {
    let has_newline = line.spans.iter().any(|s| s.content.contains('\n'));
    if !has_newline {
        return vec![line.clone()];
    }

    let mut out = Vec::new();
    let mut current_spans = Vec::new();

    for span in &line.spans {
        let parts: Vec<&str> = span.content.split('\n').collect();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                out.push(Line {
                    spans: std::mem::take(&mut current_spans),
                    style: line.style,
                    alignment: line.alignment,
                });
            }
            if !part.is_empty() {
                let clean = part.strip_suffix('\r').unwrap_or(part);
                current_spans.push(Span::styled(clean.to_string(), span.style));
            }
        }
    }
    out.push(Line {
        spans: current_spans,
        style: line.style,
        alignment: line.alignment,
    });
    out
}

fn wrap_line<'a>(line: &Line<'a>, max_width: usize) -> Vec<Line<'a>> {
    // Flatten the line's spans into one string, wrap it, then re-split into
    // spans by walking the wrapped byte ranges. This preserves per-span styles
    // across wrap boundaries.
    let mut flat = String::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new(); // (byte_start, byte_end) per span in flat
    for span in &line.spans {
        let start = flat.len();
        flat.push_str(&span.content);
        ranges.push((start, flat.len()));
    }
    let lines = wrap(&flat, max_width);
    let mut out = Vec::with_capacity(lines.len());
    for (i, wl) in lines.iter().enumerate() {
        let mut spans: Vec<Span<'a>> = Vec::new();
        let lo = wl.start_byte;
        let hi = wl.end_byte;
        for (j, span) in line.spans.iter().enumerate() {
            let (s, e) = ranges[j];
            // Intersect [lo,hi) with [s,e).
            let a = lo.max(s);
            let b = hi.min(e);
            if a < b {
                let rel = a - s;
                let len = b - a;
                if let Some(sub) = span.content.get(rel..rel + len) {
                    spans.push(Span::styled(sub.to_string(), span.style));
                }
            }
        }
        out.push(Line {
            spans,
            style: line.style,
            alignment: line.alignment,
        });
        let _ = i;
    }
    if out.is_empty() {
        out.push(Line::raw(""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::grid::Grid;

    #[test]
    fn clip_to_cols_keeps_whole_graphemes() {
        assert_eq!(clip_to_cols("hello", 3), "hel");
        assert_eq!(clip_to_cols("hello", 99), "hello");
        assert_eq!(clip_to_cols("hello", 0), "");
        // A wide (CJK) glyph that would straddle the boundary is dropped, not
        // split in half.
        assert_eq!(clip_to_cols("世界", 1), "");
        assert_eq!(clip_to_cols("世界", 2), "世");
    }

    #[test]
    fn unwrapped_paragraph_clips_to_rect_not_terminal() {
        // A single-line (non-wrapped) Paragraph longer than its rect must stop
        // at the rect's right edge — never spill into the cells beyond it. This
        // is the guard against modal header/footer hints overflowing the panel.
        let mut grid = Grid::new(40, 1);
        // Sentinel content past the rect so we can detect a spill.
        grid.fill_rect(0, 0, 40, 1, Style::default());
        for x in 10..40 {
            grid.put(x, 0, crate::buffer::grid::Fit::Clip, Style::default(), "#");
        }
        let para = Paragraph::new("xxxxxxxxxxxxxxxxxxxxxxxxxxxx"); // 28 x's
        // Render into a 10-wide rect starting at column 0.
        para.render(Rect::new(0, 0, 10, 1), &mut grid);
        // Columns 0..10 are the x's; column 10 onward must keep the sentinel.
        for x in 0..10 {
            assert_eq!(grid.get(x, 0).unwrap().symbol(), "x", "col {x} in-rect");
        }
        assert_eq!(
            grid.get(10, 0).unwrap().symbol(),
            "#",
            "the cell just past the rect must not be overwritten"
        );
    }

    #[test]
    fn clip_to_cols_stops_at_newline() {
        assert_eq!(clip_to_cols("hello\nworld", 10), "hello");
        assert_eq!(clip_to_cols("hello\r\nworld", 10), "hello");
        assert_eq!(clip_to_cols("hello\rworld", 10), "hello");
    }

    #[test]
    fn split_line_newlines_preserves_styles() {
        let style = Style::default().fg(Color::Rgb(10, 20, 30));
        let line = Line::from(vec![
            Span::styled("first\nsecond ", style),
            Span::styled("part\nthird", Style::default()),
        ]);
        let lines = split_line_newlines(&line);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "first");
        assert_eq!(lines[0].spans[0].style.fg, Color::Rgb(10, 20, 30));

        assert_eq!(lines[1].spans.len(), 2);
        assert_eq!(lines[1].spans[0].content, "second ");
        assert_eq!(lines[1].spans[1].content, "part");

        assert_eq!(lines[2].spans.len(), 1);
        assert_eq!(lines[2].spans[0].content, "third");
    }

    #[test]
    fn paragraph_with_embedded_newlines_respects_rect_bounds() {
        let mut grid = Grid::new(20, 3);
        let line = Line::raw("line1\nline2\nline3\nline4");
        let para = Paragraph::new(line);
        // Render into a 20-wide, 2-high rect starting at y=1.
        para.render(Rect::new(2, 1, 10, 2), &mut grid);

        // Row 0 is untouched.
        assert_eq!(grid.get(2, 0).unwrap().symbol(), " ");
        // Row 1 has "line1" starting at col 2.
        assert_eq!(grid.get(2, 1).unwrap().symbol(), "l");
        assert_eq!(grid.get(6, 1).unwrap().symbol(), "1");
        assert_eq!(grid.get(0, 1).unwrap().symbol(), " ");
        assert_eq!(grid.get(1, 1).unwrap().symbol(), " ");
        // Row 2 has "line2" starting at col 2.
        assert_eq!(grid.get(2, 2).unwrap().symbol(), "l");
        assert_eq!(grid.get(6, 2).unwrap().symbol(), "2");
        assert_eq!(grid.get(0, 2).unwrap().symbol(), " ");
        assert_eq!(grid.get(1, 2).unwrap().symbol(), " ");
        // line3 and line4 are clipped because rect height is 2!
    }
}
