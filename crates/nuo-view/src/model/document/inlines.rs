//! Inline tokens, styled spans, and link ranges.
use super::{clamp_link_ranges, clamp_ranges, scan_inline};

/// A byte range `[start, end)` within a prose block's `content` that should be
/// rendered as inline code. The in-house parser keeps the backtick delimiters
/// in the flattened `content` and records the range here so the renderer can
/// paint it on the code surface without disturbing the byte-addressable
/// copy/selection model (which still sees plain text).
///
/// Ranges always cover the full `` `…` `` span including both backticks, and
/// are clamped to `content.len()`. An empty vector means "no inline code".
pub type CodeRange = (usize, usize);

/// A byte range `[start, end)` within a prose block's `content` that should be
/// rendered as inline math. The source delimiters stay in `content` for exact
/// copy/selection; renderers may elide them visually.
pub type MathRange = (usize, usize);

/// A byte range for a recognized hyperlink inside prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRange {
    /// Full source range, including markdown / TeX link delimiters when present.
    pub range: (usize, usize),
    /// The visible label range inside `range`. For bare URLs this equals `range`.
    pub label_range: (usize, usize),
    /// Normalized URL target. First-pass support intentionally records only
    /// browser-safe `http://` and `https://` targets.
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineScan {
    pub code_ranges: Vec<CodeRange>,
    pub bold_ranges: Vec<CodeRange>,
    pub math_ranges: Vec<MathRange>,
    pub link_ranges: Vec<LinkRange>,
}

/// Inline-prose payload shared by the prose block variants: the flattened
/// text plus the byte ranges of its inline markup.
///
/// The ranges are produced by `scan_inline` at parse time, address bytes in
/// `content`, and are clamped to `content.len()`. `content` keeps the original
/// delimiters (backticks, `**`, link syntax) so copy/selection yields exact
/// source while renderers may elide them visually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub content: String,
    /// Byte ranges of inline-code runs within `content` (see [`CodeRange`]).
    pub code_ranges: Vec<CodeRange>,
    /// Byte ranges of strong/bold text runs within `content`.
    pub bold_ranges: Vec<CodeRange>,
    /// Byte ranges of inline math runs within `content`.
    pub math_ranges: Vec<MathRange>,
    /// Hyperlink ranges within `content`.
    pub link_ranges: Vec<LinkRange>,
}

impl Inline {
    /// Verbatim text with no inline markup.
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }

    /// Scan `content` for inline markup, trimming trailing whitespace and
    /// clamping every range to the trimmed length.
    pub(crate) fn scanned(content: &str) -> Self {
        let scan = scan_inline(content);
        let trimmed_len = content.trim_end().len();
        Self {
            content: content[..trimmed_len].to_string(),
            code_ranges: clamp_ranges(&scan.code_ranges, trimmed_len),
            bold_ranges: clamp_ranges(&scan.bold_ranges, trimmed_len),
            math_ranges: clamp_ranges(&scan.math_ranges, trimmed_len),
            link_ranges: clamp_link_ranges(&scan.link_ranges, trimmed_len),
        }
    }
}
