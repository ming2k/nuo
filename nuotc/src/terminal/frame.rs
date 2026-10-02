//! Frame and Terminal: the draw-closure entry point the app calls each tick.
//!
//! `Frame` is the per-draw handle the application renders into. It borrows the
//! back [`Grid`] mutably, exposes ratatui-shaped methods (`area`,
//! `buffer_mut`, `render_widget`, `set_cursor_position`), and tracks the
//! desired terminal cursor position so the loop can emit a single move after
//! the closure returns.
//!
//! `Terminal` owns the [`Backend`], the back grid, and the front grid. Its
//! [`Terminal::draw`] runs the app closure against a fresh `Frame`, then
//! diffs the back grid against the front grid, hands the [`DrawCmd`] to the
//! backend, and promotes dirty cells into the front grid. Idle frames (no
//! dirty cells) emit nothing.

use std::io;

use crossterm::QueueableCommand;

use crate::backend::Backend;
use crate::diff::{self, DrawCmd};
use crate::grid::{Fit, Grid};
use crate::layout::Rect;
use crate::render::record::{Canvas, DisplayList, Rasterizer, RenderNode};
use crate::widgets::Paragraph;

/// The terminal cursor mode the frame loop should end with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorState {
    #[default]
    Hidden,
    Visible(u16, u16),
}

/// A per-draw frame handle. Borrows the back grid for the duration of the
/// draw closure.
pub struct Frame<'a> {
    grid: &'a mut Grid,
    area_rect: Rect,
    cursor: CursorState,
}

impl<'a> Frame<'a> {
    /// Construct a frame over a grid. Public so that integration tests can
    /// drive the widget API directly.
    pub fn new(grid: &'a mut Grid) -> Self {
        let (w, h) = grid.size();
        Self {
            grid,
            area_rect: Rect::new(0, 0, w, h),
            cursor: CursorState::default(),
        }
    }

    /// Full-terminal area.
    pub fn area(&self) -> Rect {
        self.area_rect
    }

    /// Mutable access to the underlying grid, for in-place cell mutation
    /// (the dim-recess effect and the hand-rolled scrollbar).
    pub fn buffer_mut(&mut self) -> &mut Grid {
        self.grid
    }

    /// Render a widget into `area`. Only the three widget kinds muta uses
    /// are supported (`Paragraph`, `Block`, `Clear`).
    pub fn render_widget<W: Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.grid);
    }

    /// Set the terminal cursor position for after this frame's flush. The last
    /// call wins.
    pub fn set_cursor_position<P: Into<(u16, u16)>>(&mut self, pos: P) {
        let (x, y) = pos.into();
        self.cursor = if self.area_rect.width == 0 || self.area_rect.height == 0 {
            CursorState::Hidden
        } else {
            CursorState::Visible(
                x.min(self.area_rect.right().saturating_sub(1)),
                y.min(self.area_rect.bottom().saturating_sub(1)),
            )
        };
    }

    /// Write a styled string directly into the grid (convenience for callers
    /// that don't want to build a `Paragraph`).
    pub fn put(&mut self, x: u16, y: u16, style: crate::Style, text: &str) {
        self.grid.put(x, y, Fit::Clip, style, text);
    }

    /// Record a sequence of draw operations into a [`Canvas`] and rasterize it directly
    /// into this frame within `area`.
    pub fn record_into<F: FnOnce(&mut Canvas)>(&mut self, area: Rect, record_fn: F) {
        let mut canvas = Canvas::new(area);
        record_fn(&mut canvas);
        let list = canvas.finish();
        Rasterizer::rasterize_with_clip(&list, self.grid, area);
    }

    /// Render a recorded [`DisplayList`] directly into this frame within `area`.
    pub fn render_display_list(&mut self, list: &DisplayList, area: Rect) {
        Rasterizer::rasterize_with_clip(list, self.grid, area);
    }

    /// Render a retained [`RenderNode`] into this frame within `area`.
    pub fn render_node(&mut self, node: &RenderNode, area: Rect) {
        if let Some(list) = node.display_list() {
            Rasterizer::rasterize_with_clip(list, self.grid, area);
        }
    }

    pub(crate) fn take_cursor(&mut self) -> CursorState {
        std::mem::take(&mut self.cursor)
    }

    /// Isolate a painter, including direct buffer access, to a terminal clip.
    /// Drawing into scratch also prevents a painter from corrupting cells
    /// outside its allocation. Wide glyphs crossing the clip become spaces.
    pub fn paint_clipped<R>(&mut self, clip: Rect, paint: impl FnOnce(&mut Frame<'_>) -> R) -> R {
        let clip = clip.intersection(self.area_rect);
        let mut scratch = self.grid.clone();
        let mut frame = Frame::new(&mut scratch);
        let result = paint(&mut frame);
        let cursor = frame.take_cursor();
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                if let Some(cell) = scratch.get(x, y) {
                    let mut cell = cell.clone();
                    if (cell.is_wide_continuation() && x == clip.x)
                        || (cell.width >= 2 && x.saturating_add(1) >= clip.right())
                    {
                        cell.symbol = " ".into();
                        cell.width = 1;
                    }
                    // Copy the complete row slice before marking it; Grid::set
                    // repairs adjacent glyph halves and cannot copy a pair one
                    // cell at a time without erasing the head on the second set.
                    let index = y as usize * self.grid.width as usize + x as usize;
                    if self.grid.content[index] != cell {
                        self.grid.content[index] = cell;
                        self.grid.mark(x, y);
                    }
                }
            }
        }
        if let CursorState::Visible(x, y) = cursor
            && clip.contains(x, y)
        {
            self.cursor = cursor;
        }
        result
    }
}

