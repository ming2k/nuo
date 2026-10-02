//! The crossterm backend: turn a [`DrawCmd`] into the minimal escape-code
//! delta on stdout, with BCE (back-color-erase) awareness.
//!
//! # Responsibilities
//!
//! - Track the **current** cursor position and applied style across draws, so
//!   consecutive runs that share a style emit no SGR, and a run already at the
//!   right position emits no cursor move. This is the cell-level minimization
//!   vim's TUI frontend does.
//! - Detect `bce` from the `TERM`-derived terminfo capability and, when
//!   available, clear a dirty blank tail with `clr_eol` (`\x1b[K`) instead of
//!   writing per-cell spaces. Without `bce`, blank tails are painted as styled
//!   space cells (the only correct fallback).
//! - Own the crossterm `Write` sink. The engine never touches `stdout`
//!   directly except through this backend.
//!
//! The backend is the only place `crossterm` import leaks into the engine's
//! runtime path; the grid/diff modules stay pure and testable.

use std::io::{self, Write};

use crossterm::{
    QueueableCommand, cursor,
    terminal::{self, ClearType, DisableLineWrap, EnableLineWrap},
};

use crate::diff::{Draw, DrawCmd};
use crate::driver::{EscapeEmitter, TerminalDriver};
use crate::profile::TerminalProfile;
use crate::{Color, Style};

/// Whether the terminal advertises back-color-erase (`bce`).
///
/// When `Bce` is available, clearing a line tail to the current background is
/// a single `\x1b[K` that inherits the active bg. Without it, the backend
/// must write explicit space cells styled with the target background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bce {
    Yes,
    No,
}

impl Bce {
    /// Detect `bce` from the environment. This checks `TERM` against the set
    /// of terminals known to set the `bce` capability, plus an explicit
    /// override (`MUTA_BCE=1` forces it on, `MUTA_BCE=0` forces it off).
    ///
    /// We do not shell out to `tput`/`infocmp` (slow, not always present);
    /// the known-good list covers the terminals muta targets (xterm,
    /// xterm-256color, foot, tmux, screen with BCE, kitty, wezterm, alacritty
    /// via its xterm emulation). This matches how other Rust TUI stacks
    /// approximate the capability.
    /// Detect `bce` from the environment (reads `TERM` and `MUTA_BCE`).
    /// Shells out to the pure [`Bce::for_term`] helper.
    pub fn detect() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let override_str = std::env::var("MUTA_BCE").ok();
        Self::for_term(&term, override_str.as_deref())
    }

    /// Pure detection logic, separable from the environment for tests.
    ///
    /// `term` is the value of `TERM`; `override_str` is the optional
    /// `MUTA_BCE` override (`"1"` forces on, `"0"` forces off). The
    /// known-bce set covers the terminals muta targets (xterm, foot, tmux,
    /// screen with BCE, kitty, wezterm, alacritty via its xterm emulation).
    /// Unknown `TERM` values default to `No` so we never emit `clr_eol` to a
    /// terminal that won't honor the current bg.
    pub fn for_term(term: &str, override_str: Option<&str>) -> Self {
        if let Some(v) = override_str {
            return match v {
                "1" | "true" | "yes" => Bce::Yes,
                "0" | "false" | "no" => Bce::No,
                _ => Self::from_known(term),
            };
        }
        Self::from_known(term)
    }

    fn from_known(term: &str) -> Self {
        const BCE_TERMS: &[&str] = &[
            "xterm",
            "xterm-256color",
            "xterm-direct",
            "foot",
            "foot-extra",
            "kitty",
            "kitty-direct",
            "wezterm",
            "alacritty",
        ];
        let base = term.split('+').next().unwrap_or(term);
        if BCE_TERMS.contains(&base) || BCE_TERMS.contains(&term) {
            Bce::Yes
        } else {
            Bce::No
        }
    }
}

impl Default for Bce {
    fn default() -> Self {
        Self::detect()
    }
}

