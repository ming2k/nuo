//! Selectable modal body: the "text is copyable by default" component.
//!
//! [`render_selectable_body`] is the selection-aware counterpart of
//! [`crate::primitives::render_body`] for *documentary* modal content — the
//! read-only text surfaces (Help, Usage Statistics, session detail, history
//! preview, …) where the body *is* the content the user came to read.
//!
//! Unlike `render_body`, which hands a `Vec<Line>` to the engine and lets
//! `Paragraph::wrap` run inside the engine, this component keeps wrapping in
//! the application layer so that **every visual row** is registered in the
//! [`LayoutMap`] as a [`BlockRegion`] under
//! [`MODAL_DOC_MSG_IDX`] — the same per-visual-row contract the transcript's
//! `paint_text_row` path uses. That is what makes the text drag-selectable and
//! copyable via the global `Ctrl+Shift+C`, and it is why selection stays
//! correct across reflow and scroll: hit-testing and copy resolve against the
//! region's own text, never against a re-derived screen projection.
//!
//! Rows are declared as [`SelectableRow`]s — styled *segments* of plain text —
//! rather than pre-built `Line`s. Building spans *after* wrapping is what
//! allows the selected byte range (computed against the wrapped slice) to
//! split the row into unselected / selected / unselected spans, exactly like
//! the transcript renders selections. The concatenated segment text is the
//! row's document text, so copy yields what the user sees, labels and all.
//!
//! Control surfaces (pickers, checklists, keycap rows, footer strips) stay on
//! `render_body`: their rows are interactive *targets*, not documents, and
//! selection there would fight the click affordances.

use nuotc::{Frame, Line, Rect, Span, Style};

use crate::design::MODAL_INNER_H_PADDING;
use crate::model::layout::{BlockRegion, LayoutMap, LinkHit, MODAL_DOC_MSG_IDX};
use crate::model::selection::SelectionState;
use crate::primitives::{
    ContentModalSpec, SCROLL_EDGE_MARGIN, content_modal_probe, draw_scrollbar, modal_chrome_rows,
    resolve_scroll,
};
use crate::text_layout::{WrappedLine, block_selection_range, line_selection, wrap_text};
use crate::theme::Theme;

/// One styled segment of a selectable row. The row's document text is the
/// concatenation of its segments' texts, in order.
#[derive(Debug, Clone)]
pub(crate) struct RowSegment {
    pub text: String,
    pub style: Style,
}

