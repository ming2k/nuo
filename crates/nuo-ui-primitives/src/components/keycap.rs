//! Unified keyboard-key ("keycap") styling and affordances.
//!
//! Every surface that shows a keybinding label to the user — the activity-bar
//! interrupt hint, the Help modal rows, the in-modal keymap page, the header
//! hint strips, and the footer hint bar — routes through here so there is a
//! single, consistent, theme-driven affordance across the app.
//!
//! Visual hierarchy (Visual Language R0 & ADR-0054):
//! - Keycaps use high-contrast neutral or semantic tones + BOLD:
//!   - Primary/Action: `theme.keycap_accent()` + BOLD (e.g. `Enter send`).
//!   - Neutral/Navigation: `theme.keycap_fg()` + BOLD (e.g. `Tab`, `↑↓`, `Esc`).
//!   - Warning/Interrupt: `theme.keycap_warn()` + BOLD (e.g. `Esc Esc interrupt`).
//! - Action labels use dedicated subdued silver/dark gray tones (`theme.keycap_label()`).
//! - Affordance pairs render as `[Key (Bold)]` + ` ` + `[Label (Subdued)]` with zero bracket noise.
//! - Restricted-width groups adapt deterministically via the floor-1 overflow rule:
//!   `floor(max_fit) - 1` items are displayed, reserving the final slot for `F1 more`.

use nuotc::{Color, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::super::Theme;

/// Semantic intent governing keycap color hierarchy (ADR-0054).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AffordanceIntent {
    /// Action submit / primary action (`Enter`, `Tab`). High contrast accent color.
    Primary,
    /// Standard keycap neutral (`Esc`, `↑↓`, `F1`).
    #[default]
    Neutral,
    /// Alert / warning / interrupt (`Esc Esc`, `Ctrl+C`, `Del`).
    Warn,
}

/// The single, theme-aware style applied to standard keycap labels.
pub fn keycap_style(theme: &Theme) -> Style {
    theme.keycap_style()
}

#[allow(dead_code)]
pub fn keycap_accent_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.keycap_accent())
        .add_modifier(Modifier::BOLD)
}

pub fn keycap_warn_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.keycap_warn())
        .add_modifier(Modifier::BOLD)
}

/// A styled keycap span for `text`.
pub fn keycap_span<'a>(theme: &Theme, text: &str) -> Span<'a> {
    Span::styled(text.to_string(), keycap_style(theme))
}

/// A styled warn keycap span for `text`.
pub fn keycap_warn_span<'a>(theme: &Theme, text: &str) -> Span<'a> {
    Span::styled(text.to_string(), keycap_warn_style(theme))
}

/// An atomic keycap + action label pair, strictly adhering to ADR-0054 visual grammar.
///
/// An affordance joins a keycap with its action label (e.g. `Ctrl+X menu`,
/// `Enter send`, `Esc back`). The key and label must never be empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyAffordance {
    pub key: &'static str,
    pub label: &'static str,
    pub intent: AffordanceIntent,
    pub rank: u8,
}

