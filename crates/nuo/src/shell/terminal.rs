//! Raw-mode / alternate-screen lifecycle: initialization, graceful cleanup, signal-guard.

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, execute};

use crate::shell::mouse_mode::{DisableButtonMouse, EnableButtonMouse};

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static KEYBOARD_ENHANCEMENT_ENABLED: AtomicBool = AtomicBool::new(false);
static MOUSE_CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);
static BRACKETED_PASTE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Set up raw mode, alternate screen, mouse capture, and bracketed paste.
///
/// Deliberately does **not** perform the keyboard-enhancement handshake: that
/// query writes `ESC[?u ESC[c` and then blocks until the terminal answers with
/// its primary device attributes, bounded by a 2 s timeout in crossterm. A
/// terminal that never answers (many multiplexers, some CI/pty harnesses, and
/// any emulator without kitty-protocol support) therefore stalled the first
/// frame for the full 2 s — measured at ~2057 ms versus ~53 ms on a terminal
/// that replies. Everything above this line is a write that cannot block, so
/// the caller paints immediately; [`negotiate_keyboard_enhancement`] upgrades
/// the session afterwards, off the first-frame path.
pub(super) fn enter_terminal(profile: &nuotc::TerminalProfile) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Button-event (Mode 1002) + SGR-1006 mouse tracking [INV-SURFACE-09]
    if profile.supports_mouse && execute!(stdout, EnableButtonMouse).is_ok() {
        MOUSE_CAPTURE_ENABLED.store(true, Ordering::Relaxed);
    }

    if profile.supports_color() && execute!(stdout, EnableBracketedPaste).is_ok() {
        BRACKETED_PASTE_ENABLED.store(true, Ordering::Relaxed);
    }

    Ok(())
}

/// Whether the host terminal honours the kitty keyboard protocol, resolved once
/// per process.
///
/// `None` means "not yet probed". The probe is the expensive part — it waits up
/// to 2 s for a reply — so the result is cached and never repeated: the
/// terminal's capabilities cannot change under a running process, and a second
/// probe would pay the timeout again.
static KEYBOARD_ENHANCEMENT_SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Push the progressive keyboard-enhancement flags (Kitty keyboard protocol),
/// which let modifier-bearing keys disambiguate (e.g. Ctrl+M vs Enter).
///
/// Split out from [`enter_terminal`] so a slow terminal costs *nothing* on the
/// first frame: call this once the UI is already on screen. Enabling the flags
/// later is safe because the input reader normalises both encodings — until the
/// push lands, keys simply arrive in their legacy form.
pub(super) fn negotiate_keyboard_enhancement(profile: &nuotc::TerminalProfile) {
    if profile.is_monochrome() {
        return;
    }
    let supported = *KEYBOARD_ENHANCEMENT_SUPPORTED
        .get_or_init(|| supports_keyboard_enhancement().unwrap_or(false));
    if supported
        && execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KEYBOARD_ENHANCEMENT_ENABLED.store(true, Ordering::Relaxed);
    }
}

/// Undo raw mode, leave the alternate screen, disable bracketed paste, and turn off mouse tracking.
/// Used both on graceful shutdown and from the signal guard so an externally
/// killed process (e.g. `pkill nuo`) does not strand the terminal in a
/// state where every mouse move spews SGR escape codes into the shell.
pub(super) fn restore_terminal() {
    let mut stdout = io::stdout();

    if KEYBOARD_ENHANCEMENT_ENABLED.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }

    if BRACKETED_PASTE_ENABLED.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout, DisableBracketedPaste);
    }

    let _ = execute!(stdout, LeaveAlternateScreen);

    if MOUSE_CAPTURE_ENABLED.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout, DisableButtonMouse);
    }

    let _ = execute!(stdout, cursor::Show);
    let _ = disable_raw_mode();
    let _ = stdout.flush();
}

/// Catch termination signals and restore the terminal before exiting. Without
/// this, SIGTERM/SIGHUP (as sent by `pkill`) terminates the process without
/// running `run_tui`'s normal cleanup, leaving the host terminal in raw mode
/// with mouse capture enabled.
pub(super) fn spawn_signal_guard() {
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut hangup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut quit = match signal(SignalKind::quit()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            _ = hangup.recv() => {}
            _ = quit.recv() => {}
        }
        restore_terminal();
        std::process::exit(130);
    });

    #[cfg(windows)]
    tokio::spawn(async move {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
        let mut c = ctrl_c().ok();
        let mut b = ctrl_break().ok();
        let mut cl = ctrl_close().ok();
        let mut l = ctrl_logoff().ok();
        let mut s = ctrl_shutdown().ok();
        tokio::select! {
            _ = async { if let Some(ref mut c) = c { c.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut b) = b { b.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut cl) = cl { cl.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut l) = l { l.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut s) = s { s.recv().await } else { std::future::pending().await } } => {}
        }
        restore_terminal();
        std::process::exit(130);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monochrome terminal must skip the probe *before* it runs, not merely
    /// avoid pushing flags afterwards.
    ///
    /// The distinction is observable: the probe's verdict is memoised in
    /// `KEYBOARD_ENHANCEMENT_SUPPORTED`, so if the guard sat after the probe —
    /// or were deleted — the cell would be initialised on a monochrome
    /// terminal, which is the expensive path (up to 2 s) this whole change
    /// exists to avoid on the first frame. Asserting on the cell rather than on
    /// the flag is what makes the test fail when the guard is removed; the flag
    /// alone stays false either way in a non-tty test process.
    ///
    /// Order-independent because no other test here negotiates a colour
    /// profile, so nothing else initialises the cell.
    #[test]
    fn monochrome_short_circuits_before_the_probe_runs() {
        KEYBOARD_ENHANCEMENT_ENABLED.store(false, Ordering::Relaxed);
        negotiate_keyboard_enhancement(&monochrome_profile());

        assert!(
            KEYBOARD_ENHANCEMENT_SUPPORTED.get().is_none(),
            "monochrome must return before the probe: the memo must stay unset"
        );
        assert!(
            !KEYBOARD_ENHANCEMENT_ENABLED.load(Ordering::Relaxed),
            "monochrome must never be pushed enhancement flags"
        );
    }

    /// The first-frame handshake must not reference the probe at all.
    ///
    /// This is a source-level contract, and deliberately so: the property that
    /// matters is *where the call site sits*, which no runtime assertion inside
    /// this module can observe. `enter_terminal` runs before the first paint;
    /// `negotiate_keyboard_enhancement` must never be reachable from it.
    #[test]
    fn the_first_paint_handshake_does_not_probe() {
        const SRC: &str = include_str!("terminal.rs");
        let handshake = SRC
            .split("pub(super) fn negotiate_keyboard_enhancement")
            .next()
            .expect("the negotiate fn must exist");
        // Look only at the handshake's own body, before the probe's definition.
        let body_start = handshake
            .find("pub(super) fn enter_terminal")
            .expect("enter_terminal must exist");
        let body = &handshake[body_start..];
        assert!(
            !body.contains("supports_keyboard_enhancement"),
            "enter_terminal must not query the terminal: that call blocks the \
             first frame for up to 2 s on a terminal that does not answer"
        );
    }

    fn monochrome_profile() -> nuotc::TerminalProfile {
        nuotc::TerminalProfile {
            color_standard: nuotc::ColorStandard::Monochrome,
            charset_standard: nuotc::CharsetStandard::Ascii,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: false,
        }
    }
}