impl RowSegment {
    /// Single-segment constructor.
    pub(crate) fn styled(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// One logical row of a selectable modal body: optional decoration prefixes
/// (indent, `│ ` rail) plus the row's styled content segments. `block_idx` is
/// derived from position (the row's logical index), matching the OAuth
/// sheet's existing region layout.
#[derive(Debug, Clone)]
pub(crate) struct SelectableRow {
    /// Decoration painted before the content on the row's **first** visual
    /// row (indent, `✓` glyph + gutter). Occupies display columns
    /// (subtracted from the wrap budget) but is **not** part of the document
    /// text: the region records it via `prefix_cols`, so copy yields the
    /// content only.
    pub prefix: Option<RowSegment>,
    /// Decoration painted before the content on **continuation** visual rows
    /// of this logical row. When the row wraps, continuation rows align under
    /// the content column (hanging indent). Defaults to the same width as
    /// [`Self::prefix`] (whitespace-filled) when `None` but a prefix exists.
    pub hang_prefix: Option<RowSegment>,
    pub segments: Vec<RowSegment>,
}

impl SelectableRow {
    /// A single-segment row.
    pub(crate) fn styled(text: impl Into<String>, style: Style) -> Self {
        Self {
            prefix: None,
            hang_prefix: None,
            segments: vec![RowSegment::styled(text, style)],
        }
    }

    /// A row from pre-built segments, in order.
    pub(crate) fn from_segments(segments: Vec<RowSegment>) -> Self {
        Self {
            prefix: None,
            hang_prefix: None,
            segments,
        }
    }

    /// An empty (spacer) row — occupies one visual row, copies as "".
    #[allow(dead_code)]
    pub(crate) fn empty() -> Self {
        Self {
            prefix: None,
            hang_prefix: None,
            segments: Vec::new(),
        }
    }

    /// Attach a decoration prefix (indent / rail) to a row, returned for
    /// chaining. The prefix paints on the row's first visual row and is
    /// excluded from copy. When the row wraps, continuation rows default to a
    /// whitespace prefix of the same width; override with
    /// [`Self::with_hang_prefix`].
    #[allow(dead_code)]
    pub(crate) fn with_prefix(mut self, prefix: RowSegment) -> Self {
        self.prefix = Some(prefix);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn with_hang_prefix(mut self, prefix: RowSegment) -> Self {
        self.hang_prefix = Some(prefix);
        self
    }

    /// Flatten a pre-built engine `Line` into a row. Each span becomes one
    /// segment carrying its style; the row's document text is the spans'
    /// concatenation, which is what the user sees and what copy yields.
    ///
    /// This is the migration path for modal bodies that already build
    /// `Line`s: `SelectableRow::from_line(line)` + `render_selectable_body`
    /// gives them selection without touching their body builders. Rows whose
    /// spans are pure layout (full-width padding, pills, right-aligned
    /// meters) should NOT be converted — copying alignment whitespace is
    /// noise; those surfaces stay on `render_body`.
    ///
    /// A single leading all-whitespace span is lifted into the row's
    /// decoration prefix (excluded from copy) so indented documentary rows
    /// migrate without their indent polluting copied text.
    pub(crate) fn from_line(line: nuotc::Line<'static>) -> Self {
        let mut spans = line.spans;
        let prefix = spans
            .first()
            .filter(|s| !s.content.is_empty() && s.content.trim().is_empty())
            .map(|s| RowSegment {
                text: s.content.clone().into_owned(),
                style: s.style,
            });
        if prefix.is_some() {
            spans.remove(0);
        }
        Self {
            prefix,
            hang_prefix: None,
            segments: spans
                .into_iter()
                .map(|span| RowSegment {
                    text: span.content.into_owned(),
                    style: span.style,
                })
                .collect(),
        }
    }

    /// The row's document text (content segments only — the decoration
    /// prefix is not part of the document).
    pub(crate) fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>()
    }

    /// Calculate the visual row count for this logical row when wrapped to `body_width`.
    ///
    /// Mirrors the wrap-budgeting logic in [`render_selectable_body`].
    #[allow(dead_code)]
    pub(crate) fn visual_row_count(&self, body_width: usize) -> usize {
        let seg_width = |s: &RowSegment| nuotc::text::str_width(&s.text);
        let prefix_w = self.prefix.as_ref().map(seg_width).unwrap_or(0);
        let budget = body_width.saturating_sub(prefix_w).max(1);
        let full = self.text();
        let rows = wrap_text(&full, budget);
        if rows.is_empty() { 1 } else { rows.len() }
    }
}

/// Compute the total visual rows for a slice of [`SelectableRow`]s within `body_width`.
#[allow(dead_code)]
pub(crate) fn selectable_body_visual_rows(rows: &[SelectableRow], body_width: usize) -> usize {
    rows.iter().map(|r| r.visual_row_count(body_width)).sum()
}

/// Compute the desired modal height (in rows, including chrome) for a selectable body
/// inside a [`ContentModalSpec`].
///
/// Probes the modal's available body width in `frame` and wraps every row, matching
/// [`render_selectable_body`]'s visual row accounting so content-sized modals open
/// at the exact height needed to show their content without false scrollbars.
#[allow(dead_code)]
pub(crate) fn selectable_body_desired_rows(
    frame: &Frame,
    geometry: ContentModalSpec,
    rows: &[SelectableRow],
) -> u16 {
    let probe = content_modal_probe(frame, geometry);
    let body_w = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);
    let visual_rows = selectable_body_visual_rows(rows, body_w);
    (visual_rows as u16) + modal_chrome_rows(geometry.modal_spec())
}

/// Render a documentary modal body with default-on text selection.
///
/// * Wraps each row to `body_rect.width` (the same `wrap_text` the transcript
///   uses), so wrapping happens in the application layer where byte offsets
///   are known.
/// * Applies `scroll`/`follow` with [`resolve_scroll`] — identical semantics
///   to `render_body`, including the [`SCROLL_EDGE_MARGIN`] follow band.
/// * Paints the selection highlight (`theme.selected()`) on the intersecting
///   byte range of every visible row, splitting at segment boundaries so each
///   glyph keeps its own style under the highlight.
/// * Registers one [`BlockRegion`] per **visual** row under
///   [`MODAL_DOC_MSG_IDX`], anchored to the row's byte range within its
///   logical row (`block_idx` = logical row index).
///
/// Selection state is global (`app.selection`), so a range started on the
/// transcript before the modal opened is simply not intersected here and never
/// paints — the modal's own regions are the only ones on screen behind the
/// backdrop.
#[allow(clippy::too_many_arguments)] // mirrors render_body's shape; the args are the modal render contract
pub(crate) fn render_selectable_body(
    frame: &mut Frame,
    body_rect: Rect,
    rows: &[SelectableRow],
    scroll: &mut usize,
    follow: Option<usize>,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) {
    let width = body_rect.width as usize;
    // Wrap every logical row up front: the follow index and the scroll window
    // are expressed in *visual* rows once wrapping is applied, exactly like
    // `render_body`'s engine-internal accounting (`Paragraph` wraps first,
    // then scrolls the wrapped rows). Decoration prefixes (indent, glyph
    // gutters) are excluded from the wrap budget and from the document text:
    // the first visual row paints `prefix`, continuation rows paint
    // `hang_prefix` (or a whitespace fill of the prefix width), and each
    // region records its own prefix width via `prefix_cols` so hit-testing
    // skips the decoration.
    let seg_width = |s: &RowSegment| -> u16 { nuotc::text::str_width(&s.text) as u16 };
    let wrap_budget = |row: &SelectableRow| -> usize {
        let prefix_w = row.prefix.as_ref().map(&seg_width).unwrap_or(0);
        width.saturating_sub(prefix_w as usize).max(1)
    };
    // Resolve the effective decoration for visual row `k` (0 = first) of a
    // logical row: the prefix, the hang prefix, or a whitespace fill matching
    // the prefix width.
    let decoration_for = |row: &SelectableRow, k: usize| -> (String, Style, u16) {
        match (&row.prefix, &row.hang_prefix) {
            (Some(p), _) if k == 0 => {
                let mut t = p.text.clone();
                let w = seg_width(p);
                if p.text.chars().all(|c| c == ' ')
                    && let Some(h) = &row.hang_prefix
                {
                    // Whitespace prefix with a hanging override: pad the
                    // first row to the hang width so both align.
                    let hw = seg_width(h);
                    if hw > w {
                        t = " ".repeat(hw as usize);
                        return (t, p.style, hw);
                    }
                }
                (t, p.style, w)
            }
            (Some(_), Some(h)) => (h.text.clone(), h.style, seg_width(h)),
            (Some(p), None) => (" ".repeat(seg_width(p) as usize), p.style, seg_width(p)),
            _ => (String::new(), Style::default(), 0),
        }
    };
    let wrapped: Vec<(usize, &SelectableRow, String, Style, u16, WrappedLine)> = rows
        .iter()
        .enumerate()
        .flat_map(|(logical_idx, row)| {
            let full = row.text();
            // A logical row renders as at least one visual row even when empty
            // (spacer), mirroring `render_body`'s one-`Line`-one-row contract.
            let rows = wrap_text(&full, wrap_budget(row));
            let rows = if rows.is_empty() {
                vec![WrappedLine {
                    text: String::new(),
                    start_byte: 0,
                    end_byte: 0,
                }]
            } else {
                rows
            };
            rows.into_iter().enumerate().map(move |(k, wl)| {
                let (deco_text, deco_style, deco_w) = decoration_for(row, k);
                (logical_idx, row, deco_text, deco_style, deco_w, wl)
            })
        })
        .collect();

    let visible = body_rect.height as usize;
    let (start, max_scroll) =
        resolve_scroll(scroll, visible, wrapped.len(), follow, SCROLL_EDGE_MARGIN);

    for (vi, (logical_idx, row, deco_text, deco_style, deco_w, wl)) in
        wrapped.iter().enumerate().skip(start)
    {
        if vi >= start + visible {
            break;
        }
        let y = body_rect.y + (vi - start) as u16;
        let logical_idx = *logical_idx;
        let selected = line_selection(
            block_selection_range(selection, MODAL_DOC_MSG_IDX, logical_idx),
            wl,
        );
        let mut line = render_row_line(&wl.text, wl.start_byte, &row.segments, selected, theme);
        if !deco_text.is_empty() {
            line.spans
                .insert(0, Span::styled(deco_text.clone(), *deco_style));
        }
        let rect = Rect::new(body_rect.x, y, body_rect.width, 1);
        frame.render_widget(nuotc::Paragraph::new(line), rect);
        layout_map.push(BlockRegion {
            message_idx: MODAL_DOC_MSG_IDX,
            block_idx: logical_idx,
            start_byte: wl.start_byte,
            end_byte: wl.end_byte,
            text: wl.text.clone(),
            prefix_cols: *deco_w,
            rect,
            hidden_ranges: Vec::new(),
        });

        for (start_byte, _) in wl
            .text
            .match_indices("http://")
            .chain(wl.text.match_indices("https://"))
        {
            let tail = &wl.text[start_byte..];
            let len = tail
                .find(|c: char| {
                    c.is_whitespace() || c == ')' || c == ']' || c == '>' || c == '"' || c == '\''
                })
                .unwrap_or(tail.len());
            let end_byte = start_byte + len;
            if start_byte < end_byte {
                let full_row = row.text();
                let link_url =
                    if full_row.starts_with("http://") || full_row.starts_with("https://") {
                        full_row.trim().to_string()
                    } else {
                        wl.text[start_byte..end_byte].to_string()
                    };
                let x_offset = unicode_width::UnicodeWidthStr::width(&wl.text[..start_byte]);
                let w =
                    unicode_width::UnicodeWidthStr::width(&wl.text[start_byte..end_byte]).max(1);
                layout_map.push_link_hit(LinkHit {
                    message_idx: MODAL_DOC_MSG_IDX,
                    block_idx: logical_idx,
                    range: (wl.start_byte + start_byte, wl.start_byte + end_byte),
                    url: link_url,
                    rect: Rect::new(rect.x + deco_w + x_offset as u16, rect.y, w as u16, 1),
                });
            }
        }
    }

    draw_scrollbar(frame, body_rect, start, max_scroll, theme);
}

/// Build one visual row's `Line`: walk segment boundaries + selection
/// boundaries and emit one span per uniform (segment, selected) run. With no
/// selection (or no intersection) this degenerates to one span per segment.
///
/// `text` is the **wrapped slice** of the row's document text and
/// `base_offset` is that slice's byte offset within the full row — segment
/// boundaries are therefore translated into the slice's coordinate space
/// before slicing. (Slicing the wrapped text with full-row offsets silently
/// drops text on continuation rows: a range that runs past the end of the
/// slice is skipped, which is how `bash  ls | head` rendered as `l | he`.)
fn render_row_line(
    text: &str,
    base_offset: usize,
    segments: &[RowSegment],
    selected: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    if text.is_empty() {
        return Line::from(Vec::<Span<'static>>::new());
    }
    // Segment byte ranges over the concatenated row text, intersected with
    // the wrapped slice's [base_offset, base_offset + text.len()) window.
    let slice_end = base_offset + text.len();
    let mut points: Vec<usize> = Vec::with_capacity(segments.len() * 2 + 3);
    points.push(0);
    let mut acc = 0usize;
    for seg in segments {
        let seg_start = acc;
        acc += seg.text.len();
        let seg_end = acc;
        // Only segments that overlap this visual row become split points.
        let (lo, hi) = (seg_start.max(base_offset), seg_end.min(slice_end));
        if lo < hi {
            points.push(lo - base_offset);
            points.push(hi - base_offset);
        }
    }
    if let Some((lo, hi)) = selected {
        points.push(lo.min(text.len()));
        points.push(hi.min(text.len()));
    }
    points.push(text.len());
    points.sort_unstable();
    points.dedup();

    let seg_style = |p: usize| -> Option<&Style> {
        let full = p + base_offset;
        let mut acc = 0usize;
        for seg in segments {
            acc += seg.text.len();
            if full < acc {
                return Some(&seg.style);
            }
        }
        None
    };

    let mut spans = Vec::new();
    for pair in points.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        if lo >= hi || hi > text.len() {
            continue;
        }
        let Some(base) = seg_style(lo) else {
            continue;
        };
        let is_sel = matches!(selected, Some((slo, shi)) if lo >= slo && hi <= shi);
        let style = if is_sel {
            base.bg(theme.selected()).fg(theme.fg())
        } else {
            *base
        };
        spans.push(Span::styled(text[lo..hi].to_string(), style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::selection::{SelectionDrag, SelectionState};

    fn body_rect(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    /// Every logical row — including one that soft-wraps — registers exactly
    /// one region per *visual* row, under the MODAL_DOC sentinel, with byte
    /// ranges that tile the row's text.
    #[test]
    fn registers_one_region_per_visual_row() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(20, 10);
        let rows = vec![SelectableRow::styled(
            "abcdefghij klmnopqrst uvwxyz", // wraps at 20 cols
            nuotc::Style::default(),
        )];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(20, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });
        assert_eq!(map.region_at(0, 0).map(|r| r.block_idx), Some(0));
        // Two wrapped rows, same logical row idx, disjoint byte windows that
        // reassemble to the source text.
        let r1 = map.region_at(0, 0).expect("wrapped row 1");
        let r2 = map.region_at(0, 1).expect("wrapped row 2");
        assert_eq!(r1.block_idx, r2.block_idx);
        assert_eq!(
            format!("{}{}", r1.text, r2.text),
            "abcdefghij klmnopqrst uvwxyz"
        );
        assert!(r1.end_byte <= r2.start_byte || r2.end_byte <= r1.start_byte);
    }

    /// An empty spacer row still occupies one visual row with an empty region,
    /// so layout accounting matches `render_body`'s one-line-per-`Line`.
    #[test]
    fn empty_row_occupies_one_visual_row() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(20, 10);
        let rows = vec![
            SelectableRow::styled("one", nuotc::Style::default()),
            SelectableRow::empty(),
            SelectableRow::styled("two", nuotc::Style::default()),
        ];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(20, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });
        let middle = map.region_at(0, 1).expect("spacer region");
        assert_eq!(middle.text, "");
        assert_eq!(map.region_at(0, 2).map(|r| r.text.as_str()), Some("two"));
    }

    /// A selection spanning two logical rows resolves through
    /// `extract_text_for_range` to the wrapped text of both, exactly what the
    /// copy action runs for MODAL_DOC regions.
    #[test]
    fn selection_extract_spans_rows() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(40, 10);
        let rows = vec![
            SelectableRow::styled("hello world", nuotc::Style::default()),
            SelectableRow::styled("second row", nuotc::Style::default()),
        ];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        // Drag from (0,0) to end of row 1.
        let sel = {
            terminal.draw(|f| {
                render_selectable_body(
                    f,
                    body_rect(40, 5),
                    &rows,
                    &mut scroll,
                    None,
                    &theme,
                    &SelectionState::None,
                    &mut map,
                );
            });
            let anchor = map.cursor_at(0, 0).expect("anchor");
            let head = map.cursor_at(6, 1).expect("head");
            SelectionState::Range { anchor, head }
        };
        // Drag from (0,0) to (6,1): column 6 of "second row" is the space
        // after "second", so the inclusive-end extraction stops there.
        let text = map.extract_text_for_range(&sel).expect("extracted");
        assert_eq!(text, "hello world\nsecond ");
    }

