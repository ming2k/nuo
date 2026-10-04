//! Permission sheet (inline) and question modal.

use nuotc::{
    Frame, Rect, {Block as RtBlock, Clear, Paragraph}, {Line, Span}, {Modifier, Style},
};

use nuo_wire::{PermissionRequest, UserQuestionRequest};

use crate::components::options::{ChoiceMarker, ChoiceOptionRow, ChoiceTone, push_wrapped_styled};
use crate::design::MODAL_INNER_H_PADDING;
use crate::model::layout::{PermissionActionHit, QuestionOptionHit};
use crate::primitives::{
    FooterHint, contrast_fg, keyvocab, modal_footer_text, modal_frame, panel_block, render_body,
    render_modal_footer,
};
use crate::render::Theme;
use crate::text_layout::wrap_text;
use crate::ui::ComponentTree;
use unicode_width::UnicodeWidthStr;

// The permission sheet renders inline, replacing the composer (input box)
// area. Collapsed it shows a one-line summary plus the action footer;
// expanding "Details" grows the body upward into the transcript.
const PERMISSION_H_PADDING: u16 = 2;
const PERMISSION_TOP_PADDING: u16 = 1;
const PERMISSION_FOOTER_HEIGHT: u16 = 1;
const PERMISSION_BODY_FOOTER_GAP: u16 = 1;
/// Max body rows in the collapsed (summary-only) state.
const PERMISSION_COLLAPSED_BODY_CAP: u16 = 2;
/// Max body rows when "Details" is expanded; the rest is scrollable.
const PERMISSION_MAX_BODY_ROWS: u16 = 14;

/// Total number of interactive actions in the permission sheet footer.
pub fn permission_action_count(confirm_always: bool, one_off: bool) -> usize {
    if confirm_always && !one_off {
        2
    } else if one_off {
        3
    } else {
        4
    }
}

/// options; the user navigates with ↑/↓, selects with Space, and advances with
/// Enter. Multi-select questions use checkboxes; single-select
/// shows no marker at all — the highlight *is* the selection (it moves live
/// with ↑/↓ and a digit jump). Enter advances to the next question or submits
/// all answers on the final page. A numbered digit key (1-9) jumps directly to
/// an option; Shift+Tab returns to the previous question.
const OTHER_OPTION_LABEL: &str = "Other";

