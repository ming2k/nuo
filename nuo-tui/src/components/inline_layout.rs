//! Semantic inline layout and 1D flex constraint solver.
//!
//! Provides [`SemanticLine`] and [`InlineSlot`] for constructing structured,
//! layout-aware lines (such as tool step invocation summaries) where fixed tokens,
//! flexible arguments, and adaptive filesystem paths can be resolved at paint time
//! against the terminal viewport width without premature stringification.

use std::borrow::Cow;
use std::fmt;

use nuotc::{Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::components::path::{PathFormatStrategy, PathStyle, PathView};
use crate::theme::Theme;

/// A single segment within a [`SemanticLine`].
#[derive(Clone, Debug)]
pub enum InlineSlot<'a> {
    /// Incompressible token with fixed visual width.
    /// E.g. "+ ", "Search ", " in ".
    Fixed(Span<'a>),

    /// Plain text that is rendered with the container's base style.
    FixedText(Cow<'a, str>),

    /// Elastic secondary text (e.g. query pattern) that can shrink under constrained budgets.
    Flexible {
        text: Cow<'a, str>,
        priority: u8,
        min_width: usize,
    },

    /// Adaptive filesystem path component.
    Path(PathView<'a>),
}

/// A structured line consisting of semantic slots evaluated either as lossless
/// plain text or as a constrained `Line` using a 1D flex solver.
#[derive(Clone, Debug, Default)]
pub struct SemanticLine<'a> {
    slots: Vec<InlineSlot<'a>>,
}

impl<'a> SemanticLine<'a> {
    /// Create an empty semantic line.
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Create a semantic line from a pre-formatted plain text string (fallback wrapper).
    pub fn plain(text: impl Into<Cow<'a, str>>) -> Self {
        let mut line = Self::new();
        line.slots.push(InlineSlot::FixedText(text.into()));
        line
    }

    /// Append a fixed unstyled text segment.
    pub fn push_fixed(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        self.slots.push(InlineSlot::FixedText(text.into()));
        self
    }

    /// Append a fixed styled [`Span`].
    pub fn push_fixed_span(mut self, span: Span<'a>) -> Self {
        self.slots.push(InlineSlot::Fixed(span));
        self
    }

    /// Append an elastic text segment (default priority 1, min_width 4).
    pub fn push_flexible(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        self.slots.push(InlineSlot::Flexible {
            text: text.into(),
            priority: 1,
            min_width: 4,
        });
        self
    }

    /// Append an elastic text segment with explicit priority and minimum width.
    pub fn push_flexible_with(
        mut self,
        text: impl Into<Cow<'a, str>>,
        priority: u8,
        min_width: usize,
    ) -> Self {
        self.slots.push(InlineSlot::Flexible {
            text: text.into(),
            priority,
            min_width,
        });
        self
    }

    /// Append an adaptive [`PathView`] component.
    pub fn push_path(mut self, path: PathView<'a>) -> Self {
        self.slots.push(InlineSlot::Path(path));
        self
    }

    /// Convert into an owned [`SemanticLine<'static>`].
    pub fn into_owned(self) -> SemanticLine<'static> {
        let slots = self
            .slots
            .into_iter()
            .map(|slot| match slot {
                InlineSlot::Fixed(span) => {
                    InlineSlot::Fixed(Span::styled(span.content.into_owned(), span.style))
                }
                InlineSlot::FixedText(text) => InlineSlot::FixedText(Cow::Owned(text.into_owned())),
                InlineSlot::Flexible {
                    text,
                    priority,
                    min_width,
                } => InlineSlot::Flexible {
                    text: Cow::Owned(text.into_owned()),
                    priority,
                    min_width,
                },
                InlineSlot::Path(path) => InlineSlot::Path(path.into_owned()),
            })
            .collect();
        SemanticLine { slots }
    }

    /// Convert the semantic line into a full-fidelity, unconstrained plain text string.
    /// Used for testing, snapshot comparisons, clipboard copy, and headless execution.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for slot in &self.slots {
            match slot {
                InlineSlot::Fixed(span) => out.push_str(span.content.as_ref()),
                InlineSlot::FixedText(text) => out.push_str(text),
                InlineSlot::Flexible { text, .. } => out.push_str(text),
                InlineSlot::Path(path) => {
                    // Lossless: no max_width constraint applied
                    out.push_str(&path.format_text());
                }
            }
        }
        out
    }

    /// Resolve the semantic line against a concrete width budget, rendering styled Ratatui spans.
    ///
    /// An optional `suffix` (e.g. status/duration badge `(" (3ms)", suffix_style)`) is treated
    /// with highest priority so telemetry is never pushed off-screen.
    pub fn resolve(
        &self,
        budget: usize,
        theme: &Theme,
        base_style: Style,
        suffix: Option<(&str, Style)>,
    ) -> Vec<Span<'static>> {
        if budget == 0 {
            return Vec::new();
        }

        let suffix_width = suffix.map(|(s, _)| s.width()).unwrap_or(0);

        // Pass 1: Calculate fixed column requirements
        let mut fixed_width = suffix_width;
        let mut flex_count = 0usize;
        let mut path_count = 0usize;

        for slot in &self.slots {
            match slot {
                InlineSlot::Fixed(span) => fixed_width += span.content.width(),
                InlineSlot::FixedText(text) => fixed_width += text.width(),
                InlineSlot::Flexible { .. } => flex_count += 1,
                InlineSlot::Path(_) => path_count += 1,
            }
        }

        let remaining_slack = budget.saturating_sub(fixed_width);

        // Pass 2: Allocate budget between flexible text and path slots
        let (flex_budgets, path_budget) =
            distribute_slack(&self.slots, remaining_slack, flex_count, path_count);

        // Pass 3: Assemble spans
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(self.slots.len() + 2);
        let mut flex_idx = 0usize;

        for slot in &self.slots {
            match slot {
                InlineSlot::Fixed(span) => {
                    let mut style = span.style;
                    if style == Style::default() {
                        style = base_style;
                    }
                    spans.push(Span::styled(span.content.to_string(), style));
                }
                InlineSlot::FixedText(text) => {
                    spans.push(Span::styled(text.to_string(), base_style));
                }
                InlineSlot::Flexible { text, .. } => {
                    let allocated = flex_budgets.get(flex_idx).copied().unwrap_or(text.width());
                    flex_idx += 1;
                    if text.width() <= allocated {
                        spans.push(Span::styled(text.to_string(), base_style));
                    } else {
                        let truncated = crate::components::path::truncate_middle(text, allocated);
                        spans.push(Span::styled(truncated, base_style));
                    }
                }
                InlineSlot::Path(path) => {
                    let mut configured_path = path.clone();
                    let style = match path.path_style() {
                        PathStyle::Semantic => PathStyle::Plain(base_style),
                        other => other,
                    };
                    configured_path = configured_path
                        .max_width(path_budget)
                        .strategy(PathFormatStrategy::Adaptive)
                        .style(style);
                    for mut s in configured_path.to_spans(theme) {
                        if base_style.bg != nuotc::Color::Reset {
                            s.style = s.style.bg(base_style.bg);
                        }
                        spans.push(s);
                    }
                }
            }
        }

        // Pass 4: Append protected suffix
        if let Some((suffix_text, suffix_style)) = suffix {
            let mut style = suffix_style;
            if base_style.bg != nuotc::Color::Reset {
                style = style.bg(base_style.bg);
            }
            spans.push(Span::styled(suffix_text.to_string(), style));
        }

        spans
    }
}