    /// Hit-testing maps a screen column to the byte offset inside the *row's*
    /// text, so a click past the end of a short row lands at end-of-row, not
    /// in a neighbouring row.
    #[test]
    fn cursor_at_clamps_to_row_end() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(40, 10);
        let rows = vec![SelectableRow::styled("ab", nuotc::Style::default())];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(40, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });
        let cursor = map.cursor_at(39, 0).expect("cursor past end");
        assert_eq!(cursor.byte_offset, 2);
    }

    /// Multi-segment rows (label + value, the usage-stats / session-detail
    /// shape) flatten to their concatenated text; selection painting splits
    /// at segment boundaries without disturbing hit-testing.
    #[test]
    fn multi_segment_row_round_trips() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(40, 10);
        let rows = vec![SelectableRow::from_segments(vec![
            RowSegment::styled("Total  ", nuotc::Style::default()),
            RowSegment::styled("12345", nuotc::Style::default()),
        ])];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(40, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });
        let region = map.region_at(0, 0).expect("region");
        assert_eq!(region.text, "Total  12345");
        // Click on the digits (column 7) resolves inside the value segment.
        let cursor = map.cursor_at(7, 0).expect("cursor");
        assert_eq!(cursor.byte_offset, 7);
    }

    /// A multi-segment row that soft-wraps must render every character on the
    /// continuation row. Segment byte boundaries are computed against the
    /// *full* row text while the painted text is the *wrapped slice*: slicing
    /// the slice with full-row offsets skips any range that runs past the
    /// slice's end (`hi > text.len()`), which silently dropped mid-row text —
    /// e.g. a permission header `bash  ls | head` rendering as `l | he`.
    #[test]
    fn wrapped_multi_segment_row_keeps_every_segment() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(12, 10);
        // Two segments: label (4 cols) + separator (2) + value (9) = 15 cols,
        // so the row wraps at the 12-col body width.
        let rows = vec![SelectableRow::from_segments(vec![
            RowSegment::styled("bash", nuotc::Style::default()),
            RowSegment::styled("  ", nuotc::Style::default()),
            RowSegment::styled("ls | head", nuotc::Style::default()),
        ])];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(12, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });

        // Grid text: every non-whitespace character of the row survives.
        let buf = terminal.buffer();
        let mut painted = String::new();
        for y in 0..5 {
            for x in 0..12 {
                painted.push_str(buf.get(x, y).map(|c| c.symbol()).unwrap_or(" "));
            }
        }
        let non_ws = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(
            non_ws(&painted),
            non_ws("bash  ls | head"),
            "wrapped row dropped characters: {painted:?}"
        );

        // Regions: the wrapped rows tile the full document text.
        let r1 = map.region_at(0, 0).expect("wrapped row 1");
        let r2 = map.region_at(0, 1).expect("wrapped row 2");
        assert_eq!(format!("{}{}", r1.text, r2.text), "bash  ls | head");
    }

    /// A decoration prefix (indent / rail) paints on every visual row but is
    /// excluded from the document text: copy yields the content only, and
    /// hit-testing skips the prefix columns via `prefix_cols`.
    #[test]
    fn prefix_excluded_from_copy_and_hit_testing() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(20, 10);
        let rows = vec![
            SelectableRow::styled(
                "indented content that is long enough to wrap once or twice",
                nuotc::Style::default(),
            )
            .with_prefix(RowSegment::styled("  ", nuotc::Style::default())),
        ];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(20, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });
        let region = map.region_at(0, 0).expect("region");
        assert_eq!(region.prefix_cols, 2);
        assert!(!region.text.starts_with(' '), "copy text has no indent");
        // Column 1 is inside the prefix: the cursor resolves to the content
        // start (byte 0), never a negative/hijacked offset.
        let cursor = map.cursor_at(1, 0).expect("cursor in prefix");
        assert_eq!(cursor.byte_offset, 0);
    }

    /// The full mouse-drag interaction chain the event loop runs for a
    /// selectable modal: press (arm via `begin_range`), move
    /// (`update_from_point`), release (`finish`), copy
    /// (`extract_text_for_range`). A press on the backdrop *outside* every
    /// region resolves to no cursor — the signal the dismiss branch keys on.
    #[test]
    fn drag_chain_from_press_to_copy() {
        use crate::model::selection::SelectionDrag;

        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(40, 10);
        let rows = vec![
            SelectableRow::styled("alpha", nuotc::Style::default()),
            SelectableRow::styled("beta", nuotc::Style::default()),
            SelectableRow::styled("gamma", nuotc::Style::default()),
        ];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(40, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });

        // Press inside the document: resolves and arms.
        let press = map.cursor_at(2, 0).expect("press resolves");
        assert_eq!(press.message_idx, MODAL_DOC_MSG_IDX);
        let mut drag = SelectionDrag::default();
        let mut selection = SelectionState::None;
        drag.begin_range(&mut selection, press);

        // Drag to the third row: head follows the pointer.
        drag.update_from_point(&mut selection, &map, 3, 2);
        drag.finish(&mut selection);
        assert!(selection.is_active(), "drag must leave a live selection");

        let text = map
            .extract_text_for_range(&selection)
            .expect("copy extracts");
        // The head character is included (the transcript's inclusive-end
        // selection rule), so column 3 of "gamma" copies through "gamm".
        assert_eq!(text, "pha\nbeta\ngamm");

        // Press on the backdrop (below the last painted row of this 5-row
        // body): no region there, so the dismiss branch keeps ownership.
        assert!(
            map.cursor_at(10, 4).is_none(),
            "empty panel area must not resolve to a document cursor"
        );
    }

    #[test]
    fn visual_row_count_and_desired_rows_match_wrapped_rendering() {
        let mut grid = nuotc::Grid::new(80, 24);
        let frame = nuotc::Frame::new(&mut grid);
        let geometry = ContentModalSpec::PERMISSIONS;

        let rows = vec![
            SelectableRow::styled("Header", nuotc::Style::default()),
            SelectableRow::styled(
                "This is a long line of text that is expected to wrap across multiple visual lines when given a standard content modal width.",
                nuotc::Style::default(),
            ),
            SelectableRow::empty(),
            SelectableRow::styled("Footer", nuotc::Style::default()),
        ];

        let probe = content_modal_probe(&frame, geometry);
        let body_w = (probe.width as usize)
            .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
            .max(1);

        let visual_rows = selectable_body_visual_rows(&rows, body_w);
        assert!(visual_rows >= 4);

        let desired = selectable_body_desired_rows(&frame, geometry, &rows);
        assert_eq!(
            desired,
            visual_rows as u16 + modal_chrome_rows(geometry.modal_spec())
        );
    }

    #[test]
    fn selection_extract_wrapped_single_row_url() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(30, 10);
        let url = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=123456";
        let rows = vec![SelectableRow::styled(url, nuotc::Style::default())];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(30, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });

        // The URL wraps across multiple visual rows with the same logical block_idx = 0.
        // Start selection on the second line (y = 1) and drag across to line 3 (y = 2).
        let start_cursor = map.cursor_at(0, 1).expect("line 1 cursor");

        let mut drag = SelectionDrag::default();
        let mut selection = SelectionState::None;
        drag.begin_range(&mut selection, start_cursor);
        drag.update_from_point(&mut selection, &map, 10, 2);
        drag.finish(&mut selection);

        assert!(selection.is_active());
        let text = map
            .extract_text_for_range(&selection)
            .expect("must extract text across wrapped lines of a single row");
        assert!(!text.is_empty());
        assert!(url.contains(&text));
    }

    #[test]
    fn selectable_body_registers_link_hits() {
        let theme = Theme::default();
        let mut terminal = nuotc::TestTerminal::new(50, 10);
        let url = "https://example.com/oauth";
        let rows = vec![SelectableRow::styled(
            format!("Click {url} to authenticate"),
            nuotc::Style::default(),
        )];
        let mut scroll = 0;
        let mut map = LayoutMap::new();
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(50, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });

        // Link hit should be registered at column 6 (where "https://..." starts)
        let link_hit = map.link_at(8, 0).expect("link hit should be found");
        assert_eq!(link_hit.url, url);
    }
}
