//! One-line metadata strips shared by transcript chrome.
//!
//! A [`MetaStrip`] is a horizontal rail made of small replaceable [`MetaChip`]s:
//! anchors (`round 61`, `turn 61`), status chips (`⏸ Queued`), and muted
//! details (`GLM-5.2`) and an optional right-aligned time (`19:46`). It
//! centralizes the repeated two-tone metadata treatment used by assistant
//! turn headers and sent user-message headers.

use std::borrow::Cow;

use nuotc::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::render::Theme;
use crate::text_layout::padded_tail;

/// Semantic visual tone for a metadata chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetaTone {
    /// Info-tone, bold primary anchor (`round N`, `turn N`, `steer`, `follow-up`).
    Accent,
    /// Secondary metadata (`model`, `HH:MM`, fallback labels, upright status).
    Muted,
    /// Status tone for pending or highlight states (upright text).
    Status,
    /// Warning tone for cancelled or interrupted states.
    Warn,
}

impl MetaTone {
    fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Accent => Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
            Self::Muted => Style::default().fg(theme.muted()),
            Self::Status => Style::default().fg(theme.info()),
            Self::Warn => Style::default().fg(theme.warn()),
        }
    }
}

/// A replaceable piece of metadata inside a [`MetaStrip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MetaChip<'a> {
    text: Cow<'a, str>,
    tone: MetaTone,
    separated: bool,
}

impl<'a> MetaChip<'a> {
    fn new(text: impl Into<Cow<'a, str>>, tone: MetaTone, separated: bool) -> Self {
        Self {
            text: text.into(),
            tone,
            separated,
        }
    }
}

/// A one-row metadata strip with optional left padding and background tail fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetaStrip<'a> {
    chips: Vec<MetaChip<'a>>,
    trailing: Option<MetaChip<'a>>,
    separator: Cow<'a, str>,
    left_pad_cols: usize,
    fill_bg: Option<Color>,
}

impl<'a> Default for MetaStrip<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> MetaStrip<'a> {
    pub(crate) fn new() -> Self {
        Self {
            chips: Vec::new(),
            trailing: None,
            // R2 enumeration: metadata chips are separated by plain whitespace.
            separator: Cow::Borrowed("  "),
            left_pad_cols: 0,
            fill_bg: None,
        }
    }

    /// Reserve plain left padding before the first chip. When [`Self::fill_tail`]
    /// is set, the padding uses the same background as the tail.
    pub(crate) fn left_pad(mut self, cols: usize) -> Self {
        self.left_pad_cols = cols;
        self
    }

    /// Fill the remaining row with spaces on `bg`, useful for header rows that
    /// should explicitly repaint their whole surface.
    pub(crate) fn fill_tail(mut self, bg: Color) -> Self {
        self.fill_bg = Some(bg);
        self
    }

    /// Override the inter-chip separator (defaults to two spaces).
    pub(crate) fn separator(mut self, sep: impl Into<Cow<'a, str>>) -> Self {
        self.separator = sep.into();
        self
    }

