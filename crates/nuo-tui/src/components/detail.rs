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

use nuotc::{Modifier, Span, Style};
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

/// Render a stylized text progress bar: `[████░░░░]`.
///
/// Progress colors:
/// - `>= 0.90`: `theme.err()`
/// - `>= 0.70`: `theme.warn()`
/// - `< 0.70`: `theme.ok()`
pub fn render_progress_bar_spans(frac: f32, bar_width: usize, theme: &Theme) -> Vec<Span<'static>> {
    let clamped = frac.clamp(0.0, 1.0);
    let filled_count = ((clamped * bar_width as f32).round() as usize).min(bar_width);
    let empty_count = bar_width.saturating_sub(filled_count);

    let color = if clamped >= 0.90 {
        theme.err()
    } else if clamped >= 0.70 {
        theme.warn()
    } else {
        theme.ok()
    };

    let filled_str = "█".repeat(filled_count);
    let empty_str = "░".repeat(empty_count);

    vec![
        Span::styled("[", Style::default().fg(theme.dim())),
        Span::styled(filled_str, Style::default().fg(color)),
        Span::styled(empty_str, Style::default().fg(theme.dim())),
        Span::styled("]", Style::default().fg(theme.dim())),
    ]
}

/// Canonical layout builder for structured key-value detail / inspector views.
///
/// Encapsulates Gestalt layout invariants:
/// 1. Section separation with automatic blank row padding
/// 2. Standardized field label padding ([`DETAIL_LABEL_WIDTH`]) and minimum whitespace gap ([`DETAIL_MIN_GAP`])
/// 3. Consistent semantic styling (headers in bold primary accent, labels in dim, values in foreground / semantic colors)
/// 4. Document-model row semantics with [`SelectableRow`] for mouse selection and clipboard copy
/// 5. Integrated progress meters for resource/capacity utilization
#[derive(Clone)]
pub struct DetailLayout<'a> {
    theme: &'a Theme,
    rows: Vec<SelectableRow>,
}

