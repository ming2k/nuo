//! Width-aware modal footer hints.
//!
//! Each hint carries a numeric priority. When the footer is too narrow to show
//! everything, lower-priority items are dropped first and remaining labels are
//! compacted to keys only.

use nuotc::{Frame, Line, Paragraph, Rect, Span};

use super::super::Theme;
use super::keycap::AffordanceIntent;

/// Coarse priority band for a footer hint. Higher-ranked variants survive
/// longer under width pressure.
///
/// This enum covers the common cases. For a finer-grained priority (e.g. a
/// destructive action that must outlive a plain secondary), use
/// [`FooterHint::with_band`] with an explicit numeric band.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FooterPriority {
    /// Dismiss / cancel (`Esc`). Highest survival.
    Always,
    /// Primary confirm / activate action (`Enter`, destructive actions).
    Primary,
    /// Cursor / list navigation (`↑↓`).
    Navigation,
    /// Secondary or uncommon actions. First to collapse.
    Secondary,
}

impl FooterPriority {
    /// Numeric rank for sorting: higher = kept longer under width pressure.
    fn rank(self) -> u8 {
        match self {
            FooterPriority::Always => 100,
            FooterPriority::Primary => 80,
            FooterPriority::Navigation => 60,
            FooterPriority::Secondary => 40,
        }
    }
}

#[derive(Clone, Copy)]
pub struct FooterHint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: FooterPriority,
}

impl FooterHint {
    #[allow(dead_code)]
    pub const fn always(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Always,
        }
    }

    pub const fn primary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Primary,
        }
    }

    pub const fn navigation(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Navigation,
        }
    }

    pub const fn secondary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Secondary,
        }
    }

    /// Convert this footer hint to an ADR-0054 [`crate::components::keycap::KeyAffordance`].
    #[allow(dead_code)]
    pub fn to_key_affordance(self) -> super::keycap::KeyAffordance {
        let intent = match self.priority {
            FooterPriority::Primary => AffordanceIntent::Primary,
            _ => AffordanceIntent::Neutral,
        };
        super::keycap::KeyAffordance::with_intent_and_rank(
            self.key,
            self.label,
            intent,
            self.priority.rank(),
        )
    }

    /// Constructor for a hint whose priority falls between the coarse bands.
    /// The numeric `band` is compared against `FooterPriority::rank` bands:
    /// pass `70` to sit between Primary (80) and Navigation (60) — the right
    /// spot for a destructive action (`D delete`) that should outlive plain
    /// secondaries but not the always-keep `Esc`.
    pub const fn with_band(key: &'static str, label: &'static str, band: u8) -> FooterHintWithBand {
        FooterHintWithBand {
            key,
            label,
            rank: band,
        }
    }
}

/// A footer hint with an explicit numeric priority, produced by
/// [`FooterHint::with_band`]. Renders identically to [`FooterHint`]; the only
/// difference is the custom rank used for collapse ordering. Convertible into
/// the ranked slice the layout consumes.
#[derive(Clone, Copy)]
pub struct FooterHintWithBand {
    pub key: &'static str,
    pub label: &'static str,
    pub rank: u8,
}

/// Render the one-line modal command strip with width-aware degradation.
pub fn render_modal_footer(frame: &mut Frame, rect: Rect, hints: &[FooterHint], theme: &Theme) {
    render_footer_impl(frame, rect, hints, &[], theme);
}

/// Like [`render_modal_footer`], but accepts an extra slice of custom-band
/// hints (e.g. `D delete` at band 70).
pub fn render_modal_footer_with_extra(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    theme: &Theme,
) {
    render_footer_impl(frame, rect, hints, extra, theme);
}

/// Result of laying out a footer for a given width (ADR-0054).
struct FooterLayout {
    text: String,
    spans: Vec<Span<'static>>,
}

fn render_footer_impl(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    theme: &Theme,
) {
    let layout = layout_footer_with_theme(hints, extra, rect.width as usize, theme);
    frame.render_widget(&mut Paragraph::new(Line::from(layout.spans)), rect);
}

/// Build the footer text for `width`.
pub fn modal_footer_text(hints: &[FooterHint], width: usize) -> String {
    layout_footer(hints, &[], width).text
}

/// Build the footer text for `width`, accepting custom-band hints.
#[cfg(test)]
pub(crate) fn modal_footer_text_with_extra(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
) -> String {
    layout_footer(hints, extra, width).text
}

