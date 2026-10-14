//! Adaptive GFM-style table grid layout. Sizes columns to their widest cell,
//! proportionally shrinks them when the table would overflow, wraps cell text
//! to the allotted width, and produces the rendered border + data lines along
//! with per-cell byte spans used by selection and click hit-testing.

use nuotc::{
    Span, {Color, Style},
};

use crate::model::document::{CodeRange, InlineScan, MathRange, TableAlignment, scan_inline};

use super::text_layout::{
    WrappedLine, markup_hidden_ranges, markup_visible_width, wrap_text_markup,
};

/// Push one segment of a table grid line, splitting it around the selection
/// overlay (if any). `seg_lo`/`seg_hi` are byte offsets within `text`;
/// `style` is the base style for the segment (border vs. content); `sel_bg`
/// is painted under the selected portion.
pub(crate) fn push_table_segment(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    seg_lo: usize,
    seg_hi: usize,
    style: Style,
    sel: Option<(usize, usize)>,
    sel_bg: Color,
) {
    if seg_hi <= seg_lo {
        return;
    }
    match sel {
        Some((slo, shi)) if slo < seg_hi && seg_lo < shi => {
            let cut_lo = slo.max(seg_lo) - seg_lo;
            let cut_hi = shi.min(seg_hi) - seg_lo;
            let segment = &text[seg_lo..seg_hi];
            if cut_lo > 0 {
                spans.push(Span::styled(segment[..cut_lo].to_string(), style));
            }
            spans.push(Span::styled(
                segment[cut_lo..cut_hi].to_string(),
                style.bg(sel_bg),
            ));
            if cut_hi < segment.len() {
                spans.push(Span::styled(segment[cut_hi..].to_string(), style));
            }
        }
        _ => {
            spans.push(Span::styled(text[seg_lo..seg_hi].to_string(), style));
        }
    }
}

/// Result of laying out a table: the rendered grid lines plus, for each data
/// line, the row index and the byte span of each column's (padded) content
/// within that line. Border lines carry `None`. The spans let the renderer
/// highlight one cell at a time and resolve clicks to a specific cell.
pub(crate) struct TableRender {
    pub lines: Vec<String>,
    pub line_info: Vec<Option<TableRowInfo>>,
}

pub(crate) struct TableRowInfo {
    pub row: usize,
    /// `(byte_start, byte_end)` of each column's padded content within the
    /// line text. Length equals the column count.
    pub col_spans: Vec<(usize, usize)>,
    /// `(byte_start, byte_end)` of the actual cell text content (without
    /// alignment padding) within the line text. A sub-range of `col_spans`.
    pub col_content_spans: Vec<(usize, usize)>,
    /// Start byte of this line's cell content within the original (unwrapped,
    /// unpadded) cell text. Used to map code/bold ranges onto the displayed
    /// substring.
    pub col_offsets: Vec<usize>,
    /// Per-cell original code ranges (absolute, relative to the original
    /// cell text). Same for every wrapped line of the same cell.
    pub col_code_ranges: Vec<Vec<CodeRange>>,
    /// Per-cell original bold ranges (absolute, relative to the original
    /// cell text). Same for every wrapped line of the same cell.
    pub col_bold_ranges: Vec<Vec<CodeRange>>,
    /// Per-cell original inline-math ranges (absolute, relative to the original
    /// cell text). Same for every wrapped line of the same cell.
    pub col_math_ranges: Vec<Vec<MathRange>>,
}