/// The crossterm-backed renderer. Owns the output writer and the
/// "what's currently on screen" tracking (cursor pos + last applied style),
/// so each draw emits only the delta.
pub struct Backend<W: Write> {
    out: W,
    bce: Bce,
    driver: TerminalDriver,
    /// Last cursor position we moved to. `None` until the first move, which
    /// means the next draw must always reposition.
    cur: Option<(u16, u16)>,
    /// Last cursor **visibility** we set (`?25h`/`?25l`). `None` until the
    /// first hide/show, which means the next one always emits — exactly like
    /// `cur` for position. Tracking this lets us skip re-emitting the same
    /// visibility sequence every frame, which on light, frequent incremental
    /// redraws shows up as a caret flicker.
    cursor_visible: Option<bool>,
}

impl<W: Write> Backend<W> {
    /// Wrap an output writer (typically `io::stdout()`), detecting `bce` and
    /// terminal profile from the environment.
    pub fn new(out: W) -> Self {
        let profile = TerminalProfile::detect();
        Self::with_bce_and_driver(out, Bce::detect(), TerminalDriver::for_profile(&profile))
    }

    /// Construct with an explicit `bce` setting (for tests / overrides).
    pub fn with_bce(out: W, bce: Bce) -> Self {
        let profile = TerminalProfile::detect();
        Self::with_bce_and_driver(out, bce, TerminalDriver::for_profile(&profile))
    }

    /// Construct with an explicit [`TerminalProfile`] setting.
    pub fn with_profile(out: W, profile: TerminalProfile) -> Self {
        Self::with_bce_and_driver(out, Bce::detect(), TerminalDriver::for_profile(&profile))
    }

    /// Construct with an explicit [`TerminalDriver`] setting.
    pub fn with_driver(out: W, driver: TerminalDriver) -> Self {
        Self::with_bce_and_driver(out, Bce::detect(), driver)
    }

    /// Construct with explicit `bce` and [`TerminalProfile`] settings.
    pub fn with_bce_and_profile(out: W, bce: Bce, profile: TerminalProfile) -> Self {
        Self::with_bce_and_driver(out, bce, TerminalDriver::for_profile(&profile))
    }

    /// Construct with explicit `bce` and [`TerminalDriver`] settings.
    pub fn with_bce_and_driver(out: W, bce: Bce, driver: TerminalDriver) -> Self {
        Self {
            out,
            bce,
            driver,
            cur: None,
            cursor_visible: None,
        }
    }

    /// Return the active terminal driver.
    pub fn driver(&self) -> TerminalDriver {
        self.driver
    }

    /// Borrow the underlying writer (for the app to queue alt-screen, raw
    /// mode, etc. via crossterm directly).
    pub fn writer(&mut self) -> &mut W {
        &mut self.out
    }

    /// Open a DEC synchronized-update (mode 2026) envelope: queue the begin
    /// marker *without* flushing, so it reaches the terminal in the same
    /// ordered stream as (and ahead of) everything written until the matching
    /// [`Self::end_sync_update`]. Delegated to the driver (no-op on legacy terminals).
    pub fn begin_sync_update(&mut self) -> io::Result<()> {
        self.driver.begin_sync(&mut self.out)
    }

    /// Close a synchronized-update envelope opened by
    /// [`Self::begin_sync_update`], presenting everything written in between
    /// atomically. Delegated to the driver.
    pub fn end_sync_update(&mut self) -> io::Result<()> {
        self.driver.end_sync(&mut self.out)
    }