impl KeyAffordance {
    /// Construct a new default neutral KeyAffordance.
    ///
    /// # Panics
    /// Panics if `key` or `label` is empty — key affordances require a descriptive action label.
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        assert!(!key.is_empty(), "key token must not be empty");
        assert!(
            !label.is_empty(),
            "label must not be empty — key affordances require a descriptive action"
        );
        Self {
            key,
            label,
            intent: AffordanceIntent::Neutral,
            rank: 50,
        }
    }

    /// Construct an affordance with custom semantic intent and priority rank.
    pub const fn with_intent_and_rank(
        key: &'static str,
        label: &'static str,
        intent: AffordanceIntent,
        rank: u8,
    ) -> Self {
        assert!(!key.is_empty(), "key token must not be empty");
        assert!(!label.is_empty(), "label must not be empty");
        Self {
            key,
            label,
            intent,
            rank,
        }
    }

    /// Construct a primary action affordance (Accent tone + BOLD, rank 80).
    #[allow(dead_code)]
    pub const fn primary(key: &'static str, label: &'static str) -> Self {
        Self::with_intent_and_rank(key, label, AffordanceIntent::Primary, 80)
    }

    /// Construct an essential affordance (Esc / dismiss, rank 100).
    #[allow(dead_code)]
    pub const fn always(key: &'static str, label: &'static str) -> Self {
        Self::with_intent_and_rank(key, label, AffordanceIntent::Neutral, 100)
    }

    /// Construct a list/item navigation affordance (↑↓, rank 60).
    #[allow(dead_code)]
    pub const fn navigation(key: &'static str, label: &'static str) -> Self {
        Self::with_intent_and_rank(key, label, AffordanceIntent::Neutral, 60)
    }

    /// Construct a secondary or low-frequency affordance (rank 40).
    #[allow(dead_code)]
    pub const fn secondary(key: &'static str, label: &'static str) -> Self {
        Self::with_intent_and_rank(key, label, AffordanceIntent::Neutral, 40)
    }

    /// Construct a warning or interrupt affordance (Warn tone + BOLD, rank 90).
    #[allow(dead_code)]
    pub const fn warn(key: &'static str, label: &'static str) -> Self {
        Self::with_intent_and_rank(key, label, AffordanceIntent::Warn, 90)
    }

    /// Construct a compact TabBar menu affordance: prominent label on the left,
    /// dim chord "C-x" on the right (ADR-0043 [INV-UI-02]).
    pub const fn tabbar_menu(label: &'static str) -> Self {
        Self {
            key: "C-x",
            label,
            intent: AffordanceIntent::Primary,
            rank: 100,
        }
    }

    /// The visual column width of the keycap + space + label unit.
    pub fn width(&self) -> usize {
        self.key.width() + 1 + self.label.width()
    }

    /// Render this affordance as a pair of styled spans:
    /// keycap (intent fg + bold) + space + label (keycap_label).
    #[allow(dead_code)]
    pub fn render_spans(&self, theme: &Theme, bg: Color) -> [Span<'static>; 2] {
        let key_fg = match self.intent {
            AffordanceIntent::Primary => theme.keycap_accent(),
            AffordanceIntent::Neutral => theme.keycap_fg(),
            AffordanceIntent::Warn => theme.keycap_warn(),
        };
        let key_style = Style::default()
            .fg(key_fg)
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let label_style = theme.keycap_label_style().bg(bg);
        [
            Span::styled(self.key.to_string(), key_style),
            Span::styled(format!(" {}", self.label), label_style),
        ]
    }

    /// Render this affordance in unified key-first style (ADR-0054):
    /// prominent shortcut on the left (accent + bold), action label on the right (dark gray).
    pub fn render_tabbar_spans(&self, theme: &Theme, bg: Color) -> [Span<'static>; 2] {
        let key_style = Style::default()
            .fg(theme.keycap_accent())
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let label_style = theme.keycap_label_style().bg(bg);
        [
            Span::styled(self.key.to_string(), key_style),
            Span::styled(format!(" {}", self.label), label_style),
        ]
    }
}

/// Result of laying out an affordance group under width constraints (ADR-0054).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AffordanceLayout {
    /// Materialized styled spans ready for rendering into a Paragraph/Line.
    pub spans: Vec<Span<'static>>,
    /// Total visual column width of the visible spans.
    #[allow(dead_code)]
    pub visual_width: usize,
    /// Whether any lower-priority items were relegated to overflow.
    pub has_overflow: bool,
    /// The indices of items relegated to overflow (in caller's original slice).
    pub overflow_indices: Vec<usize>,
    /// The actual items relegated to overflow.
    #[allow(dead_code)]
    pub overflow_items: Vec<KeyAffordance>,
}

/// Inter-affordance spacing string (matches design::JOIN_ENUMERATE_COLS).
#[allow(dead_code)]
pub(crate) const AFFORDANCE_GAP_STR: &str = "  ";
/// Inter-affordance spacing column width.
#[allow(dead_code)]
pub(crate) const AFFORDANCE_GAP_WIDTH: usize = 2;

