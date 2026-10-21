//! Key-value detail and inspector components with standardized Gestalt column layout.
//!
//! This is the canonical standard for structured key-value detail / inspector sub-views
//! across modal surfaces (e.g. Model Info, Connection Details, Session Details).
//!
//! ## Layout Rules
//!
//! 1. **Fixed Label Column with Guaranteed Minimum Gap**:
//!    The label column is padded to at least [`DETAIL_LABEL_WIDTH`] (18 columns).
//!    To ensure property names never collide with their values (such as `Prompt Cache Hit`
//!    or `Reasoning Effort` which are 16 characters long), a minimum gap of at least
//!    [`DETAIL_MIN_GAP`] (2 columns) is strictly enforced. Even if a label reaches or exceeds
//!    [`DETAIL_LABEL_WIDTH`], it will always maintain at least [`DETAIL_MIN_GAP`] of whitespace.
//!
//! 2. **Consistent Semantic Styling**:
//!    - Section headers use the brand / primary accent tone with bold weight.
//!    - Field labels use the subdued dim tone (`theme.dim()`).
//!    - Field values default to primary foreground (`theme.fg()`), or semantic tokens
//!      (`theme.ok()`, `theme.warn()`, `theme.muted()`).
//!
//! 3. **Document-Model Interoperability**:
//!    All inspector rows are declared as [`SelectableRow`]s, guaranteeing mouse selection,
//!    hit-testing, and clipboard copy support across all detail surfaces. When line-oriented
//!    rendering is needed, [`SelectableRow::to_line`] losslessly projects back to `Line`.

use nuotc::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::selectable_body::{RowSegment, SelectableRow};
use crate::render::Theme;

/// Standard column width allocated for detail / inspector field labels.
pub const DETAIL_LABEL_WIDTH: usize = 18;

/// Minimum whitespace gap guaranteed between a label and its value.
pub const DETAIL_MIN_GAP: usize = 2;

/// Format a label for a detail / inspector row with standard width and minimum gap.
///
/// Pads `label` to at least [`DETAIL_LABEL_WIDTH`], guaranteeing at least
/// [`DETAIL_MIN_GAP`] whitespace columns so a label never collides with its value.
pub fn format_detail_label(label: &str) -> String {
    format_detail_label_indented(0, label)
}

/// Format an indented label (e.g. sub-items under a section) with guaranteed gap.
pub fn format_detail_label_indented(indent: usize, label: &str) -> String {
    let w = label.width() + indent;
    let pad = DETAIL_LABEL_WIDTH.saturating_sub(w).max(DETAIL_MIN_GAP);
    format!("{}{label}{}", " ".repeat(indent), " ".repeat(pad))
}

/// Section header row for selectable detail views.
pub fn detail_section_row(title: &str, theme: &Theme) -> SelectableRow {
    SelectableRow::styled(
        title.to_string(),
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    )
}

/// Key-value row for selectable detail views.
pub fn detail_kv_row(label: &str, value: &str, value_style: Style, theme: &Theme) -> SelectableRow {
    SelectableRow::styled(value.to_string(), value_style).with_prefix(RowSegment::styled(
        format_detail_label(label),
        Style::default().fg(theme.dim()),
    ))
}

/// Key-value row with indentation for selectable detail views.
pub fn detail_kv_row_indented(
    indent: usize,
    label: &str,
    value: &str,
    value_style: Style,
    theme: &Theme,
) -> SelectableRow {
    SelectableRow::styled(value.to_string(), value_style).with_prefix(RowSegment::styled(
        format_detail_label_indented(indent, label),
        Style::default().fg(theme.dim()),
    ))
}

/// Key-value row with multi-segment value for selectable detail views.
pub fn detail_kv_row_segments(
    label: &str,
    segments: Vec<RowSegment>,
    theme: &Theme,
) -> SelectableRow {
    SelectableRow::from_segments(segments).with_prefix(RowSegment::styled(
        format_detail_label(label),
        Style::default().fg(theme.dim()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_label_padding_and_minimum_gap() {
        // Short label padded to DETAIL_LABEL_WIDTH (18)
        let short = format_detail_label("Wire ID");
        assert_eq!(short.width(), DETAIL_LABEL_WIDTH);
        assert_eq!(short, "Wire ID           ");

        // 14-char label
        let rates = format_detail_label("Standard Rates");
        assert_eq!(rates.width(), DETAIL_LABEL_WIDTH);
        assert_eq!(rates, "Standard Rates    ");

        // 16-char labels: MUST have at least 2 spaces gap, reaching 18
        let cache = format_detail_label("Prompt Cache Hit");
        assert_eq!(cache.width(), DETAIL_LABEL_WIDTH);
        assert_eq!(cache, "Prompt Cache Hit  ");
        assert!(cache.ends_with("  "));

        let effort = format_detail_label("Reasoning Effort");
        assert_eq!(effort.width(), DETAIL_LABEL_WIDTH);
        assert_eq!(effort, "Reasoning Effort  ");
        assert!(effort.ends_with("  "));

        // Exactly 18-char label: DETAIL_MIN_GAP ensures 2 spaces added!
        let exact = format_detail_label("123456789012345678");
        assert_eq!(exact.width(), DETAIL_LABEL_WIDTH + DETAIL_MIN_GAP);
        assert_eq!(exact, "123456789012345678  ");

        // Exceeding label: preserves DETAIL_MIN_GAP
        let long = format_detail_label("Longer Than Eighteen Chars");
        assert!(long.ends_with("  "));
        assert_eq!(
            long.width(),
            "Longer Than Eighteen Chars".len() + DETAIL_MIN_GAP
        );
    }

    #[test]
    fn detail_indented_label_maintains_gap() {
        let indented = format_detail_label_indented(2, "Primary Balance");
        assert!(indented.starts_with("  Primary Balance"));
        assert!(indented.ends_with("  "));
        assert!(indented.width() >= DETAIL_LABEL_WIDTH);
    }

    #[test]
    fn detail_row_construction_and_to_line() {
        let theme = Theme::default();
        let row = detail_kv_row(
            "Reasoning Effort",
            "high",
            Style::default().fg(theme.fg()),
            &theme,
        );
        assert_eq!(
            row.prefix.as_ref().map(|p| p.text.as_str()),
            Some("Reasoning Effort  ")
        );
        assert_eq!(row.segments.len(), 1);
        assert_eq!(row.segments[0].text, "high");

        let line = row.to_line();
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "Reasoning Effort  ");
        assert_eq!(line.spans[1].content, "high");
    }
}
