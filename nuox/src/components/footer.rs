//! Width-aware modal footer hints.
//!
//! Each hint carries a numeric priority. When the footer is too narrow to show
//! everything, lower-priority items are dropped first and remaining labels are
//! compacted to keys only.

use nuotc::{Frame, Line, Paragraph, Rect, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::Theme;
use super::keycap::keycap_style;

/// Coarse priority band for a footer hint. Higher-ranked variants survive
/// longer under width pressure.
///
/// This enum covers the common cases. For a finer-grained priority (e.g. a
/// destructive action that must outlive a plain secondary), use
/// [`FooterHint::with_band`] with an explicit numeric band.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FooterPriority {
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
pub(crate) struct FooterHint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: FooterPriority,
}

impl FooterHint {
    pub(crate) const fn key_always(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::always(key.display(), label)
    }

    pub(crate) const fn key_primary(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::primary(key.display(), label)
    }

    #[allow(dead_code)]
    pub(crate) const fn key_navigation(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::navigation(key.display(), label)
    }

    pub(crate) const fn key_secondary(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::secondary(key.display(), label)
    }

    #[allow(dead_code)]
    pub(crate) const fn key_with_band(
        key: crate::keymap::Key,
        label: &'static str,
        band: u8,
    ) -> FooterHintWithBand {
        Self::with_band(key.display(), label, band)
    }

    pub(crate) const fn always(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Always,
        }
    }

    pub(crate) const fn primary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Primary,
        }
    }

    pub(crate) const fn navigation(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Navigation,
        }
    }

    pub(crate) const fn secondary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Secondary,
        }
    }

    /// Constructor for a hint whose priority falls between the coarse bands.
    /// The numeric `band` is compared against [`FooterPriority::rank`] bands:
    /// pass `70` to sit between Primary (80) and Navigation (60) — the right
    /// spot for a destructive action (`D delete`) that should outlive plain
    /// secondaries but not the always-keep `Esc`.
    pub(crate) const fn with_band(
        key: &'static str,
        label: &'static str,
        band: u8,
    ) -> FooterHintWithBand {
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
pub(crate) struct FooterHintWithBand {
    pub key: &'static str,
    pub label: &'static str,
    pub rank: u8,
}

/// Internal: every hint flattened to a (key, label, rank) row, so the layout
/// can sort/drop uniformly regardless of enum-vs-custom origin.
struct RankedHint {
    key: &'static str,
    label: &'static str,
    rank: u8,
    /// Original index in the caller's order, for stable display ordering.
    order: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FooterLabelMode {
    Full,
    Compact,
}

/// Render the one-line modal command strip with width-aware degradation.
pub(crate) fn render_modal_footer(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    theme: &Theme,
) {
    render_footer_impl(frame, rect, hints, &[], theme);
}

/// Like [`render_modal_footer`], but accepts an extra slice of custom-band
/// hints (e.g. `D delete` at band 70).
pub(crate) fn render_modal_footer_with_extra(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    theme: &Theme,
) {
    render_footer_impl(frame, rect, hints, extra, theme);
}

/// Build the footer text for `width`. Used by tests and modals that only need
/// the string.
pub(crate) fn modal_footer_text(hints: &[FooterHint], width: usize) -> String {
    layout_footer(hints, &[], width).text
}

/// Build the footer text for `width`, accepting custom-band hints. (Used by
/// tests.)
#[cfg(test)]
pub(crate) fn modal_footer_text_with_extra(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
) -> String {
    layout_footer(hints, extra, width).text
}

/// A single rendered segment of the footer line. Keys are tagged so the
/// renderer can apply the unified keycap style while everything else stays
/// muted — without the layout having to know about styling.
#[derive(Clone)]
enum FooterSeg {
    /// A keyboard-key label (rendered with the keycap style).
    Key(String),
    /// Any other text: a hint label or a ` · ` separator.
    Text(String),
}

impl FooterSeg {
    fn text(&self) -> &str {
        match self {
            FooterSeg::Key(s) | FooterSeg::Text(s) => s,
        }
    }
}

/// Flatten a segment list to its plain-text form (used by the width-only /
/// test-facing string APIs, which must not change just because keys are now
/// styled differently).
fn segs_to_string(segs: &[FooterSeg]) -> String {
    segs.iter().map(|s| s.text()).collect()
}

/// Materialize the segment list as styled spans: keys take the unified keycap
/// style, hint labels take the keycap label style. This is the single place that decides how
/// footer keys look, so it can never drift from the activity bar.
fn segs_to_spans(segs: &[FooterSeg], theme: &Theme) -> Vec<Span<'static>> {
    let key_style = keycap_style(theme);
    let label_style = theme.keycap_label_style();
    segs.iter()
        .map(|seg| match seg {
            FooterSeg::Key(s) => Span::styled(s.clone(), key_style),
            FooterSeg::Text(s) => Span::styled(s.clone(), label_style),
        })
        .collect()
}

/// Result of laying out a footer for a given width.
struct FooterLayout {
    text: String,
    segs: Vec<FooterSeg>,
}

fn render_footer_impl(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    theme: &Theme,
) {
    let layout = layout_footer(hints, extra, rect.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(segs_to_spans(&layout.segs, theme))),
        rect,
    );
}