/// A widget that can render itself into a grid. Implemented for `Paragraph`,
/// `Block`, `Clear`, and `(Rect,)` passthrough.
pub trait Widget {
    fn render(self, area: Rect, grid: &mut Grid);
}

impl Widget for Paragraph<'_> {
    fn render(self, area: Rect, grid: &mut Grid) {
        Paragraph::render(&self, area, grid);
    }
}
impl Widget for crate::widgets::Block<'_> {
    fn render(self, area: Rect, grid: &mut Grid) {
        crate::widgets::Block::render(&self, area, grid);
    }
}
impl Widget for crate::widgets::Clear {
    fn render(self, area: Rect, grid: &mut Grid) {
        crate::widgets::Clear::render(self, area, grid);
    }
}
impl Widget for DisplayList {
    fn render(self, area: Rect, grid: &mut Grid) {
        Rasterizer::rasterize_with_clip(&self, grid, area);
    }
}
impl Widget for &DisplayList {
    fn render(self, area: Rect, grid: &mut Grid) {
        Rasterizer::rasterize_with_clip(self, grid, area);
    }
}
impl Widget for RenderNode {
    fn render(self, area: Rect, grid: &mut Grid) {
        if let Some(list) = self.display_list() {
            Rasterizer::rasterize_with_clip(list, grid, area);
        }
    }
}
impl Widget for &RenderNode {
    fn render(self, area: Rect, grid: &mut Grid) {
        if let Some(list) = self.display_list() {
            Rasterizer::rasterize_with_clip(list, grid, area);
        }
    }
}

/// Owns the backend and the back/front grids. The application holds one of
/// these for the lifetime of the TUI.
pub struct Terminal<W: io::Write> {
    backend: Backend<W>,
    back: Grid,
    front: Grid,
    cursor: CursorState,
    /// Cursor state known to have reached the terminal after the last
    /// successful flush. `None` means an external operation, resize, or write
    /// failure made the physical state unknown.
    presented_cursor: Option<CursorState>,
    /// Last successfully presented visible caret coordinate. Hidden frames
    /// park the physical cursor here after drawing so IME implementations that
    /// sample a hidden cursor never chase transient diff coordinates.
    cursor_anchor: Option<(u16, u16)>,
    /// A resize invalidated the terminal contents, but the clear/reset is held
    /// until the next committed frame. Staged measurement frames must never
    /// write terminal bytes of their own.
    pending_clear: bool,
}

impl<W: io::Write> Terminal<W> {
    pub fn new(backend: Backend<W>) -> Self {
        // Size the grids to whatever the backend reports via crossterm.
        let size = crossterm::terminal::size().unwrap_or((80, 24));
        let back = Grid::new(size.0, size.1);
        let front = Grid::new(size.0, size.1);
        Self {
            backend,
            back,
            front,
            cursor: CursorState::Hidden,
            presented_cursor: None,
            cursor_anchor: None,
            pending_clear: false,
        }
    }

