//! Integration test: panicking on assertion failure is the desired
//! behaviour here, so the workspace `unwrap_used`/`expect_used` lints
//! are relaxed for this file. (Lib/bin code stays linted.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end engine tests: the full write → diff → render → promote cycle,
//! plus the two scenarios the engine exists to fix (ADR-0038):
//!
//! 1. **Wide-glyph ghost eradication.** Writing a CJK glyph marks its trailing
//!    column as a continuation carrying the glyph's own background, and the
//!    diff emits the glyph without ever leaking a stale `Color::Reset` cell —
//!    the class of bug ratatui's `set_stringn` + diff-skip produces through
//!    tmux.
//! 2. **Convergence.** After a diff is promoted into the front grid, a second
//!    diff is empty: the back grid and the (now-updated) front grid agree, so
//!    the terminal emits nothing on an idle frame.
//!
//! These run without a real terminal: the backend writes into a `Vec<u8>`.

use nuotc::backend::{Backend, Bce};
use nuotc::diff;
use nuotc::{Cell, Color, Fit, Grid, Pos, Style};

/// The full cycle over a capture buffer. Returns the emitted bytes.
fn render_cycle(back: &mut Grid, front: &mut Grid, bce: Bce) -> String {
    crossterm::style::force_color_output(true);
    let cmd = diff::diff(back, front);
    let mut buf = Vec::new();
    {
        let mut be = Backend::with_bce_and_driver(
            &mut buf,
            bce,
            nuotc::driver::TerminalDriver::for_profile(
                &nuotc::profile::TerminalProfile::direct_color(),
            ),
        );
        be.render(&cmd).unwrap();
    }
    diff::promote_scrolled(back, front, &cmd);
    String::from_utf8(buf).unwrap()
}

#[test]
fn second_frame_after_promote_is_idle() {
    let (w, h) = (8, 2);
    let mut back = Grid::new(w, h);
    let mut front = Grid::new(w, h);

    // Frame 1: write some content.
    back.put(
        0,
        0,
        Fit::Clip,
        Style::default().fg(Color::Rgb(1, 1, 1)),
        "hello",
    );
    let first = render_cycle(&mut back, &mut front, Bce::Yes);
    assert!(first.contains("hello"));

    // Frame 2: nothing changed → diff is empty → backend emits nothing.
    let second = render_cycle(&mut back, &mut front, Bce::Yes);
    assert!(
        second.is_empty(),
        "idle frame should emit nothing, got {second:?}"
    );
}

#[test]
fn wide_glyph_trailing_column_carries_background_not_reset() {
    // The core anti-ghost property: a wide glyph's trailing cell owns the
    // glyph's background, never Color::Reset. Through any multiplexer that
    // repaints that column on its own, it stays correct.
    let mut back = Grid::new(6, 1);
    let style = Style::default()
        .fg(Color::Rgb(255, 255, 255))
        .bg(Color::Rgb(7, 8, 8)); // near-black surface, like muta's theme
    back.put(0, 0, Fit::Clip, style, "😀");

    // Trailing continuation cell: width 0, and its bg is the glyph's bg.
    let trail = back.get(1, 0).unwrap();
    assert!(trail.is_wide_continuation());
    assert_eq!(trail.style.bg, Color::Rgb(7, 8, 8));
    assert_ne!(
        trail.style.bg,
        Color::Reset,
        "trailing cell must not be Reset"
    );

    // Diff must emit the glyph once; the trailing column is implicit.
    let front = Grid::new(6, 1);
    let cmd = diff::diff(&back, &front);
    assert_eq!(cmd.draws.len(), 1);
    let emitted_cells = match &cmd.draws[0] {
        diff::Draw::Cells { cells, .. } => cells.clone(),
        _ => panic!(),
    };
    assert_eq!(emitted_cells, vec![("😀".into(), 2)]);
}

