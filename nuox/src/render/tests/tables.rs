//! Markdown table rendering tests: ragged rows, visible-width sizing, column shrinking.

use super::*;

/// Wide tables (including CJK content) must keep borders intact and never
/// overflow the viewport: columns shrink to fit, cell text wraps, and
/// every rendered line stays within the available width.
#[test]
fn wide_table_shrinks_columns_and_keeps_borders_intact() {
    use crate::model::document::TableAlignment;

    let headers = vec![
        "Tool".to_string(),
        "Type".to_string(),
        "Implementation".to_string(),
        "Key Feature".to_string(),
    ];
    let rows = vec![
        vec![
            "execute_command".to_string(),
            "Write".to_string(),
            "std::process::Command (sh -c / cmd /C)".to_string(),
            "execute shell command, supports timeout, truncates output".to_string(),
        ],
        vec![
            "read_text".to_string(),
            "Read".to_string(),
            "std::fs::read_to_string".to_string(),
            "supports offset/limit".to_string(),
        ],
    ];
    let aligns = vec![
        TableAlignment::None,
        TableAlignment::None,
        TableAlignment::None,
        TableAlignment::None,
    ];

    // Narrow terminal (34 cols): table is far wider, must shrink
    let lines = build_table_render(&headers, &rows, &aligns, 34).lines;
    assert!(!lines.is_empty(), "table must produce output");

    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.width() <= 34,
            "line {i} overflows: {} cols: {}",
            line.width(),
            line
        );
    }
    assert!(lines.first().unwrap().starts_with('┌'));
    assert!(lines.last().unwrap().starts_with('└'));
    assert!(
        lines.iter().any(|l| l.starts_with('├')),
        "missing header/body separator"
    );
    // Two body rows → one separator between them (plus one after header).
    let sep_count = lines.iter().filter(|l| l.starts_with('├')).count();
    assert_eq!(
        sep_count, 2,
        "expected 2 separators (header→body + row→row), got {sep_count}"
    );
    let pipe_counts: Vec<usize> = lines
        .iter()
        .filter(|l| l.starts_with('│'))
        .map(|l| l.matches('│').count())
        .collect();
    assert!(!pipe_counts.is_empty(), "must have data lines");
    assert!(
        pipe_counts.iter().all(|&c| c == pipe_counts[0]),
        "all data lines must have the same number of column separators"
    );

    // Wide terminal (80 cols): table fits without shrinking
    let wide_lines = build_table_render(&headers, &rows, &aligns, 76).lines;
    for (i, line) in wide_lines.iter().enumerate() {
        assert!(
            line.width() <= 76,
            "wide line {i} overflows: {} cols",
            line.width()
        );
    }
    // When it fits, the table should be shorter (no wrapping needed).
    assert!(
        wide_lines.len() <= lines.len(),
        "wide table should have fewer lines than shrunk table"
    );
}

/// Ragged body rows (fewer cells than the header, and more) must not panic
/// the adaptive renderer and must still produce a rectangular grid: every
/// data line carries the same number of `│` column separators. Regression
/// test for the `index out of bounds: the len is 1 but the index is 1`
/// panic at `markdown_table.rs` (`cell_styles[i]`) caused by a body row
/// with a single cell in a two-column table.
#[test]
fn table_render_handles_ragged_rows_without_panicking() {
    use crate::model::document::TableAlignment;

    let headers = vec!["A".to_string(), "B".to_string()];
    // 0, 1, 2, and 3 cells — exercises both the under- and over-wide paths.
    let rows = vec![
        vec![],
        vec!["only".to_string()],
        vec!["x".to_string(), "y".to_string()],
        vec!["p".to_string(), "q".to_string(), "r".to_string()],
    ];
    let aligns = vec![TableAlignment::None, TableAlignment::None];

    let table = build_table_render(&headers, &rows, &aligns, 40);
    assert!(!table.lines.is_empty(), "ragged table must still render");

    // Every data line must have the same number of column separators, i.e.
    // the grid stays rectangular regardless of input raggedness.
    let pipe_counts: Vec<usize> = table
        .lines
        .iter()
        .filter(|l| l.starts_with('│'))
        .map(|l| l.matches('│').count())
        .collect();
    assert!(!pipe_counts.is_empty(), "must have data lines");
    assert!(
        pipe_counts.iter().all(|&c| c == pipe_counts[0]),
        "ragged rows produced uneven column counts: {pipe_counts:?}"
    );

    // Every data line carries per-cell geometry for exactly `ncols` cells,
    // so hit-testing / selection never indexes out of bounds.
    for info in table.line_info.iter().flatten() {
        assert_eq!(
            info.col_spans.len(),
            2,
            "each data line must describe exactly 2 cells"
        );
    }
}

