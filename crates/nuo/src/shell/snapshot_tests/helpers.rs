//! Shared test fixtures, render harnesses, and grid extraction utilities
//! for transcript and tool-step snapshot assertions.

use nuo_view::model::document::{MessageKind, TranscriptMessage};
use nuo_view::model::interactive::InteractiveEntry;
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;
use nuotc::Rect;

use crate::shell::disclosure::draw_tool_step;
use crate::shell::render::Theme;

/// Build a finished tool-step message with optional output and expand state.
///
/// Disclosure is set through the entry's own verb rather than by writing the
/// flag, because setting `expanded` also re-derives the message's semantic
/// blocks — and those blocks are what selection and copy index. A fixture that
/// merely flipped the flag produced a step whose painted rows and whose block
/// model described different text, which is exactly the divergence the copy
/// tests exist to catch.
pub(crate) fn tool_step(
    name: &str,
    arguments: &str,
    output: Option<&str>,
    expanded: bool,
) -> TranscriptMessage {
    use nuo_view::model::interactive::InteractiveEntry;
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    if let MessageKind::ToolStep {
        output: out,
        status,
        ..
    } = &mut m.kind
    {
        *out = output.map(str::to_string);
        if output.is_some() {
            *status = nuo_view::model::document::ToolStepStatus::Ok;
        }
    }
    m.pin_expanded(expanded);
    m
}

/// Like [`tool_step`] but also sets the structured payload + terminal status,
/// mirroring what `finish_tool_step` produces in production.
pub(crate) fn tool_step_structured(
    name: &str,
    arguments: &str,
    structured: nuo_tool::ToolOutput,
    expanded: bool,
) -> TranscriptMessage {
    use nuo_view::model::interactive::InteractiveEntry;
    let text = structured.to_text();
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    m.finish_tool_step("call_test", text, structured, 0);
    // `pin_expanded` re-derives the body blocks; writing the flag alone leaves
    // the block model describing the collapsed summary.
    m.pin_expanded(expanded);
    m
}

/// A still-running tool step carrying a partial structured payload, mirroring
/// what `push_tool_stream` produces mid-stream (status stays `Running`).
pub(crate) fn tool_step_streaming(
    name: &str,
    arguments: &str,
    structured: nuo_tool::ToolOutput,
    expanded: bool,
) -> TranscriptMessage {
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    if let MessageKind::ToolStep { structured: s, .. } = &mut m.kind {
        *s = Some(Box::new(structured));
    }
    m.pin_expanded(expanded);
    m
}

/// Render the PreAttach interstitial into a fresh `width x height` buffer and
/// return the painted grid as trimmed text rows.
///
/// The gate owns the whole terminal when mounted, so its painted output *is* the
/// user's entire view — and it had no render coverage at all. That is how it
/// shipped telling users `Esc` would quit, and describing a re-review as a first
/// contact: the copy was only ever asserted in `PreAttachState`, never in what
/// actually reached the screen.
pub(crate) fn render_pre_attach_grid(
    state: &crate::shell::PreAttachState,
    width: u16,
    height: u16,
) -> String {
    let mut terminal = nuotc::TestTerminal::new(width, height);
    terminal.draw(|f| {
        crate::shell::pre_attach::draw_pre_attach(f, state, &Theme::default());
    });
    painted_rows(terminal.buffer(), width, height).0
}

/// Extract trimmed text rows from a painted buffer.
pub(crate) fn painted_rows(
    buf: &nuotc::CellGrid,
    width: u16,
    height: u16,
) -> (String, (String, String)) {
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            row.push_str(buf.cells()[y * bw + x].symbol.as_str());
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    (rows.join("\n"), background_map(buf))
}

/// Render `msg` as a tool step into a fresh `width x height` buffer and
/// return the painted grid as trimmed text rows joined by newlines.
pub(crate) fn render_grid(msg: &TranscriptMessage, width: u16, height: u16) -> String {
    render_grid_with_map(msg, width, height).0
}

