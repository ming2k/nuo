//! Selection styling and option rows.
//!
//! This is the **single source of truth** for how a selectable row looks
//! across the TUI — modal lists, the question modal, inline suggestion
//! dropdowns, and preset choosers all flow through here. There are two
//! visual tones, but one set of rules:
//!
//! - [`ChoiceTone::Filled`]: the cursor fills the whole row with the brand
//!   tone (background + contrast foreground). Used by centered modal lists
//!   (config, tools, mcp, sessions, …). This is what [`super::list::row_style`]
//!   delegates to.
//! - [`ChoiceTone::Flat`]: quiet inline tone without row background fill;
//!   highlighted text uses brand tone and bold. Used where a quiet look is wanted.
//!   Flat composes cleanly with multi-row wrapped descriptions without painting behind them.
//!
//! Multi-select semantics layer on top: a checkbox marker `[x]`/`[ ]` (in the
//! [`ChoiceMarker`]) is independent of the cursor, so a row can be "checked
//! but not currently highlighted" or vice-versa.

use nuotc::{Color, Line, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::super::Theme;
use super::super::primitives::contrast_fg;
use super::super::text_layout::wrap_text;

/// Visual treatment of the cursor on a selectable row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChoiceTone {
    /// Quiet inline tone without row background fill; highlighted text uses
    /// brand tone and bold. Composes cleanly with multi-line wrapped
    /// descriptions and stays quiet in dense surfaces.
    #[default]
    Flat,
    /// The whole row is filled with the brand tone (brand background, contrast
    /// foreground). Used by centered modal lists where each row is a single
    /// selectable line.
    Filled,
}

/// Whether a row carries a selection marker (independent of the cursor) and
/// which glyph set it uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChoiceMarker {
    /// No marker at all — single-select where the highlight *is* the selection
    /// (e.g. the question modal's single-select mode).
    #[default]
    None,
    /// A checkbox `[x]`/`[ ]` reflecting `selected`.
    Checkbox,
}

/// The resolved foreground/background palette for one selectable row, derived
/// once from the cursor / selection state so every span in the row paints
/// consistently. All bespoke row builders should read from this rather than
/// re-deriving colors.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChoiceStyle {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
}

impl ChoiceStyle {
    /// The marker glyph this row should render (already pre-styled via
    /// [`Self::marker_style`]). Empty for [`ChoiceMarker::None`].
    pub fn marker_glyph(self, marker: ChoiceMarker, selected: bool) -> &'static str {
        match marker {
            ChoiceMarker::Checkbox => {
                if selected {
                    "[x]"
                } else {
                    "[ ]"
                }
            }
            ChoiceMarker::None => "",
        }
    }

    /// Style for the marker glyph itself.
    pub fn marker_style(
        self,
        marker: ChoiceMarker,
        selected: bool,
        highlighted: bool,
        theme: &Theme,
    ) -> Style {
        match marker {
            ChoiceMarker::Checkbox => {
                if selected {
                    Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD)
                } else if highlighted {
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted())
                }
            }
            // No marker: no glyph to style.
            ChoiceMarker::None => Style::default().fg(self.dim),
        }
    }
}

/// The marker prefix's display width for a given marker/selection pair —
/// the layout twin of [`ChoiceStyle::marker_glyph`], for callers that must
/// measure a row's wrap budget before the row's style exists.
pub(crate) fn style_marker_width(marker: ChoiceMarker, selected: bool) -> usize {
    match marker {
        ChoiceMarker::Checkbox => {
            if selected {
                "[x] ".width()
            } else {
                "[ ] ".width()
            }
        }
        ChoiceMarker::None => 0,
    }
}

/// Resolve the row palette from a tone + cursor flag. This is the one function
/// every selectable surface routes through.
pub(crate) fn choice_style(tone: ChoiceTone, highlighted: bool, theme: &Theme) -> ChoiceStyle {
    let (bg, fg, dim) = match tone {
        ChoiceTone::Filled => {
            if highlighted {
                (
                    theme.brand(),
                    contrast_fg(theme.brand()),
                    contrast_fg(theme.brand()),
                )
            } else {
                (theme.panel(), theme.fg(), theme.muted())
            }
        }
        ChoiceTone::Flat => {
            if highlighted {
                (theme.panel(), theme.brand(), theme.brand())
            } else {
                (theme.panel(), theme.fg(), theme.dim())
            }
        }
    };
    ChoiceStyle { bg, fg, dim }
}

/// A selectable option row with an optional description. Wraps to `body_width`
/// (a description may occupy several lines). Pushes 1+ [`Line`]s into `lines`.
///
/// This is what the question modal and any future wrapped-choice surface use.
/// For single-line, columnar modal rows (config, tools, …) read the palette
/// from [`choice_style`] and build the spans directly — that path keeps the
/// *colors* centralized even when the *column layout* is bespoke.
pub(crate) struct ChoiceOptionRow<'a> {
    pub label: &'a str,
    pub description: Option<&'a str>,
    pub selected: bool,
    pub highlighted: bool,
    pub tone: ChoiceTone,
    pub marker: ChoiceMarker,
}

impl<'a> ChoiceOptionRow<'a> {
    /// How many body rows this row will occupy when pushed at `body_width`
    /// — the same wrap passes `push_lines` performs, without building the
    /// spans. Callers that must size their frame before building the body
    /// (the question sheet grows upward into the transcript) measure the
    /// demand here instead of rendering a throwaway copy.
    pub(crate) fn measure_lines(&self, body_width: usize) -> usize {
        let marker_w = style_marker_width(self.marker, self.selected);
        let wrap_width = body_width.saturating_sub(marker_w).max(1);
        let mut rows = wrap_text(self.label, wrap_width).len().max(1);
        if let Some(desc) = self.description {
            rows += wrap_text(desc, body_width.saturating_sub(marker_w).max(1))
                .len()
                .max(1);
        }
        rows
    }