/// Inline-code / bold markup delimiters (`` ` ``, `**`) are rendered at zero
/// width, so a column holding markup must be sized and wrapped by its
/// *visible* width — otherwise the column is inflated, the wrapped text can
/// split a `` `…` ``/`**…**` pair across lines, and data-row `│` separators
/// drift out of line with the border grid. A plain table and a markup table
/// carrying the same visible content must therefore share identical borders
/// and the same line count (no spurious wrap).
#[test]
fn table_markup_columns_size_to_visible_width() {
    use crate::model::document::TableAlignment;

    let plain = build_table_render(
        &["a".to_string(), "b".to_string()],
        &[vec!["bold".to_string(), "code".to_string()]],
        &[TableAlignment::None, TableAlignment::None],
        80,
    );
    let markup = build_table_render(
        &["a".to_string(), "b".to_string()],
        &[vec!["**bold**".to_string(), "`code`".to_string()]],
        &[TableAlignment::None, TableAlignment::None],
        80,
    );

    // Borders are markup-free, so plain and markup grids must match exactly
    // once columns are sized to visible width.
    let plain_borders: Vec<&String> = plain.lines.iter().filter(|l| !l.starts_with('│')).collect();
    let markup_borders: Vec<&String> = markup
        .lines
        .iter()
        .filter(|l| !l.starts_with('│'))
        .collect();
    assert_eq!(
        plain_borders, markup_borders,
        "markup must not inflate column width"
    );

    // The markup cell fits its column on a single line (no delimiter split):
    // same number of data lines as the plain version.
    let plain_data = plain.lines.iter().filter(|l| l.starts_with('│')).count();
    let markup_data = markup.lines.iter().filter(|l| l.starts_with('│')).count();
    assert_eq!(
        plain_data, markup_data,
        "markup must not introduce extra wrapped lines"
    );
}

#[test]
fn shrink_columns_preserves_minimum_and_proportions() {
    // Intrinsic [10, 5, 20], target 24, min 3.
    // total_min = 9, shrinkable = 26, available = 15.
    // col0: 3 + 7*15/26 = 3 + 4 = 7
    // col1: 3 + 2*15/26 = 3 + 1 = 4
    // col2: 3 + 17*15/26 = 3 + 9 = 12
    let result = shrink_column_widths(&[10, 5, 20], 24, 3);
    assert_eq!(result.len(), 3);
    assert!(result.iter().all(|&w| w >= 3), "must respect minimum");
    assert!(
        result.iter().sum::<usize>() <= 24,
        "must fit within target, got {}",
        result.iter().sum::<usize>()
    );
    // Largest intrinsic column stays largest after shrinking.
    let max_val = *result.iter().max().unwrap();
    let max_idx = result.iter().position(|&v| v == max_val).unwrap();
    assert_eq!(max_idx, 2);
}

#[test]
fn shrink_columns_with_tiny_target_returns_all_minimum() {
    let result = shrink_column_widths(&[10, 20, 30], 5, 3);
    assert_eq!(result, vec![3, 3, 3]);
}