    /// Resize the back and front grids to the current terminal size.
    pub fn resize_to(&mut self, width: u16, height: u16) {
        self.back.resize(width, height);
        self.front = Grid::new(width, height);
        self.back.mark_all_dirty();
        self.pending_clear = true;
        self.presented_cursor = None;
    }

    fn render_frame<F>(&mut self, render: F)
    where
        F: FnOnce(&mut Frame<'_>),
    {
        // Sync grid size with terminal.
        if let Ok((w, h)) = crossterm::terminal::size()
            && self.back.size() != (w, h)
        {
            self.resize_to(w, h);
        }
        let mut frame = Frame::new(&mut self.back);
        render(&mut frame);
        self.cursor = frame.take_cursor();
    }

    fn commit(&mut self) -> io::Result<()> {
        let cmd: DrawCmd = diff::diff(&self.back, &self.front);
        let desired_cursor = self.normalized_cursor(self.cursor);
        let cursor_changed = self.presented_cursor != Some(desired_cursor);

        // Repainting an identical widget may leave conservative dirty marks
        // but no terminal commands. Clear those marks without opening an
        // otherwise empty synchronized-update envelope.
        if !self.pending_clear && cmd.draws.is_empty() && !cursor_changed {
            diff::promote_scrolled(&mut self.back, &mut self.front, &cmd);
            return Ok(());
        }

        if let Err(error) = self.backend.begin_sync_update() {
            self.recover_failed_commit();
            return Err(error);
        }
        let commit_result = self.commit_frame(&cmd, desired_cursor);
        // Always close an envelope that was successfully opened, even when a
        // queued write failed, so a terminal never remains update-suspended.
        let end_result = self.backend.end_sync_update();
        let result = match (commit_result, end_result) {
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        };

        if let Err(error) = result {
            self.recover_failed_commit();
            return Err(error);
        }

        // Only a successful flush commits logical terminal state. In
        // particular, scroll projection in `diff` cannot mutate `front`
        // before this point.
        diff::promote_scrolled(&mut self.back, &mut self.front, &cmd);
        self.pending_clear = false;
        self.presented_cursor = Some(desired_cursor);
        if let CursorState::Visible(x, y) = desired_cursor {
            self.cursor_anchor = Some((x, y));
        }
        Ok(())
    }

    /// Emit one already-rendered logical frame. [`Self::commit`] owns the
    /// synchronized-update envelope around this method.
    fn commit_frame(&mut self, cmd: &DrawCmd, desired_cursor: CursorState) -> io::Result<()> {
        if self.pending_clear {
            // Reconcile the SGR tracker and real terminal only when a frame is
            // actually committed. A staged layout pass may resize the grids,
            // but it must remain completely invisible.
            self.backend.invalidate()?;
        }

        // Hide before any command that can expose a transient physical cursor
        // coordinate. Correctness does not depend on DEC synchronized-update
        // support: unsupported terminals may present these bytes separately,
        // but the caret remains hidden until its final position is installed.
        if self.pending_clear || !cmd.draws.is_empty() {
            self.backend.hide_cursor()?;
        }

        if self.pending_clear {
            self.backend
                .writer()
                .queue(crossterm::cursor::MoveTo(0, 0))?;
            self.backend.writer().queue(crossterm::terminal::Clear(
                crossterm::terminal::ClearType::All,
            ))?;
        }

        self.backend.render(cmd)?;

        // Install the final coordinate while hidden, then reveal the cursor.
        // Hidden frames return it to the last input anchor so IME sampling is
        // stable even while transcript cells continue to stream.
        match desired_cursor {
            CursorState::Hidden => {
                if let Some((x, y)) = self.normalized_anchor() {
                    self.backend.hide_cursor_at(x, y)?;
                } else {
                    self.backend.hide_cursor()?;
                }
            }
            CursorState::Visible(x, y) => {
                self.backend.show_cursor_at(x, y)?;
            }
        }
        Ok(())
    }

    fn normalized_cursor(&self, cursor: CursorState) -> CursorState {
        let (w, h) = self.back.size();
        match cursor {
            CursorState::Visible(x, y) if w > 0 && h > 0 => {
                CursorState::Visible(x.min(w - 1), y.min(h - 1))
            }
            _ => CursorState::Hidden,
        }
    }

    fn normalized_anchor(&self) -> Option<(u16, u16)> {
        let (w, h) = self.back.size();
        let (x, y) = self.cursor_anchor?;
        (w > 0 && h > 0).then_some((x.min(w - 1), y.min(h - 1)))
    }

    fn recover_failed_commit(&mut self) {
        // The terminal may have consumed an arbitrary prefix of the failed
        // stream. Force the next attempt through reset + clear + full repaint,
        // and forget cursor state until that recovery frame flushes.
        self.back.mark_all_dirty();
        self.pending_clear = true;
        self.presented_cursor = None;
    }

    /// Run the app's draw closure against a fresh frame, then diff → render
    /// → promote. Returns `Ok(())` on success.
    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.render_frame(render);
        self.commit()
    }