/// Lay out the footer for `width`, dropping lowest-priority hints first.
///
/// Algorithm:
/// 1. Try full labels for the entire set.
/// 2. Drop the lowest-priority hint(s) one at a time (still full labels).
/// 3. Compact remaining (keys only), same drop ladder.
/// 4. Last resort: the always-keep keys compact, truncated to fit.
fn layout_footer(hints: &[FooterHint], extra: &[FooterHintWithBand], width: usize) -> FooterLayout {
    if width == 0 || hints.is_empty() && extra.is_empty() {
        return FooterLayout {
            text: String::new(),
            segs: Vec::new(),
        };
    }

    // Flatten to ranked rows in stable display order (hints, then extras).
    let ranked: Vec<RankedHint> = hints
        .iter()
        .enumerate()
        .map(|(i, h)| RankedHint {
            key: h.key,
            label: h.label,
            rank: h.priority.rank(),
            order: i,
        })
        .chain(extra.iter().enumerate().map(|(i, h)| RankedHint {
            key: h.key,
            label: h.label,
            rank: h.rank,
            order: hints.len() + i,
        }))
        .collect();

    // Drop order: lowest rank first; tiebreak by later display order so a
    // trailing secondary goes before an earlier one of equal rank.
    let mut drop_order: Vec<usize> = (0..ranked.len()).collect();
    drop_order.sort_by_key(|&i| (ranked[i].rank, usize::MAX - ranked[i].order));

    // Wrap a segment list into a complete FooterLayout (text mirrors the
    // segments so the width-only / test APIs stay byte-identical to before).
    let finish = |segs: Vec<FooterSeg>| FooterLayout {
        text: segs_to_string(&segs),
        segs,
    };

    // Pass 1: full labels, progressively dropping lowest-priority items.
    for drop_count in 0..=ranked.len().saturating_sub(1) {
        if let Some(segs) = try_subset(
            &ranked,
            &drop_order,
            drop_count,
            FooterLabelMode::Full,
            width,
        ) {
            return finish(segs);
        }
    }

    // Pass 2: compact (keys only), same drop ladder.
    for drop_count in 0..=ranked.len().saturating_sub(1) {
        if let Some(segs) = try_subset(
            &ranked,
            &drop_order,
            drop_count,
            FooterLabelMode::Compact,
            width,
        ) {
            return finish(segs);
        }
    }

    // Last resort: Always keys only, compact, truncated to the available width.
    let always: Vec<&RankedHint> = ranked.iter().filter(|r| r.rank >= 100).collect();
    let base_set = if always.is_empty() {
        ranked.iter().collect()
    } else {
        always
    };
    let base = join_hints(&base_set, FooterLabelMode::Compact);

    let text = segs_to_string(&base);
    let segs = if text.width() <= width {
        base
    } else {
        only_text_truncated(&text, width)
    };
    finish(segs)
}

/// A segment list that is just one plain-text run.
fn only_text(s: &str) -> Vec<FooterSeg> {
    vec![FooterSeg::Text(s.to_string())]
}

