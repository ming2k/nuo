//! The live editable prompt box at the bottom of the screen: a tinted panel
//! (`input_surface` / `input_surface_inactive`) laid out as four rows for a
//! one-line draft — a blank breathing row, the text row(s), another blank
//! gap row, and a meta row carrying the hint sentence (`Enter send prompt ·
//! 12 chars`). Inside the panel, in-box text wrapping with vertical scroll
//! keeps the caret visible, and per-row layout-map recording drives semantic
//! selection / copy.
use nuotc::str_len;
use nuotc::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};

use nuo_view::composer_attachments::{ChipKind, iter_chips};
use nuo_view::model::layout::{BlockRegion, LayoutMap};
use nuo_view::model::selection::SelectionState;

use super::Theme;
use super::composer_grid::TextGrid;
use super::design::{
    COMPOSER_PROMPT_PREFIX_COLS, COMPOSER_RIGHT_PAD_COLS, COMPOSER_TEXT_ROW_OFFSET,
    COMPOSER_VERTICAL_CHROME_ROWS, INPUT_PROMPT_GLYPH,
};
use super::text_layout::{block_selection_range, line_selection};
use crate::shell::composer_hints::hint_row_parts;

/// Right-aligned overflow label for a chrome row (the breathing row above
/// the text / the gap row below it): direction glyph + hidden-row count.
/// `↑ 12 lines` on comfortable widths, `↑ 12` once the long form would crowd
/// a narrow panel — the same degrade-then-simplify ladder the keys row
/// performs via `ActionDensity`.
fn overflow_label(dir: char, hidden: usize, full_w: usize) -> String {
    if full_w >= 24 {
        format!("{dir} {hidden} lines")
    } else {
        format!("{dir} {hidden}")
    }
}

/// Build a chrome row: a full-width panel-bg blank that gives the text its
/// breathing room, except when `hidden > 0` wrapped rows are clipped
/// off-screen on that side — then it right-aligns the muted, quantified
/// overflow indicator with one column of air before the panel edge, the
/// same discipline the meta row's right-aligned readouts follow. Reusing
/// the existing chrome rows as indicators (instead of adding rows) keeps
/// the box height identical whether or not anything is hidden, so the
/// panel never jitters as the draft crosses the overflow threshold.
fn chrome_row(full_w: usize, bg: Color, theme: &Theme, dir: char, hidden: usize) -> Line<'static> {
    if hidden == 0 {
        return Line::from(Span::styled(" ".repeat(full_w), Style::default().bg(bg)));
    }
    let label = overflow_label(dir, hidden, full_w);
    let gap_cols = full_w.saturating_sub(label.chars().count() + 1);
    Line::from(vec![
        Span::styled(" ".repeat(gap_cols), Style::default().bg(bg)),
        Span::styled(label, Style::default().bg(bg).fg(theme.muted())),
        Span::styled(" ".to_string(), Style::default().bg(bg)),
    ])
}