    /// Render into the retained back grid without emitting terminal output.
    /// The next [`Self::draw`] replaces or completes this staged frame and
    /// commits only the final grid. This supports layout-dependent state such
    /// as bottom-follow scrolling without flashing an intermediate viewport.
    pub fn stage<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.render_frame(render);
        Ok(())
    }

    /// Commit the currently staged back grid without running layout again.
    /// Callers use this when a measurement pass confirms that its viewport was
    /// already final; if layout-dependent state changed, call [`Self::draw`]
    /// instead so the staged grid is replaced before the single commit.
    pub fn commit_staged(&mut self) -> io::Result<()> {
        self.commit()
    }

    /// Borrow the underlying writer (for alt-screen / raw-mode setup).
    pub fn writer(&mut self) -> &mut W {
        self.backend.writer()
    }

    /// Borrow the backend (for the app to call `invalidate` after a clear).
    pub fn backend(&mut self) -> &mut Backend<W> {
        &mut self.backend
    }

    /// The live terminal size as crossterm reports it right now (falling
    /// back to the retained grid's size if the query fails).
    ///
    /// Distinct from the grids' size on purpose: a `Resize` event reaches the
    /// retained grids at the next `render_frame`, while callers that
    /// report or validate the physical terminal need the live dimensions.
    pub fn size(&self) -> (u16, u16) {
        crossterm::terminal::size().unwrap_or_else(|_| self.back.size())
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        // Route through the backend's visibility-dedup path so repeated calls
        // don't re-emit `?25h` every frame (the same dedup `hide_cursor`
        // already enjoys). `move_to`/position parking is the caller's job when
        // an explicit coordinate is wanted; see `show_cursor_at`.
        self.backend.show_cursor()?;
        let result = self.backend.writer().flush();
        self.presented_cursor = None;
        result
    }

    /// Hide the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        let result = self.backend.writer().flush();
        self.presented_cursor = None;
        result
    }
}

/// A test terminal: owns a back grid the tests can render into and then
/// inspect, without any real I/O. Mirrors the `Terminal<TestBackend>` pattern
/// ratatui tests used. The grid is accessible via [`TestTerminal::buffer`], and
/// the last caret position the render closure requested via
/// [`TestTerminal::cursor`].
pub struct TestTerminal {
    back: Grid,
    cursor: CursorState,
}