/// Render one step and hand back the layout map the paint produced.
///
/// The snapshot tests only need the grid; the copy-path tests need the *regions*
/// the same paint registered, because that is where the click hit-test and the
/// clipboard read their byte offsets from. Sharing one paint keeps the two from
/// ever describing different frames.
pub(crate) fn render_grid_with_map(
    msg: &TranscriptMessage,
    width: u16,
    height: u16,
) -> (String, LayoutMap) {
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let mut layout_map = LayoutMap::default();
    terminal.draw(|f| {
        let area: Rect = f.area();
        let selection = SelectionState::default();
        let theme = Theme::default();
        let mut diff_cache = nuo_view::tools::DiffCache::default();
        let mut skip_rows = 0usize;
        let mut current_y = area.y;
        let mut content_lines = 0usize;
        let mut sticky = Vec::new();
        let mut wrap_cache = crate::shell::render::BlockWrapCache::default();
        let mut ctx = crate::shell::disclosure::renderers::RenderCtx::from_cursor(
            f,
            area,
            area.width as usize,
            &theme,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &mut wrap_cache,
        );
        draw_tool_step(
            &mut ctx,
            msg,
            0,
            &selection,
            None,
            &mut diff_cache,
            &mut sticky,
            false,
            false,
        );
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            let cell = &buf.cells()[y * bw + x];
            let sym: &str = cell.symbol.as_str();
            row.push_str(sym);
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let grid = rows.join("\n");

    // Style layer: a compact run-length map of the background per row, plus a
    // legend. Text snapshots can't see color, so this makes palette/banding
    // changes (Step 2) visible and reviewable. Symbols are assigned per-frame
    // in first-appearance order; `.` is the terminal default.
    let (bgmap, legend) = background_map(buf);
    let grid = if bgmap.is_empty() {
        grid
    } else {
        format!("{grid}\n\nbackgrounds:\n{legend}\n{bgmap}")
    };
    (grid, layout_map)
}

/// Compact per-row background run-length map + legend for a rendered buffer.
/// Skips rows that are entirely the terminal default so the output stays
/// focused on the painted step.
pub(crate) fn background_map(buf: &nuotc::CellGrid) -> (String, String) {
    use nuotc::Color;
    type Bg = Color;

    let bw = buf.area().width as usize;
    let bh = buf.area().height as usize;

    let is_default = |bg: Bg| bg == Color::Reset;

    // Distinct bg colors in first-appearance order.
    let mut order: Vec<Bg> = Vec::new();
    for y in 0..bh {
        for x in 0..bw {
            let bg = buf.cells()[y * bw + x].style.bg;
            if !order.contains(&bg) {
                order.push(bg);
            }
        }
    }
    let sym_of = |bg: Bg| -> char {
        if is_default(bg) {
            '.'
        } else {
            let i = order
                .iter()
                .position(|x| is_default(*x) == is_default(bg) && x == &bg)
                .unwrap_or(0);
            (b'A' + i as u8) as char
        }
    };
    let fmt_color = |bg: Bg| -> String {
        match bg {
            Color::Reset => "reset".to_string(),
            Color::Rgb(r, g, b) => format!("#{:02X}{:02X}{:02X}", r, g, b),
            other => format!("{:?}", other),
        }
    };
    let legend = order
        .iter()
        .map(|&bg| format!("{}={}", sym_of(bg), fmt_color(bg)))
        .collect::<Vec<_>>()
        .join("  ");

    let mut lines: Vec<String> = Vec::new();
    for y in 0..bh {
        let mut runs: Vec<(char, usize)> = Vec::new();
        for x in 0..bw {
            let s = sym_of(buf.cells()[y * bw + x].style.bg);
            match runs.last_mut() {
                Some((last, n)) if *last == s => *n += 1,
                _ => runs.push((s, 1)),
            }
        }
        while matches!(runs.last(), Some(('.', _))) {
            runs.pop();
        }
        if runs.is_empty() {
            continue;
        }
        let line = runs
            .iter()
            .map(|(s, n)| format!("{}{}", s, n))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!("{}: {}", y, line));
    }
    (lines.join("\n"), legend)
}

/// Render `steps` (already finalized tool-step messages) through the full
/// transcript pipeline and return the painted grid as trimmed rows. Unlike
/// [`render_grid`], this exercises `draw_transcript` so the message-level
/// spacing between consecutive steps is captured. Backgrounds are omitted:
/// these tests are about row counts, not palette.
pub(crate) struct RenderedTranscript {
    pub(crate) grid: String,
    pub(crate) content_lines: usize,
    pub(crate) view_height: u16,
}

pub(crate) fn render_transcript_frame(
    messages: &[TranscriptMessage],
    width: u16,
    height: u16,
    scroll: u16,
    height_cache: Option<&mut crate::shell::render::HeightCache>,
) -> RenderedTranscript {
    use crate::shell::render::{
        EmptyStateGuidance, QueueBarProps, Theme, TranscriptProps, draw_transcript,
    };
    use nuo_view::model::layout::LayoutMap;

    let theme = Theme::default();
    let selection = SelectionState::default();
    let mut terminal = nuotc::TestTerminal::new(width, height);
    let mut layout_map = LayoutMap::new();
    let mut render = None;
    terminal.draw(|f| {
        render = Some(draw_transcript(
            f,
            &mut layout_map,
            TranscriptProps {
                messages,
                scroll,
                selection: &selection,
                cell_selection: None,
                backoff: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                chrome_hidden: false,
                queue_bar: QueueBarProps {
                    items: &[],
                    blocked: false,
                    expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                },
                tasks_bar: Default::default(),
                interrupt_key: None,
                subagent_bar: None,
                page_hints: None,
                tab_bar: None,
                thread_id: None,
                thread_context: None,
                round_started_at: None,
                hovered_step: None,
                focused_entry: None,
                focus_plane: crate::shell::render::FocusPlane::Composer,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: nuo_view::layout::Strategy::default(),
                height_cache,
            },
        ));
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            row.push_str(buf.cells()[y * bw + x].symbol.as_str());
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let render = render.expect("draw_transcript must return layout metrics");
    RenderedTranscript {
        grid: rows.join("\n"),
        content_lines: render.content_lines,
        view_height: render.view_height,
    }
}

pub(crate) fn render_transcript_grid(
    messages: &[TranscriptMessage],
    width: u16,
    height: u16,
) -> String {
    render_transcript_frame(messages, width, height, 0, None).grid
}

/// The first painted (non-blank) row of a rendered transcript grid, or `""`.
pub(crate) fn first_content_row(grid: &str) -> &str {
    grid.lines()
        .find(|row| !row.trim().is_empty())
        .unwrap_or("")
}