#[allow(clippy::too_many_arguments)] // modal draw fns thread many context args by nature
pub fn draw_question_modal(
    frame: &mut Frame,
    hit_map: &mut ComponentTree,
    request: &UserQuestionRequest,
    current_question: usize,
    selected: &[Vec<usize>],
    other_text: &[String],
    highlighted: usize,
    scroll: &mut usize,
    follow_highlight: bool,
    queue_depth: usize,
    slot: Rect,
    // The frame-level caret verdict (ADR-0205): `true` only while this sheet is
    // the layer that owns the physical cursor. The sheet never places the
    // cursor on its own authority.
    show_caret: bool,
    theme: &Theme,
) -> nuotc::Rect {
    // The minimum body height that keeps the sheet usable. Below this the
    // body paints zero (or highlight-starved) rows — the "blank sheet that
    // eats every keypress" failure — so the frame falls back to a
    // centered panel sized for the content instead.
    const MIN_BODY_ROWS: u16 = 2;
    // ADR-0173 §3: the question sheet is anchored to the composer slot —
    // the same bottom edge, extended over the hint bar — not centered over
    // the surface. The body scrolls within whatever height the slot leaves.
    //
    // The slot's height is only partially ours to spend: a sheet pinned
    // over the hint bar leaves just the composer's raw height, and the
    // frame's header/footer/padding chrome is deducted from it. A request
    // with a header row, a multi-line question, or descriptions per option
    // does not fit in that budget, and the body would open at (or below)
    // zero rows — a blank, key-dead sheet. Measure the demand first
    // (mirroring the wrap passes the body build performs below) and grow
    // the sheet upward into the transcript when the slot's budget falls
    // short, capped at the terminal so the header always stays on screen.
    let measure_width = slot.width.saturating_sub(2 * MODAL_INNER_H_PADDING).max(1) as usize;
    let demand = request
        .questions
        .get(current_question)
        .map(|q| {
            let mut rows = 0usize;
            if let Some(header) = &q.header {
                rows += wrap_text(header, measure_width).len();
            }
            rows += wrap_text(&q.question, measure_width).len();
            rows += 1; // the blank gap row between the question and the options
            let q_selected = selected.get(current_question);
            let other_idx = q.options.len();
            for (i, option) in q.options.iter().enumerate() {
                let row = ChoiceOptionRow {
                    label: &option.label,
                    description: option.description.as_deref(),
                    selected: q_selected.is_some_and(|s| s.contains(&i)),
                    highlighted: false,
                    tone: ChoiceTone::Flat,
                    marker: ChoiceMarker::Checkbox,
                }
                .measure_lines(measure_width);
                rows += row;
            }
            // The synthetic "Other" row.
            rows += ChoiceOptionRow {
                label: OTHER_OPTION_LABEL,
                description: None,
                selected: q_selected.is_some_and(|s| s.contains(&other_idx)),
                highlighted: false,
                tone: ChoiceTone::Flat,
                marker: ChoiceMarker::Checkbox,
            }
            .measure_lines(measure_width);
            // The "Other" free-text field — now always rendered beneath the
            // "Other" option line (focused: live value; unfocused: the typed
            // value or a dim "Other" placeholder), so it always occupies its
            // wrapped rows in the demand measurement.
            let field = other_text
                .get(current_question)
                .map(String::as_str)
                .unwrap_or("");
            rows += wrap_text(
                field,
                measure_width.saturating_sub(OTHER_FIELD_INDENT).max(1),
            )
            .len()
            .max(1);
            rows
        })
        .unwrap_or(0);
    let content_h = demand.min(u16::MAX as usize) as u16;
    let terminal_bottom = frame.area().bottom();
    let slot_bottom = slot.y.saturating_add(slot.height).min(terminal_bottom);
    // The `+2` lets the follow nudge keep the highlighted row one row clear
    // of the scrollbar's bottom cap (`▼`), so the selected option is never
    // overlapped by the indicator.
    let desired = content_h.saturating_add(2);
    // The sheet grows upward into the transcript but only up to the shared
    // interaction-slot maximum (`sheet_max_height` — the same
    // `terminal_height / 2` rule the composer input box caps itself at), so
    // the composer and every sheet agree on "how tall the slot can get" and
    // neither ever covers more than half the terminal. Content that exceeds
    // the cap scrolls in the body. The slot's own height is the minimum (so
    // a short question still reads as the drop-in composer replacement it is
    // designed to be).
    let available_above = crate::design::sheet_max_height(frame.area().height);
    let desired_h = desired.max(slot.height).min(available_above);
    let sheet_top = slot_bottom.saturating_sub(desired_h);
    let area = Rect::new(
        slot.x,
        sheet_top,
        slot.width,
        slot_bottom.saturating_sub(sheet_top).max(1),
    );
    let f = modal_frame(frame, area, theme, true, true);
    hit_map.mount(
        crate::ui::UiKey::Sheet(crate::sheet::SheetKind::Question),
        area,
    );
    // Degrade gracefully rather than silently: if even the enlarged sheet
    // still cannot show a single option row (an extreme — tiny terminal, or
    // a resize race that collapsed the slot), the anchored layout would
    // paint an empty, key-dead panel. Render a minimal centered panel
    // instead — every key the sheet owns still applies, so the user can
    // answer and move on. This is the last-line defense for the
    // "blank sheet that ate my terminal" failure.
    let body_rows = f.body.height as usize;
    if body_rows < MIN_BODY_ROWS as usize && area.height >= 3 {
        return draw_question_modal_fallback(
            frame,
            hit_map,
            request,
            current_question,
            selected,
            highlighted,
            queue_depth,
            terminal_bottom,
            theme,
        );
    }

    let question = request.questions.get(current_question);
    let total = request.questions.len();

    if let Some(h) = f.header {
        let mut title = if total > 1 {
            format!("Question {}/{}", current_question + 1, total)
        } else {
            "Question".to_string()
        };
        if queue_depth > 0 {
            title.push_str(&format!(" · +{queue_depth} queued"));
        }
        let mut spans = Vec::new();
        if let Some(origin) = &request.origin {
            spans.push(Span::styled(
                format!("[{}] ", origin),
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            title,
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), h);
    }

    let mut body_lines: Vec<Line> = Vec::new();
    let mut option_rows: Vec<(usize, usize, usize)> = Vec::new();
    let body_width = f.body.width as usize;
    let mut highlighted_row = None;
    // Body row index + column of the "Other" free-text field's caret,
    // captured only while "Other" is highlighted. Unlike a plain list row,
    // the field can span several wrapped lines, so both the body-scroll
    // follow target and the real terminal cursor position refer to the
    // *caret row* (last wrapped line), not the "Other" label row —
    // otherwise a multi-line field leaves the caret scrolled out of view.
    let mut other_caret_row: Option<usize> = None;
    let mut other_caret_col: usize = 0;
    // 2-column indent of the "Other" free-text field: the `› ` prompt
    // glyph (1 col) plus one gap col, matching the `"› "` prefix passed to
    // `push_wrapped_styled`. Continuation rows use the same 2 columns so
    // wrapped lines stay aligned under the prompt.
    const OTHER_FIELD_INDENT: usize = 2;
    let other_highlighted = question.is_some_and(|q| highlighted == q.options.len());
    if let Some(q) = question {
        if let Some(header) = &q.header {
            push_wrapped_styled(
                &mut body_lines,
                "",
                "",
                header,
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
                body_width,
            );
        }
        push_wrapped_styled(
            &mut body_lines,
            "",
            "",
            &q.question,
            Style::default().fg(theme.fg()),
            body_width,
        );
        body_lines.push(Line::from(""));

        let q_selected = selected.get(current_question);
        let other_index = q.options.len();
        let other_text_value = other_text
            .get(current_question)
            .map(String::as_str)
            .unwrap_or("");

        for (i, option) in q.options.iter().enumerate() {
            let is_selected = q_selected.is_some_and(|s| s.contains(&i));
            let is_highlighted = i == highlighted;
            let row = body_lines.len();
            if is_highlighted {
                highlighted_row = Some(row);
            }
            let start = body_lines.len();
            render_question_option(
                &mut body_lines,
                i,
                &option.label,
                option.description.as_deref(),
                is_selected,
                is_highlighted,
                q.multi_select,
                body_width,
                theme,
            );
            option_rows.push((i, start, body_lines.len()));
        }

        let row = body_lines.len();
        if other_highlighted {
            highlighted_row = Some(row);
        }
        let other_start = body_lines.len();
        render_question_option(
            &mut body_lines,
            other_index,
            OTHER_OPTION_LABEL,
            None,
            q_selected.is_some_and(|s| s.contains(&other_index)),
            other_highlighted,
            q.multi_select,
            body_width,
            theme,
        );
        // The free-text field row sits directly beneath the "Other" option
        // line, and it is ALWAYS shown (not only while highlighted): a
        // decision sheet whose last option is a text field must keep that
        // field visible so the user can see their typed "Other" value (or
        // that the option even has an input) without having to re-navigate
        // to it. While highlighted it becomes the live input surface.
        {
            let field_start_row = body_lines.len();
            // The field is a real text-input line: a brand `›` prompt glyph
            // (mirroring the composer's shell-style prompt) plus a gap, then
            // the typed text. The 2-column prefix (the field's own prompt)
            // sits under the checkbox column, so the input reads as "the
            // Other value being typed" aligned with the label text start.
            let field_prefix = "› ";
            let field_indent = "  ";
            if other_text_value.is_empty() {
                // Empty: paint a muted placeholder so the user always sees
                // the field is an input surface — the shell prompt + a dim
                // "type your answer…" hint (shown while focused) or a quieter
                // "your own…" (while the row is not active, so the row still
                // reads as having an input without repeating the "Other"
                // label verbatim below itself).
                let hint = if other_highlighted {
                    "type your answer…"
                } else {
                    "your own…"
                };
                push_wrapped_styled(
                    &mut body_lines,
                    field_prefix,
                    field_indent,
                    hint,
                    Style::default().fg(theme.muted()),
                    body_width,
                );
            } else {
                // The user has typed: show the value in the brand color (the
                // active input surface) or muted (a filled-but-unfocused
                // field, still reviewing the choice).
                push_wrapped_styled(
                    &mut body_lines,
                    field_prefix,
                    field_indent,
                    other_text_value,
                    Style::default().fg(if other_highlighted {
                        theme.brand()
                    } else {
                        theme.muted()
                    }),
                    body_width,
                );
            }
            // Resolve the caret location through the *same* `wrap_text` pass
            // the renderer used (same prefix budget) so the body-scroll follow
            // target and the cursor placement both point at the caret's real
            // visual row + column. The field is append-only, so the caret is
            // always at the end of the text: last wrapped row, end column.
            let wrap_budget = body_width.saturating_sub(OTHER_FIELD_INDENT).max(1);
            let wrapped = wrap_text(other_text_value, wrap_budget);
            let wrapped_rows = wrapped.len().max(1);
            let caret_local_col = wrapped
                .last()
                .map(|wl| nuotc::text::cursor_column(&wl.text, wl.text.len()))
                .unwrap_or(0);
            if other_highlighted {
                other_caret_row = Some(field_start_row + wrapped_rows.saturating_sub(1));
                other_caret_col = caret_local_col;
            }
        }
        option_rows.push((other_index, other_start, body_lines.len()));
    }

    // Auto-follow the highlight only while navigating (the default after open /
    // ↑↓ / digit-jump); a manual wheel/page scroll clears the flag so the user
    // can browse a long question or option list without the body snapping back
    // to the cursor. Mirrors the session / history modals.
    //
    // The follow target is the **end** (last wrapped row) of the highlighted
    // option, not its first line: an option with a wrapped label + a
    // description spans several visual rows, and pinning the first row leaves
    // the tail clipped below the fold. Following the last row guarantees the
    // whole option stays inside the viewport whenever space allows (the
    // shared `resolve_scroll` edge-pin logic then walks the scroll back up so
    // the first row is also visible). `option_rows` records each option's
    // `[start, end)` row range; the "Other" free-text field, when active,
    // must keep its **caret** row (the very bottom of the field) visible
    // instead — see below.
    let follow_target = other_caret_row.or_else(|| {
        // Find the highlighted option's end row (exclusive) minus one.
        highlighted_row.map(|start_row| {
            option_rows
                .iter()
                .find(|(_, start, _)| *start == start_row)
                .map(|(_, _, end)| end.saturating_sub(1))
                .unwrap_or(start_row)
        })
    });
    let follow = if follow_highlight {
        follow_target
    } else {
        None
    };
    render_body(
        frame,
        f.body,
        body_lines,
        scroll,
        crate::primitives::BodyRenderOptions::follow(follow),
        theme,
    );
    record_question_hits(hit_map, f.body, &option_rows, *scroll);

    // Place the real terminal cursor in the "Other" free-text field — the only
    // text-input surface in this modal. This is what the host IME samples to
    // anchor its composition window; without it, IME-based input (CJK, etc.)
    // cannot bind to the field. The field's 2-column `› ` prompt matches the
    // `"› "` prefix passed to `push_wrapped_styled`, and the caret sits at
    // the end of the typed text (the field is append-only, so the caret is
    // always at the end).
    //
    // The caret row was resolved through the *same* `wrap_text` pass used for
    // the follow target above, and `follow` has already nudged `scroll` to
    // keep it on screen. We still guard by the visible window: if the field is
    // scrolled away (e.g. the user is browsing with wheel/Pg), there is no
    // honest coordinate and the event loop leaves the cursor hidden.
    if show_caret && let Some(caret_row) = other_caret_row {
        let visible_top = *scroll;
        let visible_bottom = scroll.saturating_add(f.body.height as usize);
        if caret_row >= visible_top && caret_row < visible_bottom {
            let indent: u16 = OTHER_FIELD_INDENT as u16;
            let cursor_x = f
                .body
                .x
                .saturating_add(indent)
                .saturating_add(other_caret_col.min(u16::MAX as usize) as u16);
            let cursor_y = f
                .body
                .y
                .saturating_add((caret_row - visible_top).min(u16::MAX as usize) as u16);
            // Clamp to the body's right edge so a wide-glyph caret at the last
            // column never lands in the scrollbar gutter.
            let cursor_x = cursor_x.min(f.body.right().saturating_sub(1));
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    if let Some(fo) = f.footer {
        // Single-select is live (the highlight is the selection), so there is
        // no "select" action to advertise — Space is a no-op there. Only
        // multi-select offers the Space toggle.
        let enter_label = if current_question + 1 < total {
            "next"
        } else {
            "submit"
        };
        let mut hints = vec![
            FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
            FooterHint::navigation("wheel/Pg", "scroll"),
            FooterHint::key_primary(crate::keymap::Key::ENTER, enter_label),
        ];
        if current_question > 0 {
            hints.push(FooterHint::secondary(keyvocab::SHIFT_TAB, "back"));
        }
        if question.is_some_and(|q| q.multi_select) {
            hints.push(FooterHint::secondary(keyvocab::SPACE, "select"));
        }
        hints.push(FooterHint::secondary("1-9", "jump"));
        if other_highlighted {
            // The "Other" free-text field is the active input surface; tell
            // the user they can type into it (and that Enter still advances).
            hints.push(FooterHint::secondary("type", "fill"));
        }
        hints.push(FooterHint::key_always(crate::keymap::Key::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }
    area
}

/// Last-line fallback: a minimal, *usable* rendering of the question sheet
/// for terminals where the anchored slot collapsed to fewer rows than the
/// sheet's chrome needs. Draws a small centered panel with the header, the
/// question line, one option row per line (the highlight marked), and the
/// decision footer — everything the keyboard flow needs. It records the
/// same hit-map rows the main renderer would, so mouse clicks keep working.
#[allow(clippy::too_many_arguments)]
fn draw_question_modal_fallback(
    frame: &mut Frame,
    hit_map: &mut ComponentTree,
    request: &UserQuestionRequest,
    current_question: usize,
    selected: &[Vec<usize>],
    highlighted: usize,
    queue_depth: usize,
    max_height: u16,
    theme: &Theme,
) -> nuotc::Rect {
    let question = request.questions.get(current_question);
    let total = request.questions.len();
    let enter_label = if current_question + 1 < total {
        "next"
    } else {
        "submit"
    };

    // Compact body: the question text (truncated to its first line — the
    // width is the terminal's, which is what limits us, so the line budget
    // is what a normal sheet would use at full width), then one row per
    // option. Options render as `› label` for the highlighted row (the
    // highlight *is* the selection in single-select) or `[x]/[ ] label` for
    // multi-select.
    let mut body_lines: Vec<Line> = Vec::new();
    let mut option_rows: Vec<(usize, usize, usize)> = Vec::new();
    let body_width = frame.area().width.saturating_sub(4).max(1) as usize;
    if let Some(q) = question {
        if let Some(header) = &q.header {
            push_wrapped_styled(
                &mut body_lines,
                "",
                "",
                header,
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
                body_width,
            );
        }
        push_wrapped_styled(
            &mut body_lines,
            "",
            "",
            &q.question,
            Style::default().fg(theme.fg()),
            body_width,
        );
        body_lines.push(Line::from(""));
        let q_selected = selected.get(current_question);
        for (i, option) in q.options.iter().enumerate() {
            let row = body_lines.len();
            ChoiceOptionRow {
                label: &option.label,
                description: None, // descriptions dropped in the fallback
                selected: q_selected.is_some_and(|s| s.contains(&i)),
                highlighted: i == highlighted,
                tone: ChoiceTone::Flat,
                marker: ChoiceMarker::Checkbox,
            }
            .push_lines(&mut body_lines, body_width, theme);
            option_rows.push((i, row, body_lines.len()));
        }
        // The synthetic "Other" row: selectable, but its free-text field is
        // dropped in the compact fallback (typed text is preserved in the
        // model and flows into the reply as before).
        let other_index = q.options.len();
        let row = body_lines.len();
        ChoiceOptionRow {
            label: OTHER_OPTION_LABEL,
            description: None,
            selected: q_selected.is_some_and(|s| s.contains(&other_index)),
            highlighted: highlighted == other_index,
            tone: ChoiceTone::Flat,
            marker: ChoiceMarker::Checkbox,
        }
        .push_lines(&mut body_lines, body_width, theme);
        option_rows.push((other_index, row, body_lines.len()));
    }
    // Truncate from the top if the terminal cannot hold every line: keep
    // the options (the decision surface) over the question header.
    let visible = max_height.saturating_sub(3).max(1) as usize; // chrome rows
    let skip = body_lines.len().saturating_sub(visible);
    if skip > 0 {
        body_lines.drain(..skip);
        option_rows = option_rows
            .into_iter()
            .filter_map(|(i, start, end)| {
                let s = start.saturating_sub(skip);
                let e = end.saturating_sub(skip);
                (end > skip).then_some((i, s, e))
            })
            .collect();
    }

    let body_h = (body_lines.len().min(visible) as u16).max(1);
    let panel_h = body_h
        .saturating_add(3) // top pad + footer + bottom pad
        .min(max_height)
        .max(3);
    let full = frame.area();
    let w = full.width.clamp(20, 80);
    let x = full.x + full.width.saturating_sub(w) / 2;
    let y = full.y + full.height.saturating_sub(panel_h) / 2;
    let area = Rect::new(x, y, w, panel_h);
    hit_map.mount(
        crate::ui::UiKey::Sheet(crate::sheet::SheetKind::Question),
        area,
    );

    frame.render_widget(Clear, area);
    frame.render_widget(panel_block(theme, theme.brand(), theme.panel()), area);

    let body_rect = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        body_h.min(area.height.saturating_sub(2)),
    );
    let mut scroll = 0usize;
    render_body(
        frame,
        body_rect,
        body_lines,
        &mut scroll,
        crate::primitives::BodyRenderOptions::follow(None),
        theme,
    );
    record_question_hits(hit_map, body_rect, &option_rows, scroll);

    let footer_y = area.y.saturating_add(area.height).saturating_sub(1);
    let hints = vec![
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, enter_label),
        FooterHint::secondary("1-9", "jump"),
        FooterHint::key_always(crate::keymap::Key::ESC, "cancel"),
    ];
    render_modal_footer(
        frame,
        Rect::new(
            area.x.saturating_add(1),
            footer_y,
            area.width.saturating_sub(2),
            1,
        ),
        &hints,
        theme,
    );
    let _ = (total, queue_depth); // compact panel skips the paged/queued badges
    area
}

fn record_question_hits(
    hit_map: &mut ComponentTree,
    body: Rect,
    option_rows: &[(usize, usize, usize)],
    scroll: usize,
) {
    if body.width == 0 || body.height == 0 {
        return;
    }
    let visible_top = scroll;
    let visible_bottom = scroll + body.height as usize;
    for &(option_index, start, end) in option_rows {
        let top = start.max(visible_top);
        let bottom = end.max(start + 1).min(visible_bottom);
        if top >= bottom {
            continue;
        }
        hit_map.mount_question_option(QuestionOptionHit {
            option_index,
            rect: Rect::new(
                body.x,
                body.y + (top - visible_top) as u16,
                body.width,
                (bottom - top) as u16,
            ),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_question_option(
    lines: &mut Vec<Line<'static>>,
    _index: usize,
    label: &str,
    description: Option<&str>,
    is_selected: bool,
    is_highlighted: bool,
    _multi_select: bool,
    body_width: usize,
    theme: &Theme,
) {
    // A checkbox marker (`[x]`/`[ ]`) is shown for EVERY question row —
    // single-select included. That gives single-select rows a stable
    // selection affordance: `[x]` marks the currently-chosen option, `[ ]`
    // the rest, and the `›` cursor rides alongside (painted by the marker
    // when highlighted) so the user's current position and their committed
    // choice stay visually distinct in both modes.
    ChoiceOptionRow {
        label,
        description,
        selected: is_selected,
        highlighted: is_highlighted,
        tone: ChoiceTone::Flat,
        marker: ChoiceMarker::Checkbox,
    }
    .push_lines(lines, body_width, theme);
}

/// Draw a blocking tool permission request inline, replacing the composer
/// (input box) area. The transcript above stays visible and scrollable.
///
/// Collapsed (the default) the sheet is a one-line summary — the tool name
/// plus its scope (the specific path/command being touched) — followed by a
/// footer of inline actions. Selecting "Details" expands the body upward to
/// reveal the full description and arguments without leaving the prompt.
#[allow(clippy::too_many_arguments)]
pub fn draw_permission_sheet(
    frame: &mut Frame,
    hit_map: &mut ComponentTree,
    request: &PermissionRequest,
    selected: usize,
    confirm_always: bool,
    show_details: bool,
    scroll: usize,
    queue_depth: usize,
    input_rect: Rect,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> usize {
    let area_bottom = input_rect.y + input_rect.height;

    let arguments = serde_json::from_str::<serde_json::Value>(&request.arguments)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| request.arguments.clone());
    let scope_meaningful = !request.scope.is_empty() && request.scope != "*";

    // Header line: human-friendly label (falling back to the raw tool name
    // for safety), plus the concrete scope (path/command) when it adds
    // information. The scope is the single most useful detail, so it earns a
    // spot in the collapsed summary; everything else hides behind "Details".
    // The left bar carries the severity cue.
    let label = if request.label.is_empty() {
        request.tool.clone()
    } else {
        request.label.clone()
    };
    let mut header = Vec::new();
    if let Some(origin) = &request.origin {
        header.push(Span::styled(
            format!("[{}] ", origin),
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        ));
    }
    header.push(Span::styled(
        label,
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    ));
    // #10: an elevation prompt (out-of-scope target) is flagged ⚠ so the
    // operator understands they are authorising access *beyond* the configured
    // boundary, not a routine in-scope call. Rendered in the error colour.
    if request.elevation {
        header.push(Span::styled("  ", Style::default()));
        header.push(Span::styled(
            "⚠ out of scope",
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if confirm_always {
        header.push(Span::styled(
            " — always allow until exit?",
            Style::default().fg(theme.fg()),
        ));
    } else if request.one_off {
        // A one-off dangerous-command confirm: flag that this grant will not be
        // remembered, so the user is not surprised to be re-prompted next time.
        header.push(Span::styled(
            " — one-off (not remembered)",
            Style::default().fg(theme.warn()),
        ));
    } else if scope_meaningful {
        header.push(Span::styled("  ", Style::default()));
        header.push(Span::styled(
            request.scope.clone(),
            Style::default().fg(theme.info()),
        ));
    }
    if queue_depth > 1 {
        header.push(Span::styled("  ", Style::default()));
        header.push(Span::styled(
            format!("{queue_depth} queued"),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut body_lines: Vec<Line> = Vec::new();
    body_lines.push(Line::from(header));

    if confirm_always {
        body_lines.push(Line::from(Span::styled(
            "Grants this tool until muta exits.",
            Style::default().fg(theme.muted()),
        )));
    } else if show_details {
        body_lines.push(Line::from(""));
        body_lines.push(Line::from(Span::styled(
            request.description.clone(),
            Style::default().fg(theme.fg()),
        )));
        body_lines.push(Line::from(""));
        body_lines.push(Line::from(Span::styled(
            "Arguments",
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        )));
        body_lines.extend(arguments.lines().map(|line| {
            Line::from(Span::raw(line.to_string())).style(Style::default().fg(theme.code_text()))
        }));
    }

    let fixed = PERMISSION_TOP_PADDING + PERMISSION_BODY_FOOTER_GAP + PERMISSION_FOOTER_HEIGHT;
    let content_w = input_rect
        .width
        .saturating_sub(1 + 2 * PERMISSION_H_PADDING)
        .max(1);
    let body_total_rows: usize = body_lines
        .iter()
        .map(|line| {
            let width: usize = line.spans.iter().map(|span| span.content.width()).sum();
            width.max(1).div_ceil(content_w as usize)
        })
        .sum();

    // How tall the body may grow. Collapsed stays compact; expanded climbs
    // into the transcript but never past the top of the viewport.
    let body_cap: u16 = if confirm_always {
        body_total_rows.min(2).min(u16::MAX as usize) as u16
    } else if show_details {
        let room = area_bottom.saturating_sub(fixed).max(1);
        PERMISSION_MAX_BODY_ROWS.min(room)
    } else {
        PERMISSION_COLLAPSED_BODY_CAP
    };
    let body_h = (body_total_rows as u16).min(body_cap);
    let max_scroll = body_total_rows.saturating_sub(body_h as usize);
    let body_scroll = scroll.min(max_scroll);

    let desired_h = fixed + body_h;
    // Fill the composer slot when collapsed (so it reads as a drop-in
    // replacement for the input box); grow above it when expanded.
    let sheet_h = desired_h.max(input_rect.height).min(area_bottom).max(1);
    let sheet_top = area_bottom.saturating_sub(sheet_h);
    let area = Rect::new(input_rect.x, sheet_top, input_rect.width, sheet_h);
    hit_map.mount_permission_sheet(area);

    frame.render_widget(Clear, area);
    frame.render_widget(panel_block(theme, theme.warn(), theme.panel()), area);

    let content_x = area.x + 1 + PERMISSION_H_PADDING;
    let body_area = Rect::new(
        content_x,
        area.y + PERMISSION_TOP_PADDING,
        content_w,
        body_h,
    );
    // Selectable document: the tool-call arguments JSON (and the description
    // above it) is exactly what a user wants to copy while deciding. The
    // body's line-level scroll (`body_scroll` counts wrapped visual rows,
    // same accounting `resolve_scroll` uses) is passed straight through.
    let rows: Vec<crate::components::selectable_body::SelectableRow> = body_lines
        .into_iter()
        .map(crate::components::selectable_body::SelectableRow::from_line)
        .collect();
    let mut body_scroll_usize = body_scroll;
    crate::components::selectable_body::render_selectable_body(
        frame,
        body_area,
        &rows,
        &mut body_scroll_usize,
        None,
        theme,
        selection,
        layout_map,
    );

    let footer_y = area
        .y
        .saturating_add(sheet_h)
        .saturating_sub(PERMISSION_FOOTER_HEIGHT);
    let footer_band = Rect::new(
        area.x + 1,
        footer_y,
        area.width.saturating_sub(1),
        PERMISSION_FOOTER_HEIGHT,
    );
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.raised())),
        footer_band,
    );

    let details_label = if show_details { "Hide" } else { "Details" };
    // #2: a one-off prompt (the bash dangerous-command confirm) deliberately
    // does not persist an `Always` reply, so the "Always allow" option is
    // suppressed entirely — offering a button whose choice is silently ignored
    // is a UI/behaviour lie. The decision collapses to Allow once / Reject /
    // Details. (The confirm_always keyboard shortcut is also inert for these
    // prompts, since there is no Always choice to confirm.)
    let labels: Vec<&str> = if confirm_always && !request.one_off {
        vec!["Confirm always", "Cancel"]
    } else if request.one_off {
        vec!["Allow once", "Reject", details_label]
    } else {
        vec!["Allow once", "Always allow", "Reject", details_label]
    };

    let mut footer_spans: Vec<Span> = Vec::new();
    let mut action_x = content_x;
    for (index, label) in labels.iter().enumerate() {
        let is_cancel = confirm_always && index == 1;
        let is_reject = !confirm_always && index == 2;
        let is_selected = index == selected;
        let bg = if is_selected {
            if is_reject || is_cancel {
                theme.err()
            } else {
                theme.brand()
            }
        } else {
            theme.raised()
        };
        let fg = if is_selected {
            contrast_fg(bg)
        } else {
            theme.fg()
        };
        if index > 0 {
            footer_spans.push(Span::styled("  ", Style::default().bg(theme.raised())));
            action_x = action_x.saturating_add(2);
        }
        let text = format!(" {} ", label);
        let width = text.width().min(u16::MAX as usize) as u16;
        hit_map.mount_permission_action(PermissionActionHit {
            action_index: index,
            rect: Rect::new(action_x, footer_y, width, PERMISSION_FOOTER_HEIGHT),
        });
        footer_spans.push(Span::styled(
            text,
            Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD),
        ));
        action_x = action_x.saturating_add(width);
    }
    let hints: &[FooterHint] = if confirm_always {
        &[
            FooterHint::navigation(keyvocab::ARROWS_LR, "select"),
            FooterHint::key_primary(crate::keymap::Key::ENTER, "confirm"),
            FooterHint::key_always(crate::keymap::Key::ESC, "back"),
        ]
    } else if max_scroll > 0 {
        &[
            FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"),
            FooterHint::navigation(keyvocab::ARROWS_LR, "select"),
            FooterHint::key_primary(crate::keymap::Key::ENTER, "confirm"),
            FooterHint::key_always(crate::keymap::Key::ESC, "reject"),
        ]
    } else {
        &[
            FooterHint::navigation(keyvocab::ARROWS_LR, "select"),
            FooterHint::key_primary(crate::keymap::Key::ENTER, "confirm"),
            FooterHint::key_always(crate::keymap::Key::ESC, "reject"),
        ]
    };
    let footer_width = content_w as usize;
    let used: usize = footer_spans.iter().map(|s| s.content.width()).sum();
    let hint = modal_footer_text(hints, footer_width.saturating_sub(used));
    let hint_width = hint.width();
    if used + hint_width <= footer_width {
        footer_spans.push(Span::styled(
            " ".repeat(footer_width - used - hint_width),
            Style::default().bg(theme.raised()),
        ));
        footer_spans.push(Span::styled(
            hint,
            Style::default().bg(theme.raised()).fg(theme.muted()),
        ));
    } else if used < footer_width {
        footer_spans.push(Span::styled(
            " ".repeat(footer_width - used),
            Style::default().bg(theme.raised()),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(footer_spans)),
        Rect::new(content_x, footer_y, content_w, PERMISSION_FOOTER_HEIGHT),
    );
    max_scroll
}

/// Inline input-injection panel (L3.5 β): rendered over the composer rect when
/// an interactive `bash` command needs operator input. A one-line prompt
/// (the command + what to enter) above an input line that mirrors the
/// composer. When `secret` is set the typed text is masked as `•` so a
/// password/passphrase isn't shown in the clear. The panel is a left-bar
/// panel (`panel_block`) so it reads as the same surface language as the
/// permission sheet. Returns the rect it drew into.
pub fn draw_input_injection(
    frame: &mut Frame,
    request: &nuo_wire::InputRequest,
    input: &str,
    _cursor: usize,
    input_rect: Rect,
    theme: &Theme,
) -> Rect {
    use nuotc::Layout;
    // Split the composer rect into a prompt row + the input row(s).
    let chunks = Layout::default()
        .direction(nuotc::Direction::Vertical)
        .constraints([
            nuotc::Constraint::Length(1),
            nuotc::Constraint::Min(0),
        ])
        .split(input_rect);

    let prompt_rect = chunks[0];
    let entry_rect = chunks[1];

    // Prompt line: the command for context, then what to enter.
    let secret_label = if request.secret {
        " (input hidden)"
    } else {
        ""
    };
    let prompt_text = format!("{}  —  {}{}", request.command, request.prompt, secret_label);
    let prompt_line = Line::from(vec![
        Span::styled("┃ ", Style::default().fg(theme.warn())),
        Span::styled(
            request.command.clone(),
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  —  {}{}", request.prompt, secret_label),
            Style::default().fg(theme.muted()),
        ),
    ]);
    let _ = prompt_text;
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.user_surface())),
        prompt_rect,
    );
    frame.render_widget(Paragraph::new(prompt_line), prompt_rect);

    // Entry line: mask the typed input when secret, else show it verbatim.
    let display: String = if request.secret {
        "•".repeat(input.chars().count())
    } else {
        input.to_string()
    };
    let entry_prefix = "> ";
    let entry_line = Line::from(vec![
        Span::styled(
            entry_prefix,
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(display, Style::default().fg(theme.fg())),
        Span::styled(
            "  Enter=submit  Esc=skip (runs non-interactively)",
            Style::default().fg(theme.dim()),
        ),
    ]);
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.input_surface())),
        entry_rect,
    );
    frame.render_widget(Paragraph::new(entry_line), entry_rect);

    input_rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::{UserQuestion, UserQuestionOption};

    #[test]
    fn question_modal_records_option_hit_boxes() {
        let request = UserQuestionRequest {
            id: "q".into(),
            questions: vec![UserQuestion {
                header: None,
                question: "Pick one".into(),
                options: vec![
                    UserQuestionOption {
                        label: "A".into(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "B".into(),
                        description: Some("Second option".into()),
                    },
                ],
                multi_select: false,
            }],
            origin: None,
        };
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut hit_map = ComponentTree::new();
        terminal.draw(|frame| {
            hit_map.begin(frame.area());
            let mut scroll = 0;
            draw_question_modal(
                frame,
                &mut hit_map,
                &request,
                0,
                &[vec![0]],
                &[String::new()],
                0,
                &mut scroll,
                true,
                0,
                Rect::new(0, 0, 78, 16),
                true,
                &Theme::default(),
            );
        });

        hit_map.commit();
        assert!(find_question_hit(&hit_map, 80, 24, 0));
        assert!(find_question_hit(&hit_map, 80, 24, 1));
        assert!(find_question_hit(&hit_map, 80, 24, 2));
    }

    #[test]
    fn permission_sheet_records_footer_action_hit_boxes() {
        let request = PermissionRequest {
            id: "p".into(),
            tool: "execute_command".into(),
            label: "execute_command".into(),
            description: "Run a command".into(),
            arguments: r#"{"command":"cargo test"}"#.into(),
            scope: "*".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut hit_map = ComponentTree::new();
        terminal.draw(|frame| {
            hit_map.begin(frame.area());
            let rect = Rect::new(0, 16, 80, 8);
            let _ = draw_permission_sheet(
                frame,
                &mut hit_map,
                &request,
                0,
                false,
                false,
                0,
                0,
                rect,
                &Theme::default(),
                &crate::model::selection::SelectionState::None,
                &mut crate::model::layout::LayoutMap::new(),
            );
        });

        hit_map.commit();
        for action_index in 0..4 {
            assert!(
                find_permission_hit(&hit_map, 80, 24, action_index),
                "missing permission action {action_index}"
            );
        }
    }

    #[test]
    fn permission_sheet_shows_queue_depth_badge() {
        // ADR-0173 §3: concurrent AI interactions queue FIFO behind the front
        // sheet; the badge tells the user more decisions are pending.
        let request = PermissionRequest {
            id: "p".into(),
            tool: "execute_command".into(),
            label: "execute_command".into(),
            description: "Run a command".into(),
            arguments: r#"{"command":"ls"}"#.into(),
            scope: "*".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        let render = |depth: usize| {
            let mut terminal = nuotc::TestTerminal::new(80, 24);
            let mut hit_map = ComponentTree::new();
            terminal.draw(|frame| {
                hit_map.begin(frame.area());
                let rect = Rect::new(0, 16, 80, 8);
                let _ = draw_permission_sheet(
                    frame,
                    &mut hit_map,
                    &request,
                    0,
                    false,
                    false,
                    0,
                    depth,
                    rect,
                    &Theme::default(),
                    &crate::model::selection::SelectionState::None,
                    &mut crate::model::layout::LayoutMap::new(),
                );
            });
            sheet_text(&terminal)
        };

        assert!(
            !render(1).contains("queued"),
            "a lone request carries no badge"
        );
        assert!(
            render(3).contains("3 queued"),
            "the badge names how many requests are queued"
        );
    }

    fn find_question_hit(
        map: &ComponentTree,
        width: u16,
        height: u16,
        option_index: usize,
    ) -> bool {
        (0..height).any(|y| {
            (0..width).any(|x| {
                map.question_option_at(x, y)
                    .is_some_and(|hit| hit.option_index == option_index)
            })
        })
    }

    fn find_permission_hit(
        map: &ComponentTree,
        width: u16,
        height: u16,
        action_index: usize,
    ) -> bool {
        (0..height).any(|y| {
            (0..width).any(|x| {
                map.permission_action_at(x, y)
                    .is_some_and(|hit| hit.action_index == action_index)
            })
        })
    }

    /// The whole visible text of the sheet as one newline-joined string, so
    /// tests can assert on what the user actually sees.
    fn sheet_text(terminal: &nuotc::TestTerminal) -> String {
        let buf = terminal.buffer();
        let w = buf.area().width;
        let h = buf.area().height;
        let mut rows = Vec::new();
        for y in 0..h {
            let mut row = String::new();
            for x in 0..w {
                row.push_str(buf.get(x, y).map(|c| c.symbol()).unwrap_or(" "));
            }
            rows.push(row.trim_end().to_string());
        }
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows.join("\n")
    }

    /// A wrapped multi-span body row must keep every character. The header is
    /// `label` + `"  "` + scope (three spans); when the concatenation is wider
    /// than the body, the continuation row re-slices spans with byte
    /// boundaries computed against the *full* row text — this guards against
    /// that re-slicing silently dropping text (the `l | he` regression, where
    /// `ls | head` lost its `s`, `a`, `d`).
    #[test]
    fn permission_sheet_wrapped_header_keeps_every_character() {
        let request = PermissionRequest {
            id: "p".into(),
            tool: "run_command".into(),
            label: "bash".into(),
            description: "Run a command".into(),
            arguments: r#"{"command":"ls | head"}"#.into(),
            scope: "ls | head".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        let mut terminal = nuotc::TestTerminal::new(30, 24);
        let mut hit_map = ComponentTree::new();
        terminal.draw(|frame| {
            hit_map.begin(frame.area());
            let rect = Rect::new(0, 16, 30, 8);
            let _ = draw_permission_sheet(
                frame,
                &mut hit_map,
                &request,
                0,
                false,
                false,
                0,
                0,
                rect,
                &Theme::default(),
                &crate::model::selection::SelectionState::None,
                &mut crate::model::layout::LayoutMap::new(),
            );
        });

        // Row 0 is the header (`bash  ls | head` — 16 cols, fits at width 30
        // in one visual row); so instead force a wrap by narrowing the sheet.
        let text = sheet_text(&terminal);
        assert!(
            text.contains("ls | head"),
            "wrapped header lost characters: {text:?}"
        );
    }

    /// The same multi-span row, forced to wrap by a body narrower than the
    /// header: every character of `ls | head` must survive the wrap.
    #[test]
    fn permission_sheet_wrapped_header_survives_wrap() {
        let request = PermissionRequest {
            id: "p".into(),
            tool: "run_command".into(),
            label: "bash".into(),
            description: "Run a command".into(),
            arguments: r#"{"command":"ls | head"}"#.into(),
            scope: "ls | head".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        };
        let mut terminal = nuotc::TestTerminal::new(14, 24);
        let mut hit_map = ComponentTree::new();
        terminal.draw(|frame| {
            hit_map.begin(frame.area());
            let rect = Rect::new(0, 16, 14, 8);
            let _ = draw_permission_sheet(
                frame,
                &mut hit_map,
                &request,
                0,
                false,
                false,
                0,
                0,
                rect,
                &Theme::default(),
                &crate::model::selection::SelectionState::None,
                &mut crate::model::layout::LayoutMap::new(),
            );
        });

        // Body width = 14 - 1 - 2*1 = 11; header `bash  ls | head` is 15
        // cols, so it wraps. No non-whitespace character may be dropped by
        // the wrap — the concatenated header rows must reassemble to the
        // full header (whitespace at a wrap point may trail and go unrendered).
        let text = sheet_text(&terminal);
        let header: String = text
            .lines()
            .map(|l| l.trim_start_matches('┃').trim())
            .filter(|l| l.starts_with("bash") || l.starts_with('|') || l.starts_with("head"))
            .flat_map(|l| l.chars().filter(|c| !c.is_whitespace()))
            .collect();
        assert_eq!(
            header, "bashls|head",
            "wrapped header lost characters: {text:?}"
        );
    }
}
