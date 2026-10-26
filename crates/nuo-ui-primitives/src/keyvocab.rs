//! Pure key vocabulary: the glyph tokens footers and legends repeat.
//!
//! These are display strings, not keys — they name how a chord *reads*
//! (`"↑↓"`, `"Space"`, `"⇧Tab"`) and carry no driver dependency. The shell's
//! `keymap` re-exports them so a footer's key and label come from one place.

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
pub const SHIFT_TAB: &str = "⇧Tab";
/// Shift+Enter.
pub const SHIFT_ENTER: &str = "⇧Enter";
/// Shift+Delete.
pub const SHIFT_DELETE: &str = "⇧Del";
/// Delete key.
pub const DELETE: &str = "Del";