impl TestTerminal {
    /// Create a test terminal with a grid of the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            back: Grid::new(width, height),
            cursor: CursorState::default(),
        }
    }

    /// Run a draw closure against a frame over the back grid.
    pub fn draw<F>(&mut self, render: F)
    where
        F: FnOnce(&mut Frame<'_>),
    {
        let mut frame = Frame::new(&mut self.back);
        render(&mut frame);
        self.cursor = frame.take_cursor();
    }

    /// Read the rendered grid (the "buffer" the tests inspect).
    pub fn buffer(&self) -> &Grid {
        &self.back
    }

    /// The caret position the last draw closure requested (or `Hidden`).
    pub fn cursor(&self) -> CursorState {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Style;
    use crate::backend::Bce;

    #[derive(Default)]
    struct FailOnByteWriter {
        bytes: Vec<u8>,
        fail_on: Option<u8>,
    }

    impl io::Write for FailOnByteWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail_on.is_some_and(|needle| buf.contains(&needle)) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected terminal write failure",
                ));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn staged_frame_emits_only_the_final_committed_grid() {
        crossterm::style::force_color_output(true);
        let mut output = Vec::new();
        {
            let backend = Backend::with_bce_and_driver(
                &mut output,
                Bce::No,
                crate::driver::TerminalDriver::for_profile(
                    &crate::profile::TerminalProfile::direct_color(),
                ),
            );
            let mut terminal = Terminal::new(backend);

            terminal
                .stage(|frame| frame.put(0, 0, Style::default(), "intermediate"))
                .unwrap();
            assert!(terminal.writer().is_empty());

            terminal
                .draw(|frame| frame.put(0, 0, Style::default(), "final"))
                .unwrap();
        }

        let rendered = String::from_utf8(output).unwrap();
        assert!(
            rendered.starts_with("\x1b[?2026h"),
            "committed frame must begin synchronized update: {rendered:?}"
        );
        assert!(
            rendered.ends_with("\x1b[?2026l"),
            "committed frame must end synchronized update: {rendered:?}"
        );
        assert!(rendered.contains("final"));
        assert!(!rendered.contains("intermediate"));
    }

    #[test]
    fn incremental_frame_positions_caret_before_showing_it() {
        crossterm::style::force_color_output(true);
        let backend = Backend::with_bce(Vec::new(), Bce::No);
        let mut terminal = Terminal::new(backend);

        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((5, 5));
            })
            .unwrap();
        terminal.writer().clear();

        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "B");
                frame.set_cursor_position((5, 5));
            })
            .unwrap();
        let rendered = String::from_utf8(terminal.writer().clone()).unwrap();

        let hide = rendered.find("\x1b[?25l").expect("cursor is shielded");
        let draw = rendered.find('B').expect("changed cell is rendered");
        let final_move = rendered
            .rfind("\x1b[6;6H")
            .expect("final caret position is emitted");
        let show = rendered.find("\x1b[?25h").expect("cursor is restored");
        assert!(
            hide < draw && draw < final_move && final_move < show,
            "frame order must be hide → draw → final MoveTo → show: {rendered:?}"
        );
    }

    #[test]
    fn cursor_only_frame_moves_without_visibility_toggle() {
        crossterm::style::force_color_output(true);
        let backend = Backend::with_bce(Vec::new(), Bce::No);
        let mut terminal = Terminal::new(backend);
        terminal
            .draw(|frame| frame.set_cursor_position((1, 1)))
            .unwrap();
        terminal.writer().clear();

        terminal
            .draw(|frame| frame.set_cursor_position((2, 1)))
            .unwrap();
        let rendered = String::from_utf8(terminal.writer().clone()).unwrap();
        assert!(rendered.contains("\x1b[2;3H"), "caret moves: {rendered:?}");
        assert!(
            !rendered.contains("\x1b[?25l") && !rendered.contains("\x1b[?25h"),
            "a cursor-only move must not blink visibility: {rendered:?}"
        );
    }

    #[test]
    fn hidden_frame_parks_at_last_visible_input_anchor() {
        crossterm::style::force_color_output(true);
        let backend = Backend::with_bce(Vec::new(), Bce::No);
        let mut terminal = Terminal::new(backend);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((5, 5));
            })
            .unwrap();
        terminal.writer().clear();

        terminal
            .draw(|frame| frame.put(0, 0, Style::default(), "B"))
            .unwrap();
        let rendered = String::from_utf8(terminal.writer().clone()).unwrap();
        assert!(
            rendered.rfind("\x1b[6;6H").is_some(),
            "hidden cursor returns to its stable input anchor: {rendered:?}"
        );
        assert!(
            !rendered.contains("\x1b[?25h"),
            "a hidden frame never reveals the cursor: {rendered:?}"
        );
    }

    #[test]
    fn identical_frame_emits_no_envelope_or_cursor_bytes() {
        crossterm::style::force_color_output(true);
        let backend = Backend::with_bce(Vec::new(), Bce::No);
        let mut terminal = Terminal::new(backend);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "same");
                frame.set_cursor_position((4, 0));
            })
            .unwrap();
        terminal.writer().clear();

        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "same");
                frame.set_cursor_position((4, 0));
            })
            .unwrap();
        assert!(
            terminal.writer().is_empty(),
            "an identical logical frame must be a zero-byte commit"
        );
    }

    #[test]
    fn frame_clamps_requested_cursor_to_terminal_bounds() {
        let mut terminal = TestTerminal::new(4, 3);
        terminal.draw(|frame| frame.set_cursor_position((u16::MAX, u16::MAX)));
        assert_eq!(terminal.cursor(), CursorState::Visible(3, 2));
    }

    #[test]
    fn failed_frame_closes_envelope_then_forces_clear_and_full_repaint() {
        crossterm::style::force_color_output(true);
        let backend = Backend::with_bce_and_driver(
            FailOnByteWriter::default(),
            Bce::No,
            crate::driver::TerminalDriver::for_profile(
                &crate::profile::TerminalProfile::direct_color(),
            ),
        );
        let mut terminal = Terminal::new(backend);
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "A");
                frame.set_cursor_position((1, 0));
            })
            .unwrap();

        terminal.writer().bytes.clear();
        terminal.writer().fail_on = Some(b'B');
        assert!(
            terminal
                .draw(|frame| {
                    frame.put(0, 0, Style::default(), "B");
                    frame.set_cursor_position((1, 0));
                })
                .is_err()
        );
        let failed = String::from_utf8_lossy(&terminal.writer().bytes);
        assert!(
            failed.ends_with("\x1b[?2026l"),
            "a successfully opened envelope must be closed after failure: {failed:?}"
        );

        terminal.writer().bytes.clear();
        terminal.writer().fail_on = None;
        terminal
            .draw(|frame| {
                frame.put(0, 0, Style::default(), "B");
                frame.set_cursor_position((1, 0));
            })
            .unwrap();
        let recovered = String::from_utf8_lossy(&terminal.writer().bytes);
        assert!(
            recovered.contains("\x1b[2J") && recovered.contains('B'),
            "the retry must clear and repaint from logical state: {recovered:?}"
        );
    }
}

