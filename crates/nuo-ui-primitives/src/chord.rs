//! Pure chord model: `KeyCode`, `KeyModifiers`, and the `Key` a binding names.
//!
//! This is the semantic key layer. It carries no terminal-driver dependency: the
//! shell folds a driver event into a [`Key`] at exactly one boundary
//! ([INV-ARCH-UI-ZERO-IO], ADR-0103 §1).

use std::ops::BitOr;

/// A semantic key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// Any code the binding layer does not model.
    Other,
}

/// Modifier keys held during a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const CONTROL: Self = Self(2);
    pub const ALT: Self = Self(4);
    pub const SUPER: Self = Self(8);

    /// Whether every modifier in `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any modifier in `other` is present.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Union of two modifier sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Remove `other` from this set.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Insert `other` into this set.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Whether no modifier is held.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// The compact token for a core [`KeyCode`] in lowercase chord notation.
pub const fn chord_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            'z' => "z",
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            '9' => "9",
            '?' => "?",
            '/' => "/",
            _ => "·",
        },
        KeyCode::Enter => "enter",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "shift-tab",
        KeyCode::Delete => "del",
        KeyCode::Backspace => "backspace",
        KeyCode::Esc => "esc",
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pgup",
        KeyCode::PageDown => "pgdn",
        KeyCode::F(1) => "f1",
        KeyCode::F(2) => "f2",
        KeyCode::F(3) => "f3",
        KeyCode::F(4) => "f4",
        KeyCode::F(5) => "f5",
        _ => "·",
    }
}

/// The display token for a core [`KeyCode`] in human notation, preserving exact case.
pub const fn display_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            'z' => "z",
            'A' => "A",
            'B' => "B",
            'C' => "C",
            'D' => "D",
            'E' => "E",
            'F' => "F",
            'G' => "G",
            'H' => "H",
            'I' => "I",
            'J' => "J",
            'K' => "K",
            'L' => "L",
            'M' => "M",
            'N' => "N",
            'O' => "O",
            'P' => "P",
            'Q' => "Q",
            'R' => "R",
            'S' => "S",
            'T' => "T",
            'U' => "U",
            'V' => "V",
            'W' => "W",
            'X' => "X",
            'Y' => "Y",
            'Z' => "Z",
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            '9' => "9",
            '?' => "?",
            '/' => "/",
            '[' => "[",
            ']' => "]",
            ' ' => "Space",
            _ => "·",
        },
        KeyCode::Enter => "Enter",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => crate::keyvocab::SHIFT_TAB,
        KeyCode::Delete => crate::keyvocab::DELETE,
        KeyCode::Backspace => "Backspace",
        KeyCode::Esc => "Esc",
        KeyCode::Up => crate::keyvocab::UP,
        KeyCode::Down => crate::keyvocab::DOWN,
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::F(1) => "F1",
        KeyCode::F(2) => "F2",
        KeyCode::F(3) => "F3",
        KeyCode::F(4) => "F4",
        KeyCode::F(5) => "F5",
        _ => "·",
    }
}

/// A physical key with optional modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

