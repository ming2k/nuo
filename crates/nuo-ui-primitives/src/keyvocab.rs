//! Pure key vocabulary: the glyph tokens footers and legends repeat.
//!
//! These are display strings, not keys — they name how a chord *reads*
//! (`"↑↓"`, `"Space"`, `"Shift-Tab"`) and carry no driver dependency. The
//! shell's `keymap` re-exports them so a footer's key and label come from one
//! place.
//!
//! # One Spelling Per Modifier
//!
//! A key token is a contract with the keyboard the user is holding, so a
//! modifier is spelled the same way everywhere: `Ctrl-`, `Alt-`, `Shift-`.
//! `⇧` (U+21E7 UPWARDS WHITE ARROW) was the single exception, and it cost more
//! than it saved:
//!
//! - **Two spellings for one modifier.** The vocabulary already spells shift
//!   chords as words — `Ctrl-Shift-c`, `Shift-y`. A footer that says `Shift-y`
//!   three columns from a keycap reading `⇧Tab` names the same modifier two
//!   ways, and a reader has to learn that the glyph is the word.
//! - **It is not a label the keyboard carries.** `Ctrl` and `Alt` are printed
//!   words on every keycap. The shift arrow is Apple's menu-symbol convention;
//!   a PC keyboard says `Shift`, so `⇧Tab` asks the user to decode a symbol
//!   before decoding the chord. There is no equivalent for `Ctrl` or `Alt`,
//!   which is exactly why they were never given glyphs.
//! - **It can vanish.** When a terminal's font lacks U+21E7 the cell renders
//!   as `□`, leaving `□Tab` — the token loses the one word it existed to
//!   carry. `Shift-Tab` is ASCII and renders on any font.
//! - **Its width is locale-dependent.** U+21E7 is East-Asian *Ambiguous*, so
//!   it measures one column under `unicode-width` and two under a CJK-locale
//!   terminal, shifting the tail of every row that carries it.
//!
//! The direction arrows (`↑↓`, `←→`) are deliberately *not* affected. They are
//! ambiguous-width too, but they name the keys themselves rather than a
//! modifier around one, no word is shorter (`Up`/`Down` would spend columns on
//! every navigation hint), and they sit in the arrow block that terminal fonts
//! ship for its own cursor drawing.

/// Vertical arrow pair.
pub const ARROWS_UD: &str = "↑↓";
/// Horizontal arrow pair.
pub const ARROWS_LR: &str = "←→";
/// Up arrow.
pub const UP: &str = "↑";
/// Down arrow.
pub const DOWN: &str = "↓";
/// Space key.
pub const SPACE: &str = "Space";
/// Shift+Tab.
pub const SHIFT_TAB: &str = "Shift-Tab";
/// Shift+Enter.
pub const SHIFT_ENTER: &str = "Shift-Enter";
/// Shift+Delete.
pub const SHIFT_DELETE: &str = "Shift-Del";
/// Delete key.
pub const DELETE: &str = "Del";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every *modified-chord* token is a plain word — `Shift-`, not `⇧`.
    ///
    /// The gate is mechanical on purpose: a modifier that is spelled with a
    /// symbol in one place and a word in another is a vocabulary split, and a
    /// glyph that a font may be missing is a token that can disappear from a
    /// footer. Both failures are invisible in a unit test of a single constant
    /// and obvious here.
    #[test]
    fn modifier_tokens_are_ascii_words() {
        for (name, token) in [
            ("SHIFT_TAB", SHIFT_TAB),
            ("SHIFT_ENTER", SHIFT_ENTER),
            ("SHIFT_DELETE", SHIFT_DELETE),
            ("SPACE", SPACE),
            ("DELETE", DELETE),
        ] {
            assert!(
                token.is_ascii(),
                "`{name}` must be ASCII so it renders on any terminal font, got `{token}`"
            );
            assert!(
                token.starts_with("Shift-") || !token.contains("Shift"),
                "`{name}` must spell the modifier as `Shift-`, got `{token}`"
            );
        }
    }

    /// The arrow tokens are the deliberate exception: they *are* the key, not a
    /// modifier around one, and a word would cost columns in every hint.
    #[test]
    fn direction_tokens_stay_glyphs() {
        assert_eq!(ARROWS_UD, "↑↓");
        assert_eq!(ARROWS_LR, "←→");
        assert_eq!(UP, "↑");
        assert_eq!(DOWN, "↓");
    }
}