/// Build the top breathing row: overflow count when clipped above, or the
/// inline recall pointer badge (`[history 3/17 · draft saved]`) when the
/// ↑/↓ pointer sits on a history row, or a contextual mode declaration badge
/// (e.g. `[history search · draft saved]`) when idle at top.
///
/// Priority: overflow indicator > recall badge > extension badge. The
/// overflow indicator wins because clipped content is a spatial fact about
/// this very row's role; the recall badge outranks the extension badge
/// because the recall pointer is a *content* state (what the buffer holds)
/// while an extension badge is a *surface* state (what owns the keyboard),
/// and content honesty always outranks surface naming when the two compete
/// for the same row.
fn top_chrome_row(
    full_w: usize,
    bg: Color,
    theme: &Theme,
    hidden_above: usize,
    hints: &crate::shell::composer_hints::ComposerHints,
    focused: bool,
) -> Line<'static> {
    if hidden_above > 0 {
        return chrome_row(full_w, bg, theme, '↑', hidden_above);
    }
    let target = hints.compose_target;
    if focused && let Some((position, total, edited)) = hints.history_recall {
        // Width-degradation ladder mirrors the overflow label's
        // degrade-then-simplify discipline: the full clause set on
        // comfortable widths, the bare pointer once space runs out. The
        // pointer itself (`3/17`) is the last element to go — it is the
        // badge's entire point.
        let label = if full_w >= 44 {
            let mut label = format!("[history {position}/{total}");
            if edited {
                label.push_str(" · edited");
            } else if hints.recall_draft_saved {
                label.push_str(" · draft saved");
            }
            label.push(']');
            label
        } else if full_w >= 24 {
            format!("[history {position}/{total}]")
        } else {
            format!("[{position}/{total}]")
        };
        let label_len = label.chars().count();
        if full_w > label_len + 2 {
            let gap_cols = full_w.saturating_sub(label_len + 1);
            return Line::from(vec![
                Span::styled(" ".repeat(gap_cols), Style::default().bg(bg)),
                Span::styled(
                    label,
                    Style::default()
                        .bg(bg)
                        .fg(theme.brand())
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(" ".to_string(), Style::default().bg(bg)),
            ]);
        }
    }
    if focused && target == crate::shell::composer_hints::ComposeTarget::HistorySearch {
        let label = if full_w >= 36 {
            "[history search · draft saved]"
        } else if full_w >= 20 {
            "[history search]"
        } else {
            "[history]"
        };
        let label_len = label.chars().count();
        if full_w > label_len + 2 {
            let gap_cols = full_w.saturating_sub(label_len + 1);
            return Line::from(vec![
                Span::styled(" ".repeat(gap_cols), Style::default().bg(bg)),
                Span::styled(
                    label,
                    Style::default()
                        .bg(bg)
                        .fg(theme.brand())
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(" ".to_string(), Style::default().bg(bg)),
            ]);
        }
    }
    chrome_row(full_w, bg, theme, '↑', 0)
}

/// Render plumbing for the composer draw family: frame, target rect,
/// theme, layout map, scroll state, and selection. Bundled so the three
/// composer entry points and the shared impl take (props, text, flags)
/// instead of threading eleven positional args.
pub struct ComposerProps<'a, 'f: 'a> {
    pub frame: &'a mut Frame<'f>,
    pub input_rect: Rect,
    pub theme: &'a Theme,
    pub layout_map: &'a mut LayoutMap,
    pub input_scroll: &'a mut usize,
    pub selection: &'a SelectionState,
}

/// The composer text being rendered plus its byte cursor.
pub struct ComposerText<'a> {
    pub input: &'a str,
    pub byte_cursor: usize,
}

/// Special message_idx for the live input box in the layout map, so semantic
/// selection / copy works on input text just like transcript messages.
pub use nuo_view::model::layout::INPUT_MSG_IDX;

/// Display width of the composer's text area inside a box of `full_width`
/// columns (the total minus the left prompt prefix and right padding).
pub fn composer_text_width(full_width: usize) -> usize {
    full_width
        .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
        .max(1)
}

/// Compute the caret's screen coordinates `(x, y)` for `input` at `byte_cursor`
/// laid out inside `input_rect`, updating `input_scroll` in place to keep the
/// caret within the visible window.
///
/// This is the **single source of truth** for the caret's screen position. The
/// draw path resolves it once, then the terminal commit installs that final
/// coordinate while the physical cursor is hidden.
///
/// `follow_cursor` distinguishes editing from deliberate viewport browsing.
/// When false, this clamps `input_scroll` only to the valid scroll range and
/// returns `None` if the caret is outside the preserved viewport; the caller
/// can then keep the physical cursor hidden until editing resumes.
///
/// Returns `None` when `input_rect` has no room for text rows. The caller is
/// responsible for the remaining visibility policy (modal owning the
/// keyboard, active selection, etc.).
pub fn cursor_screen_pos(
    input_rect: Rect,
    input: &str,
    byte_cursor: usize,
    input_scroll: &mut usize,
    follow_cursor: bool,
) -> Option<(u16, u16)> {
    let full_w = input_rect.width as usize;
    if full_w == 0 || input_rect.height == 0 {
        return None;
    }
    let text_width = full_w
        .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
        .max(1);
    let grid = TextGrid::new(input, text_width);
    let wrapped = grid.rows();

    let visible_rows = (input_rect.height as usize)
        .saturating_sub(COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);

    // Project the caret's byte onto the grid — the exact projection the
    // completion popup and the click path use, so no consumer can disagree
    // about which row the caret occupies (empty rows included).
    let (cursor_line, cursor_col) = grid.cell(byte_cursor);

    // Clamp the scroll window the same way the draw loop does.
    let max_scroll = wrapped.len().saturating_sub(visible_rows);
    if wrapped.len() <= visible_rows {
        *input_scroll = 0;
    } else {
        *input_scroll = (*input_scroll).min(max_scroll);
        if follow_cursor {
            if cursor_line < *input_scroll {
                *input_scroll = cursor_line;
            } else if cursor_line >= *input_scroll + visible_rows {
                *input_scroll = cursor_line.saturating_sub(visible_rows - 1);
            }
        }
    }

    if cursor_line < *input_scroll || cursor_line >= *input_scroll + visible_rows {
        return None;
    }

    let visible_cursor_line = cursor_line.saturating_sub(*input_scroll);
    let cursor_y = input_rect.y + COMPOSER_TEXT_ROW_OFFSET + visible_cursor_line as u16;
    let cursor_x =
        input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16 + cursor_col.min(text_width) as u16;
    Some((cursor_x, cursor_y))
}