    /// Apply a diff's draw commands: move the cursor into place, set the
    /// style delta, and write each run's symbols. Returns the number of draw
    /// commands processed.
    pub fn render(&mut self, cmd: &DrawCmd) -> io::Result<usize> {
        let terminal_w = cmd.w;
        let terminal_h = cmd.h;
        let mut processed = 0usize;

        for draw in &cmd.draws {
            processed += 1;
            match draw {
                Draw::ScrollDown { y, height, amount } => {
                    self.scroll_region(*y, *height, *amount, terminal_h, false)?;
                }
                Draw::ScrollUp { y, height, amount } => {
                    self.scroll_region(*y, *height, *amount, terminal_h, true)?;
                }
                Draw::Cells { x, y, style, cells } => {
                    self.move_to(*x, *y)?;
                    self.apply_style(*style)?;
                    let mut current_x = *x;
                    for (sym, w) in cells {
                        let reaches_bottom_right = *y == terminal_h.saturating_sub(1)
                            && current_x + (*w as u16) >= terminal_w;
                        if reaches_bottom_right {
                            self.out.queue(DisableLineWrap)?;
                        }
                        self.out.queue(crossterm::style::Print(sym.as_str()))?;
                        if reaches_bottom_right {
                            self.out.queue(EnableLineWrap)?;
                        }
                        current_x += *w as u16;
                        // Advance our tracked cursor by the glyph's width; the
                        // trailing continuation column is implicit.
                        if let Some((cx, _cy)) = self.cur.as_mut() {
                            *cx = cx.saturating_add(*w as u16);
                        }
                    }
                }
                Draw::ClearEol { x, y, style, width } => {
                    self.move_to(*x, *y)?;
                    self.apply_style(*style)?;
                    let use_bce = matches!(self.bce, Bce::Yes) && style.bg == Color::Reset;
                    if use_bce {
                        // `\x1b[K` clears from the cursor to EOL with the
                        // currently-set background (which is default/reset here).
                        self.out.queue(terminal::Clear(ClearType::UntilNewLine))?;
                    } else {
                        // No BCE or non-default background: paint explicit styled spaces to the edge.
                        let reaches_bottom_right = *y == terminal_h.saturating_sub(1)
                            && (*x).saturating_add(*width) >= terminal_w;
                        if reaches_bottom_right {
                            self.out.queue(DisableLineWrap)?;
                        }
                        if *width > 0 {
                            let spaces = " ".repeat(*width as usize);
                            self.out.queue(crossterm::style::Print(spaces))?;
                        }
                        if reaches_bottom_right {
                            self.out.queue(EnableLineWrap)?;
                        }
                        if let Some((cx, _cy)) = self.cur.as_mut() {
                            *cx = cx.saturating_add(*width);
                        }
                    }
                }
            }
        }
        Ok(processed)
    }

    /// Scroll the screen rows `[y, y+height)` using DECSTBM (scroll region)
    /// plus `CSI S` (SU, content up) or `CSI T` (SD, content down). The
    /// region is set, the scroll issued, and the region reset — all queued in
    /// order. Cursor position tracking is reset to unknown because the
    /// terminal's post-scroll cursor position is not specified by the
    /// standard and varies by terminal.
    fn scroll_region(
        &mut self,
        y: u16,
        height: u16,
        amount: u16,
        terminal_h: u16,
        up: bool,
    ) -> io::Result<()> {
        if amount == 0 || height == 0 {
            return Ok(());
        }
        let top = y;
        // DECSTBM bounds are inclusive 1-based rows.
        let bottom = y.saturating_add(height).min(terminal_h).saturating_sub(1);
        if bottom < top {
            return Ok(());
        }
        // Set the scroll region, scroll, restore the region. `CSI r` with no
        // args resets to the full screen. SGR is not touched by these ops.
        write!(self.out, "\x1b[{};{}r", top + 1, bottom + 1)?;
        // Moving to the region's home keeps the cursor inside the region
        // across the scroll on every xterm-family terminal (SU/SD scroll the
        // region regardless of cursor position, but the cursor itself may be
        // clamped into it — starting at the home makes the clamp a no-op).
        write!(self.out, "\x1b[{};1H", top + 1)?;
        if up {
            write!(self.out, "\x1b[{}S", amount)?;
        } else {
            write!(self.out, "\x1b[{}T", amount)?;
        }
        // Reset the scroll region; the cursor tracking is invalidated below.
        write!(self.out, "\x1b[r")?;
        self.cur = None; // terminal moved the cursor; forget our tracking
        Ok(())
    }

    /// Move the terminal cursor to `(x, y)` if we aren't already there.
    fn move_to(&mut self, x: u16, y: u16) -> io::Result<()> {
        if self.cur == Some((x, y)) {
            return Ok(());
        }
        self.out.queue(cursor::MoveTo(x, y))?;
        self.cur = Some((x, y));
        Ok(())
    }