/// Build the visual lines of a GFM-style table grid that fits within
/// `max_width` display columns. Columns are sized to their widest cell
/// (intrinsic width) when space allows; when the table would overflow,
/// columns shrink proportionally to a minimum of 3 columns and cell text
/// wraps within the allotted width.
pub(crate) fn build_table_render(
    headers: &[String],
    rows: &[Vec<String>],
    aligns: &[TableAlignment],
    max_width: usize,
) -> TableRender {
    let ncols = headers.len();
    if ncols == 0 {
        return TableRender {
            lines: Vec::new(),
            line_info: Vec::new(),
        };
    }

    // Scan inline code / bold ranges for every cell first (original text,
    // before wrapping or padding). The ranges use byte offsets relative to the
    // start of each cell's original string, and double as the markup map for
    // computing the *visible* width below. Iterate by column index over
    // `0..ncols` so a ragged row yields empty styles for its missing cells
    // rather than producing a short vec that `format_data_line` would
    // out-of-bounds index (`cell_styles[i]`).
    let header_cell_styles: Vec<InlineScan> = headers.iter().map(|h| scan_inline(h)).collect();
    let row_cell_styles: Vec<Vec<InlineScan>> = rows
        .iter()
        .map(|row| {
            (0..ncols)
                .map(|i| row.get(i).map(|cell| scan_inline(cell)).unwrap_or_default())
                .collect()
        })
        .collect();

    // Per-column intrinsic *visible* width — the display columns a cell
    // actually occupies once `` ` ``/`**` markup delimiters are elided to zero
    // width by the renderer. Sizing on the raw width would reserve space for
    // delimiters that never paint, making markup columns too wide and shifting
    // data-row `│` separators out of line with the border grid.
    // `.take(ncols)` ignores any cells beyond the header width (GFM drops
    // over-wide cells); short rows simply contribute fewer candidates, which is
    // correct for a column-wise maximum.
    let mut widths = vec![0usize; ncols];
    for (i, (h, inline)) in headers.iter().zip(&header_cell_styles).enumerate() {
        widths[i] = widths[i].max(markup_visible_width(
            h,
            0,
            &inline.code_ranges,
            &inline.bold_ranges,
            &inline.math_ranges,
        ));
    }
    for (row, styles) in rows.iter().zip(&row_cell_styles) {
        for (i, (cell, inline)) in row.iter().zip(styles).enumerate().take(ncols) {
            widths[i] = widths[i].max(markup_visible_width(
                cell,
                0,
                &inline.code_ranges,
                &inline.bold_ranges,
                &inline.math_ranges,
            ));
        }
    }

    // "│ cell │ cell │": each column contributes width + 2 padding, plus 1
    // separator per column boundary. Total = sum(widths) + 3*ncols + 1.
    let border_overhead = 3 * ncols + 1;
    let total: usize = widths.iter().sum::<usize>() + border_overhead;
    if total > max_width {
        let content_available = max_width.saturating_sub(border_overhead);
        widths = shrink_column_widths(&widths, content_available, 3);
    }

    // Wrap each cell to its (possibly shrunk) column width, preserving byte
    // offsets via WrappedLine. Markup delimiters are zero-width, so wrap on the
    // visible width — otherwise they'd consume column budget and could split a
    // `` `…` `` / `**…**` pair across lines.
    let wrap_cell = |cell: &str, w: usize, inline: &InlineScan| -> Vec<WrappedLine> {
        if cell.is_empty() {
            return vec![WrappedLine {
                text: String::new(),
                start_byte: 0,
                end_byte: 0,
            }];
        }
        let hidden = markup_hidden_ranges(
            cell,
            0,
            &inline.code_ranges,
            &inline.bold_ranges,
            &inline.math_ranges,
        );
        let wrapped = wrap_text_markup(cell, w.max(1), &hidden);
        if wrapped.is_empty() {
            vec![WrappedLine {
                text: String::new(),
                start_byte: 0,
                end_byte: 0,
            }]
        } else {
            wrapped
        }
    };

    let wrapped_headers: Vec<Vec<WrappedLine>> = (0..ncols)
        .map(|i| wrap_cell(&headers[i], widths[i], &header_cell_styles[i]))
        .collect();
    let wrapped_rows: Vec<Vec<Vec<WrappedLine>>> = rows
        .iter()
        .enumerate()
        .map(|(r, row)| {
            (0..ncols)
                .map(|i| {
                    let cell = row.get(i).map(String::as_str).unwrap_or("");
                    wrap_cell(cell, widths[i], &row_cell_styles[r][i])
                })
                .collect()
        })
        .collect();

    let join_horizontal = |sep: &str| -> String {
        widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join(sep)
    };

    // Build one data line from a row of wrapped cells, returning the line
    // text together with per-cell geometry and style metadata.
    let format_data_line = |cells: &[Vec<WrappedLine>],
                            cell_styles: &[InlineScan],
                            line_idx: usize|
     -> (String, TableRowInfo) {
        let mut line = String::from("│ ");
        let mut col_spans = Vec::with_capacity(ncols);
        let mut col_content_spans = Vec::with_capacity(ncols);
        let mut col_offsets = Vec::with_capacity(ncols);
        let mut col_code_ranges = Vec::with_capacity(ncols);
        let mut col_bold_ranges = Vec::with_capacity(ncols);
        let mut col_math_ranges = Vec::with_capacity(ncols);
        for i in 0..ncols {
            let wl = cells[i].get(line_idx);
            let cell_line = wl.map(|w| w.text.as_str()).unwrap_or("");
            let cell_start_byte = wl.map(|w| w.start_byte).unwrap_or(0);
            let align = aligns.get(i).copied().unwrap_or(TableAlignment::None);
            let inline = &cell_styles[i];

            // Visible width this wrapped line occupies once markup
            // delimiters are elided — drives the padding so the rendered
            // content fills exactly `widths[i]` display columns and the
            // row's `│` separators line up with the border grid.
            let cell_visible_w = markup_visible_width(
                cell_line,
                cell_start_byte,
                &inline.code_ranges,
                &inline.bold_ranges,
                &inline.math_ranges,
            );

            // `pad_cell_text` returns the padded string along with the byte
            // range of the actual content within it, derived from the same
            // construction (leading spaces + cell bytes). Using these offsets
            // keeps them in byte space and in lock-step with the string,
            // rather than re-deriving them from display widths here.
            let (part, content_lo_in_part, content_hi_in_part) =
                pad_cell_text(cell_line, widths[i], cell_visible_w, align);

            let padded_start = line.len();
            line.push_str(&part);
            let padded_end = line.len();

            col_spans.push((padded_start, padded_end));
            col_content_spans.push((
                padded_start + content_lo_in_part,
                padded_start + content_hi_in_part,
            ));
            col_offsets.push(cell_start_byte);

            col_code_ranges.push(inline.code_ranges.clone());
            col_bold_ranges.push(inline.bold_ranges.clone());
            col_math_ranges.push(inline.math_ranges.clone());

            if i + 1 < ncols {
                line.push_str(" │ ");
            }
        }
        line.push_str(" │");
        (
            line,
            TableRowInfo {
                row: 0, // filled by caller
                col_spans,
                col_content_spans,
                col_offsets,
                col_code_ranges,
                col_bold_ranges,
                col_math_ranges,
            },
        )
    };

    let mut lines = Vec::new();
    let mut line_info: Vec<Option<TableRowInfo>> = Vec::new();

    lines.push(format!("┌{}┐", join_horizontal("┬")));
    line_info.push(None);

    let header_height = wrapped_headers.iter().map(|v| v.len()).max().unwrap_or(1);
    for line_idx in 0..header_height {
        let (l, mut info) = format_data_line(&wrapped_headers, &header_cell_styles, line_idx);
        info.row = 0;
        lines.push(l);
        line_info.push(Some(info));
    }

    lines.push(format!("├{}┤", join_horizontal("┼")));
    line_info.push(None);

    let separator = format!("├{}┤", join_horizontal("┼"));

    for (row_idx, wrapped_row) in wrapped_rows.iter().enumerate() {
        let row_height = wrapped_row.iter().map(|v| v.len()).max().unwrap_or(1);
        let cell_styles = &row_cell_styles[row_idx];
        for line_idx in 0..row_height {
            let (l, mut info) = format_data_line(wrapped_row, cell_styles, line_idx);
            info.row = row_idx + 1;
            lines.push(l);
            line_info.push(Some(info));
        }
        // Horizontal separator between body rows (not after the last one).
        if row_idx + 1 < wrapped_rows.len() {
            lines.push(separator.clone());
            line_info.push(None);
        }
    }

    lines.push(format!("└{}┘", join_horizontal("┴")));
    line_info.push(None);

    TableRender { lines, line_info }
}