impl Key {
    pub const ESC: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Esc,
    };
    pub const ENTER: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Enter,
    };
    pub const TAB: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Tab,
    };
    pub const BACKTAB: Key = Key {
        modifiers: KeyModifiers::SHIFT,
        code: KeyCode::BackTab,
    };
    pub const BRACKET_LEFT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char('['),
    };
    pub const BRACKET_RIGHT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Char(']'),
    };
    pub const ALT_BRACKET_LEFT: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Char('['),
    };
    pub const ALT_BRACKET_RIGHT: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Char(']'),
    };
    pub const UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Up,
    };
    pub const DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Down,
    };
    pub const LEFT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Left,
    };
    pub const RIGHT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Right,
    };
    pub const DELETE: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Delete,
    };
    pub const SHIFT_DELETE: Key = Key {
        modifiers: KeyModifiers::SHIFT,
        code: KeyCode::Delete,
    };
    pub const PAGE_UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageUp,
    };
    pub const PAGE_DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageDown,
    };
    pub const HOME: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Home,
    };
    pub const END: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::End,
    };
    pub const F1: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(1),
    };
    pub const F5: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(5),
    };

    pub const CTRL_L: Key = Key::ctrl('l');
    pub const CTRL_C: Key = Key::ctrl('c');
    pub const CTRL_X: Key = Key::ctrl('x');
    pub const CTRL_P: Key = Key::ctrl('p');
    pub const CTRL_Q: Key = Key::ctrl('q');
    pub const CTRL_R: Key = Key::ctrl('r');
    pub const CTRL_O: Key = Key::ctrl('o');
    pub const CTRL_N: Key = Key::ctrl('n');
    pub const CTRL_T: Key = Key::ctrl('t');
    pub const CTRL_M: Key = Key::ctrl('m');
    pub const CTRL_S: Key = Key::ctrl('s');
    pub const CTRL_G: Key = Key::ctrl('g');
    pub const CTRL_J: Key = Key::ctrl('j');
    pub const CTRL_U: Key = Key::ctrl('u');
    pub const CTRL_A: Key = Key::ctrl('a');
    pub const CTRL_E: Key = Key::ctrl('e');
    pub const CTRL_K: Key = Key::ctrl('k');
    pub const CTRL_W: Key = Key::ctrl('w');
    pub const CTRL_V: Key = Key::ctrl('v');

    pub const ALT_S: Key = Key::alt('s');
    pub const ALT_W: Key = Key::alt('w');
    pub const ALT_T: Key = Key::alt('t');
    pub const ALT_P: Key = Key::alt('p');
    pub const ALT_N: Key = Key::alt('n');
    pub const ALT_H: Key = Key::alt('h');
    pub const ALT_COMMA: Key = Key::alt(',');
    pub const ALT_DOT: Key = Key::alt('.');
    pub const ALT_UP: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Up,
    };
    pub const ALT_DOWN: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Down,
    };
    pub const ALT_ENTER: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Enter,
    };

    pub const CTRL_SHIFT_C: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('c'),
    };
    pub const CTRL_SHIFT_P: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('p'),
    };
    pub const CTRL_SHIFT_R: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('r'),
    };
    pub const CMD_C: Key = Key {
        modifiers: KeyModifiers::SUPER,
        code: KeyCode::Char('c'),
    };

    pub const fn ctrl(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::CONTROL,
            code: KeyCode::Char(c),
        }
    }

    pub const fn alt(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Char(c),
        }
    }

    pub const fn plain(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char(c),
        }
    }

    pub const fn shift_code(code: KeyCode) -> Self {
        Self {
            modifiers: KeyModifiers::SHIFT,
            code,
        }
    }

    pub const fn chord(&self) -> &'static str {
        let ctrl = self.modifiers.contains(KeyModifiers::CONTROL);
        let alt = self.modifiers.contains(KeyModifiers::ALT);
        let shift = self.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = self.modifiers.contains(KeyModifiers::SUPER);

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "ctrl-shift-c",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "ctrl-a",
                    'b' => "ctrl-b",
                    'c' => "ctrl-c",
                    'd' => "ctrl-d",
                    'e' => "ctrl-e",
                    'f' => "ctrl-f",
                    'g' => "ctrl-g",
                    'h' => "ctrl-h",
                    'i' => "ctrl-i",
                    'j' => "ctrl-j",
                    'k' => "ctrl-k",
                    'l' => "ctrl-l",
                    'm' => "ctrl-m",
                    'n' => "ctrl-n",
                    'o' => "ctrl-o",
                    'p' => "ctrl-p",
                    'q' => "ctrl-q",
                    'r' => "ctrl-r",
                    's' => "ctrl-s",
                    't' => "ctrl-t",
                    'u' => "ctrl-u",
                    'v' => "ctrl-v",
                    'w' => "ctrl-w",
                    'x' => "ctrl-x",
                    'y' => "ctrl-y",
                    'z' => "ctrl-z",
                    _ => "·",
                },
                KeyCode::Up => "ctrl-↑",
                KeyCode::Down => "ctrl-↓",
                KeyCode::Left => "ctrl-←",
                KeyCode::Right => "ctrl-→",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "alt-a",
                    'b' => "alt-b",
                    'c' => "alt-c",
                    'd' => "alt-d",
                    'e' => "alt-e",
                    'f' => "alt-f",
                    'g' => "alt-g",
                    'h' => "alt-h",
                    'i' => "alt-i",
                    'j' => "alt-j",
                    'k' => "alt-k",
                    'l' => "alt-l",
                    'm' => "alt-m",
                    'n' => "alt-n",
                    'o' => "alt-o",
                    'p' => "alt-p",
                    'q' => "alt-q",
                    'r' => "alt-r",
                    's' => "alt-s",
                    't' => "alt-t",
                    'u' => "alt-u",
                    'v' => "alt-v",
                    'w' => "alt-w",
                    'x' => "alt-x",
                    'y' => "alt-y",
                    'z' => "alt-z",
                    _ => "·",
                },
                KeyCode::Enter => "alt-enter",
                KeyCode::Up => "alt-↑",
                KeyCode::Down => "alt-↓",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab | KeyCode::BackTab => "shift-tab",
                KeyCode::Delete => "shift-delete",
                _ => chord_token(self.code),
            }
        } else if cmd {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "cmd-c",
                _ => "·",
            }
        } else {
            chord_token(self.code)
        }
    }

    pub const fn display(&self) -> &'static str {
        let ctrl = self.modifiers.contains(KeyModifiers::CONTROL);
        let alt = self.modifiers.contains(KeyModifiers::ALT);
        let shift = self.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = self.modifiers.contains(KeyModifiers::SUPER);

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Ctrl-Shift-c",
                KeyCode::Char('p') | KeyCode::Char('P') => "Ctrl-Shift-p",
                KeyCode::Char('r') | KeyCode::Char('R') => "Ctrl-Shift-r",
                KeyCode::Char('q') | KeyCode::Char('Q') => "Ctrl-Shift-q",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "Ctrl-a",
                    'b' => "Ctrl-b",
                    'c' => "Ctrl-c",
                    'd' => "Ctrl-d",
                    'e' => "Ctrl-e",
                    'f' => "Ctrl-f",
                    'g' => "Ctrl-g",
                    'h' => "Ctrl-h",
                    'i' => "Ctrl-i",
                    'j' => "Ctrl-j",
                    'k' => "Ctrl-k",
                    'l' => "Ctrl-l",
                    'm' => "Ctrl-m",
                    'n' => "Ctrl-n",
                    'o' => "Ctrl-o",
                    'p' => "Ctrl-p",
                    'q' => "Ctrl-q",
                    'r' => "Ctrl-r",
                    's' => "Ctrl-s",
                    't' => "Ctrl-t",
                    'u' => "Ctrl-u",
                    'v' => "Ctrl-v",
                    'w' => "Ctrl-w",
                    'x' => "Ctrl-x",
                    'y' => "Ctrl-y",
                    'z' => "Ctrl-z",
                    _ => "·",
                },
                KeyCode::Up => "Ctrl-↑",
                KeyCode::Down => "Ctrl-↓",
                KeyCode::Left => "Ctrl-←",
                KeyCode::Right => "Ctrl-→",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "Alt-a",
                    'b' => "Alt-b",
                    'c' => "Alt-c",
                    'd' => "Alt-d",
                    'e' => "Alt-e",
                    'f' => "Alt-f",
                    'g' => "Alt-g",
                    'h' => "Alt-h",
                    'i' => "Alt-i",
                    'j' => "Alt-j",
                    'k' => "Alt-k",
                    'l' => "Alt-l",
                    'm' => "Alt-m",
                    'n' => "Alt-n",
                    'o' => "Alt-o",
                    'p' => "Alt-p",
                    'q' => "Alt-q",
                    'r' => "Alt-r",
                    's' => "Alt-s",
                    't' => "Alt-t",
                    'u' => "Alt-u",
                    'v' => "Alt-v",
                    'w' => "Alt-w",
                    'x' => "Alt-x",
                    'y' => "Alt-y",
                    'z' => "Alt-z",
                    _ => "·",
                },
                KeyCode::Enter => "Alt-Enter",
                KeyCode::Up => "Alt-↑",
                KeyCode::Down => "Alt-↓",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab | KeyCode::BackTab => crate::keyvocab::SHIFT_TAB,
                KeyCode::Delete => crate::keyvocab::SHIFT_DELETE,
                KeyCode::Char(c) => match c {
                    'a' | 'A' => "Shift-a",
                    'b' | 'B' => "Shift-b",
                    'c' | 'C' => "Shift-c",
                    'd' | 'D' => "Shift-d",
                    'e' | 'E' => "Shift-e",
                    'f' | 'F' => "Shift-f",
                    'g' | 'G' => "Shift-g",
                    'h' | 'H' => "Shift-h",
                    'i' | 'I' => "Shift-i",
                    'j' | 'J' => "Shift-j",
                    'k' | 'K' => "Shift-k",
                    'l' | 'L' => "Shift-l",
                    'm' | 'M' => "Shift-m",
                    'n' | 'N' => "Shift-n",
                    'o' | 'O' => "Shift-o",
                    'p' | 'P' => "Shift-p",
                    'q' | 'Q' => "Shift-q",
                    'r' | 'R' => "Shift-r",
                    's' | 'S' => "Shift-s",
                    't' | 'T' => "Shift-t",
                    'u' | 'U' => "Shift-u",
                    'v' | 'V' => "Shift-v",
                    'w' | 'W' => "Shift-w",
                    'x' | 'X' => "Shift-x",
                    'y' | 'Y' => "Shift-y",
                    'z' | 'Z' => "Shift-z",
                    _ => display_token(self.code),
                },
                _ => display_token(self.code),
            }
        } else if cmd {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Cmd-c",
                _ => "·",
            }
        } else {
            display_token(self.code)
        }
    }
}