impl fmt::Display for SemanticLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_plain_text())
    }
}

/// Distribute remaining horizontal slack between flexible text and path slots.
fn distribute_slack(
    slots: &[InlineSlot<'_>],
    available: usize,
    flex_count: usize,
    path_count: usize,
) -> (Vec<usize>, usize) {
    if flex_count == 0 && path_count == 0 {
        return (Vec::new(), available);
    }

    if path_count == 0 {
        // Only flexible texts: divide evenly
        let per_flex = available.checked_div(flex_count).unwrap_or(0);
        let budgets = vec![per_flex; flex_count];
        return (budgets, 0);
    }

    if flex_count == 0 {
        // Only path(s): divide evenly
        let per_path = available / path_count;
        return (Vec::new(), per_path);
    }

    // Both flexible text and path exist:
    // Try to satisfy flexible text desired width, but reserve at least a minimum path budget
    let min_path_reserve = 12usize.min(available);
    let slack_for_flex = available.saturating_sub(min_path_reserve);

    let mut flex_budgets = Vec::with_capacity(flex_count);
    let mut total_flex_used = 0usize;

    for slot in slots {
        if let InlineSlot::Flexible {
            text, min_width, ..
        } = slot
        {
            let desired = text.width();
            let allocated = desired.min(slack_for_flex / flex_count).max(*min_width);
            flex_budgets.push(allocated);
            total_flex_used += allocated;
        }
    }

    let remaining_for_path = available.saturating_sub(total_flex_used);
    let per_path = remaining_for_path / path_count;

    (flex_budgets, per_path)
}

#[cfg(test)]
mod tests {
    use nuotc::Modifier;
    use std::path::Path;

    use super::*;

    #[test]
    fn test_semantic_line_plaintext_lossless() {
        let line = SemanticLine::new()
            .push_fixed("Search ")
            .push_flexible("\"draw.rs\"")
            .push_fixed(" in ")
            .push_path(PathView::from_str(
                "apps/terminal/crates/mutx/src/overlays/telemetry",
            ));

        assert_eq!(
            line.to_plain_text(),
            "Search \"draw.rs\" in apps/terminal/crates/mutx/src/overlays/telemetry"
        );
        assert_eq!(
            format!("{}", line),
            "Search \"draw.rs\" in apps/terminal/crates/mutx/src/overlays/telemetry"
        );
    }

    #[test]
    fn test_semantic_line_resolve_wide_budget() {
        let theme = Theme::default();
        let line = SemanticLine::new()
            .push_fixed("Search ")
            .push_flexible("\"draw.rs\"")
            .push_fixed(" in ")
            .push_path(PathView::from_str(
                "apps/terminal/crates/mutx/src/overlays/telemetry",
            ));

        let spans = line.resolve(
            120,
            &theme,
            Style::default().add_modifier(Modifier::BOLD),
            Some((" (3ms)", Style::default())),
        );

        let full_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(full_text.ends_with(" (3ms)"));
        assert!(full_text.contains("apps/terminal/crates/mutx/src/overlays/telemetry"));
        assert!(full_text.width() <= 120);
    }

    #[test]
    fn test_semantic_line_resolve_constrained_preserves_suffix() {
        let theme = Theme::default();
        let line = SemanticLine::new()
            .push_fixed("Search ")
            .push_flexible("\"draw.rs\"")
            .push_fixed(" in ")
            .push_path(PathView::from_str(
                "apps/terminal/crates/mutx/src/overlays/telemetry",
            ));

        // Constrained width: 50 columns
        let spans = line.resolve(
            50,
            &theme,
            Style::default().add_modifier(Modifier::BOLD),
            Some((" (3ms)", Style::default())),
        );

        let full_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // Crucial invariant [INV-TUI-PATH-02]: suffix (3ms) must never be dropped
        assert!(
            full_text.ends_with(" (3ms)"),
            "Suffix must be preserved: got '{}'",
            full_text
        );
        // Adaptive path compression must trigger
        assert!(
            full_text.width() <= 50,
            "Width must be <= 50, got {}",
            full_text.width()
        );
        assert!(
            full_text.contains("telemetry"),
            "Leaf must be retained: got '{}'",
            full_text
        );
    }

    #[test]
    fn test_semantic_line_with_base_dir() {
        let base = Path::new("/workspace/muta");
        let line = SemanticLine::new()
            .push_fixed("Read ")
            .push_path(PathView::from_str("/workspace/muta/src/main.rs").base_dir(base));

        assert_eq!(line.to_plain_text(), "Read src/main.rs");
    }

    #[test]
    fn test_semantic_line_path_inherits_base_style_for_consistency() {
        let theme = Theme::default();
        let line = SemanticLine::new()
            .push_fixed("Search ")
            .push_flexible("\"render\"")
            .push_fixed(" in ")
            .push_path(PathView::from_str("crates/chrome/tessera-hud"));

        let base_style = Style::default().fg(theme.muted());
        let spans = line.resolve(100, &theme, base_style, None);

        for s in &spans {
            assert_eq!(
                s.style.fg, base_style.fg,
                "Span '{}' must have base style color {:?}",
                s.content, base_style.fg
            );
        }
    }
}
