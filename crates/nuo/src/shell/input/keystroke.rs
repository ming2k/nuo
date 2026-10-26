//! Terminal-driver → pure key translation (the input boundary).
//!
//! The substrate models keys as [`Key`] / [`KeyStroke`] and never names the
//! terminal driver ([INV-ARCH-UI-ZERO-IO]). This module is the single place the
//! shell folds a `crossterm` event into that shape.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use nuo_ui_primitives::chord::{Key as ChordKey, KeyCode as ChordCode, KeyModifiers as ChordMods};
use nuo_ui_primitives::key::{Key, KeyStroke};

/// Fold a driver key event into the pure [`Key`].
pub(crate) fn key_from_event(event: KeyEvent) -> Key {
    match event.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => Key::Other,
    }
}

/// Fold a driver key event into a [`KeyStroke`]: the pure key plus whether
/// Shift was held (the only modifier a component distinguishes).
pub(crate) fn stroke_from_event(event: KeyEvent) -> KeyStroke {
    KeyStroke {
        key: key_from_event(event),
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
    }
}

/// Build a driver key event from a pure chord key.
///
/// The inverse of [`key_from_event`], used by tests that express a chord with
/// the substrate's types and then drive the real event pipeline.
#[cfg(test)]
pub(crate) fn event_from_chord(key: ChordKey) -> KeyEvent {
    KeyEvent {
        code: driver_code(key.code),
        modifiers: driver_modifiers(key.modifiers),
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

#[cfg(test)]
fn driver_code(code: ChordCode) -> KeyCode {
    match code {
        ChordCode::Char(c) => KeyCode::Char(c),
        ChordCode::Enter => KeyCode::Enter,
        ChordCode::Esc => KeyCode::Esc,
        ChordCode::Tab => KeyCode::Tab,
        ChordCode::BackTab => KeyCode::BackTab,
        ChordCode::Backspace => KeyCode::Backspace,
        ChordCode::Delete => KeyCode::Delete,
        ChordCode::Up => KeyCode::Up,
        ChordCode::Down => KeyCode::Down,
        ChordCode::Left => KeyCode::Left,
        ChordCode::Right => KeyCode::Right,
        ChordCode::Home => KeyCode::Home,
        ChordCode::End => KeyCode::End,
        ChordCode::PageUp => KeyCode::PageUp,
        ChordCode::PageDown => KeyCode::PageDown,
        ChordCode::F(n) => KeyCode::F(n),
        ChordCode::Other => KeyCode::Null,
    }
}

#[cfg(test)]
fn driver_modifiers(mods: ChordMods) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    for (ours, theirs) in [
        (ChordMods::SHIFT, KeyModifiers::SHIFT),
        (ChordMods::CONTROL, KeyModifiers::CONTROL),
        (ChordMods::ALT, KeyModifiers::ALT),
        (ChordMods::SUPER, KeyModifiers::SUPER),
    ] {
        if mods.contains(ours) {
            out.insert(theirs);
        }
    }
    out
}