/// Draw the flat input box panel at the bottom of the screen.
///
/// `focused` selects the panel palette (the input's dedicated active /
/// inactive background pair). The live composer passes `true` when
/// no transcript step carries keyboard focus, and `false` when the user has
/// navigated into the transcript with Ctrl+↑/↓ — the recessed "read-only"
/// band signals that the next keypress targets the step, not the input box.
///
/// `image_count` / `paste_count` are the numbers of attachments actually
/// staged behind the input's chips (`pending_images.len()` /
/// `pending_text_pastes.len()`). Only a chip whose `#N` has a real payload
/// (`N <= count`) is painted as a colored pill; an orphan label — typed by
/// hand, or left over after the paste was undone — renders as ordinary text
/// so it never reads as an attachment that isn't there.
///
/// The elevated delegated state is no longer signalled here; it lives on the
/// state bar directly below the input, separate from composer state.
#[allow(clippy::too_many_arguments)]
pub fn draw_composer(
    props: ComposerProps<'_, '_>,
    text: ComposerText<'_>,
    focused: bool,
    show_caret: bool,
    record: bool,
    image_count: usize,
    paste_count: usize,
    hints: crate::shell::composer_hints::ComposerHints,
) {
    draw_composer_impl(
        props,
        text,
        ComposerDrawOptions {
            focused,
            show_caret,
            follow_caret: true,
            record,
            image_count,
            paste_count,
            hints,
            ghost_text: None,
        },
        None,
    )
}

#[derive(Debug, Clone, Copy)]
pub struct ComposerDrawOptions<'a> {
    pub focused: bool,
    pub show_caret: bool,
    /// Keep the viewport pinned to the logical caret. The live composer
    /// suspends this during wheel browsing and selection edge-autoscroll so
    /// rendering preserves the user's viewport instead of snapping back.
    pub follow_caret: bool,
    pub record: bool,
    pub image_count: usize,
    pub paste_count: usize,
    /// Owned meta-row inputs: what the buffer is and what Enter does. Built
    /// by the caller before the mutable composer borrow begins.
    pub hints: crate::shell::composer_hints::ComposerHints,
    /// Optional ghost text preview (e.g. active history candidate).
    pub ghost_text: Option<&'a str>,
}

impl<'a> Default for ComposerDrawOptions<'a> {
    fn default() -> Self {
        Self {
            focused: false,
            show_caret: false,
            follow_caret: true,
            record: false,
            image_count: 0,
            paste_count: 0,
            hints: crate::shell::composer_hints::ComposerHints::default(),
            ghost_text: None,
        }
    }
}

/// Like [`draw_composer`], but paints the `highlight_len`-byte run at the
/// start of the input in bold + the theme's accent color. Used by the shell
/// to mark a resolved `/command` token so it reads differently from plain
/// prose (and from an unmatched `/`-prefix, which stays in the normal text
/// color). The length is clamped per wrapped row so the accent never bleeds
/// into the argument text when the input wraps.
pub fn draw_composer_highlighted(
    props: ComposerProps<'_, '_>,
    text: ComposerText<'_>,
    options: ComposerDrawOptions<'_>,
    highlight_len: usize,
) {
    draw_composer_impl(props, text, options, Some(highlight_len));
}

/// Paint the ordinary (non-highlighted, non-igniting) composer while honoring
/// the caller's complete viewport policy. The convenience [`draw_composer`]
/// keeps its historical always-follow behavior for isolated render callers;
/// the live event loop uses this entry point because it owns scroll intent.
pub fn draw_composer_with_options(
    props: ComposerProps<'_, '_>,
    text: ComposerText<'_>,
    options: ComposerDrawOptions<'_>,
) {
    draw_composer_impl(props, text, options, None);
}