/// A segment list that is one plain-text run, truncated to `max` cells.
fn only_text_truncated(s: &str, max: usize) -> Vec<FooterSeg> {
    only_text(&truncate_to_width(s, max))
}

/// Try a subset (dropping the first `drop_count` lowest-priority rows) for the
/// given label mode. Returns the segment list if it fits, else None.
fn try_subset(
    ranked: &[RankedHint],
    drop_order: &[usize],
    drop_count: usize,
    mode: FooterLabelMode,
    width: usize,
) -> Option<Vec<FooterSeg>> {
    let dropped: std::collections::HashSet<usize> =
        drop_order.iter().take(drop_count).copied().collect();
    let subset: Vec<&RankedHint> = ranked
        .iter()
        .enumerate()
        .filter(|(i, _)| !dropped.contains(i))
        .map(|(_, r)| r)
        .collect();
    if subset.is_empty() {
        return None;
    }
    let base = join_hints(&subset, mode);
    if base.is_empty() {
        return None;
    }
    let text = segs_to_string(&base);
    (text.width() <= width).then_some(base)
}

/// Join hints in stable display order (`order`) for a given label mode into a
/// segment list. Keys are tagged `FooterSeg::Key` so the renderer can apply the
/// keycap style; separators and labels are `Text`.
///
/// Each hint is a same-rank peer affordance (R2), so hints are separated by
/// plain whitespace — no `·` (which the join ladder reserves for the
/// keycap → label modification inside each hint).
fn join_hints(hints: &[&RankedHint], mode: FooterLabelMode) -> Vec<FooterSeg> {
    let mut ordered: Vec<&&RankedHint> = hints.iter().collect();
    ordered.sort_by_key(|r| r.order);
    let mut segs: Vec<FooterSeg> = Vec::new();
    for (idx, hint) in ordered.iter().enumerate() {
        if idx > 0 {
            segs.push(FooterSeg::Text(
                " ".repeat(super::super::design::JOIN_ENUMERATE_COLS),
            ));
        }
        segs.push(FooterSeg::Key(hint.key.to_string()));
        if let FooterLabelMode::Full = mode
            && !hint.label.is_empty()
        {
            segs.push(FooterSeg::Text(format!(" {}", hint.label)));
        }
    }
    segs
}

fn truncate_to_width(s: &str, max: usize) -> String {
    if s.width() <= max && !s.contains(['\n', '\r']) {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        if c == '\n' || c == '\r' {
            break;
        }
        let cw = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
        if width + cw > max - 1 {
            break;
        }
        out.push(c);
        width += cw;
    }
    out.push('…');
    out
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
    fn full_width_keeps_every_label() {
        let hints = sample_hints();
        let text = modal_footer_text_with_extra(&hints, &[], 80);
        // R2: same-rank peer affordances are separated by plain whitespace
        // (JOIN_ENUMERATE_COLS), not the `·` reserved for key→label joins.
        assert_eq!(
            text,
            "type filter  ↑↓ navigate  Enter activate  * favorite  Esc close"
        );
        assert!(!text.contains('·'));
        assert!(!text.contains('?'));
    }

    #[test]
    fn collapsed_strip_never_emits_help_chip() {
        // The `? help` chip is gone: however narrow the footer gets, it must
        // never reappear, and the strip must always fit the given width.
        let hints = sample_hints();
        let full = modal_footer_text_with_extra(&hints, &[], 80);
        for width in 1..=full.width() {
            let text = modal_footer_text_with_extra(&hints, &[], width);
            assert!(
                !text.contains('?'),
                "width {width}: collapsed footer must not show the help chip, got {text:?}"
            );
            assert!(
                text.width() <= width,
                "width {width}: footer overflowed, got {text:?}"
            );
        }
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
        let text = modal_footer_text_with_extra(&hints, &extra, 44);
        assert!(text.contains('D'), "band-70 D must survive: {text:?}");
        assert!(!text.contains('*'), "band-40 * should drop first: {text:?}");
    }

    #[test]
    fn default_path_never_appends_more() {
        // modal_footer_text must never append `?`.
        let hints = sample_hints();
        let mid = modal_footer_text(&hints, 40);
        assert!(!mid.contains('?'));
    }
}