    /// Add an arbitrary chip with no automatic separator.
    pub(crate) fn chip(mut self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, tone, false));
        }
        self
    }

    /// Add a leading chip, usually an icon/prefix (`◆ `).
    pub(crate) fn lead(self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        self.chip(text, tone)
    }

    /// Add the primary info-tone anchor (`round N`, `turn N`).
    pub(crate) fn anchor(self, text: impl Into<Cow<'a, str>>) -> Self {
        self.chip(text, MetaTone::Accent)
    }

    /// Add an upright status chip such as `queued` or `held for next round`.
    pub(crate) fn status(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, MetaTone::Status, true));
        }
        self
    }

    /// Add an upright status chip with a specific tone (e.g. `Warn` for cancelled).
    pub(crate) fn status_toned(mut self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, tone, true));
        }
        self
    }

    /// Add muted trailing metadata. The separator is inserted only when a
    /// previous visible chip already exists, so detail-only strips degrade
    /// cleanly (`Sent`, not ` · Sent`).
    pub(crate) fn detail(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, MetaTone::Muted, true));
        }
        self
    }

    /// Add secondary metadata pinned to the right edge when it fits. This is
    /// intended for timestamps: time is scan context, not another member of
    /// the left-hand identity cluster. At narrow widths it falls back to the
    /// normal inter-chip separator instead of hiding information.
    pub(crate) fn trailing_detail(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.trailing = Some(MetaChip::new(text, MetaTone::Muted, true));
        }
        self
    }

    /// Render the strip into a one-row rect.
    pub(crate) fn render(self, frame: &mut Frame, rect: Rect, theme: &Theme) {
        let line = self.into_line(rect.width as usize, theme);
        frame.render_widget(Paragraph::new(line), rect);
    }

    fn into_line(self, full_width: usize, theme: &Theme) -> Line<'a> {
        let fill_style = self
            .fill_bg
            .map(|bg| Style::default().bg(bg))
            .unwrap_or_default();
        let mut spans = Vec::with_capacity(self.chips.len().saturating_mul(2) + 4);
        let mut used = 0usize;
        let mut rendered_any = false;

        if self.left_pad_cols > 0 {
            spans.push(Span::styled(" ".repeat(self.left_pad_cols), fill_style));
            used += self.left_pad_cols;
        }

        for chip in self.chips {
            if chip.separated && rendered_any {
                used += self.separator.as_ref().width();
                spans.push(Span::styled(
                    self.separator.to_string(),
                    MetaTone::Muted.style(theme),
                ));
            }
            used += chip.text.as_ref().width();
            spans.push(Span::styled(chip.text, chip.tone.style(theme)));
            rendered_any = true;
        }

        if let Some(chip) = self.trailing {
            let trailing_width = chip.text.as_ref().width();
            let separator_width = if chip.separated && rendered_any {
                self.separator.as_ref().width()
            } else {
                0
            };
            if rendered_any && used + separator_width + trailing_width <= full_width {
                let gap = full_width.saturating_sub(used + trailing_width);
                spans.push(Span::styled(" ".repeat(gap), fill_style));
                used += gap;
            } else if chip.separated && rendered_any {
                spans.push(Span::styled(
                    self.separator.to_string(),
                    MetaTone::Muted.style(theme),
                ));
                used += separator_width;
            }
            used += trailing_width;
            spans.push(Span::styled(chip.text, chip.tone.style(theme)));
        }

        if self.fill_bg.is_some() {
            spans.push(Span::styled(padded_tail(full_width, used), fill_style));
        }

        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_separator_uses_whitespace() {
        let theme = Theme::default();
        let strip = MetaStrip::new()
            .lead("> ", MetaTone::Accent)
            .anchor("turn 1")
            .detail("detail 1")
            .detail("detail 2");
        let line = strip.into_line(80, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "> turn 1  detail 1  detail 2");
    }

    #[test]
    fn custom_separator_joins_with_custom_string() {
        let theme = Theme::default();
        let strip = MetaStrip::new()
            .separator(" | ")
            .lead("> ", MetaTone::Accent)
            .anchor("turn 13")
            .detail("glm-5.3 xhigh")
            .detail("13:51");
        let line = strip.into_line(80, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "> turn 13 | glm-5.3 xhigh | 13:51");
    }

    #[test]
    fn trailing_detail_is_right_aligned() {
        let theme = Theme::default();
        let strip = MetaStrip::new()
            .anchor("turn 13")
            .detail("glm-5.3 (xhigh)")
            .trailing_detail("13:51");
        let line = strip.into_line(40, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.width(), 40);
        assert!(text.starts_with("turn 13  glm-5.3 (xhigh)"));
        assert!(text.ends_with("13:51"));
    }
}