/// Proportionally shrink column widths so they fit within `target` display
/// columns. Each column keeps at least `min_col` characters; the remaining
/// budget is distributed in proportion to how much above the minimum each
/// column's intrinsic width is.
pub(crate) fn shrink_column_widths(
    intrinsic: &[usize],
    target: usize,
    min_col: usize,
) -> Vec<usize> {
    let ncols = intrinsic.len();
    if ncols == 0 {
        return Vec::new();
    }
    let total_min = min_col * ncols;
    if target <= total_min {
        return vec![min_col; ncols];
    }
    let total_intrinsic: usize = intrinsic.iter().sum();
    let shrinkable = total_intrinsic.saturating_sub(total_min);
    if shrinkable == 0 {
        return intrinsic.to_vec();
    }
    let available = target - total_min;
    intrinsic
        .iter()
        .map(|&w| {
            let above_min = w.saturating_sub(min_col);
            min_col + above_min * available / shrinkable
        })
        .collect()
}

/// Pad `cell` to `width` display columns according to `align`, returning the
/// padded string together with the byte range of the actual cell content within
/// it. `visible_w` is the on-screen width the cell occupies (markup delimiters
/// excluded); padding is sized from it so the rendered cell fills exactly
/// `width` columns. The content range is measured from the string's own
/// construction (leading spaces + cell bytes), so it is always a valid char
/// boundary and never drifts from the padding arithmetic.
pub(crate) fn pad_cell_text(
    cell: &str,
    width: usize,
    visible_w: usize,
    align: TableAlignment,
) -> (String, usize, usize) {
    let pad = width.saturating_sub(visible_w);
    let (leading, trailing) = match align {
        TableAlignment::Right => (pad, 0),
        TableAlignment::Center => {
            let left = pad / 2;
            (left, pad - left)
        }
        TableAlignment::None | TableAlignment::Left => (0, pad),
    };
    let content_start = leading;
    let content_end = leading + cell.len();
    let text = format!("{}{}{}", " ".repeat(leading), cell, " ".repeat(trailing));
    (text, content_start, content_end)
}
