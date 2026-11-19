//! Raw-mode / alternate-screen lifecycle backed by `graticule::TerminalSession`.

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use graticule::{
    MouseMode, ProbeHandle, TerminalProfile, TerminalSession, restore_global,
    session_config_for_profile,
};

static KEYBOARD_ENHANCEMENT_SUPPORTED: OnceLock<bool> = OnceLock::new();
static KEYBOARD_ENHANCEMENT_ENABLED: AtomicBool = AtomicBool::new(false);

/// Detect the host terminal capability profile, refining standards detection
/// with safety fences for Linux virtual consoles (`TERM=linux`), serial lines,
/// and unadorned consoles.
pub fn detect_terminal_profile() -> TerminalProfile {
    let mut profile = TerminalProfile::detect();
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term == "linux" || term == "console" || term.starts_with("vt52") {
        // Linux virtual console (getty on /dev/tty1..6) uses VGA/fbcon 256/512-glyph
        // fonts where non-ASCII UTF-8 glyphs are frequently missing or broken.
        profile.charset_standard = graticule::CharsetStandard::Ascii;
        profile.supports_mouse = false;
        profile.supports_sync_update = false;
    }
    profile
}

/// Set up raw mode, alternate screen, all-motion SGR mouse tracking (`?1003h`
/// + `?1006h`), and bracketed paste (`?2004h`) via [`TerminalSession`].
///
/// Deliberately does **not** perform the keyboard-enhancement handshake: that
/// query writes `ESC[?u ESC[c` and waits for a reply, which would stall the
/// first frame on terminals that do not answer. [`negotiate_keyboard_enhancement`]
/// upgrades the session afterwards, off the first-frame path.
pub(super) fn enter_terminal(profile: &TerminalProfile) -> io::Result<TerminalSession> {
    let config = session_config_for_profile(profile, MouseMode::AllMotion);
    TerminalSession::enter(config)
}

/// Push Kitty `DISAMBIGUATE_ESCAPE_CODES` off the first-frame path using the
/// single-reader [`ProbeHandle`].
pub(super) fn negotiate_keyboard_enhancement(profile: &TerminalProfile, probe: &ProbeHandle) {
    if profile.is_monochrome() {
        return;
    }
    let supported = *KEYBOARD_ENHANCEMENT_SUPPORTED
        .get_or_init(|| probe.probe_keyboard_enhancement(Duration::from_secs(2)));
    if supported && graticule::push_disambiguate_escape_codes() {
        KEYBOARD_ENHANCEMENT_ENABLED.store(true, Ordering::Relaxed);
    }
}

/// Undo raw mode, leave the alternate screen, disable bracketed paste, and turn
/// off mouse tracking idempotently.
pub fn restore_terminal() {
    KEYBOARD_ENHANCEMENT_ENABLED.store(false, Ordering::Relaxed);
    restore_global();
}

/// Catch termination signals and restore the terminal before exiting.
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

    /// The first-frame handshake must not reference the probe at all.
    #[test]
    fn the_first_paint_handshake_does_not_probe() {
        const SRC: &str = include_str!("terminal.rs");
        let handshake = SRC
            .split("pub(super) fn negotiate_keyboard_enhancement")
            .next()
            .expect("the negotiate fn must exist");
        let body_start = handshake
            .find("pub(super) fn enter_terminal")
            .expect("enter_terminal must exist");
        let body = &handshake[body_start..];
        assert!(
            !body.contains("probe_keyboard_enhancement"),
            "enter_terminal must not query the terminal: that call blocks the \
             first frame for up to 2 s on a terminal that does not answer"
        );
    }

    #[test]
    fn monochrome_profile_skips_mouse_and_sync_in_session_config() {
        let cfg = session_config_for_profile(&monochrome_profile(), MouseMode::AllMotion);
        assert_eq!(cfg.mouse_mode, MouseMode::None);
    }

    fn monochrome_profile() -> TerminalProfile {
        TerminalProfile {
            color_standard: graticule::ColorStandard::Monochrome,
            charset_standard: graticule::CharsetStandard::Ascii,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: false,
        }
    }

    #[test]
    fn detect_terminal_profile_clamps_linux_console_to_ascii() {
        let prev = std::env::var("TERM").ok();
        unsafe {
            std::env::set_var("TERM", "linux");
        }
        let profile = detect_terminal_profile();
        if let Some(val) = prev {
            unsafe { std::env::set_var("TERM", val) };
        } else {
            unsafe { std::env::remove_var("TERM") };
        }
        assert_eq!(profile.charset_standard, graticule::CharsetStandard::Ascii);
        assert!(!profile.supports_mouse);
        assert!(!profile.supports_sync_update);
    }
}