    /// Hide the terminal cursor, deduped against the last visibility we set.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        if self.cursor_visible != Some(false) {
            self.out.queue(cursor::Hide)?;
            self.cursor_visible = Some(false);
        }
        Ok(())
    }

    /// Hide the terminal cursor and park its physical position at `(x, y)`.
    ///
    /// Some terminal/multiplexer/IME stacks still sample the hidden cursor's
    /// physical coordinate as a composition-window hint. Parking it after
    /// hiding keeps "no visible caret" states from inheriting the last diff
    /// write position, which can move during streaming transcript updates.
    pub fn hide_cursor_at(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.hide_cursor()?;
        self.move_to(x, y)
    }

    /// Show the terminal cursor, deduped against the last visibility we set.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        if self.cursor_visible != Some(true) {
            self.out.queue(cursor::Show)?;
            self.cursor_visible = Some(true);
        }
        Ok(())
    }

    /// Show the terminal cursor at `(x, y)`, keeping the backend's cursor
    /// tracker in sync with the real terminal.
    ///
    /// Positioning happens while the cursor is still hidden. Showing first
    /// would expose the last draw coordinate for one terminal update before
    /// the final `MoveTo`, which is visible as a jumping caret whenever a
    /// terminal, multiplexer, or IME does not present synchronized updates as
    /// one indivisible operation.
    pub fn show_cursor_at(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.move_to(x, y)?;
        self.show_cursor()
    }

    /// Apply style delta using the configured terminal protocol driver.
    fn apply_style(&mut self, want: Style) -> io::Result<()> {
        self.driver.apply_style(want, &mut self.out)
    }

    /// Reset style/cursor tracking — call after the app does a wholesale
    /// screen clear or enters the alt screen, where the terminal's state no
    /// longer matches our tracked style.
    pub fn invalidate(&mut self) -> io::Result<()> {
        self.driver.invalidate(&mut self.out)?;
        self.cur = None;
        self.cursor_visible = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;
    use crate::grid::{Fit, Grid};

    /// Capture backend: accumulates the raw bytes emitted so tests can assert
    /// the exact escape sequence without a real terminal.
    fn render_to_string(cmd: &DrawCmd, bce: Bce) -> String {
        crossterm::style::force_color_output(true);
        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce_and_driver(
                &mut buf,
                bce,
                TerminalDriver::for_profile(&crate::profile::TerminalProfile::direct_color()),
            );
            be.render(cmd).unwrap();
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_diff_emits_nothing() {
        let cmd = DrawCmd::default();
        assert_eq!(render_to_string(&cmd, Bce::Yes), "");
    }

    #[test]
    fn single_run_emits_move_sgr_and_text() {
        crossterm::style::force_color_output(true);
        let mut back = Grid::new(4, 1);
        back.put(
            0,
            0,
            Fit::Clip,
            Style::default().fg(Color::Rgb(1, 2, 3)),
            "ab",
        );
        let front = Grid::new(4, 1);
        let cmd = crate::diff::diff(&back, &front);
        let s = render_to_string(&cmd, Bce::Yes);
        // crossterm emits RGB foreground as `\x1b[38;2;r;g;bm`.
        assert!(s.contains("\x1b[38;2;1;2;3m"), "fg SGR present: {s:?}");
        assert!(s.contains("ab"));
    }

    #[test]
    fn repeated_style_emits_no_duplicate_sgr() {
        crossterm::style::force_color_output(true);
        // Two adjacent runs with the same style should emit the SGR once.
        let mut back = Grid::new(4, 1);
        let style = Style::default().fg(Color::Rgb(9, 9, 9));
        back.put(0, 0, Fit::Clip, style, "a");
        back.set(2, 0, Cell::narrow("b", style));
        let front = Grid::new(4, 1);
        let cmd = crate::diff::diff(&back, &front);
        let s = render_to_string(&cmd, Bce::Yes);
        // Count occurrences of the SGR set; should appear exactly once.
        let count = s.matches("\x1b[38;2;9;9;9m").count();
        assert_eq!(count, 1, "SGR emitted once, got: {s:?}");
    }

    #[test]
    fn cursor_positioning_keeps_next_frame_draws_honest() {
        crossterm::style::force_color_output(true);
        let first = DrawCmd {
            w: 4,
            h: 3,
            draws: vec![Draw::Cells {
                x: 0,
                y: 0,
                style: Style::default(),
                cells: vec![("A".into(), 1)],
            }],
        };
        let second = DrawCmd {
            w: 4,
            h: 3,
            draws: vec![Draw::Cells {
                // This is exactly where the first render left the backend's
                // cursor tracker. If the visible caret move below does not
                // update that tracker, the second render omits MoveTo and
                // writes at the real caret position instead.
                x: 1,
                y: 0,
                style: Style::default(),
                cells: vec![("B".into(), 1)],
            }],
        };

        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce(&mut buf, Bce::Yes);
            be.render(&first).unwrap();
            be.show_cursor_at(0, 2).unwrap();
            assert_eq!(be.cur, Some((0, 2)));
            be.render(&second).unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let caret_move = s
            .find("\x1b[3;1H")
            .expect("caret move to row 3 col 1 must be emitted");
        let redraw_move = s[caret_move..]
            .find("\x1b[1;2H")
            .expect("next frame must move back to row 1 col 2 before drawing")
            + caret_move;
        let redraw_text = s[redraw_move..]
            .find('B')
            .expect("second frame text must be emitted")
            + redraw_move;
        assert!(redraw_move < redraw_text, "MoveTo must precede B: {s:?}");
    }

    #[test]
    fn show_cursor_at_positions_before_revealing_the_cursor() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce(&mut buf, Bce::Yes);
            be.hide_cursor().unwrap();
            be.show_cursor_at(5, 7).unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let move_at = s
            .find("\x1b[8;6H")
            .expect("final caret MoveTo must be emitted");
        let show_at = s.find("\x1b[?25h").expect("cursor Show must be emitted");
        assert!(
            move_at < show_at,
            "the cursor must be positioned while hidden, then shown: {s:?}"
        );
    }

    #[test]
    fn hidden_cursor_can_be_parked_at_stable_coordinate() {
        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce(&mut buf, Bce::Yes);
            be.hide_cursor_at(3, 2).unwrap();
            assert_eq!(be.cur, Some((3, 2)));
        }

        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("\x1b[?25l"),
            "cursor hide must be emitted: {s:?}"
        );
        assert!(
            s.contains("\x1b[3;4H"),
            "hidden cursor must be moved to row 3 col 4: {s:?}"
        );
    }

    #[test]
    fn repeated_hide_does_not_reamit_visibility_sequence() {
        // Regression for the post-incremental-sync flicker: a redraw that
        // leaves the cursor hidden must not re-queue `?25l` every frame just
        // because `Terminal::draw` parks the hidden caret each tick.
        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce(&mut buf, Bce::Yes);
            be.hide_cursor_at(3, 2).unwrap();
            // Second frame, different parking coordinate — visibility is
            // unchanged, so only the move may re-emit, never `?25l`.
            be.hide_cursor_at(4, 5).unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let hide_count = s.matches("\x1b[?25l").count();
        assert_eq!(hide_count, 1, "hide emitted once, got: {s:?}");
        // Position tracking is independent and must still move to the new park.
        assert!(
            s.contains("\x1b[6;5H"),
            "second park must move to row 6 col 5: {s:?}"
        );
    }

    #[test]
    fn show_hide_toggle_emits_only_on_transition() {
        // Visibility dedup must emit on a real state change and stay silent
        // otherwise — including the very first call (cursor_visible starts
        // unknown, so it always fires, exactly like `cur` for position).
        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce(&mut buf, Bce::Yes);
            be.hide_cursor().unwrap(); // None -> Some(false): emit ?25l
            be.hide_cursor().unwrap(); // Some(false): silent
            be.show_cursor().unwrap(); // -> Some(true): emit ?25h
            be.show_cursor().unwrap(); // Some(true): silent
            be.hide_cursor().unwrap(); // -> Some(false): emit ?25l
        }

        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.matches("\x1b[?25l").count(),
            2,
            "hide emitted on transitions only: {s:?}"
        );
        assert_eq!(
            s.matches("\x1b[?25h").count(),
            1,
            "show emitted on transition only: {s:?}"
        );
    }

    #[test]
    fn invalidate_forces_next_visibility_to_reemit() {
        // After a wholesale screen reset (alt-screen / resize), the terminal's
        // real cursor visibility is outside our control, so `invalidate` must
        // forget our tracked state and force the next hide/show to re-emit.
        let mut buf = Vec::new();
        {
            let mut be = Backend::with_bce(&mut buf, Bce::Yes);
            be.hide_cursor().unwrap(); // emit ?25l
            be.hide_cursor().unwrap(); // silent
            be.invalidate().unwrap();
            let _ = be.writer().flush();
            be.hide_cursor().unwrap(); // must re-emit after invalidate
        }

        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.matches("\x1b[?25l").count(),
            2,
            "invalidate resets visibility tracking: {s:?}"
        );
    }

    #[test]
    fn non_bce_clear_eol_paints_bottom_right_under_line_wrap_guard() {
        crossterm::style::force_color_output(true);
        let cmd = DrawCmd {
            w: 4,
            h: 2,
            draws: vec![Draw::ClearEol {
                x: 2,
                y: 1,
                style: Style::default().bg(Color::Rgb(7, 8, 9)),
                width: 2,
            }],
        };

        let s = render_to_string(&cmd, Bce::No);
        let guard_off = s
            .find("\x1b[?7l")
            .expect("bottom-right clear must disable line wrap");
        let guard_on = s[guard_off..]
            .find("\x1b[?7h")
            .expect("bottom-right clear must restore line wrap")
            + guard_off;
        let spaces = s[guard_off..guard_on].matches(' ').count();
        assert_eq!(spaces, 2, "clear must paint through the corner: {s:?}");
    }

    #[test]
    fn scroll_up_emits_scroll_region_and_su() {
        crossterm::style::force_color_output(true);
        let cmd = DrawCmd {
            w: 20,
            h: 10,
            draws: vec![Draw::ScrollUp {
                y: 1,
                height: 8,
                amount: 2,
            }],
        };
        let s = render_to_string(&cmd, Bce::Yes);
        // DECSTBM for rows 2..=9 (1-based), home cursor inside, SU by 2, reset.
        assert!(s.contains("\x1b[2;9r"), "scroll region set: {s:?}");
        assert!(s.contains("\x1b[2;1H"), "cursor homed inside region: {s:?}");
        assert!(s.contains("\x1b[2S"), "scroll-up issued: {s:?}");
        assert!(s.contains("\x1b[r"), "scroll region reset: {s:?}");
    }

    #[test]
    fn scroll_down_emits_scroll_region_and_sd() {
        crossterm::style::force_color_output(true);
        let cmd = DrawCmd {
            w: 20,
            h: 10,
            draws: vec![Draw::ScrollDown {
                y: 0,
                height: 10,
                amount: 1,
            }],
        };
        let s = render_to_string(&cmd, Bce::Yes);
        assert!(s.contains("\x1b[1;10r"), "full-screen region: {s:?}");
        assert!(s.contains("\x1b[1T"), "scroll-down issued: {s:?}");
        assert!(s.contains("\x1b[r"), "region reset: {s:?}");
    }

    #[test]
    fn bce_detection_defaults_for_known_terms() {
        assert_eq!(Bce::for_term("xterm-256color", None), Bce::Yes);
        assert_eq!(Bce::for_term("tmux-256color", None), Bce::No);
        assert_eq!(Bce::for_term("foot", None), Bce::Yes);
        assert_eq!(Bce::for_term("dumb", None), Bce::No);
        assert_eq!(Bce::for_term("unknown-term", None), Bce::No);
    }

    #[test]
    fn bce_override_env_wins() {
        // Override forces on/off regardless of TERM.
        assert_eq!(Bce::for_term("xterm-256color", Some("0")), Bce::No);
        assert_eq!(Bce::for_term("dumb", Some("1")), Bce::Yes);
        // Garbage override falls back to the TERM-based decision.
        assert_eq!(Bce::for_term("xterm-256color", Some("maybe")), Bce::Yes);
    }
    /// The out-of-band sync-update primitives must bracket atomically: begin
    /// queues without flushing, end queues + flushes, and the emitted bytes
    /// are exactly `?2026h` … `?2026l`.
    #[test]
    fn sync_update_envelope_brackets_out_of_band_writes() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::with_bce_and_driver(
                &mut buf,
                Bce::Yes,
                TerminalDriver::for_profile(&crate::profile::TerminalProfile::direct_color()),
            );
            be.begin_sync_update().unwrap();
            be.show_cursor_at(5, 7).unwrap();
            be.end_sync_update().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("\x1b[?2026h"), "envelope opens first: {s:?}");
        assert!(s.ends_with("\x1b[?2026l"), "envelope closes last: {s:?}");
        // `MoveTo(x=5, y=7)` is 1-based in the escape: row 8, column 6.
        assert!(
            s.contains("\x1b[?25h") && s.contains("\x1b[8;6H"),
            "the out-of-band caret write sits inside the envelope: {s:?}"
        );
        assert!(
            s.find("\x1b[8;6H") < s.find("\x1b[?25h"),
            "the caret is positioned before it becomes visible: {s:?}"
        );
    }

    /// Repeated envelopes must not re-open while one is conceptually live;
    /// more importantly the *ordering* contract: every begin is followed by
    /// its end before the next begin (a begin/begin/end sequence would
    /// suspend updates past the intended window).
    #[test]
    fn sync_update_envelopes_never_nest() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::with_bce_and_driver(
                &mut buf,
                Bce::Yes,
                TerminalDriver::for_profile(&crate::profile::TerminalProfile::direct_color()),
            );
            for _ in 0..3 {
                be.begin_sync_update().unwrap();
                be.hide_cursor_at(0, 0).unwrap();
                be.end_sync_update().unwrap();
            }
        }
        let s = String::from_utf8(buf).unwrap();
        let begins = s.matches("\x1b[?2026h").count();
        let ends = s.matches("\x1b[?2026l").count();
        assert_eq!(begins, 3, "one begin per envelope: {s:?}");
        assert_eq!(ends, 3, "one end per envelope: {s:?}");
        // No begin appears between another begin and its end, and none is
        // left open: scan the marker occurrences in byte order and track
        // depth.
        let mut depth = 0i32;
        let mut pos = 0usize;
        let mut properly_nested = true;
        while let Some(rel) = s[pos..].find("\x1b[?2026") {
            let at = pos + rel;
            let is_begin = s[at + 7..].starts_with('h');
            if is_begin {
                depth += 1;
                if depth > 1 {
                    properly_nested = false;
                }
            } else {
                depth -= 1;
            }
            pos = at + 7;
        }
        assert!(properly_nested, "envelopes never nest: {s:?}");
        assert_eq!(depth, 0, "every envelope closes: {s:?}");
    }

    #[test]
    fn monochrome_profile_emits_no_color_escape_codes() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::with_profile(&mut buf, TerminalProfile::dec_vt100_monochrome());
            let cmd = DrawCmd {
                w: 10,
                h: 2,
                draws: vec![
                    Draw::Cells {
                        x: 0,
                        y: 0,
                        style: Style::default()
                            .fg(Color::Rgb(255, 0, 0))
                            .bg(Color::Rgb(0, 0, 255)),
                        cells: vec![("A".into(), 1)],
                    },
                    Draw::Cells {
                        x: 1,
                        y: 0,
                        style: Style::default().fg(Color::White),
                        cells: vec![("B".into(), 1)],
                    },
                ],
            };
            be.render(&cmd).unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        // Must contain NO TrueColor or ANSI color SGR codes
        assert!(
            !s.contains("\x1b[38;"),
            "no 38; color codes in monochrome: {s:?}"
        );
        assert!(
            !s.contains("\x1b[48;"),
            "no 48; color codes in monochrome: {s:?}"
        );
        assert!(
            !s.contains("\x1b[31m"),
            "no ANSI color codes in monochrome: {s:?}"
        );
        // Highlighted cell with bg tint should emit SGR 7 (reverse video)
        assert!(
            s.contains("\x1b[7m"),
            "highlighted cell uses reverse video: {s:?}"
        );
        assert!(s.contains('A'));
        assert!(s.contains('B'));
    }

    #[test]
    fn monochrome_profile_disables_sync_update() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::with_profile(&mut buf, TerminalProfile::dec_vt100_monochrome());
            be.begin_sync_update().unwrap();
            be.end_sync_update().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(
            !s.contains("?2026h"),
            "monochrome profile must not emit sync update: {s:?}"
        );
        assert!(
            !s.contains("?2026l"),
            "monochrome profile must not emit sync update: {s:?}"
        );
    }

    #[test]
    fn ansi16_profile_quantizes_rgb_to_standard_ansi() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut be = Backend::with_profile(&mut buf, TerminalProfile::ecma48_ansi16());
            let cmd = DrawCmd {
                w: 10,
                h: 2,
                draws: vec![Draw::Cells {
                    x: 0,
                    y: 0,
                    style: Style::default().fg(Color::Rgb(255, 10, 10)),
                    cells: vec![("X".into(), 1)],
                }],
            };
            be.render(&cmd).unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(
            !s.contains("\x1b[38;2;"),
            "ANSI16 profile must not emit DirectColor 38;2: {s:?}"
        );
        assert!(s.contains('X'));
    }
}
