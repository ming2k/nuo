//! Pure input key for declarative components (ADR-0072 / ADR-0073).
//!
//! Components receive **semantic keys**, never a terminal driver's event type,
//! so the component tree stays a zero-IO projection ([INV-ARCH-UI-ZERO-IO]).
//! The shell translates its driver's events into [`Key`] at the boundary.

/// A semantic key press routed to a declarative component.
///
/// Only the keys the components actually act on are modelled; anything else is
/// [`Key::Other`], which every component's default arm ignores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Other,
}

/// A pure key stroke: a semantic key plus the Shift modifier.
///
/// Components distinguish only Shift (it changes the character itself);
/// Ctrl/Alt/Super chords belong to the shell's global layer, so the substrate
/// models exactly what a component can act on and the shell folds the driver's
/// modifiers into this shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStroke {
    pub key: Key,
    pub shift: bool,
}

impl KeyStroke {
    /// An unmodified stroke.
    pub const fn plain(key: Key) -> Self {
        Self { key, shift: false }
    }

    /// A Shift-modified stroke.
    pub const fn shifted(key: Key) -> Self {
        Self { key, shift: true }
    }
}