fn draw_composer_impl(
    props: ComposerProps<'_, '_>,
    text: ComposerText<'_>,
    options: ComposerDrawOptions<'_>,
    highlight_len: Option<usize>,
) {
    let ComposerDrawOptions {
        focused,
        show_caret,
        follow_caret,
        record,
        image_count,
        paste_count,
        hints,
        ghost_text,
    } = options;
    let ComposerProps {
        frame,
        input_rect,
        theme,
        layout_map,
        input_scroll,
        selection,
    } = props;
    let ComposerText { input, byte_cursor } = text;
    // The input box is a flat tinted panel: each text row carries `panel_bg`
    // and is prefixed with `› ` on the first wrapped line / a matching indent
    // on continuations. The box is laid out as four rows for a one-line
    // draft: a blank breathing row above the text, the text row(s), a blank
    // gap row, and the meta row (hint sentence + char count) closing the box.
    // Using a solid background (not `▄`/`▀` half-block glyphs) keeps the edge
    // identical across terminals, since a cell can only carry one bg color.
    //
    // `focused` drives only the palette: when `false` the panel drops to the
    // recessed input-inactive band and the prompt glyph uses `text_muted`, so
    // the box visibly recedes — while staying an input-owned surface (it is
    // deliberately *not* the sent-user-message panel, so the input's two
    // states remain a related but independent pair). The live composer passes
    // `true`. The caret is gated separately by `show_caret`: it is suppressed
    // whenever a modal owns the keyboard (the full-screen modal backdrop
    // already signals "typing lands elsewhere"), so the panel never shows a
    // live caret inside a surface that no longer accepts input.
    let panel_bg = if focused {
        theme.input_surface()
    } else {
        theme.input_surface_inactive()
    };
    let prompt_fg = if focused {
        theme.brand()
    } else {
        theme.muted()
    };
    let full_w = input_rect.width as usize;
    // Inner text budget: the full width minus the `› ` prompt prefix and the
    // matching right pad, so text never touches either edge of the panel.
    let text_width = full_w
        .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
        .max(1);
    let grid = TextGrid::new(input, text_width);
    let wrapped = grid.rows();

    // Number of text rows that fit inside the box (top/bottom transition rows
    // consume two lines). The box is sized by draw_transcript to fit the wrapped text
    // up to half the terminal height, so when the text exceeds this height we
    // scroll to keep the cursor visible.
    let visible_rows = (input_rect.height as usize)
        .saturating_sub(COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);

    // The caret position (and the scroll clamp that keeps it on screen) is the
    // single source of truth in [`cursor_screen_pos`]. The draw path reuses it
    // so the rendered caret and the terminal cursor can never disagree — which
    // is what previously let the IME composition window drift by a frame.
    let cursor = cursor_screen_pos(input_rect, input, byte_cursor, input_scroll, follow_caret);

    // Overflow bookkeeping for the chrome-row indicators and the meta row's
    // position readout: how many wrapped rows sit above / below the visible
    // window once the caret-follow clamp in [`cursor_screen_pos`] has
    // settled the scroll offset for this frame.
    let hidden_above = *input_scroll;
    let hidden_below = if input.is_empty() {
        if let Some(ghost) = ghost_text.filter(|g| !g.is_empty()) {
            let ghost_grid = TextGrid::new(ghost, text_width);
            let ghost_wrapped = ghost_grid.rows();
            ghost_wrapped
                .len()
                .saturating_sub(*input_scroll + visible_rows)
        } else {
            0
        }
    } else {
        wrapped.len().saturating_sub(*input_scroll + visible_rows)
    };

    let mut lines: Vec<Line> =
        Vec::with_capacity(visible_rows + COMPOSER_VERTICAL_CHROME_ROWS as usize);

    // Row 1: blank breathing row
    // A full panel-bg padding row giving the text a line of air above. When
    // rows hide above the viewport it moonlights as the `↑ N lines`
    // indicator (see [`chrome_row`]); otherwise it presents the active mode tag.
    lines.push(top_chrome_row(
        full_w,
        panel_bg,
        theme,
        hidden_above,
        &hints,
        focused,
    ));

    // Text rows: the first logical line opens with the `›` prompt glyph plus
    // a gap, and every wrapped continuation indents to the same column so the
    // box reads as a shell-style prompt on a tinted panel. Only the visible
    // slice is rendered so overflowing content can scroll while the box stays
    // within its terminal-sized bounds.
    let prompt_glyph = Span::styled(
        INPUT_PROMPT_GLYPH,
        Style::default().bg(panel_bg).fg(prompt_fg),
    );
    let prompt_gap = Span::styled(
        " ".repeat(COMPOSER_PROMPT_PREFIX_COLS - 1),
        Style::default().bg(panel_bg),
    );
    let indent = Span::styled(
        " ".repeat(COMPOSER_PROMPT_PREFIX_COLS),
        Style::default().bg(panel_bg),
    );
    if input.is_empty() {
        if let Some(ghost) = ghost_text.filter(|g| !g.is_empty()) {
            let ghost_style = Style::default()
                .bg(panel_bg)
                .fg(theme.muted())
                .add_modifier(Modifier::DIM);
            let ghost_grid = TextGrid::new(ghost, text_width);
            let ghost_wrapped = ghost_grid.rows();
            let start = *input_scroll;
            let end = (*input_scroll + visible_rows).min(ghost_wrapped.len());
            for (i, wl) in ghost_wrapped[start..end].iter().enumerate() {
                let mut spans = if start + i == 0 {
                    vec![prompt_glyph.clone(), prompt_gap.clone()]
                } else {
                    vec![indent.clone()]
                };
                spans.push(Span::styled(wl.text.clone(), ghost_style));
                let used = COMPOSER_PROMPT_PREFIX_COLS + str_len(&wl.text);
                let tail_cols = full_w.saturating_sub(used);
                spans.push(Span::styled(
                    " ".repeat(tail_cols),
                    Style::default().bg(panel_bg),
                ));
                lines.push(Line::from(spans));
            }
        } else {
            let used = COMPOSER_PROMPT_PREFIX_COLS;
            // Suffix after the (empty) text: the row's remaining panel columns.
            // used + tail = full_w.
            let tail_cols = full_w.saturating_sub(used);
            lines.push(Line::from(vec![
                prompt_glyph.clone(),
                prompt_gap.clone(),
                Span::styled(" ".repeat(tail_cols), Style::default().bg(panel_bg)),
            ]));
        }
    } else {
        let start = *input_scroll;
        let end = (*input_scroll + visible_rows).min(wrapped.len());
        // Resolve the selection byte range for the whole input box once; each
        // wrapped line intersects it to find its own highlighted slice. The
        // composer records itself as a single block at `INPUT_MSG_IDX` /
        // block 0, so a drag or triple-click inside the box resolves here.
        let input_range_sel = match selection {
            SelectionState::InputRange {
                anchor_byte,
                head_byte,
            } if anchor_byte != head_byte => Some((
                (*anchor_byte).min(*head_byte),
                (*anchor_byte).max(*head_byte),
            )),
            _ => None,
        };
        let sel_range = if input_range_sel.is_none() {
            block_selection_range(selection, INPUT_MSG_IDX, 0)
        } else {
            None
        };
        let selected_bg = theme.selected();
        let text_fg = theme.fg();
        let base_text = Style::default().bg(panel_bg).fg(text_fg);
        // Resolved `/command` token: bold + accent color, echoing the
        // completion menu's command column so the two surfaces read alike.
        let accent_text = Style::default()
            .bg(panel_bg)
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD);
        // Attachment chips (`[Image #N (size)]` / `[Pasted text #N +M lines
        // (size)]`) render as tinted "pills" so a pasted block reads as a
        // distinct object inside the live input instead of ordinary prose.
        // Paste chips take the calm blue, image chips the warm amber; each is
        // a bold colored label on a tinted band derived from the current
        // panel, so the identifier is both informative and identifiable.
        let chips = iter_chips(input);
        let chip_paste_fg = theme.chip_paste_fg();
        let chip_image_fg = theme.chip_image_fg();
        let chip_paste_style = Style::default()
            .bg(theme.chip_paste_bg(panel_bg))
            .fg(chip_paste_fg)
            .add_modifier(Modifier::BOLD);
        let chip_image_style = Style::default()
            .bg(theme.chip_image_bg(panel_bg))
            .fg(chip_image_fg)
            .add_modifier(Modifier::BOLD);
        for (i, wl) in wrapped[start..end].iter().enumerate() {
            let used = COMPOSER_PROMPT_PREFIX_COLS + str_len(&wl.text);
            let mut spans = if start + i == 0 {
                vec![prompt_glyph.clone(), prompt_gap.clone()]
            } else {
                vec![indent.clone()]
            };
            let selected = if let Some((sel_start, sel_end)) = input_range_sel {
                let lo = sel_start.max(wl.start_byte);
                let hi = sel_end.min(wl.end_byte);
                if lo < hi {
                    Some((lo - wl.start_byte, hi - wl.start_byte))
                } else {
                    None
                }
            } else {
                line_selection(sel_range, wl)
            };
            // A resolved `/command` token is accented from the input's first
            // byte; clamp to this wrapped row so the accent stops at the wrap
            // boundary instead of bleeding into the argument text.
            let hl_end = highlight_len
                .filter(|_| start + i == 0)
                .map(|len| len.min(wl.text.len()));
            // Clamp each chip to this wrapped row, but only when the chip has
            // a payload really staged behind it (`N <= count` for its kind).
            // A chip split across a wrap boundary paints both fragments with
            // the same pill, so a pasted block stays visually contiguous as
            // it wraps. An orphan label — typed by hand, or left over after
            // the paste was undone — has no backing payload and renders as
            // plain text, so the colored pill never lies about an attachment
            // that isn't there.
            let mut chip_ranges: Vec<(usize, usize, ChipKind)> = Vec::new();
            for chip in &chips {
                let backed = match chip.kind {
                    ChipKind::Image => chip.number <= image_count,
                    ChipKind::Paste => chip.number <= paste_count,
                };
                if !backed || chip.end_byte <= wl.start_byte || chip.start_byte >= wl.end_byte {
                    continue;
                }
                let lo = chip.start_byte.saturating_sub(wl.start_byte);
                let hi = (chip.end_byte - wl.start_byte).min(wl.text.len());
                if lo < hi {
                    chip_ranges.push((lo, hi, chip.kind));
                }
            }
            push_styled_runs(
                &mut spans,
                &wl.text,
                hl_end,
                &chip_ranges,
                selected,
                RunStyles {
                    base: base_text,
                    accent: accent_text,
                    chip_paste: chip_paste_style,
                    chip_image: chip_image_style,
                    selected_bg,
                    chip_paste_fg,
                    chip_image_fg,
                },
            );
            let mut ghost_cols = 0;
            if let Some(ghost) = ghost_text.filter(|g| !g.is_empty()) {
                if start + i == wrapped.len() - 1 {
                    if let Some(suffix) = ghost.strip_prefix(input) {
                        let ghost_first_line = match suffix.find('\n') {
                            Some(pos) => &suffix[..pos],
                            None => suffix,
                        };
                        if !ghost_first_line.is_empty() {
                            let available = full_w.saturating_sub(used);
                            let to_show = crate::shell::overlays::common::truncate_ellipsis(
                                ghost_first_line,
                                available,
                            );
                            ghost_cols = str_len(&to_show);
                            spans.push(Span::styled(
                                to_show,
                                Style::default()
                                    .bg(panel_bg)
                                    .fg(theme.muted())
                                    .add_modifier(Modifier::DIM),
                            ));
                        }
                    } else if visible_rows == 1 {
                        let ghost_first_line = match ghost.find('\n') {
                            Some(pos) => &ghost[..pos],
                            None => ghost,
                        };
                        let available = full_w.saturating_sub(used);
                        if available > 6 {
                            let preview = format!("  ↳ {ghost_first_line}");
                            let to_show = crate::shell::overlays::common::truncate_ellipsis(
                                &preview, available,
                            );
                            ghost_cols = str_len(&to_show);
                            spans.push(Span::styled(
                                to_show,
                                Style::default()
                                    .bg(panel_bg)
                                    .fg(theme.muted())
                                    .add_modifier(Modifier::DIM),
                            ));
                        }
                    }
                }
            }
            // Close the row with panel-bg air to the right edge.
            // used + tail = full_w.
            let tail_cols = full_w.saturating_sub(used + ghost_cols);
            spans.push(Span::styled(
                " ".repeat(tail_cols),
                Style::default().bg(panel_bg),
            ));
            lines.push(Line::from(spans));
        }
        if let Some(ghost) = ghost_text.filter(|g| !g.is_empty()) {
            if !ghost.starts_with(input) {
                let ghost_grid = TextGrid::new(ghost, text_width);
                let ghost_wrapped = ghost_grid.rows();
                for (gi, gw) in ghost_wrapped.iter().enumerate() {
                    if lines.len() >= visible_rows {
                        break;
                    }
                    let prefix = if gi == 0 { "↳ " } else { "  " };
                    let preview_text = format!("{prefix}{}", gw.text);
                    let to_show = crate::shell::overlays::common::truncate_ellipsis(
                        &preview_text,
                        text_width,
                    );
                    let text_cols = str_len(&to_show);
                    let tail_cols = full_w.saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + text_cols);
                    lines.push(Line::from(vec![
                        indent.clone(),
                        Span::styled(
                            to_show,
                            Style::default()
                                .bg(panel_bg)
                                .fg(theme.muted())
                                .add_modifier(Modifier::DIM),
                        ),
                        Span::styled(" ".repeat(tail_cols), Style::default().bg(panel_bg)),
                    ]));
                }
            }
        }
    }

    // Row 3: blank gap row
    // A second breathing row between the text and the meta row, so the hint
    // sentence reads as the box's own footnote rather than another text line.
    // When rows hide below the viewport it carries the `↓ N lines`
    // indicator (see [`chrome_row`]).
    lines.push(chrome_row(full_w, panel_bg, theme, '↓', hidden_below));

    // Row 4: the hint row
    // Navigation/actions lead on the left, execution/submit anchors on the
    // right. Keycaps and verbs tint with `panel_bg` so they blend into the box.
    // While unfocused the active sentence is replaced with a muted inactive indicator.
    {
        use crate::shell::composer_hints::ActionDensity;
        let keys_width = full_w
            .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
            .max(8);
        let density = ActionDensity::for_width(keys_width);
        let mut spans: Vec<Span<'static>> = vec![indent.clone()];
        if focused {
            let (left_spans, right_spans) = hint_row_parts(
                hints.can_retry,
                density,
                hints.compose_target,
                theme,
                panel_bg,
                hints.toggle_mode_key,
                hints.entry_focused,
            );
            let left_w: usize = left_spans.iter().map(|s| str_len(&s.content)).sum();
            let right_w: usize = right_spans.iter().map(|s| str_len(&s.content)).sum();

            let position_label = if wrapped.len() > visible_rows {
                let first = *input_scroll + 1;
                let last = (*input_scroll + visible_rows).min(wrapped.len());
                Some(format!("{first}–{last}/{} lines", wrapped.len()))
            } else {
                None
            };
            let pos_w = position_label.as_ref().map(|s| str_len(s) + 3).unwrap_or(0);

            let prefix_w = COMPOSER_PROMPT_PREFIX_COLS;
            let target_right_w = right_w + pos_w;
            let available = full_w.saturating_sub(COMPOSER_RIGHT_PAD_COLS);
            let needed = prefix_w + left_w + target_right_w;

            if available >= needed {
                spans.extend(left_spans);
                let gap = available - needed;
                spans.push(Span::styled(" ".repeat(gap), Style::default().bg(panel_bg)));
            } else {
                // Width constrained: apply Floor-1 rule (ADR-0054 [INV-AFFORDANCE-02]).
                // If left hints overflow the available budget, collapse them to `F1 more`
                // to guarantee non-destructive discovery without layout clipping.
                let space_for_left = available.saturating_sub(prefix_w + target_right_w + 3);
                let overflow_affordance =
                    nuo_ui_primitives::components::keycap::KeyAffordance::new("F1", "more");
                let overflow_w = overflow_affordance.width();
                if space_for_left >= overflow_w {
                    let [k, l] = overflow_affordance.render_spans(theme, panel_bg);
                    spans.push(k);
                    spans.push(l);
                    let gap = space_for_left - overflow_w + 3;
                    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(panel_bg)));
                } else {
                    let gap = available.saturating_sub(prefix_w + target_right_w);
                    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(panel_bg)));
                }
            }

            if let Some(pos) = position_label
                && available > needed
            {
                spans.push(Span::styled(
                    pos,
                    Style::default().bg(panel_bg).fg(theme.muted()),
                ));
                spans.push(Span::styled("   ", Style::default().bg(panel_bg)));
            }

            spans.extend(right_spans);
        } else {
            spans.push(Span::styled(
                "(Composer inactive)",
                Style::default().bg(panel_bg).fg(theme.muted()),
            ));
        }
        let used = spans
            .iter()
            .map(|span| str_len(&span.content))
            .sum::<usize>();
        let tail_cols = full_w.saturating_sub(used);
        spans.push(Span::styled(
            " ".repeat(tail_cols),
            Style::default().bg(panel_bg),
        ));
        lines.push(Line::from(spans));
    }

    frame.render_widget(&mut Paragraph::new(lines), input_rect);

    // Record each visible text row in the layout map so mouse drag selection
    // and copy work on the live input. Skipped when the API-key modal masks
    // the display (byte offsets wouldn't match the real input).
    if record {
        layout_map.set_composer_rect(input_rect);
        let start = *input_scroll;
        let end = (*input_scroll + visible_rows).min(wrapped.len());
        for (i, wl) in wrapped[start..end].iter().enumerate() {
            let row_y = input_rect.y + COMPOSER_TEXT_ROW_OFFSET + i as u16;
            layout_map.push(BlockRegion {
                message_idx: INPUT_MSG_IDX,
                block_idx: 0,
                start_byte: wl.start_byte,
                end_byte: wl.end_byte,
                text: wl.text.clone(),
                prefix_cols: COMPOSER_PROMPT_PREFIX_COLS as u16,
                rect: Rect::new(input_rect.x, row_y, full_w as u16, 1),
                hidden_ranges: Vec::new(),
            });
        }
    }

    // Position the caret relative to the visible slice, after the `> ` /
    // indent prefix. Gated by `show_caret` rather than `focused`: the caret is
    // hidden whenever a modal takes over input or a selection is active, so it
    // never sits inside a box that doesn't accept keypresses. The coordinates
    // come from the shared [`cursor_screen_pos`] so the rendered caret and the
    // terminal cursor are always identical.
    if show_caret && let Some((cursor_x, cursor_y)) = cursor {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Push `text` onto `spans`, splitting it at every style boundary so each
/// emitted run is uniform. Boundaries come from the optional leading
/// `/command` accent (`accent_len` bytes, relative to `text`), the selection
/// range (`selected`, relative to `text`), and the attachment-chip ranges
/// (`chip_ranges`, `(lo, hi, kind)` relative to `text`). Precedence:
///
/// 1. **Selection** wins on background — the highlighted slice stays a
///    uniform `selected_bg` — but a chip keeps its identity color, so the
///    user can still see which pasted block is selected.
/// 2. **Chip pills** paint their tinted band + bold label (`chip_paste` /
///    `chip_image`), so paste chips and image chips read as distinct blocks.
/// 3. The **command accent** (bold + brand color).
/// 4. Plain base text.
///    Style set for `push_styled_runs`: the base/accent text styles, the two
///    chip styles, and the selection/chip foreground colors.
struct RunStyles {
    base: Style,
    accent: Style,
    chip_paste: Style,
    chip_image: Style,
    selected_bg: Color,
    chip_paste_fg: Color,
    chip_image_fg: Color,
}

fn push_styled_runs(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    accent_len: Option<usize>,
    chip_ranges: &[(usize, usize, ChipKind)],
    selected: Option<(usize, usize)>,
    styles: RunStyles,
) {
    if text.is_empty() {
        return;
    }
    // Every byte offset where the run style can change: the text edges, the
    // accent boundary, the selection edges, and each chip's edges.
    let mut points: Vec<usize> = Vec::with_capacity(6 + chip_ranges.len() * 2);
    points.push(0);
    points.push(text.len());
    if let Some(len) = accent_len {
        points.push(len);
    }
    if let Some((lo, hi)) = selected {
        points.push(lo);
        points.push(hi);
    }
    for &(lo, hi, _) in chip_ranges {
        points.push(lo);
        points.push(hi);
    }
    points.sort_unstable();
    points.dedup();

    let is_selected = |p: usize| matches!(selected, Some((lo, hi)) if p >= lo && p < hi);
    let chip_of = |p: usize| {
        chip_ranges
            .iter()
            .find(|&&(lo, hi, _)| p >= lo && p < hi)
            .map(|&(_, _, kind)| kind)
    };
    let in_accent = |p: usize| accent_len.map(|len| p < len).unwrap_or(false);

    let mut i = 0;
    while i + 1 < points.len() {
        let lo = points[i];
        let hi = points[i + 1];
        i += 1;
        if lo >= hi {
            continue;
        }
        let style = if is_selected(lo) {
            match chip_of(lo) {
                Some(ChipKind::Paste) => Style::default()
                    .fg(styles.chip_paste_fg)
                    .bg(styles.selected_bg),
                Some(ChipKind::Image) => Style::default()
                    .fg(styles.chip_image_fg)
                    .bg(styles.selected_bg),
                None => styles.base.bg(styles.selected_bg),
            }
        } else if let Some(kind) = chip_of(lo) {
            match kind {
                ChipKind::Paste => styles.chip_paste,
                ChipKind::Image => styles.chip_image,
            }
        } else if in_accent(lo) {
            styles.accent
        } else {
            styles.base
        };
        spans.push(Span::styled(text[lo..hi].to_string(), style));
    }
}