/// Lay out a slice of affordances within `available_width` using the Floor-1 rule (ADR-0054 [INV-AFFORDANCE-02]).
///
/// If all items fit, all items are displayed with no overflow indicator.
/// If items exceed `available_width`, the layout finds the maximum items `M` that fit
/// unconditionally, takes `K = floor(M) - 1` items (ranked by priority), and reserves
/// the remaining space for `F1 {overflow_label}` (default `"more"`).
#[allow(dead_code)]
pub fn layout_affordances(
    items: &[KeyAffordance],
    available_width: usize,
    theme: &Theme,
    bg: Color,
    overflow_label: Option<&'static str>,
) -> AffordanceLayout {
    if items.is_empty() || available_width == 0 {
        return AffordanceLayout {
            spans: Vec::new(),
            visual_width: 0,
            has_overflow: false,
            overflow_indices: Vec::new(),
            overflow_items: Vec::new(),
        };
    }

    // 1. Check if all items fit in caller order without degradation
    let mut full_width = 0usize;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            full_width += AFFORDANCE_GAP_WIDTH;
        }
        full_width += item.width();
    }

    if full_width <= available_width {
        let mut spans = Vec::with_capacity(items.len() * 3);
        let gap_style = theme.keycap_label_style().bg(bg);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(AFFORDANCE_GAP_STR, gap_style));
            }
            let [k, l] = item.render_spans(theme, bg);
            spans.push(k);
            spans.push(l);
        }
        return AffordanceLayout {
            spans,
            visual_width: full_width,
            has_overflow: false,
            overflow_indices: Vec::new(),
            overflow_items: Vec::new(),
        };
    }

    // 2. We must degrade and overflow.
    // The canonical overflow affordance: F1 more (or F1 help, ADR-0054)
    let overflow_affordance = KeyAffordance::new("F1", overflow_label.unwrap_or("more"));
    let overflow_w = overflow_affordance.width();

    // Tag items with original caller order for stable relative positioning
    // Sort candidates by priority rank descending; tie-break with original index ascending
    let mut ranked: Vec<(usize, KeyAffordance)> = items.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.rank.cmp(&a.1.rank).then_with(|| a.0.cmp(&b.0)));

    // Calculate maximum number of ranked items M that fit without overflow slot
    let mut max_fit = 0usize;
    let mut test_w = 0usize;
    for (i, (_, item)) in ranked.iter().enumerate() {
        let added_w = if i == 0 {
            item.width()
        } else {
            AFFORDANCE_GAP_WIDTH + item.width()
        };
        if test_w + added_w <= available_width {
            test_w += added_w;
            max_fit += 1;
        } else {
            break;
        }
    }

    // Floor-1 rule: target K = max_fit - 1 items
    let mut target_k = max_fit.saturating_sub(1);

    // Verify whether target_k items + GAP + overflow_affordance fits in available_width.
    // If not, decrement target_k until it fits or reaches 0.
    loop {
        if target_k == 0 {
            break;
        }
        let mut subset: Vec<(usize, KeyAffordance)> =
            ranked.iter().take(target_k).copied().collect();
        // Measure in caller original display order
        subset.sort_by_key(|(idx, _)| *idx);

        let mut candidate_w = 0usize;
        for (i, (_, item)) in subset.iter().enumerate() {
            if i > 0 {
                candidate_w += AFFORDANCE_GAP_WIDTH;
            }
            candidate_w += item.width();
        }
        candidate_w += AFFORDANCE_GAP_WIDTH + overflow_w;

        if candidate_w <= available_width {
            break;
        }
        target_k = target_k.saturating_sub(1);
    }

    // Build visible items from top target_k
    let visible_ranked: Vec<(usize, KeyAffordance)> =
        ranked.iter().take(target_k).copied().collect();
    let mut visible_indices: Vec<usize> = visible_ranked.iter().map(|(idx, _)| *idx).collect();
    visible_indices.sort_unstable();

    let mut overflow_indices = Vec::new();
    let mut overflow_items = Vec::new();
    for (idx, item) in items.iter().copied().enumerate() {
        if !visible_indices.contains(&idx) {
            overflow_indices.push(idx);
            overflow_items.push(item);
        }
    }

    let gap_style = theme.keycap_label_style().bg(bg);
    let mut spans = Vec::new();
    let mut rendered_w = 0usize;

    for (pos, &idx) in visible_indices.iter().enumerate() {
        if pos > 0 {
            spans.push(Span::styled(AFFORDANCE_GAP_STR, gap_style));
            rendered_w += AFFORDANCE_GAP_WIDTH;
        }
        let [k, l] = items[idx].render_spans(theme, bg);
        rendered_w += items[idx].width();
        spans.push(k);
        spans.push(l);
    }

    // Append overflow affordance if there is space
    if rendered_w > 0 {
        spans.push(Span::styled(AFFORDANCE_GAP_STR, gap_style));
        rendered_w += AFFORDANCE_GAP_WIDTH;
    }
    let [ok, ol] = overflow_affordance.render_spans(theme, bg);
    rendered_w += overflow_w;
    spans.push(ok);
    spans.push(ol);

    AffordanceLayout {
        spans,
        visual_width: rendered_w,
        has_overflow: true,
        overflow_indices,
        overflow_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn keycap_style_uses_keycap_fg_and_bold() {
        let theme = Theme::default();
        let style = keycap_style(&theme);
        assert_eq!(style.fg, theme.keycap_fg());
        assert!(style.add.contains(Modifier::BOLD));
    }

    #[test]
    fn key_affordance_renders_atomic_unit() {
        let theme = Theme::default();
        let affordance = KeyAffordance::new("Esc", "back");
        assert_eq!(affordance.width(), 3 + 1 + 4);

        let [key_span, label_span] = affordance.render_spans(&theme, theme.body());
        assert_eq!(key_span.content, "Esc");
        assert_eq!(key_span.style.fg, theme.keycap_fg());
        assert_eq!(label_span.content, " back");
        assert_eq!(label_span.style.fg, theme.keycap_label());
    }

    #[test]
    fn key_affordance_primary_uses_accent_fg() {
        let theme = Theme::default();
        let affordance = KeyAffordance::primary("Enter", "send");
        let [key_span, label_span] = affordance.render_spans(&theme, theme.body());
        assert_eq!(key_span.content, "Enter");
        assert_eq!(key_span.style.fg, theme.keycap_accent());
        assert!(key_span.style.add.contains(Modifier::BOLD));
        assert_eq!(label_span.content, " send");
        assert_eq!(label_span.style.fg, theme.keycap_label());
    }

    #[test]
    fn key_affordance_warn_uses_warn_fg() {
        let theme = Theme::default();
        let affordance = KeyAffordance::warn("Del", "delete");
        let [key_span, _label_span] = affordance.render_spans(&theme, theme.body());
        assert_eq!(key_span.content, "Del");
        assert_eq!(key_span.style.fg, theme.keycap_warn());
        assert!(key_span.style.add.contains(Modifier::BOLD));
    }

    #[test]
    fn affordance_layout_fits_all_when_width_is_sufficient() {
        let theme = Theme::default();
        let items = [
            KeyAffordance::primary("Enter", "send"),
            KeyAffordance::navigation("↑↓", "select"),
            KeyAffordance::always("Esc", "close"),
        ];
        let layout = layout_affordances(&items, 100, &theme, Color::Reset, None);
        assert!(!layout.has_overflow);
        assert!(layout.overflow_indices.is_empty());
        assert_eq!(text(&layout.spans), "Enter send  ↑↓ select  Esc close");
    }

    #[test]
    fn affordance_layout_applies_floor_1_rule_on_overflow() {
        let theme = Theme::default();
        // item 0: "Enter send" (10)
        // item 1: "↑↓ select" (9)
        // item 2: "Tab edit" (8)
        // item 3: "Esc close" (9)
        // Total = 10 + 2 + 9 + 2 + 8 + 2 + 9 = 42 cols.
        let items = [
            KeyAffordance::primary("Enter", "send"),   // rank 80
            KeyAffordance::navigation("↑↓", "select"), // rank 60
            KeyAffordance::secondary("Tab", "edit"),   // rank 40
            KeyAffordance::always("Esc", "close"),     // rank 100
        ];

        // With width 35:
        // Max fit without overflow slot:
        // Esc close (9) + Enter send (10) + ↑↓ select (9) = 9 + 2 + 10 + 2 + 9 = 32 <= 35. M = 3 items.
        // Floor-1 target: K = M - 1 = 2 items.
        // Top 2 items by rank: Esc close (rank 100), Enter send (rank 80).
        // Plus overflow: F1 more (7).
        // 9 + 2 + 10 + 2 + 7 = 30 <= 35. Fits!
        // Displayed items in original order: Enter send, Esc close, plus F1 more!
        let layout = layout_affordances(&items, 35, &theme, Color::Reset, Some("more"));
        assert!(layout.has_overflow);
        assert_eq!(text(&layout.spans), "Enter send  Esc close  F1 more");
        assert_eq!(layout.overflow_indices, vec![1, 2]); // items 1 and 2 relegated to overflow
    }

    #[test]
    fn affordance_layout_extreme_narrow_width_shows_overflow() {
        let theme = Theme::default();
        let items = [
            KeyAffordance::primary("Enter", "send"),
            KeyAffordance::always("Esc", "close"),
        ];
        // Width 12: cannot fit (Enter send (10) + GAP(3) + F1 more(7) = 20 > 12)
        // Target K drops to 0, shows F1 more
        let layout = layout_affordances(&items, 12, &theme, Color::Reset, None);
        assert!(layout.has_overflow);
        assert_eq!(text(&layout.spans), "F1 more");
        assert_eq!(layout.overflow_indices, vec![0, 1]);
    }

    #[test]
    fn key_affordance_tabbar_menu_renders_compact_key_first() {
        let theme = Theme::default();
        let affordance = KeyAffordance::tabbar_menu("menu");
        assert_eq!(affordance.width(), 8); // "C-x" (3) + " " (1) + "menu" (4)

        let [key_span, label_span] = affordance.render_tabbar_spans(&theme, theme.body());
        assert_eq!(key_span.content, "C-x");
        assert_eq!(key_span.style.fg, theme.keycap_accent());
        assert!(key_span.style.add.contains(Modifier::BOLD));
        assert_eq!(label_span.content, " menu");
        assert_eq!(label_span.style.fg, theme.keycap_label());
        assert!(!label_span.style.add.contains(Modifier::BOLD));
    }

    #[test]
    #[should_panic(expected = "label must not be empty")]
    fn key_affordance_disallows_empty_label() {
        let _ = KeyAffordance::new("Esc", "");
    }
}
