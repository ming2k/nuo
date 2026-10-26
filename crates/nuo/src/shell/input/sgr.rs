//! SGR mouse-sequence leakage guard (see module docs below).

use crossterm::event::{Event, KeyCode};

/// SGR mouse-sequence leakage guard.
///
/// Background: crossterm sometimes fails to reassemble a mouse report that
/// arrives split across two `event::read()` calls (issue #854/#668). When that
/// happens the bytes of an SGR mouse sequence (`ESC [ < btn ; col ; row M/m`)
/// are handed back as a stream of ordinary `Event::Key` / `KeyCode::Char`
/// events: `Esc`, `[`, `<`, `6`, `5`, `;`, … `M`. Because the composer's
/// `KeyCode::Char` arm inserts every printable char into the input box, the
/// split sequence shows up as garbage text (e.g. `;25M[<35;56;25M…`). This is
/// observed across terminals on resize, fast trackpad scrolling, and inside
/// multiplexers (tmux/screen/xterm.js).
///
/// `SgrLeakGuard` is a tiny state machine fed one event at a time. While it is
/// tracking what looks like a leaked SGR sequence it reports [`Feed::Drop`],
/// swallowing the fragments *before* they reach `route_event` and mutate the
/// input line. The pattern is deliberately narrow so a genuine `Esc` keypress
/// still works: it only enters the suppression state on the `ESC [ <` prefix
/// (the mouse-sequence intro) — a bare `Esc` with nothing following stays a
/// real key.
///
/// The guard is best-effort at the symbol layer; the primary defense is the
/// reader-thread reassembler in `event_loop::InputReader`, which keeps whole
/// sequences intact in the common case so the guard rarely sees anything.
#[derive(Debug, Default, Clone)]
pub struct SgrLeakGuard {
    state: SgrState,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SgrState {
    /// Idle: no suspicious prefix seen.
    #[default]
    Idle,
    /// Saw `ESC`; waiting to see if `[` follows (start of a CSI).
    SawEsc,
    /// Saw `ESC [`; waiting for `<` (SGR mouse) — anything else aborts.
    SawCsi,
    /// Inside an SGR mouse payload after `ESC [ <`. Swallow digits/`;` and the
    /// terminating `M`/`m`, then return to idle.
    InSgr,
}

/// Outcome of feeding one event to the guard.
pub enum Feed {
    /// The event is not part of a leaked sequence — handle it normally.
    Accept,
    /// The event looks like part of a leaked SGR sequence — drop it silently.
    Drop,
}

impl SgrLeakGuard {
    /// Feed one event. Returns whether the caller should still process it.
    /// Pure: performs no I/O and never mutates the input line.
    pub fn feed(&mut self, event: &Event) -> Feed {
        let Event::Key(key) = event else {
            // A non-key event (Mouse/Resize/Paste/Focus) always resets the
            // tracker: if crossterm *did* manage to parse a whole mouse event
            // we clearly are no longer mid-leak, and a resize is exactly the
            // disruption that starts one, so resync here.
            self.state = SgrState::Idle;
            return Feed::Accept;
        };
        let c = match key.code {
            KeyCode::Char(c) => c,
            // Esc as a control key (not a printable char) — a possible SGR
            // prefix start. Treat it as the intro byte.
            KeyCode::Esc => '\x1b',
            _ => {
                // Any other real key (Backspace, arrows, F-keys, Enter, …)
                // breaks a half-formed sequence.
                self.state = SgrState::Idle;
                return Feed::Accept;
            }
        };

        // The match returns (next_state, is_part_of_sequence). A character is
        // "part of a leaked sequence" — and therefore dropped — only when it is
        // a payload byte of an `ESC [ < …` mouse report (the `[`, `<`, digits,
        // `;`, and the `M`/`m` terminator). A bare `Esc` keypress is *never*
        // dropped: it is a real control key (never inserted as text), it is the
        // double-Esc interrupt path, and it clears focus / closes modals.
        // Dropping it silently — as the first version of this guard did — broke
        // double-Esc interrupt entirely. Instead we *deliver* the Esc (Accept)
        // and merely enter the tracking state, so the `[` that follows a
        // genuine leak still starts suppression without ever swallowing the Esc
        // itself.
        let (next, part) = match (self.state, c) {
            // A bare Esc from idle: deliver it, but arm the tracker so a
            // following `[` still opens a leak window.
            (SgrState::Idle, '\x1b') => (SgrState::SawEsc, false),
            // `ESC [`: the `[` is the first byte that can only be leak noise
            // (a real `[` key arrives as a printable char from idle), so start
            // suppressing here. The leading Esc was already delivered above.
            (SgrState::SawEsc, '[') => (SgrState::SawCsi, true),
            // The SGR mouse intro. Once we see this prefix the rest of the
            // payload is unambiguously a mouse report fragment.
            (SgrState::SawCsi, '<') => (SgrState::InSgr, true),
            // Terminators: the final byte of the report.
            (SgrState::InSgr, 'M') | (SgrState::InSgr, 'm') => (SgrState::Idle, true),
            // Continuation bytes of the payload.
            (SgrState::InSgr, '0'..='9' | ';' | '\u{1b}') => (SgrState::InSgr, true),
            // Aborted sequences: the bytes we tentatively buffered were not an
            // SGR mouse report after all. Hand the *current* char back for
            // normal processing (it is genuine input) and resync to idle.
            (SgrState::InSgr, _) => (SgrState::Idle, false),
            // A second Esc while one is already buffered: this is a genuine
            // double-Esc (the double-Esc interrupt pattern), not a leak — a
            // real SGR sequence has `[` next, never another Esc. Deliver it and
            // stay armed so the next non-`[` char cleanly aborts to idle.
            (SgrState::SawEsc, '\x1b') => (SgrState::SawEsc, false),
            (SgrState::SawEsc | SgrState::SawCsi, _) => (SgrState::Idle, false),
            (SgrState::Idle, _) => (SgrState::Idle, false),
        };
        self.state = next;
        if part { Feed::Drop } else { Feed::Accept }
    }

    /// Reset the tracker. Called after a resize so a fresh, fully-armed mouse
    /// session starts from a known state.
    pub fn reset(&mut self) {
        self.state = SgrState::Idle;
    }

    /// Whether the tracker is currently idle (not mid-sequence). Used by the
    /// reader-thread reassembler to know when a drain has completed.
    pub fn is_idle(&self) -> bool {
        self.state == SgrState::Idle
    }
}
