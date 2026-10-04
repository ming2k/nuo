//! Precise xterm mouse-mode control.
//!
//! [`crossterm::event::EnableMouseCapture`] emits three tracking modes at once:
//!
//! ```text
//! ?1000h  normal tracking  (press + release)
//! ?1002h  button-event     (press, release, drag while held)
//! ?1003h  any-event        (a.k.a. "all motion" — a report per pixel of travel)
//! ?1015h  + ?1006h         encoding (1006 SGR wins)
//! ```
//!
//! `?1003h` is the leak amplifier. It turns the pointer into a firehose of
//! `ESC [ < btn ; col ; row M` SGR sequences — one per pixel of travel and a
//! continuous stream during a window-resize drag. crossterm's internal parser
//! reassembles these from a 1 KiB read buffer, but when a sequence is split
//! across a read boundary (the kernel hands the `ESC` off at the tail of a
//! read) crossterm hands the trailing payload back as ordinary `KeyCode::Char`
//! events. The composer's `Char` arm inserts every printable char into the
//! input line, so the split sequence shows up as garbage text —
//! `[<35;52;28M[<35;43;30M33;32M…` (see `input::SgrLeakGuard` for the full
//! history, issues #854/#668).
//!
//! We only ever consume button events: click, release, drag, and the wheel.
//! Hover (the sole consumer of mode-1003's `MouseEventKind::Moved`) was a
//! purely cosmetic brightness lift on collapsed step-summary lines and was
//! keyboard-reachable at the *same* highlight colour via `Ctrl+↑/↓` focus, so
//! dropping it costs nothing functional. Cutting `?1003h` therefore removes
//! the leak at its source — the event volume drops by two-to-three orders of
//! magnitude, and the residual split-sequence surface shrinks with it.
//!
//! This module owns the exact, minimal CSI pair so every lifecycle path
//! (startup, graceful teardown, signal guard, resize re-arm, showcase) emits
//! the *same* modes. Crossterm's all-or-nothing `EnableMouseCapture` made that
//! invariant impossible to express; here it is the only thing the type can do.

use std::fmt;

use crossterm::Command;

/// Button-event (`?1002h`) + SGR-1006 (`?1006h`) mouse tracking.
///
/// Emits press / release / drag / wheel events with SGR-encoded coordinates
/// (so positions >223 work) and deliberately does **not** request mode 1003
/// all-motion tracking. Drop-in replacement for
/// [`crossterm::event::EnableMouseCapture`] that leaves out the leak-prone
/// per-pixel motion stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnableButtonMouse;

/// Disable button-event + SGR mouse tracking.
///
/// Emits the inverse of [`EnableButtonMouse`], in reverse order, so the
/// terminal always returns to a clean "no mouse reporting" state regardless
/// of which combination of modes a previous run left enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisableButtonMouse;

impl Command for EnableButtonMouse {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        // Button-event tracking: report button press, release, and motion only
        // while a button is held (drag). This is everything the app consumes
        // (click / drag-select / wheel) without the all-motion firehose.
        f.write_str(crossterm::csi!("?1002h"))?;
        // SGR mouse format: `ESC [ < btn ; col ; row M/m`. Preferred over the
        // legacy ?1015 (urxvt) and plain normal-tracking encodings because it
        // carries a button-press/release disambiguator and supports large
        // coordinates. Must be paired with some tracking mode (?1002 here);
        // ?1006 alone only selects the encoding.
        f.write_str(crossterm::csi!("?1006h"))
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        // The WinAPI input backend reports mouse events natively regardless of
        // the DEC private-mode tracking flags, so enabling modes is a no-op
        // there — mirroring how crossterm's own mouse commands route through
        // `enable_mouse_capture()` only for the *input-reading* side on Windows.
        Ok(())
    }
}

impl Command for DisableButtonMouse {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        // Reverse of [`EnableButtonMouse`]: drop the SGR format first, then the
        // tracking mode, so we never briefly leave the terminal in SGR format
        // with tracking still on.
        f.write_str(crossterm::csi!("?1006l"))?;
        f.write_str(crossterm::csi!("?1002l"))
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_emits_button_event_and_sgr_only() {
        let mut out = String::new();
        EnableButtonMouse.write_ansi(&mut out).unwrap();
        // Exact bytes — no ?1003 (all-motion), no ?1000 (plain), no ?1015
        // (urxvt). The leak source mode is provably absent.
        assert_eq!(out, "\x1b[?1002h\x1b[?1006h");
    }

    #[test]
    fn disable_is_the_reverse_of_enable() {
        let mut out = String::new();
        DisableButtonMouse.write_ansi(&mut out).unwrap();
        assert_eq!(out, "\x1b[?1006l\x1b[?1002l");
    }

    #[test]
    fn no_all_motion_mode_anywhere() {
        // A regression sentinel: if anyone ever widens the emitted set, this
        // guards against reintroducing ?1003h (the per-pixel leak amplifier)
        // by name.
        let mut enable = String::new();
        EnableButtonMouse.write_ansi(&mut enable).unwrap();
        let mut disable = String::new();
        DisableButtonMouse.write_ansi(&mut disable).unwrap();
        assert!(
            !enable.contains("?1003") && !disable.contains("?1003"),
            "all-motion mode 1003 must never be requested"
        );
    }
}