#[allow(dead_code)]
impl<'a> DetailLayout<'a> {
    /// Create a new detail layout builder.
    pub fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            rows: Vec::new(),
        }
    }

    /// Add a section header row.
    ///
    /// If there are already rows in the layout, an empty spacing row is automatically
    /// prepended before the section header.
    pub fn section(mut self, title: impl AsRef<str>) -> Self {
        if !self.rows.is_empty() {
            self.rows.push(SelectableRow::empty());
        }
        self.rows
            .push(detail_section_row(title.as_ref(), self.theme));
        self
    }

    /// Add a key-value row with standard value styling (`theme.fg()`).
    pub fn kv(mut self, label: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.rows.push(detail_kv_row(
            label.as_ref(),
            value.as_ref(),
            Style::default().fg(self.theme.fg()),
            self.theme,
        ));
        self
    }

    /// Add a key-value row with explicit value styling.
    pub fn kv_styled(
        mut self,
        label: impl AsRef<str>,
        value: impl AsRef<str>,
        value_style: Style,
    ) -> Self {
        self.rows.push(detail_kv_row(
            label.as_ref(),
            value.as_ref(),
            value_style,
            self.theme,
        ));
        self
    }

    /// Add a key-value row with muted value styling (`theme.muted()`).
    pub fn kv_muted(mut self, label: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.rows.push(detail_kv_row(
            label.as_ref(),
            value.as_ref(),
            Style::default().fg(self.theme.muted()),
            self.theme,
        ));
        self
    }

    /// Add a key-value row with highlighted/primary value styling (`theme.primary`).
    pub fn kv_highlight(mut self, label: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.rows.push(detail_kv_row(
            label.as_ref(),
            value.as_ref(),
            Style::default().fg(self.theme.primary),
            self.theme,
        ));
        self
    }

    /// Add a key-value row with bold primary foreground value styling.
    pub fn kv_bold(mut self, label: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.rows.push(detail_kv_row(
            label.as_ref(),
            value.as_ref(),
            Style::default()
                .fg(self.theme.fg())
                .add_modifier(Modifier::BOLD),
            self.theme,
        ));
        self
    }

    /// Add a key-value row with indented label.
    pub fn kv_indented(
        mut self,
        indent: usize,
        label: impl AsRef<str>,
        value: impl AsRef<str>,
        value_style: Style,
    ) -> Self {
        self.rows.push(detail_kv_row_indented(
            indent,
            label.as_ref(),
            value.as_ref(),
            value_style,
            self.theme,
        ));
        self
    }

    /// Add a key-value row with multi-segment value.
    pub fn kv_segments(mut self, label: impl AsRef<str>, segments: Vec<RowSegment>) -> Self {
        self.rows
            .push(detail_kv_row_segments(label.as_ref(), segments, self.theme));
        self
    }

    /// Add a key-value row featuring a progress bar meter and optional trailing text.
    pub fn kv_progress(
        mut self,
        label: impl AsRef<str>,
        frac: f32,
        bar_width: usize,
        trailing_text: Option<impl AsRef<str>>,
    ) -> Self {
        let spans = render_progress_bar_spans(frac, bar_width, self.theme);
        let mut segments = spans
            .into_iter()
            .map(|s| RowSegment::styled(s.content.into_owned(), s.style))
            .collect::<Vec<_>>();
        if let Some(extra) = trailing_text {
            segments.push(RowSegment::styled(
                format!("  {}", extra.as_ref()),
                Style::default().fg(self.theme.fg()),
            ));
        }
        self.rows
            .push(detail_kv_row_segments(label.as_ref(), segments, self.theme));
        self
    }

    /// Add an empty spacer row.
    pub fn empty_row(mut self) -> Self {
        self.rows.push(SelectableRow::empty());
        self
    }

    /// Append an arbitrary raw [`SelectableRow`].
    pub fn raw_row(mut self, row: SelectableRow) -> Self {
        self.rows.push(row);
        self
    }

    /// Append multiple raw [`SelectableRow`]s.
    pub fn raw_rows(mut self, rows: impl IntoIterator<Item = SelectableRow>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Return the active theme.
    pub fn theme(&self) -> &'a Theme {
        self.theme
    }

    /// Return a slice of the rows built so far.
    pub fn rows(&self) -> &[SelectableRow] {
        &self.rows
    }

    /// Consume the builder and return the vector of selectable rows.
    pub fn build(self) -> Vec<SelectableRow> {
        self.rows
    }

    /// Consume the builder and return the vector of selectable rows (alias of `build`).
    pub fn into_rows(self) -> Vec<SelectableRow> {
        self.rows
    }
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

    #[test]
    fn detail_layout_builder_and_spacing() {
        let theme = Theme::default();
        let rows = DetailLayout::new(&theme)
            .section("Identity")
            .kv("Name", "GPT-4")
            .kv("Provider", "OpenAI")
            .section("Capabilities")
            .kv("Context Window", "128,000 tokens")
            .kv_progress("Utilization", 0.5, 10, Some("50.0%"))
            .build();

        // Section 1 header + 2 kv rows + 1 empty spacer + Section 2 header + 1 kv row + 1 progress row = 7 rows
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0].text(), "Identity");
        assert_eq!(rows[1].prefix.as_ref().unwrap().text, "Name              ");
        assert_eq!(rows[1].text(), "GPT-4");
        assert_eq!(rows[2].prefix.as_ref().unwrap().text, "Provider          ");
        assert_eq!(rows[2].text(), "OpenAI");
        // Spacer before section 2
        assert_eq!(rows[3].text(), "");
        assert!(rows[3].segments.is_empty());
        // Section 2
        assert_eq!(rows[4].text(), "Capabilities");
        assert_eq!(rows[5].text(), "128,000 tokens");
        assert_eq!(rows[6].prefix.as_ref().unwrap().text, "Utilization       ");
        assert!(
            rows[6]
                .to_line()
                .spans
                .iter()
                .any(|s| s.content.contains("50.0%"))
        );
    }

    #[test]
    fn detail_progress_bar_spans_color_tiers() {
        let theme = Theme::default();
        let normal = render_progress_bar_spans(0.5, 10, &theme);
        assert_eq!(normal.len(), 4); // [, filled, empty, ]
        assert_eq!(normal[1].style.fg, theme.ok());

        let warn = render_progress_bar_spans(0.75, 10, &theme);
        assert_eq!(warn[1].style.fg, theme.warn());

        let err = render_progress_bar_spans(0.95, 10, &theme);
        assert_eq!(err[1].style.fg, theme.err());
    }
}
