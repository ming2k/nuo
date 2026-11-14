//! Selectable modal body: the "text is copyable by default" component.
//!
//! [`render_selectable_body`] is the selection-aware counterpart of
//! [`nuo_ui_primitives::primitives::render_body`] for *documentary* modal content — the
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

use nuotc::{Frame, Rect, Span, Style};

use crate::model::layout::{BlockRegion, LayoutMap, LinkHit, MODAL_DOC_MSG_IDX};
use crate::model::selection::SelectionState;
use crate::text_layout::{WrappedLine, block_selection_range, line_selection, wrap_text};
use nuo_ui_primitives::design::MODAL_INNER_H_PADDING;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, SCROLL_EDGE_MARGIN, content_modal_probe, draw_scrollbar, modal_chrome_rows,
    resolve_scroll,
};
use nuo_ui_primitives::theme::Theme;

pub use nuo_ui_primitives::components::row::{RowSegment, SelectableRow, render_row_line};

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
pub fn render_selectable_body(
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
    let seg_width = |s: &RowSegment| -> u16 { nuotc::str_len(&s.text) as u16 };
    let wrap_budget = |row: &SelectableRow| -> usize {
        let prefix_w = row.prefix.as_ref().map(seg_width).unwrap_or(0);
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
        frame.render_widget(&mut nuotc::Paragraph::new(line), rect);
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
                let x_offset = nuotc::str_len(&wl.text[..start_byte]);
                let w = nuotc::str_len(&wl.text[start_byte..end_byte]).max(1);
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

#[cfg(test)]
mod tests;