fn layout_footer(hints: &[FooterHint], extra: &[FooterHintWithBand], width: usize) -> FooterLayout {
    let theme = Theme::default();
    layout_footer_with_theme(hints, extra, width, &theme)
}

fn layout_footer_with_theme(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
    theme: &Theme,
) -> FooterLayout {
    if width == 0 || (hints.is_empty() && extra.is_empty()) {
        return FooterLayout {
            text: String::new(),
            spans: Vec::new(),
        };
    }

    let mut affordances = Vec::with_capacity(hints.len() + extra.len());
    for h in hints {
        affordances.push(h.to_key_affordance());
    }
    for e in extra {
        let intent = if e.label.contains("delete") || e.label.contains("clear") {
            AffordanceIntent::Warn
        } else {
            AffordanceIntent::Neutral
        };
        affordances.push(super::keycap::KeyAffordance::with_intent_and_rank(
            e.key, e.label, intent, e.rank,
        ));
    }

    let layout = super::keycap::layout_affordances(
        &affordances,
        width,
        theme,
        nuotc::Color::Reset,
        Some("more"),
    );

    let text: String = layout.spans.iter().map(|s| s.content.as_ref()).collect();
    FooterLayout {
        text,
        spans: layout.spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hints() -> [FooterHint; 5] {
        [
            FooterHint::secondary("type", "filter"),
            FooterHint::navigation("↑↓", "navigate"),
            FooterHint::primary("Enter", "activate"),
            FooterHint::secondary("*", "favorite"),
            FooterHint::always("Esc", "close"),
        ]
    }

    #[test]
    fn footer_spans_apply_primary_accent_color() {
        let theme = Theme::default();
        let hints = [
            FooterHint::primary("Enter", "activate"),
            FooterHint::always("Esc", "close"),
        ];
        let layout = layout_footer_with_theme(&hints, &[], 80, &theme);
        // First key is "Enter", should have keycap_accent fg + BOLD
        assert_eq!(layout.spans[0].content, "Enter");
        assert_eq!(layout.spans[0].style.fg, theme.keycap_accent());
        assert!(layout.spans[0].style.add.contains(nuotc::Modifier::BOLD));
        // Second span is " activate", should have keycap_label fg
        assert_eq!(layout.spans[1].content, " activate");
        assert_eq!(layout.spans[1].style.fg, theme.keycap_label());
    }

    #[test]
    fn full_width_keeps_every_label() {
        let hints = sample_hints();
        let text = modal_footer_text_with_extra(&hints, &[], 80);
        assert_eq!(
            text,
            "type filter  ↑↓ navigate  Enter activate  * favorite  Esc close"
        );
    }

    #[test]
    fn collapsed_strip_emits_f1_more_under_width_pressure() {
        // ADR-0054 Floor-1 rule: under width pressure, lower-priority hints are
        // collapsed and the final slot is reserved for `F1 more`.
        let hints = sample_hints();
        let text = modal_footer_text_with_extra(&hints, &[], 40);
        assert!(
            text.contains("F1 more"),
            "width-constrained footer must show F1 more trigger, got: {text:?}"
        );
    }

    #[test]
    fn custom_band_protects_destructive_action() {
        // `D delete` at band 70 must outlive a secondary `*` at band 40.
        let hints = [
            FooterHint::navigation("↑↓", "navigate"),
            FooterHint::primary("Enter", "select"),
            FooterHint::secondary("*", "favorite"),
            FooterHint::always("Esc", "close"),
        ];
        let extra = [FooterHint::with_band("D", "delete", 70)];
        let text = modal_footer_text_with_extra(&hints, &extra, 48);
        assert!(text.contains('D'), "band-70 D must survive: {text:?}");
        assert!(!text.contains('*'), "band-40 * should drop first: {text:?}");
        assert!(
            text.contains("F1 more"),
            "overflow must reserve F1 more: {text:?}"
        );
    }
}

#[test]
fn modal_footer_degrades_by_width_and_priority() {
    let hints = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation("↑↓", "navigate"),
        FooterHint::primary("Enter", "activate"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::always("Esc", "close"),
    ];

    // Full width: every label kept.
    assert_eq!(
        modal_footer_text(&hints, 80),
        "type filter  ↑↓ navigate  Enter activate  * favorite  Esc close"
    );
    // ADR-0054 Floor-1 rule: width pressure allocates the final slot to F1 more
    let mid = modal_footer_text(&hints, 35);
    assert!(
        mid.contains("F1 more"),
        "narrow width reserves F1 more: {mid}"
    );
}
