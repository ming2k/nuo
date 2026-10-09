//! Unified keyboard-key ("keycap") styling and affordances.
//!
//! Every surface that shows a keybinding label to the user — the activity-bar
//! interrupt hint, the Help modal rows, the in-modal keymap page, the header
//! hint strips, and the footer hint bar — routes through here so there is a
//! single, consistent, theme-driven affordance across the app.
//!
//! Visual hierarchy (Visual Language R0):
//! - Keycaps use high-contrast neutral/crisp glyphs (`theme.keycap_fg()`) + BOLD,
//!   or micro-elevated pill badges (`theme.keycap_bg()`).
//! - Action labels use dedicated readable silver/sage tones (`theme.keycap_label()`)
//!   rather than fading into the background `muted` or `dim`.
//! - Semantic intents (Accent/Warn) allow primary submit (`Enter`) or interrupt
//!   (`Esc Esc`) to stand out naturally.

use nuotc::{Color, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::super::Theme;

/// The single, theme-aware style applied to standard keycap labels.
pub(crate) fn keycap_style(theme: &Theme) -> Style {
    theme.keycap_style()
}

pub(crate) fn keycap_warn_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.keycap_warn())
        .add_modifier(Modifier::BOLD)
}

/// A styled keycap span for `text`.
pub(crate) fn keycap_span<'a>(theme: &Theme, text: &str) -> Span<'a> {
    Span::styled(text.to_string(), keycap_style(theme))
}

/// A styled warn keycap span for `text`.
pub(crate) fn keycap_warn_span<'a>(theme: &Theme, text: &str) -> Span<'a> {
    Span::styled(text.to_string(), keycap_warn_style(theme))
}

/// An atomic keycap + action label pair, strictly adhering to Visual Language R0.
///
/// An affordance joins a keycap with its action label (e.g. `Ctrl+X menu`,
/// `Ctrl+P block`, `Esc back`). The key and label must never be empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyAffordance {
    pub key: &'static str,
    pub label: &'static str,
}

impl KeyAffordance {
    /// Construct a new typed KeyAffordance from a canonical `Key`.
    #[allow(dead_code)]
    pub const fn from_key(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::new(key.display(), label)
    }

    /// Construct a new typed KeyAffordance.
    ///
    /// # Panics
    /// Panics if `key` or `label` is empty — key affordances require a descriptive action label.
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        assert!(!key.is_empty(), "key token must not be empty");
        assert!(
            !label.is_empty(),
            "label must not be empty — key affordances require a descriptive action"
        );
        Self { key, label }
    }

    /// Construct a compact TabBar menu affordance: prominent label on the left,
    /// dim chord "C-x" on the right (ADR-0043 [INV-UI-02]).
    pub const fn tabbar_menu(label: &'static str) -> Self {
        Self {
            key: "C-x",
            label,
        }
    }

    /// The visual column width of the keycap + space + label unit.
    pub fn width(&self) -> usize {
        self.key.width() + 1 + self.label.width()
    }

    /// Render this affordance as a pair of styled spans: keycap (keycap_fg + bold) + space + label (keycap_label).
    #[allow(dead_code)]
    pub fn render_spans(&self, theme: &Theme, bg: Color) -> [Span<'static>; 2] {
        let key_fg = theme.keycap_fg();
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

    /// Render this affordance in TabBar label-first style (ADR-0043 [INV-UI-02]):
    /// prominent label on the left (brand + bold), dim shortcut on the right (dim).
    pub fn render_tabbar_spans(&self, theme: &Theme, bg: Color) -> [Span<'static>; 2] {
        let label_style = Style::default()
            .fg(theme.brand())
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let key_style = Style::default().fg(theme.dim()).bg(bg);
        [
            Span::styled(self.label.to_string(), label_style),
            Span::styled(format!(" {}", self.key), key_style),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn key_affordance_tabbar_menu_renders_compact_label_first() {
        let theme = Theme::default();
        let affordance = KeyAffordance::tabbar_menu("menu");
        assert_eq!(affordance.width(), 8); // "menu" (4) + " " (1) + "C-x" (3)

        let [label_span, key_span] = affordance.render_tabbar_spans(&theme, theme.body());
        assert_eq!(label_span.content, "menu");
        assert_eq!(label_span.style.fg, theme.brand());
        assert!(label_span.style.add.contains(Modifier::BOLD));
        assert_eq!(key_span.content, " C-x");
        assert_eq!(key_span.style.fg, theme.dim());
        assert!(!key_span.style.add.contains(Modifier::BOLD));
    }

    #[test]
    #[should_panic(expected = "label must not be empty")]
    fn key_affordance_disallows_empty_label() {
        let _ = KeyAffordance::new("Esc", "");
    }
}