    pub(crate) fn push_lines(
        self,
        lines: &mut Vec<Line<'static>>,
        body_width: usize,
        theme: &Theme,
    ) {
        let style = choice_style(self.tone, self.highlighted, theme);

        // Prefix layout: the marker glyph plus a gap on the first line, a
        // matching continuation indent so wrapped label lines line up under
        // the label text (labels start at column 0, aligned with the
        // question/header text above).
        let marker_glyph = style.marker_glyph(self.marker, self.selected);
        let is_filled = self.highlighted && self.tone == ChoiceTone::Filled;
        let mut marker_style = style.marker_style(self.marker, self.selected, self.highlighted, theme);
        if is_filled {
            marker_style = marker_style.bg(style.bg).fg(style.fg);
        }

        let text_style = if is_filled {
            Style::default()
                .bg(style.bg)
                .fg(style.fg)
                .add_modifier(Modifier::BOLD)
        } else if self.highlighted {
            Style::default().fg(style.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(style.fg)
        };

        let first_prefix = format!("{} ", marker_glyph);
        // Continuation rows indent by the marker's display width so wrapped
        // label lines line up under the label text, not under the marker.
        let continuation_prefix = " ".repeat(first_prefix.width());
        push_wrapped_styled_with_prefix_style(
            lines,
            &first_prefix,
            &continuation_prefix,
            self.label,
            marker_style,
            text_style,
            body_width,
        );

        if let Some(desc) = self.description {
            let desc_style = if is_filled {
                Style::default().bg(style.bg).fg(style.dim)
            } else {
                Style::default().fg(style.dim)
            };
            // Description indents under the label (same marker width) so it
            // reads as the row's second line, aligned with the label text.
            let desc_indent = " ".repeat(first_prefix.width());
            push_wrapped_styled(
                lines,
                desc_indent.as_str(),
                desc_indent.as_str(),
                desc,
                desc_style,
                body_width,
            );
        }
    }
}

// wrap helpers (shared; re-exported so callers don't redefine them)

/// Push wrapped `text` whose first line carries a *separately styled* prefix
/// (e.g. a checkbox) and whose continuation lines use a plain indent.
fn push_wrapped_styled_with_prefix_style(
    lines: &mut Vec<Line<'static>>,
    first_prefix: &str,
    continuation_prefix: &str,
    text: &str,
    first_prefix_style: Style,
    text_style: Style,
    body_width: usize,
) {
    let first_width = first_prefix.width();
    let continuation_width = continuation_prefix.width();
    let wrap_width = body_width
        .saturating_sub(first_width.max(continuation_width))
        .max(1);
    let wrapped = wrap_text(text, wrap_width);
    if wrapped.is_empty() {
        let mut spans = vec![Span::styled(
            first_prefix.to_string(),
            first_prefix_style,
        )];
        if text_style.bg != Color::Reset {
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            if body_width > used {
                spans.push(Span::styled(" ".repeat(body_width - used), Style::default().bg(text_style.bg)));
            }
        }
        lines.push(Line::from(spans));
        return;
    }

    let cont_prefix_style = if text_style.bg != Color::Reset {
        Style::default().bg(text_style.bg)
    } else {
        Style::default()
    };

    for (idx, wrapped_line) in wrapped.into_iter().enumerate() {
        let mut spans = if idx == 0 {
            vec![
                Span::styled(first_prefix.to_string(), first_prefix_style),
                Span::styled(wrapped_line.text, text_style),
            ]
        } else {
            vec![
                Span::styled(continuation_prefix.to_string(), cont_prefix_style),
                Span::styled(wrapped_line.text, text_style),
            ]
        };
        if text_style.bg != Color::Reset {
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            if body_width > used {
                spans.push(Span::styled(" ".repeat(body_width - used), Style::default().bg(text_style.bg)));
            }
        }
        lines.push(Line::from(spans));
    }
}

/// Push wrapped `text` under a single style, with a first-line prefix and a
/// (typically equal-width) continuation prefix. Public to the render crate so
/// the permission sheet and other bodies stop re-defining their own copy.
pub(crate) fn push_wrapped_styled(
    lines: &mut Vec<Line<'static>>,
    first_prefix: &str,
    continuation_prefix: &str,
    text: &str,
    style: Style,
    body_width: usize,
) {
    let first_width = first_prefix.width();
    let continuation_width = continuation_prefix.width();
    let wrap_width = body_width
        .saturating_sub(first_width.max(continuation_width))
        .max(1);
    let wrapped = wrap_text(text, wrap_width);
    if wrapped.is_empty() {
        return;
    }

    let prefix_style = if style.bg != Color::Reset {
        Style::default().bg(style.bg)
    } else {
        Style::default()
    };

    for (idx, wrapped_line) in wrapped.into_iter().enumerate() {
        let prefix = if idx == 0 {
            first_prefix
        } else {
            continuation_prefix
        };
        let mut spans = vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled(wrapped_line.text, style),
        ];
        if style.bg != Color::Reset {
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            if body_width > used {
                spans.push(Span::styled(" ".repeat(body_width - used), Style::default().bg(style.bg)));
            }
        }
        lines.push(Line::from(spans));
    }
}