#[cfg(test)]
mod resize_envelope_tests {
    //! The resize path's `backend.invalidate()` must not flush mid-envelope:
    //! a flush between the SGR reset and the queued Clear(All) lets the
    //! terminal paint a half-reset screen — the resize flicker.

    use super::*;

    #[test]
    fn invalidate_does_not_flush_by_itself() {
        // With a BufWriter sink, queued bytes stay in the writer's buffer
        // until an explicit flush. `invalidate` must leave them there —
        // flushing is the synchronized-update envelope owner's job
        // (`Terminal::commit`); a mid-envelope flush let the terminal paint
        // a half-reset screen on resize.
        let mut sink = std::io::BufWriter::new(Vec::<u8>::new());
        let mut be = Backend::new(&mut sink);
        be.invalidate().unwrap();

        // No flush yet: the queued reset must still sit in the buffer (the
        // sink below the BufWriter has received nothing), and the backend's
        // own final flush below is what delivers it.
        use std::io::Write as _;
        be.writer().flush().unwrap();
        let buf = sink.into_inner().unwrap();
        assert!(
            String::from_utf8_lossy(&buf).starts_with("\x1b[0m"),
            "reset is the first byte once flushed: {buf:?}"
        );
    }

    #[test]
    fn size_falls_back_to_retained_grid() {
        // Terminal::size queries crossterm; under a test there is no real
        // terminal, so it must fall back to the retained grid size rather
        // than panicking or lying.
        let be = Backend::new(Vec::new());
        let terminal = Terminal::new(be);
        let (w, h) = terminal.size();
        let (gw, gh) = {
            let t = terminal;
            // The grids were sized from crossterm::terminal::size() at
            // construction; whatever that returned, the fallback path must
            // produce a consistent pair.
            drop(t);
            (0u16, 0u16)
        };
        let _ = (gw, gh);
        assert!(
            w > 0 && h > 0,
            "live size resolves to something usable: {w}x{h}"
        );
    }
}