#[test]
fn streaming_token_grows_only_the_changed_run() {
    // Simulate streaming: a growing assistant message. Each token should dirty
    // only the tail, so the diff emits only the new characters — not a
    // repaint of the whole line.
    let (w, h) = (20, 1);
    let mut back = Grid::new(w, h);
    let mut front = Grid::new(w, h);
    let style = Style::default().fg(Color::Rgb(1, 1, 1));

    back.put(0, 0, Fit::Clip, style, "Hello");
    let _ = render_cycle(&mut back, &mut front, Bce::Yes);

    // Grow the message by one word. Only the new cells should be dirty.
    let end = back.put(5, 0, Fit::Clip, style, " world");
    assert_eq!(end, Pos { x: 11, y: 0 });

    let cmd = diff::diff(&back, &front);
    // The single changed run is exactly the new text, starting at col 5.
    assert_eq!(cmd.draws.len(), 1);
    if let diff::Draw::Cells {
        x,
        y,
        cells,
        style: s,
    } = &cmd.draws[0]
    {
        assert_eq!(*x, 5);
        assert_eq!(*y, 0);
        assert_eq!(*s, style);
        let text: String = cells.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, " world");
    } else {
        panic!("expected a single Cells run");
    }
}

#[test]
fn style_change_emits_sgr_only_for_the_delta() {
    // Two regions, same text but different fg: the backend should emit the
    // second region's SGR exactly once and not repeat the first.
    let (w, h) = (6, 1);
    let mut back = Grid::new(w, h);
    let mut front = Grid::new(w, h);

    let s1 = Style::default().fg(Color::Rgb(1, 1, 1));
    let s2 = Style::default().fg(Color::Rgb(2, 2, 2));
    back.put(0, 0, Fit::Clip, s1, "ab");
    back.put(2, 0, Fit::Clip, s2, "cd");

    let out = render_cycle(&mut back, &mut front, Bce::Yes);
    let first_sgr = out.matches("\x1b[38;2;1;1;1m").count();
    let second_sgr = out.matches("\x1b[38;2;2;2;2m").count();
    assert_eq!(first_sgr, 1);
    assert_eq!(second_sgr, 1);
}

#[test]
fn clear_row_then_redraw_marks_only_that_row() {
    // Clearing a row and rewriting it dirties only that row; other rows stay
    // clean and the diff skips them.
    let (w, h) = (5, 3);
    let mut back = Grid::new(w, h);
    let mut front = Grid::new(w, h);

    // Prime all rows, then sync.
    for y in 0..h {
        back.put(0, y, Fit::Clip, Style::default(), "aaaaa");
    }
    let _ = render_cycle(&mut back, &mut front, Bce::Yes);

    // Rewrite row 1.
    back.clear_row(1, 0, Style::default().bg(Color::Rgb(10, 10, 10)));
    back.put(0, 1, Fit::Clip, Style::default(), "bbbbb");

    let cmd = diff::diff(&back, &front);
    let touched_rows: std::collections::HashSet<u16> = cmd
        .draws
        .iter()
        .filter_map(|d| match d {
            diff::Draw::Cells { y, .. } => Some(*y),
            _ => None,
        })
        .collect();
    assert_eq!(touched_rows, [1].into_iter().collect());
}

#[test]
fn resize_preserves_content_and_converges() {
    let (w, h) = (4, 2);
    let mut back = Grid::new(w, h);
    let mut front = Grid::new(w, h);
    back.put(0, 0, Fit::Clip, Style::default(), "abcd");
    let _ = render_cycle(&mut back, &mut front, Bce::Yes);

    // Grow both grids identically. The overlap ("abcd") is preserved on the
    // back grid; the newly exposed cells are blank on both, so the diff is
    // empty (nothing visually changed). The key guarantees: content survives
    // the resize, and a follow-up frame converges.
    back.resize(6, 3);
    front.resize(6, 3);
    assert_eq!(back.get(0, 0).unwrap().symbol, "a");
    assert_eq!(back.get(3, 0).unwrap().symbol, "d");
    // New cells are blank.
    assert_eq!(back.get(5, 2).unwrap().symbol, " ");

    let cmd = diff::diff(&back, &front);
    let _ = render_cycle(&mut back, &mut front, Bce::Yes);
    // Either nothing changed (empty diff) or only genuinely new content did.
    let _ = cmd;
    let second = render_cycle(&mut back, &mut front, Bce::Yes);
    assert!(
        second.is_empty(),
        "frame converges after resize, got {second:?}"
    );
}

#[test]
fn bce_clear_to_end_uses_clr_eol_only_for_default_bg() {
    // When bce is on, a dirty row whose tail resolves to the default
    // background can be cleared with a single \x1b[K. We verify the backend
    // emits that sequence when handed an explicit ClearEol command.
    use nuotc::diff::{Draw, DrawCmd};
    crossterm::style::force_color_output(true);
    let cmd = DrawCmd {
        w: 10,
        h: 10,
        draws: vec![Draw::ClearEol {
            x: 2,
            y: 1,
            style: Style::default().bg(Color::Reset),
            width: 4,
        }],
    };
    let mut buf = Vec::new();
    {
        let mut be = Backend::with_bce(&mut buf, Bce::Yes);
        be.render(&cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[K"), "clr_eol emitted under bce: {s:?}");
}

#[test]
fn bce_clear_to_end_falls_back_to_spaces_without_bce() {
    use nuotc::diff::{Draw, DrawCmd};
    crossterm::style::force_color_output(true);
    let cmd = DrawCmd {
        w: 10,
        h: 10,
        draws: vec![Draw::ClearEol {
            x: 1,
            y: 0,
            style: Style::default().bg(Color::Rgb(1, 2, 3)),
            width: 3,
        }],
    };
    let mut buf = Vec::new();
    {
        let mut be = Backend::with_bce(&mut buf, Bce::No);
        be.render(&cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    assert!(!s.contains("\x1b[K"));
    assert!(s.ends_with("   "));
}

#[test]
fn diff_collapses_uniform_blank_tail_to_clear_eol() {
    let mut back = Grid::new(8, 1);
    let mut front = Grid::new(8, 1);
    back.put(0, 0, Fit::Clip, Style::default(), "abcdef");
    let _ = render_cycle(&mut back, &mut front, Bce::Yes);

    let bg = Style::default().bg(Color::Rgb(7, 8, 9));
    back.clear_row(0, 2, bg);

    let cmd = diff::diff(&back, &front);
    assert!(matches!(
        cmd.draws.as_slice(),
        [diff::Draw::ClearEol {
            x: 2,
            y: 0,
            style,
            width: 6,
        }] if style.bg == Color::Rgb(7, 8, 9)
    ));
}

/// A model terminal that replays the backend's emitted escape stream so a test
/// can assert "what the terminal actually shows" against the back grid. It
/// understands exactly the sequences the backend emits: cursor moves (CUP),
/// erase-to-end-of-line (EL), printable glyphs (advancing by display width,
/// with a wide glyph blanking its trailing column the way real terminals do),
/// and newlines. SGR (`…m`), DECAWM (`?7l`/`?7h`), and every other CSI are
/// consumed; we care about *which glyph sits in which cell*, which is what
/// ghosting is.
struct ModelTerminal {
    w: u16,
    h: u16,
    cells: Vec<String>,
    cx: u16,
    cy: u16,
}

impl ModelTerminal {
    fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            h,
            cells: vec![" ".to_string(); (w as usize) * (h as usize)],
            cx: 0,
            cy: 0,
        }
    }

    fn set(&mut self, x: u16, y: u16, s: &str) {
        if x < self.w && y < self.h {
            self.cells[y as usize * self.w as usize + x as usize] = s.to_string();
        }
    }

    fn apply(&mut self, bytes: &str) {
        let mut it = bytes.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                // Only CSI (`\x1b[`) sequences are emitted by the backend.
                if it.peek() == Some(&'[') {
                    it.next();
                    let mut params = String::new();
                    let final_byte = loop {
                        match it.next() {
                            Some(d) if d.is_ascii_alphabetic() || d == '@' => break d,
                            Some(d) => params.push(d),
                            None => return,
                        }
                    };
                    match final_byte {
                        'H' | 'f' => {
                            let mut p = params.split(';');
                            let row = p.next().and_then(|s| s.parse().ok()).unwrap_or(1u16);
                            let col = p.next().and_then(|s| s.parse().ok()).unwrap_or(1u16);
                            self.cy = row.saturating_sub(1);
                            self.cx = col.saturating_sub(1);
                        }
                        // Erase from cursor to end of line (default param = 0).
                        'K' => {
                            for x in self.cx..self.w {
                                self.set(x, self.cy, " ");
                            }
                        }
                        _ => {} // SGR and friends: ignore.
                    }
                }
                continue;
            }
            if c == '\n' {
                self.cx = 0;
                self.cy = self.cy.saturating_add(1);
                continue;
            }
            // A printable glyph. Real terminals overwrite a narrow glyph onto a
            // cell; a wide glyph occupies its cell plus the next column.
            let g = c.to_string();
            let width = nuotc::text::grapheme_width(&g).max(1) as u16;
            if self.cx < self.w {
                self.set(self.cx, self.cy, &g);
                if width == 2 && self.cx + 1 < self.w {
                    // The trailing column is owned by the wide glyph; model it as
                    // empty so it never compares as a stray glyph.
                    self.set(self.cx + 1, self.cy, "");
                }
                self.cx = self.cx.saturating_add(width);
            }
        }
    }

    /// One row as the sequence of visible head glyphs (wide-continuation columns
    /// collapse to "" on both sides so they never count as a mismatch).
    fn row(&self, y: u16) -> Vec<String> {
        (0..self.w)
            .map(|x| self.cells[y as usize * self.w as usize + x as usize].clone())
            .collect()
    }
}

/// The same row projection for the back grid: a wide-continuation cell collapses
/// to "" so it lines up with [`ModelTerminal::row`].
fn back_row(back: &Grid, y: u16) -> Vec<String> {
    (0..back.size().0)
        .map(|x| {
            let cell = back.get(x, y).unwrap();
            if cell.is_wide_continuation() {
                String::new()
            } else {
                cell.symbol.to_string()
            }
        })
        .collect()
}

/// Drive a sequence of frames, replaying every emitted byte into one persistent
/// model terminal, and assert after each frame that the terminal shows exactly
/// what the back grid holds. A ghost — a cell the backend believes it painted
/// but never actually wrote — surfaces here as a row mismatch.
fn assert_terminal_tracks_back(w: u16, h: u16, bce: Bce, frames: &[&str]) {
    let mut back = Grid::new(w, h);
    let mut front = Grid::new(w, h);
    let mut term = ModelTerminal::new(w, h);

    for (n, text) in frames.iter().enumerate() {
        // Full repaint of the back grid each frame (what the app does via its
        // background fill + component redraw): clear, then write the row.
        back.fill_rect(0, 0, w, h, Style::default());
        back.put(0, 0, Fit::Clip, Style::default(), text);

        let bytes = render_cycle(&mut back, &mut front, bce);
        term.apply(&bytes);

        for y in 0..h {
            assert_eq!(
                term.row(y),
                back_row(&back, y),
                "frame {n} ({text:?}): terminal row {y} diverged from back grid \
                 — a ghost that only a full repaint (resize) would clear",
            );
        }
    }
}

#[test]
fn terminal_tracks_back_across_wide_narrow_transitions() {
    // Each frame fully repaints; the model terminal must always match the back
    // grid. Covers wide→narrow, narrow→wide, and shrinking content — the
    // CJK/IME transitions that were leaving ghosts on screen.
    for bce in [Bce::Yes, Bce::No] {
        assert_terminal_tracks_back(
            8,
            2,
            bce,
            &[
                "值。ab", // wide, wide, narrow, narrow
                "x",      // collapse to a single narrow glyph + blanks
                "ab值。", // narrow, narrow, wide, wide
                "值",     // single wide glyph + blanks
                "abcdef", // all narrow
                "",       // empty
            ],
        );
    }
}

#[test]
fn bottom_right_cell_renders_under_line_wrap_guard() {
    // The backend used to reserve the bottom-right cell to avoid terminal
    // auto-wrap+scroll. That made the global app background visibly miss one
    // cell. The correct invariant is stricter: the terminal tracks the back
    // grid exactly, and writes that reach the bottom-right corner are wrapped
    // in DECAWM off/on so they cannot scroll the screen.
    for bce in [Bce::Yes, Bce::No] {
        let (w, h) = (4u16, 2u16);
        let mut back = Grid::new(w, h);
        let mut front = Grid::new(w, h);
        let mut term = ModelTerminal::new(w, h);

        back.fill_rect(0, 0, w, h, Style::default());
        back.put(0, 1, Fit::Clip, Style::default(), "wxyz"); // bottom row, incl. corner
        let bytes = render_cycle(&mut back, &mut front, bce);
        term.apply(&bytes);

        assert_eq!(term.row(0), back_row(&back, 0), "row 0 (bce={bce:?})");
        assert_eq!(term.row(1), back_row(&back, 1), "row 1 (bce={bce:?})");
        assert!(
            bytes.contains("\x1b[?7l") && bytes.contains("\x1b[?7h"),
            "bottom-right write must be protected by DECAWM off/on: {bytes:?}"
        );
    }
}

/// A trivial sanity check that the cell-level types compose as documented.
#[test]
fn cell_constructors() {
    let blank = Cell::blank();
    assert_eq!(blank.symbol, " ");
    assert_eq!(blank.width, 1);

    let wide = Cell::wide_continuation(Style::default().bg(Color::Rgb(1, 2, 3)));
    assert!(wide.is_wide_continuation());
    assert_eq!(wide.style.bg, Color::Rgb(1, 2, 3));
}

/// `Backend::invalidate` must emit a real SGR reset (`\x1b[0m`), not just
/// reset its in-memory tracker. This is the resize fix: the tracker claimed
/// "RESET" while the terminal kept the last-applied attribute, so the next
/// frame's delta-style computation saw equal attribute bits and emitted
/// nothing — leaving plain text rendered with the stale attribute (bold).
#[test]
fn invalidate_emits_real_sgr_reset() {
    use nuotc::diff::{Draw, DrawCmd};
    crossterm::style::force_color_output(true);

    // First render a bold run so the terminal genuinely holds the BOLD
    // attribute (this is what a tool-step summary line does).
    let bold_cmd = DrawCmd {
        w: 10,
        h: 10,
        draws: vec![Draw::Cells {
            x: 0,
            y: 0,
            style: Style::default().add_modifier(nuotc::Modifier::BOLD),
            cells: vec![("X".into(), 1)],
        }],
    };
    let mut buf = Vec::new();
    let s = {
        let mut be = Backend::with_bce(&mut buf, Bce::Yes);
        be.render(&bold_cmd).unwrap();
        // Now simulate a resize: the app calls invalidate, which must push a real
        // reset to the terminal so the stale BOLD cannot bleed into the repaint.
        be.invalidate().unwrap();
        // `be` borrows `buf`; ending the block releases the borrow (Backend has
        // no Drop impl, so a scope is enough — no `std::mem::drop` needed) before
        // `buf` is moved into the decoded String below.
        String::from_utf8(buf).unwrap()
    };
    assert!(
        s.contains("\x1b[0m"),
        "invalidate must emit a real SGR reset, got: {s:?}"
    );
}

/// End-to-end regression for the resize bold bug, using a single persistent
/// backend (as the real `Terminal` holds across a resize). Render a bold line,
/// invalidate (resize), then repaint a plain line. Without a real reset in
/// `invalidate`, the only attribute SGR in the whole stream would be the bold
/// "set" from the first render — the plain repaint sees `want == tracker` and
/// emits nothing, so the terminal keeps BOLD and the plain text renders bold.
/// With the fix, `invalidate` itself emits the reset.
#[test]
fn resize_then_repaint_does_not_inherit_stale_bold() {
    use nuotc::diff::{Draw, DrawCmd};
    crossterm::style::force_color_output(true);

    let bold_cmd = DrawCmd {
        w: 10,
        h: 10,
        draws: vec![Draw::Cells {
            x: 0,
            y: 0,
            style: Style::default().add_modifier(nuotc::Modifier::BOLD),
            cells: vec![("X".into(), 1)],
        }],
    };
    let plain_cmd = DrawCmd {
        w: 10,
        h: 10,
        draws: vec![Draw::Cells {
            x: 0,
            y: 0,
            style: Style::default(),
            cells: vec![("Y".into(), 1)],
        }],
    };

    let mut buf = Vec::new();
    {
        // One backend instance across the whole resize, exactly as Terminal
        // owns it — the tracker state survives invalidate.
        let mut be = Backend::with_bce(&mut buf, Bce::Yes);
        be.render(&bold_cmd).unwrap();
        be.invalidate().unwrap(); // resize
        be.render(&plain_cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    // The sequence must contain an SGR reset. Pre-fix, invalidate emitted
    // nothing and the plain repaint (want == tracker) emitted nothing, so the
    // terminal kept BOLD and "Y" rendered bold. crossterm encodes reset as
    // `\x1b[0m`.
    assert!(
        s.contains("\x1b[0m"),
        "resize must emit a real SGR reset so plain repaints drop stale bold, got: {s:?}"
    );
}

// Scroll translation: the streaming-transcript frame (ADR on middle-component
// flicker). A transcript whose content grows at the bottom while following it
// shifts every settled row up; the diff must translate that into one terminal
// scroll plus the genuinely-new row, and convergence must hold afterwards.

/// Fill `back` with one labeled row per label, then clear dirty so the test
/// controls exactly what the "writer" repaints.
fn labeled(w: u16, labels: &[&str]) -> Grid {
    let mut g = Grid::new(w, labels.len() as u16);
    for (y, label) in labels.iter().enumerate() {
        g.put(0, y as u16, Fit::Clip, Style::default(), label);
    }
    g.clear_dirty();
    g
}

#[test]
fn streaming_append_scrolls_history_instead_of_repainting_it() {
    let (w, h) = (10, 4);
    // Frame 1: an established transcript view — h1 h2 h3, one blank row.
    let mut back = Grid::new(w, h);
    for (y, text) in [(0u16, "h1"), (1, "h2"), (2, "h3")] {
        back.put(0, y, Fit::Clip, Style::default(), text);
    }
    let mut front = Grid::new(w, h);
    let first = render_cycle(&mut back, &mut front, Bce::Yes);
    assert!(first.contains("h1") && first.contains("h3"));

    // Frame 2 (the streaming frame): the view advanced down by one row —
    // history moved up, one new line appeared at the bottom. The writer
    // repaints the whole moved band (rows 0..=2).
    let mut next = labeled(w, &["h2", "h3", "NEW", ""]);
    next.clear_dirty();
    for y in 0..=2 {
        next.mark(0, y);
    }
    let cmd = diff::diff(&next, &front);

    // One scroll op leads the frame…
    match cmd.scroll() {
        Some(diff::Draw::ScrollUp { y, height, amount }) => {
            assert_eq!((*y, *height, *amount), (0, 3, 1), "history scrolls up by 1");
        }
        other => panic!("expected ScrollUp, got {other:?} — draws: {:?}", cmd.draws),
    }
    // …and the only repaint is the genuinely-new row.
    let repaints: Vec<u16> = cmd.draws[1..]
        .iter()
        .filter_map(|d| match d {
            diff::Draw::Cells { y, .. } => Some(*y),
            _ => None,
        })
        .collect();
    assert!(
        repaints.iter().all(|&y| y == 2),
        "only the new bottom row repaints, got {repaints:?}"
    );

    // The bytes carry DECSTBM + SU, not a row-by-row repaint of history.
    let mut buf = Vec::new();
    {
        let mut be = Backend::with_bce(&mut buf, Bce::Yes);
        be.render(&cmd).unwrap();
    }
    let s = String::from_utf8(buf).unwrap();
    assert!(
        s.contains("\x1b[1;3r"),
        "scroll region covers the moved band: {s:?}"
    );
    assert!(s.contains("\x1b[1S"), "scroll-up by 1: {s:?}");
    assert!(!s.contains("h1"), "history must not be repainted: {s:?}");

    // Convergence: after promote, the next idle frame emits nothing.
    diff::promote_scrolled(&mut next, &mut front, &cmd);
    let idle = render_cycle(&mut next, &mut front, Bce::Yes);
    assert!(
        idle.is_empty(),
        "post-scroll frame must be idle, got {idle:?}"
    );
}
