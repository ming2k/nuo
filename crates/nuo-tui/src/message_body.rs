//! Markdown block-level rendering for a single message: text, code, tables,
//! headings, quotes, lists, rules, breaks. Emits one rendered line per row
//! and records semantic [`BlockRegion`]s / table cell hit boxes for selection
//! and click hit-testing.

use nuotc::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::components::meta_strip::{MetaStrip, MetaTone};
use crate::model::document::{Block, DeliveryStatus, Inline, LinkRange, TranscriptMessage};
use crate::model::layout::{BlockRegion, LayoutMap, LinkHit, TableCellHit, TableCellSegment};
use crate::model::selection::{
    CellDragInfo, SelectionState, floor_grapheme_boundary, inclusive_grapheme_end,
};
use crate::render::BlockWrapCache;

use super::design::{
    BLOCK_SURFACE_H_INSET, CODE_BAND_GUTTER_GAP, CODE_BAND_GUTTER_MIN_WIDTH, CODE_BAND_LEFT_INDENT,
    MATH_MARKER_GAP_COLS, QUOTE_PREFIX, QUOTE_PREFIX_COLS, USER_MESSAGE_GUTTER_GLYPH,
    USER_MESSAGE_HEADER_BODY_GAP_ROWS, USER_MESSAGE_OUTER_GUTTER_COLS, USER_MESSAGE_RIGHT_PAD_COLS,
    USER_MESSAGE_TEXT_GAP_COLS, USER_MESSAGE_TRANSITION_ROWS,
};
use super::markdown_table::{TableRowInfo, build_table_render, push_table_segment};
use super::text_layout::{
    CodeGutterParams, RichLineParams, RichTextColors, RichTextRanges, WrappedLine,
    block_selection_range, bold_delim_local_ranges, code_gutter_line, line_selection,
    line_spans_rich, link_delim_local_ranges, markup_hidden_ranges, padded_tail, visible_width,
};
use super::time::sent_time_label;
use super::{TRANSCRIPT_BODY_LEADING_INDENT, Theme};

fn display_width_u16(s: &str) -> u16 {
    s.width() as u16
}

/// Round / time label drawn *outside* a sent user-message panel (on the row
/// above it, on plain `surface`), so the panel itself holds only the typed
/// text. The "Sent" word is dropped: `round N` plus a right-aligned `HH:MM`
/// is enough provenance,
/// and queued messages keep their `⏸ Queued` pending marker.
///
/// The whole header row is composed from the shared `MetaStrip` component
/// (`render/components/meta_strip.rs`) — the same two-tone metadata
/// treatment the assistant turn header uses. The strip leads with a `<` gutter rail (accent
/// tone) representing Unix stdin redirection, matching the Unix pipeline visual language.
///
fn sent_header_anchor(msg: &TranscriptMessage) -> String {
    if let Some(ref origin) = msg.injection_origin {
        match origin.kind {
            nuo_wire::InjectionKind::Hook(event) => {
                return format!("hook:{}", format!("{event:?}").to_lowercase());
            }
            nuo_wire::InjectionKind::InterAgent => return "inter-agent".to_string(),
            nuo_wire::InjectionKind::SubagentSteer => return "subagent steer".to_string(),
            nuo_wire::InjectionKind::SubagentTask => return "subagent task".to_string(),
            nuo_wire::InjectionKind::UserSteer => return "steer".to_string(),
            nuo_wire::InjectionKind::LoopReviewNudge => return "guard:loop".to_string(),
            nuo_wire::InjectionKind::SystemReminder => return "system:reminder".to_string(),
            nuo_wire::InjectionKind::CompactionCheckpoint => return "checkpoint".to_string(),
            nuo_wire::InjectionKind::ImplicitSkill => return "skill:inject".to_string(),
            nuo_wire::InjectionKind::ImplicitFile => return "file:inject".to_string(),
            _ => {}
        }
    }
    match msg.origin {
        crate::model::document::UserMessageOrigin::Steer => "steer".to_string(),
        crate::model::document::UserMessageOrigin::FollowUp => "follow-up".to_string(),
        crate::model::document::UserMessageOrigin::Slash => "cmd".to_string(),
        _ => {
            if let Some(round) = msg.round {
                format!("round {}", round)
            } else {
                "prompt".to_string()
            }
        }
    }
}

fn sent_header_context(msg: &TranscriptMessage) -> String {
    if let Some(ref origin) = msg.injection_origin
        && let Some(reason) = &origin.reason
        && !reason.is_empty()
    {
        return reason.clone();
    }
    match msg.origin {
        crate::model::document::UserMessageOrigin::Steer => match (msg.round, msg.turn) {
            (Some(r), Some(t)) => format!("round {r} › turn {t}"),
            (Some(r), None) => format!("round {r}"),
            (None, Some(t)) => format!("turn {t}"),
            (None, None) => String::new(),
        },
        crate::model::document::UserMessageOrigin::FollowUp => {
            msg.round.map(|r| format!("round {r}")).unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn table_line_hidden_ranges(line_text: &str, info: &TableRowInfo) -> Vec<(usize, usize)> {
    let mut hidden = Vec::new();
    for ci in 0..info.col_content_spans.len() {
        let (clo, chi) = info.col_content_spans[ci];
        if chi <= clo {
            continue;
        }
        let offset = info.col_offsets.get(ci).copied().unwrap_or(0);
        let code_ranges = info
            .col_code_ranges
            .get(ci)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let bold_ranges = info
            .col_bold_ranges
            .get(ci)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let math_ranges = info
            .col_math_ranges
            .get(ci)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        hidden.extend(
            markup_hidden_ranges(
                &line_text[clo..chi],
                offset,
                code_ranges,
                bold_ranges,
                math_ranges,
            )
            .into_iter()
            .map(|(lo, hi)| (clo + lo, clo + hi)),
        );
    }
    hidden
}

fn visible_width_window(
    text: &str,
    start: usize,
    end: usize,
    hidden_ranges: &[(usize, usize)],
) -> usize {
    let local_hidden: Vec<(usize, usize)> = hidden_ranges
        .iter()
        .filter_map(|&(lo, hi)| {
            let clipped_lo = lo.max(start);
            let clipped_hi = hi.min(end);
            (clipped_lo < clipped_hi).then(|| (clipped_lo - start, clipped_hi - start))
        })
        .collect();
    visible_width(&text[start..end], &local_hidden)
}

#[allow(clippy::too_many_arguments)]
fn push_link_hits_for_line(
    layout_map: &mut LayoutMap,
    links: &[LinkRange],
    message_idx: usize,
    block_idx: usize,
    line_start_byte: usize,
    line_text: &str,
    prefix_cols: u16,
    line_rect: Rect,
    hidden_ranges: &[(usize, usize)],
) {
    let line_end_byte = line_start_byte + line_text.len();
    for link in links {
        let label_start = link.label_range.0.max(line_start_byte);
        let label_end = link.label_range.1.min(line_end_byte);
        if label_start >= label_end {
            continue;
        }
        let local_start = label_start - line_start_byte;
        let local_end = label_end - line_start_byte;
        let x_offset = visible_width_window(line_text, 0, local_start, hidden_ranges);
        let width = visible_width_window(line_text, local_start, local_end, hidden_ranges).max(1);
        layout_map.push_link_hit(LinkHit {
            message_idx,
            block_idx,
            range: link.range,
            url: link.url.clone(),
            rect: Rect::new(
                line_rect.x + prefix_cols + x_offset as u16,
                line_rect.y,
                width as u16,
                1,
            ),
        });
    }
}

fn cell_drag_selected_span(
    selection: &SelectionState,
    cell: &CellDragInfo,
    line_start_byte: usize,
    line_text: &str,
) -> Option<(usize, usize)> {
    let (start, end) = selection.active_normalized_range()?;
    let sel_start = start.byte_offset;
    let sel_end = end.byte_offset;
    let line_end_byte = line_start_byte + line_text.len();
    let mut out: Option<(usize, usize)> = None;

    for segment in &cell.segments {
        let segment_start = segment.content_range.0.max(line_start_byte);
        let segment_end = segment.content_range.1.min(line_end_byte);
        if segment_start >= segment_end || sel_end < segment_start || sel_start > segment_end {
            continue;
        }

        let raw_lo_abs = sel_start.max(segment_start);
        let raw_hi_abs = if sel_end < segment.content_range.1 {
            sel_end.min(segment_end)
        } else {
            segment_end
        };
        if raw_lo_abs > raw_hi_abs {
            continue;
        }

        let lo = floor_grapheme_boundary(line_text, raw_lo_abs - line_start_byte);
        let hi = if sel_end < segment.content_range.1 {
            inclusive_grapheme_end(line_text, raw_hi_abs - line_start_byte)
        } else {
            raw_hi_abs - line_start_byte
        };
        if lo < hi {
            out = Some(match out {
                Some((old_lo, old_hi)) => (old_lo.min(lo), old_hi.max(hi)),
                None => (lo, hi),
            });
        }
    }

    out
}

/// Render the blocks of a single message inside the given area.
///
/// This is extracted so that normal messages and tool steps can share
/// the same block-rendering logic while using different containing rects.
#[allow(clippy::too_many_arguments)]
pub fn draw_message_body(
    frame: &mut Frame,
    area: Rect,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    theme: &Theme,
    layout_map: &mut LayoutMap,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    record_layout: bool,
    wrap: &mut BlockWrapCache,
) {
    for (bi, block) in msg.blocks.iter().enumerate() {
        let sel_range = block_selection_range(selection, mi, bi);

        // Gap before a list is handled structurally: the parser's `push_block`
        // already inserts a `Block::Break` at every list↔non-list boundary (and
        // only there), so the list reads as a discrete group with exactly one
        // blank line of separation. Adjacent list items never get a break, so
        // list entries stay tight — as in rendered markdown. Adding another
        // blank row here used to double that gap (two lines instead of one).

        match block {
            Block::Text(inline) => {
                let Inline {
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                } = inline;
                let is_user = msg.role == nuo_wire::Role::User;
                // Both pending deliveries render as a waiting panel: a
                // busy-Enter steer blocked on the running turn
                // (`Queued`), and one whose round ended first and now
                // waits to ship as the next round's prompt
                // (`HeldNextRound`).
                let is_queued = is_user
                    && (msg.delivery == DeliveryStatus::Queued
                        || msg.delivery == DeliveryStatus::HeldNextRound);
                let is_sending = is_user && msg.delivery == DeliveryStatus::Sending;
                let is_cancelled = is_user && msg.delivery == DeliveryStatus::Cancelled;
                // A `Role::Tool` body reaching here is a command result
                // (ADR-0111): a harness artifact, not model prose, so it
                // reads one step quieter than assistant text. Tool *steps*
                // never take this path (they render via `draw_tool_step`).
                let base = match msg.role {
                    nuo_wire::Role::User => Style::default().fg(theme.user_text()),
                    nuo_wire::Role::System => Style::default().fg(theme.system_text()),
                    nuo_wire::Role::Tool => Style::default().fg(theme.muted()),
                    _ => Style::default().fg(theme.fg()),
                };
                let full_width = area.width as usize;
                // The horizontal gutter is applied once at the stream entry
                // point, so only the leading indent remains to subtract here.
                let body_wrap_width =
                    area.width.saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT) as usize;
                // User messages render inside their own panel, so they wrap at
                // the panel's inner width minus symmetric left/right padding
                // rather than the shared prose width — this keeps the text from
                // running into either edge of the `user_panel_bg` band.
                let user_panel_w = full_width.saturating_sub(2 * USER_MESSAGE_OUTER_GUTTER_COLS);
                let user_text_width = user_panel_w
                    .saturating_sub(USER_MESSAGE_TEXT_GAP_COLS + USER_MESSAGE_RIGHT_PAD_COLS)
                    .max(1);
                let lines = wrap.wrap_text_markup(
                    content,
                    if is_user {
                        user_text_width
                    } else {
                        body_wrap_width
                    },
                    // User messages carry no inline ranges (plain parse), so
                    // this is empty and the wrap degenerates to `wrap_text`.
                    &crate::text_layout::block_hidden_ranges(
                        content,
                        code_ranges,
                        bold_ranges,
                        math_ranges,
                        link_ranges,
                    ),
                );
                *content_lines += lines.len();

                // User messages get top/bottom padding rows (matching the input
                // box's breathing room).  The padding is a full row of solid
                // `user_panel_bg` so the message reads as a solid panel.
                // Queued messages swap in the dimmer `user_surface_queued` so a
                // pending send reads as more "pending" than delivered.
                let user_bg = if is_cancelled || is_queued {
                    theme.user_surface_queued()
                } else {
                    theme.user_surface()
                };
                let user_gutter = " ".repeat(USER_MESSAGE_OUTER_GUTTER_COLS);
                let user_content_w = full_width.saturating_sub(2 * USER_MESSAGE_OUTER_GUTTER_COLS);

                if is_user {
                    // Header: the send-metadata label (round no. + time, or a
                    // pending marker for queued messages) sits OUTSIDE the
                    // user-message panel, on plain `surface`. A blank gap row
                    // (`USER_MESSAGE_HEADER_BODY_GAP_ROWS`) is placed between
                    // the header and the user-message panel.
                    if bi == 0 {
                        *content_lines += 1;
                        if *skip_rows > 0 {
                            *skip_rows = skip_rows.saturating_sub(1);
                        } else if *current_y < area.y + area.height {
                            // Two-tone label, no background band (matches the
                            // turn header row in `turn_band`): the round
                            // anchor is info-tone bold, the time reads as
                            // muted metadata. The header leads with a `<` gutter
                            // rail (Unix stdin redirection). The rail consumes
                            // the same width as `USER_MESSAGE_TEXT_GAP_COLS`, so
                            // `round N` / `⏸ Queued` stay aligned with the message body.
                            let round_gutter = if USER_MESSAGE_TEXT_GAP_COLS == 0 {
                                String::new()
                            } else {
                                format!(
                                    "{}{}",
                                    USER_MESSAGE_GUTTER_GLYPH,
                                    " ".repeat(USER_MESSAGE_TEXT_GAP_COLS.saturating_sub(
                                        display_width_u16(USER_MESSAGE_GUTTER_GLYPH) as usize,
                                    ))
                                )
                            };
                            let gutter_tone = MetaTone::Accent;
                            let mut strip = MetaStrip::new()
                                .left_pad(USER_MESSAGE_OUTER_GUTTER_COLS)
                                .lead(round_gutter, gutter_tone)
                                .anchor(sent_header_anchor(msg))
                                .fill_tail(theme.surface());
                            if is_cancelled {
                                strip = strip.status_toned("cancelled", MetaTone::Warn);
                            } else if is_sending {
                                strip = strip.status("sending");
                            } else if is_queued {
                                let label = match msg.delivery {
                                    DeliveryStatus::HeldNextRound => "held for next round",
                                    _ => "queued",
                                };
                                strip = strip.status(label);
                            } else {
                                let context = sent_header_context(msg);
                                if !context.is_empty() {
                                    strip = strip.detail(context);
                                }
                            }
                            if let Some(sent_at_ms) = msg.sent_at_ms {
                                strip = strip.trailing_detail(sent_time_label(sent_at_ms));
                            }
                            let rect = Rect::new(area.x, *current_y, area.width, 1);
                            strip.render(frame, rect, theme);
                            *current_y += 1;
                        }

                        for _ in 0..USER_MESSAGE_HEADER_BODY_GAP_ROWS {
                            *content_lines += 1;
                            if *skip_rows > 0 {
                                *skip_rows = skip_rows.saturating_sub(1);
                            } else if *current_y < area.y + area.height {
                                let rect = Rect::new(area.x, *current_y, area.width, 1);
                                frame.render_widget(
                                    Paragraph::new("").style(Style::default().bg(theme.surface())),
                                    rect,
                                );
                                *current_y += 1;
                            }
                        }
                    }
                    for _ in 0..USER_MESSAGE_TRANSITION_ROWS {
                        *content_lines += 1;
                        if *skip_rows > 0 {
                            *skip_rows = skip_rows.saturating_sub(1);
                        } else if *current_y < area.y + area.height {
                            // Top edge: a full user_panel_bg padding row (not
                            // half-block `▄`), so the panel opens with a full
                            // row of breathing room and the edge is identical
                            // across terminals.
                            let pad = Line::from(vec![
                                Span::styled(
                                    user_gutter.clone(),
                                    Style::default().bg(theme.surface()),
                                ),
                                Span::styled(
                                    " ".repeat(user_content_w),
                                    Style::default().bg(user_bg),
                                ),
                                Span::styled(
                                    user_gutter.clone(),
                                    Style::default().bg(theme.surface()),
                                ),
                            ]);
                            let rect = Rect::new(area.x, *current_y, area.width, 1);
                            frame.render_widget(Paragraph::new(pad), rect);
                            *current_y += 1;
                        }
                    }
                }

                for wl in lines.iter() {
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                        continue;
                    }
                    if *current_y >= area.y + area.height {
                        break;
                    }

                    let line = if is_user {
                        // Sent user messages render on a dimmer `user_panel_bg`
                        // band. Selection is character-level, not line-level,
                        // so the user can highlight arbitrary substrings.
                        let bg = user_bg;
                        let text_style = Style::default().bg(bg).fg(if is_cancelled || is_queued {
                            theme.muted()
                        } else {
                            theme.user_text()
                        });
                        let sel_style = Style::default().bg(theme.selected()).fg(theme.fg());
                        let sel = line_selection(sel_range, wl);

                        let mut spans = vec![
                            Span::styled(user_gutter.clone(), Style::default().bg(theme.surface())),
                            Span::styled(
                                " ".repeat(USER_MESSAGE_TEXT_GAP_COLS),
                                Style::default().bg(bg),
                            ),
                        ];

                        match sel {
                            None => {
                                spans.push(Span::styled(wl.text.clone(), text_style));
                            }
                            Some((lo, hi)) => {
                                if lo > 0 {
                                    spans.push(Span::styled(wl.text[..lo].to_string(), text_style));
                                }
                                spans.push(Span::styled(wl.text[lo..hi].to_string(), sel_style));
                                if hi < wl.text.len() {
                                    spans.push(Span::styled(wl.text[hi..].to_string(), text_style));
                                }
                            }
                        }

                        let used = USER_MESSAGE_TEXT_GAP_COLS + wl.text.width();
                        spans.push(Span::styled(
                            padded_tail(user_content_w, used),
                            Style::default().bg(bg),
                        ));
                        spans.push(Span::styled(
                            user_gutter.clone(),
                            Style::default().bg(theme.surface()),
                        ));
                        Line::from(spans)
                    } else {
                        let prefix = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
                        line_spans_rich(RichLineParams {
                            prefix: &prefix,
                            prefix_style: Style::default(),
                            text: &wl.text,
                            line_start_byte: wl.start_byte,
                            selected: line_selection(sel_range, wl),
                            ranges: RichTextRanges {
                                code: code_ranges,
                                bold: bold_ranges,
                                math: math_ranges,
                                links: link_ranges,
                            },
                            base,
                            colors: RichTextColors::from_theme(theme),
                        })
                    };
                    let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                    frame.render_widget(Paragraph::new(line), line_rect);

                    if record_layout {
                        // User panels prefix text with the outer gutter plus a
                        // one-column gap; other roles use the body prefix.
                        let prefix_cols = if is_user {
                            (USER_MESSAGE_OUTER_GUTTER_COLS + USER_MESSAGE_TEXT_GAP_COLS) as u16
                        } else {
                            TRANSCRIPT_BODY_LEADING_INDENT
                        };
                        let hidden_ranges = if is_user {
                            Vec::new()
                        } else {
                            let mut hidden =
                                bold_delim_local_ranges(&wl.text, wl.start_byte, bold_ranges);
                            hidden.extend(markup_hidden_ranges(
                                &wl.text,
                                wl.start_byte,
                                code_ranges,
                                &[],
                                math_ranges,
                            ));
                            hidden.extend(link_delim_local_ranges(
                                &wl.text,
                                wl.start_byte,
                                link_ranges,
                            ));
                            hidden
                        };
                        push_link_hits_for_line(
                            layout_map,
                            link_ranges,
                            mi,
                            bi,
                            wl.start_byte,
                            &wl.text,
                            prefix_cols,
                            line_rect,
                            &hidden_ranges,
                        );
                        layout_map.push(BlockRegion {
                            message_idx: mi,
                            block_idx: bi,
                            start_byte: wl.start_byte,
                            end_byte: wl.end_byte,
                            text: wl.text.clone(),
                            prefix_cols,
                            rect: line_rect,
                            hidden_ranges,
                        });
                    }

                    *current_y += 1;
                }

                if is_user {
                    for _ in 0..USER_MESSAGE_TRANSITION_ROWS {
                        *content_lines += 1;
                        if *skip_rows > 0 {
                            *skip_rows = skip_rows.saturating_sub(1);
                        } else if *current_y < area.y + area.height {
                            // Bottom edge: a full user_panel_bg padding row
                            // (not half-block `▀`), closing the panel with a
                            // full row of breathing room.
                            let pad = Line::from(vec![
                                Span::styled(
                                    user_gutter.clone(),
                                    Style::default().bg(theme.surface()),
                                ),
                                Span::styled(
                                    " ".repeat(user_content_w),
                                    Style::default().bg(user_bg),
                                ),
                                Span::styled(
                                    user_gutter.clone(),
                                    Style::default().bg(theme.surface()),
                                ),
                            ]);
                            let rect = Rect::new(area.x, *current_y, area.width, 1);
                            frame.render_widget(Paragraph::new(pad), rect);
                            *current_y += 1;
                        }
                    }
                }
            }
            Block::Table {
                headers,
                rows,
                aligns,
                ..
            } => {
                // Adaptive table rendering: compute column widths that fit the
                // available terminal width, wrap cell contents within their
                // columns, and draw the grid line-by-line. This keeps borders
                // intact even for wide/CJK tables instead of letting the
                // generic line wrapper mangle `│` separators.
                let indent = TRANSCRIPT_BODY_LEADING_INDENT as usize;
                let full_width = area.width as usize;
                // The area is already inset; `indent` is the table's left visual
                // indent, matching body prose so a table's left edge lines up
                // with the text above and below it rather than drifting right.
                let available = full_width.saturating_sub(indent);
                let table = build_table_render(headers, rows, aligns, available);
                let ncols = headers.len().max(1);

                let base = Style::default().fg(theme.fg());
                let border_style = Style::default().fg(theme.muted());
                let sel_bg = theme.selected();

                // A whole-table selection (middle-click) still copies the grid
                // with borders stripped, so keep recording the displayed grid.
                if record_layout {
                    layout_map.record_table_grid(mi, bi, table.lines.join("\n"));
                }

                // If a single cell is selected in this block, resolve its
                // (row, col) so we can highlight just that cell's column across
                // every grid line it spans (including wrapped continuation
                // lines), without bleeding into adjacent cells.
                let selected_cell = match selection {
                    SelectionState::TableCell {
                        message_idx,
                        block_idx,
                        cell_idx,
                    } if *message_idx == mi && *block_idx == bi => {
                        Some((cell_idx / ncols, cell_idx % ncols))
                    }
                    _ => None,
                };
                let cell_drag_for_block = cell_selection.filter(|cell| {
                    cell.message_idx == mi
                        && cell.block_idx == bi
                        && selection.active_normalized_range().is_some()
                });

                *content_lines += table.lines.len();
                let mut line_start_byte = 0usize;
                for (line_idx, line_text) in table.lines.iter().enumerate() {
                    let row_info = table.line_info.get(line_idx).and_then(|o| o.as_ref());
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                        line_start_byte += line_text.len() + 1; // +1 for '\n'
                        continue;
                    }
                    if *current_y >= area.y + area.height {
                        break;
                    }
                    let is_border = row_info.is_none();

                    let start_byte = line_start_byte;
                    let end_byte = line_start_byte + line_text.len();
                    let wl = WrappedLine {
                        text: line_text.clone(),
                        start_byte,
                        end_byte,
                    };

                    // The byte range to highlight on this line: either the
                    // selected cell's column (cell selection), a whole-line /
                    // partial range (block/range selection), or nothing.
                    let selected_span = if let Some(cell) = cell_drag_for_block {
                        cell_drag_selected_span(selection, cell, start_byte, line_text)
                    } else if let Some((sr, sc)) = selected_cell {
                        row_info
                            .filter(|info| info.row == sr)
                            .and_then(|info| info.col_spans.get(sc).copied())
                    } else {
                        line_selection(sel_range, &wl)
                    };
                    let fully_selected =
                        matches!(selected_span, Some((s, e)) if s == 0 && e == line_text.len());
                    let pad_style = if fully_selected {
                        Style::default().bg(sel_bg)
                    } else {
                        Style::default()
                    };

                    let hidden_for_line = row_info
                        .map(|info| table_line_hidden_ranges(line_text, info))
                        .unwrap_or_default();
                    let used = indent + visible_width(line_text, &hidden_for_line);
                    let mut spans = vec![Span::styled(" ".repeat(indent), pad_style)];
                    // On data lines the `│` rules and inter-cell padding are
                    // border decoration; only the padded cell text (col_spans)
                    // is "content". Paint borders with the same muted style as
                    // the horizontal separators so the grid reads as one
                    // uniform weight — otherwise the vertical rules (drawn on
                    // every data row with the brighter text colour) look
                    // heavier than the sparse horizontal rules.
                    if let Some(info) = row_info {
                        let mut pos = 0usize;
                        for i in 0..ncols.min(info.col_spans.len()) {
                            let (lo, hi) = info.col_spans[i];
                            let (clo, chi) =
                                info.col_content_spans.get(i).copied().unwrap_or((lo, hi));
                            let offset = info.col_offsets.get(i).copied().unwrap_or(0);
                            let code_ranges = info
                                .col_code_ranges
                                .get(i)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);
                            let bold_ranges = info
                                .col_bold_ranges
                                .get(i)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);
                            let math_ranges = info
                                .col_math_ranges
                                .get(i)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);

                            // Border / inter-cell separator before this cell
                            if lo > pos {
                                push_table_segment(
                                    &mut spans,
                                    line_text,
                                    pos,
                                    lo,
                                    border_style,
                                    selected_span,
                                    sel_bg,
                                );
                            }

                            // Leading alignment padding
                            if clo > lo {
                                push_table_segment(
                                    &mut spans,
                                    line_text,
                                    lo,
                                    clo,
                                    base,
                                    selected_span,
                                    sel_bg,
                                );
                            }

                            // Cell content with inline code / bold styles
                            if chi > clo {
                                let cell_sel = selected_span.and_then(|(slo, shi)| {
                                    if slo < chi && clo < shi {
                                        let cs = slo.max(clo).saturating_sub(clo);
                                        let ce = shi.min(chi).saturating_sub(clo);
                                        if cs < ce { Some((cs, ce)) } else { None }
                                    } else {
                                        None
                                    }
                                });

                                let content_line = line_spans_rich(RichLineParams {
                                    prefix: "",
                                    prefix_style: Style::default(),
                                    text: &line_text[clo..chi],
                                    line_start_byte: offset,
                                    selected: cell_sel,
                                    ranges: RichTextRanges {
                                        code: code_ranges,
                                        bold: bold_ranges,
                                        math: math_ranges,
                                        links: &[],
                                    },
                                    base,
                                    colors: RichTextColors {
                                        selected_bg: sel_bg,
                                        ..RichTextColors::from_theme(theme)
                                    },
                                });
                                // Skip the empty-prefix span (position 0).
                                for span in content_line.spans.into_iter().skip(1) {
                                    spans.push(span);
                                }
                            }

                            // Trailing alignment padding
                            if hi > chi {
                                push_table_segment(
                                    &mut spans,
                                    line_text,
                                    chi,
                                    hi,
                                    base,
                                    selected_span,
                                    sel_bg,
                                );
                            }

                            pos = hi;
                        }
                        if pos < line_text.len() {
                            push_table_segment(
                                &mut spans,
                                line_text,
                                pos,
                                line_text.len(),
                                border_style,
                                selected_span,
                                sel_bg,
                            );
                        }
                    } else {
                        push_table_segment(
                            &mut spans,
                            line_text,
                            0,
                            line_text.len(),
                            border_style,
                            selected_span,
                            sel_bg,
                        );
                    }
                    spans.push(Span::styled(padded_tail(full_width, used), pad_style));
                    let line = Line::from(spans);
                    let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                    frame.render_widget(Paragraph::new(line), line_rect);

                    if record_layout {
                        // Register a hit box per cell so clicks resolve to a
                        // single cell (and thus its full, possibly wrapped
                        // text) instead of the whole grid line.
                        if let Some(info) = row_info {
                            for (ci, &(lo, hi)) in info.col_spans.iter().enumerate() {
                                if hi <= lo {
                                    continue;
                                }
                                let (clo, chi) =
                                    info.col_content_spans.get(ci).copied().unwrap_or((lo, hi));
                                let source_start = info.col_offsets.get(ci).copied().unwrap_or(0);
                                let source_end = source_start + chi.saturating_sub(clo);
                                let col_start =
                                    visible_width_window(line_text, 0, lo, &hidden_for_line);
                                let col_w =
                                    visible_width_window(line_text, lo, hi, &hidden_for_line);
                                let rect = Rect::new(
                                    area.x + indent as u16 + col_start as u16,
                                    *current_y,
                                    col_w as u16,
                                    1,
                                );
                                let cell_text = if info.row == 0 {
                                    headers.get(ci).cloned().unwrap_or_default()
                                } else {
                                    rows.get(info.row.saturating_sub(1))
                                        .and_then(|r| r.get(ci))
                                        .cloned()
                                        .unwrap_or_default()
                                };
                                layout_map.push_table_cell_hit(TableCellHit {
                                    message_idx: mi,
                                    block_idx: bi,
                                    cell_idx: info.row * ncols + ci,
                                    rect,
                                    cell_text,
                                    segment: TableCellSegment {
                                        rendered_range: (start_byte + lo, start_byte + hi),
                                        content_range: (start_byte + clo, start_byte + chi),
                                        source_range: (source_start, source_end),
                                    },
                                });
                            }
                        }
                        // Data lines also carry a region so non-table hit
                        // tests (e.g. step headers) keep working; border rules
                        // remain dead zones.
                        if !is_border {
                            layout_map.push(BlockRegion {
                                message_idx: mi,
                                block_idx: bi,
                                start_byte,
                                end_byte,
                                text: line_text.clone(),
                                prefix_cols: indent as u16,
                                rect: line_rect,
                                hidden_ranges: hidden_for_line.clone(),
                            });
                        }
                    }

                    line_start_byte = end_byte + 1; // +1 for '\n'
                    *current_y += 1;
                }
            }
            Block::Math { content } => {
                let math_bg = theme.code_surface();
                let band_x = area.x + BLOCK_SURFACE_H_INSET;
                let band_w = area.width.saturating_sub(2 * BLOCK_SURFACE_H_INSET).max(1);
                let full_width = band_w as usize;
                let left_indent = CODE_BAND_LEFT_INDENT;
                let marker = "∑";
                let marker_gap = MATH_MARKER_GAP_COLS;
                let indent = left_indent + marker.width() + marker_gap;
                let wrap_width = full_width.saturating_sub(indent + 1).max(1);
                let lines = wrap.wrap_text(content, wrap_width);
                let lines = if lines.is_empty() {
                    std::sync::Arc::new(vec![WrappedLine {
                        text: String::new(),
                        start_byte: 0,
                        end_byte: 0,
                    }])
                } else {
                    lines
                };
                *content_lines += lines.len();
                for wl in lines.iter() {
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                        continue;
                    }
                    if *current_y >= area.y + area.height {
                        break;
                    }
                    let selected = line_selection(sel_range, wl);
                    let mut spans = vec![
                        Span::styled(" ".repeat(left_indent), Style::default().bg(math_bg)),
                        Span::styled(
                            marker.to_string(),
                            Style::default()
                                .fg(theme.info())
                                .bg(math_bg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ".repeat(marker_gap), Style::default().bg(math_bg)),
                    ];
                    match selected {
                        None => spans.push(Span::styled(
                            wl.text.clone(),
                            Style::default()
                                .fg(theme.info())
                                .bg(math_bg)
                                .add_modifier(Modifier::ITALIC),
                        )),
                        Some((lo, hi)) => {
                            let base = Style::default()
                                .fg(theme.info())
                                .bg(math_bg)
                                .add_modifier(Modifier::ITALIC);
                            if lo > 0 {
                                spans.push(Span::styled(wl.text[..lo].to_string(), base));
                            }
                            spans.push(Span::styled(
                                wl.text[lo..hi].to_string(),
                                base.bg(theme.selected()),
                            ));
                            if hi < wl.text.len() {
                                spans.push(Span::styled(wl.text[hi..].to_string(), base));
                            }
                        }
                    }
                    let used = indent + wl.text.width();
                    spans.push(Span::styled(
                        padded_tail(full_width, used),
                        Style::default().bg(math_bg),
                    ));
                    let line_rect = Rect::new(band_x, *current_y, band_w, 1);
                    frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);
                    if record_layout {
                        layout_map.push(BlockRegion {
                            message_idx: mi,
                            block_idx: bi,
                            start_byte: wl.start_byte,
                            end_byte: wl.end_byte,
                            text: wl.text.clone(),
                            prefix_cols: indent as u16,
                            rect: line_rect,
                            hidden_ranges: Vec::new(),
                        });
                    }
                    *current_y += 1;
                }
            }
            Block::Code { language, content } => {
                // Borderless code block: a uniform `code_bg` band with a
                // line-number gutter, matching opencode's clean look. No
                // `╭─ ╰─` frame, no per-line `│` rule.
                let code_bg = theme.code_surface();
                // The solid-background band is inset inside the transcript band so
                // it reads as a distinct panel rather than bleeding into the
                // surrounding app background. Content (gutter + code) lives inside
                // the band; the surrounding cells keep `app_bg`.
                let band_x = area.x + BLOCK_SURFACE_H_INSET;
                let band_w = area.width.saturating_sub(2 * BLOCK_SURFACE_H_INSET).max(1);
                let full_width = band_w as usize;

                // Split into logical lines, tracking each one's byte offset
                // within `content` so semantic selection maps back to the raw
                // source even after per-line wrapping. The gutter width (and
                // therefore the wrap width) depends on the line count, so
                // count lines first, then resolve geometry from the
                // content-addressed cache (ADR-0184): a frozen code block is
                // split and wrapped exactly once per width.
                let line_count = content.bytes().filter(|&b| b == b'\n').count() + 1;
                let gutter_width = line_count.to_string().len().max(CODE_BAND_GUTTER_MIN_WIDTH);
                // The code band is a uniform background with a line-number
                // gutter — no left accent bar. Geometry shares the block-level
                // design contract with tool-step code bands so a code block
                // looks identical in prose and inside a tool step.
                let left_indent = CODE_BAND_LEFT_INDENT;
                let gutter_gap = CODE_BAND_GUTTER_GAP;
                let indent = left_indent + 1 /* space */ + gutter_width + gutter_gap;
                let wrap_width = full_width.saturating_sub(indent + 1);
                let prepared = wrap.prepare_code(content, wrap_width);

                // Subtle language tag on its own dim line above the gutter.
                if let Some(lang) = language.as_deref().filter(|l| !l.is_empty()) {
                    *content_lines += 1;
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                    } else if *current_y < area.y + area.height {
                        let used = left_indent + 1 + lang.len();
                        let line = Line::from(vec![
                            Span::styled(" ".repeat(left_indent), Style::default().bg(code_bg)),
                            Span::styled(" ", Style::default().bg(code_bg)),
                            Span::styled(
                                lang.to_string(),
                                Style::default().bg(code_bg).fg(theme.dim()),
                            ),
                            Span::styled(
                                padded_tail(full_width, used),
                                Style::default().bg(code_bg),
                            ),
                        ]);
                        let line_rect = Rect::new(band_x, *current_y, band_w, 1);
                        frame.render_widget(Paragraph::new(line), line_rect);
                        *current_y += 1;
                    }
                }

                for (line_idx, (line_start_byte, wrapped)) in prepared.logical.iter().enumerate() {
                    *content_lines += wrapped.len();
                    for (wrap_idx, wl) in wrapped.iter().enumerate() {
                        if *skip_rows > 0 {
                            *skip_rows = skip_rows.saturating_sub(1);
                            continue;
                        }
                        if *current_y >= area.y + area.height {
                            break;
                        }

                        let gutter = if wrap_idx == 0 {
                            format!("{:>width$}", line_idx + 1, width = gutter_width)
                        } else {
                            " ".repeat(gutter_width)
                        };

                        // Shift the wrapped line's byte offsets back into
                        // block-content coordinates for selection intersection.
                        let block_wl = WrappedLine {
                            text: wl.text.clone(),
                            start_byte: line_start_byte + wl.start_byte,
                            end_byte: line_start_byte + wl.end_byte,
                        };

                        let line = code_gutter_line(CodeGutterParams {
                            left_bar: Color::Reset,
                            left_indent,
                            gutter: &gutter,
                            gutter_gap,
                            code_bg,
                            gutter_fg: theme.dim(),
                            text: &wl.text,
                            selected: line_selection(sel_range, &block_wl),
                            code_fg: theme.code_text(),
                            selected_bg: theme.selected(),
                            full_width,
                        });
                        let line_rect = Rect::new(band_x, *current_y, band_w, 1);
                        frame.render_widget(Paragraph::new(line), line_rect);

                        if record_layout {
                            layout_map.push(BlockRegion {
                                message_idx: mi,
                                block_idx: bi,
                                start_byte: line_start_byte + wl.start_byte,
                                end_byte: line_start_byte + wl.end_byte,
                                text: wl.text.clone(),
                                prefix_cols: indent as u16,
                                rect: line_rect,
                                hidden_ranges: Vec::new(),
                            });
                        }

                        *current_y += 1;
                    }
                }
            }
            Block::Heading { level, inline } => {
                let Inline {
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                } = inline;
                let prefix = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
                let prefix_cols = TRANSCRIPT_BODY_LEADING_INDENT;
                let modifier = if *level == 1 {
                    Modifier::BOLD | Modifier::UNDERLINED
                } else {
                    Modifier::BOLD
                };
                let style = Style::default().fg(theme.heading()).add_modifier(modifier);
                // The heading *prefix* (leading `   ` indent and continuation
                // indentation) is decoration, not heading text, so it must not
                // carry the UNDERLINED modifier. Splitting the prefix off the
                // UNDERLINED run is what keeps the underline confined to the
                // actual heading text instead of bleeding left into the indent
                // whitespace (and, for wrapped headings, underlining the whole
                // continuation row's leading blanks).
                let prefix_style = Style::default()
                    .fg(theme.heading())
                    .add_modifier(Modifier::BOLD);
                let continuation = " ".repeat(prefix_cols as usize);
                let lines = wrap.wrap_text_markup(
                    content,
                    area.width.saturating_sub(prefix_cols) as usize,
                    &crate::text_layout::block_hidden_ranges(
                        content,
                        code_ranges,
                        bold_ranges,
                        math_ranges,
                        link_ranges,
                    ),
                );
                *content_lines += lines.len();
                for (line_index, wl) in lines.iter().enumerate() {
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                        continue;
                    }
                    if *current_y >= area.y + area.height {
                        break;
                    }
                    let line = line_spans_rich(RichLineParams {
                        prefix: if line_index == 0 {
                            &prefix
                        } else {
                            &continuation
                        },
                        prefix_style,
                        text: &wl.text,
                        line_start_byte: wl.start_byte,
                        selected: line_selection(sel_range, wl),
                        ranges: RichTextRanges {
                            code: code_ranges,
                            bold: bold_ranges,
                            math: math_ranges,
                            links: link_ranges,
                        },
                        base: style,
                        colors: RichTextColors::from_theme(theme),
                    });
                    // For H1 headings the terminal UNDERLINED modifier fills
                    // the entire Paragraph rect, so clamp the render width to
                    // the actual text extent to prevent the underline from
                    // bleeding into trailing whitespace.
                    let full_rect = Rect::new(area.x, *current_y, area.width, 1);
                    let mut hidden_for_line =
                        bold_delim_local_ranges(&wl.text, wl.start_byte, bold_ranges);
                    hidden_for_line.extend(markup_hidden_ranges(
                        &wl.text,
                        wl.start_byte,
                        code_ranges,
                        &[],
                        math_ranges,
                    ));
                    hidden_for_line.extend(link_delim_local_ranges(
                        &wl.text,
                        wl.start_byte,
                        link_ranges,
                    ));
                    let text_cols = prefix_cols + visible_width(&wl.text, &hidden_for_line) as u16;
                    let render_rect = if *level == 1 {
                        Rect::new(area.x, *current_y, text_cols.min(area.width), 1)
                    } else {
                        full_rect
                    };
                    frame.render_widget(Paragraph::new(line), render_rect);

                    if record_layout {
                        // Layout map always uses full width for hit-testing
                        // and selection across the entire line.
                        push_link_hits_for_line(
                            layout_map,
                            link_ranges,
                            mi,
                            bi,
                            wl.start_byte,
                            &wl.text,
                            prefix_cols,
                            full_rect,
                            &hidden_for_line,
                        );
                        layout_map.push(BlockRegion {
                            message_idx: mi,
                            block_idx: bi,
                            start_byte: wl.start_byte,
                            end_byte: wl.end_byte,
                            text: wl.text.clone(),
                            prefix_cols,
                            rect: full_rect,
                            hidden_ranges: hidden_for_line,
                        });
                    }

                    *current_y += 1;
                }
            }
            Block::Quote(inline) => {
                let Inline {
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                } = inline;
                // Fixed-width `▎` lead on every wrapped row (the area is already
                // inset, so there is no right gutter). Wrap against the *visible*
                // width: inline-code backticks, `**` bold markers, `$…$` math and
                // link syntax are painted zero-width, so counting them would make
                // the quote wrap early (and could split a `` `…` ``/`**…**` pair
                // across lines).
                let hidden = crate::text_layout::block_hidden_ranges(
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                );
                let lines = wrap.wrap_text_markup(
                    content,
                    area.width.saturating_sub(QUOTE_PREFIX_COLS as u16) as usize,
                    &hidden,
                );
                *content_lines += lines.len();
                for wl in lines.iter() {
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                        continue;
                    }
                    if *current_y >= area.y + area.height {
                        break;
                    }

                    let base = Style::default().fg(theme.quote());
                    let line = line_spans_rich(RichLineParams {
                        prefix: QUOTE_PREFIX,
                        prefix_style: Style::default().fg(theme.quote()),
                        text: &wl.text,
                        line_start_byte: wl.start_byte,
                        selected: line_selection(sel_range, wl),
                        ranges: RichTextRanges {
                            code: code_ranges,
                            bold: bold_ranges,
                            math: math_ranges,
                            links: link_ranges,
                        },
                        base,
                        colors: RichTextColors::from_theme(theme),
                    });
                    let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                    frame.render_widget(Paragraph::new(line), line_rect);

                    if record_layout {
                        let mut hidden_for_line =
                            bold_delim_local_ranges(&wl.text, wl.start_byte, bold_ranges);
                        hidden_for_line.extend(markup_hidden_ranges(
                            &wl.text,
                            wl.start_byte,
                            code_ranges,
                            &[],
                            math_ranges,
                        ));
                        hidden_for_line.extend(link_delim_local_ranges(
                            &wl.text,
                            wl.start_byte,
                            link_ranges,
                        ));
                        push_link_hits_for_line(
                            layout_map,
                            link_ranges,
                            mi,
                            bi,
                            wl.start_byte,
                            &wl.text,
                            QUOTE_PREFIX_COLS as u16,
                            line_rect,
                            &hidden_for_line,
                        );
                        layout_map.push(BlockRegion {
                            message_idx: mi,
                            block_idx: bi,
                            start_byte: wl.start_byte,
                            end_byte: wl.end_byte,
                            text: wl.text.clone(),
                            prefix_cols: QUOTE_PREFIX_COLS as u16,
                            rect: line_rect,
                            hidden_ranges: hidden_for_line,
                        });
                    }

                    *current_y += 1;
                }
            }
            Block::Rule => {
                *content_lines += 1;
                if *skip_rows > 0 {
                    *skip_rows = skip_rows.saturating_sub(1);
                } else if *current_y < area.y + area.height {
                    let indent = TRANSCRIPT_BODY_LEADING_INDENT as usize;
                    let width = (area.width as usize).saturating_sub(indent);
                    let text = format!("{}{}", " ".repeat(indent), "─".repeat(width));
                    let line =
                        Line::from(vec![Span::styled(text, Style::default().fg(theme.dim()))]);
                    let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                    frame.render_widget(Paragraph::new(line), line_rect);
                    *current_y += 1;
                }
            }
            Block::Break => {
                // Visual break, just skip a line
                *content_lines += 1;
                if *skip_rows > 0 {
                    *skip_rows = skip_rows.saturating_sub(1);
                } else if *current_y < area.y + area.height {
                    *current_y += 1;
                }
            }
            Block::ListItem {
                inline,
                ordered,
                depth,
                checked,
            } => {
                let Inline {
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                } = inline;
                let marker = match (checked, ordered) {
                    (Some(true), _) => "[x]".to_string(),
                    (Some(false), _) => "[ ]".to_string(),
                    (None, Some(index)) => format!("{}.", index),
                    (None, None) => "•".to_string(),
                };
                let indent = "  ".repeat(*depth);
                let prefix = format!("   {}{} ", indent, marker);
                let prefix_cols = display_width_u16(&prefix);
                let continuation = " ".repeat(prefix_cols as usize);
                let lines = wrap.wrap_text_markup(
                    content,
                    area.width.saturating_sub(prefix_cols) as usize,
                    &crate::text_layout::block_hidden_ranges(
                        content,
                        code_ranges,
                        bold_ranges,
                        math_ranges,
                        link_ranges,
                    ),
                );
                *content_lines += lines.len();
                for (line_index, wl) in lines.iter().enumerate() {
                    if *skip_rows > 0 {
                        *skip_rows = skip_rows.saturating_sub(1);
                        continue;
                    }
                    if *current_y >= area.y + area.height {
                        break;
                    }

                    let base = Style::default().fg(theme.fg());
                    let line = line_spans_rich(RichLineParams {
                        prefix: if line_index == 0 {
                            &prefix
                        } else {
                            &continuation
                        },
                        prefix_style: Style::default().fg(theme.brand()),
                        text: &wl.text,
                        line_start_byte: wl.start_byte,
                        selected: line_selection(sel_range, wl),
                        ranges: RichTextRanges {
                            code: code_ranges,
                            bold: bold_ranges,
                            math: math_ranges,
                            links: link_ranges,
                        },
                        base,
                        colors: RichTextColors::from_theme(theme),
                    });
                    let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                    frame.render_widget(Paragraph::new(line), line_rect);

                    if record_layout {
                        let mut hidden_for_line =
                            bold_delim_local_ranges(&wl.text, wl.start_byte, bold_ranges);
                        hidden_for_line.extend(markup_hidden_ranges(
                            &wl.text,
                            wl.start_byte,
                            code_ranges,
                            &[],
                            math_ranges,
                        ));
                        hidden_for_line.extend(link_delim_local_ranges(
                            &wl.text,
                            wl.start_byte,
                            link_ranges,
                        ));
                        push_link_hits_for_line(
                            layout_map,
                            link_ranges,
                            mi,
                            bi,
                            wl.start_byte,
                            &wl.text,
                            prefix_cols,
                            line_rect,
                            &hidden_for_line,
                        );
                        layout_map.push(BlockRegion {
                            message_idx: mi,
                            block_idx: bi,
                            start_byte: wl.start_byte,
                            end_byte: wl.end_byte,
                            text: wl.text.clone(),
                            prefix_cols,
                            rect: line_rect,
                            hidden_ranges: hidden_for_line,
                        });
                    }

                    *current_y += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::document::TableAlignment;

    #[test]
    fn table_markup_hidden_ranges_drive_hitbox_widths() {
        let table = build_table_render(
            &["a".to_string(), "b".to_string()],
            &[vec!["`中`".to_string(), "**ab**".to_string()]],
            &[TableAlignment::None, TableAlignment::None],
            80,
        );
        let row_idx = table
            .lines
            .iter()
            .position(|line| line.contains("`中`"))
            .expect("data row should render");
        let line = &table.lines[row_idx];
        let info = table.line_info[row_idx]
            .as_ref()
            .expect("data row should carry row info");
        let hidden = table_line_hidden_ranges(line, info);

        assert_eq!(visible_width_window(line, 0, line.len(), &hidden), 11);
        for &(lo, hi) in &info.col_spans {
            assert_eq!(visible_width_window(line, lo, hi, &hidden), 2);
            assert!(line[lo..hi].width() > 2, "raw markup width must be larger");
        }
    }

    #[test]
    fn cell_drag_selection_spans_only_origin_cell_segments() {
        use crate::model::layout::{SemanticCursor, TableCellSegment};

        let selection = SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 11),
            head: SemanticCursor::new(0, 0, 100),
        };
        let cell = CellDragInfo {
            message_idx: 0,
            block_idx: 0,
            cell_text: "abcdef".to_string(),
            segments: vec![
                TableCellSegment {
                    rendered_range: (10, 13),
                    content_range: (10, 13),
                    source_range: (0, 3),
                },
                TableCellSegment {
                    rendered_range: (40, 43),
                    content_range: (40, 43),
                    source_range: (3, 6),
                },
            ],
        };

        assert_eq!(
            cell_drag_selected_span(&selection, &cell, 0, &" ".repeat(20)),
            Some((11, 13))
        );
        assert_eq!(
            cell_drag_selected_span(&selection, &cell, 30, &" ".repeat(20)),
            Some((10, 13))
        );
        assert_eq!(
            cell_drag_selected_span(&selection, &cell, 60, &" ".repeat(20)),
            None,
            "rows/cells outside the origin cell must not inherit generic range selection"
        );
    }

    #[test]
    fn quote_hard_break_marker_is_stripped_from_content() {
        // The two-space hard-break marker that terminates a quote line must be
        // stripped from the stored content (as the paragraph path does); only
        // the "\n" join survives. It used to leak two trailing spaces into
        // both rendering and copy.
        let blocks = crate::model::document::parse_blocks("> a  \n> b");
        assert!(
            matches!(&blocks[0], Block::Quote(inline) if inline.content == "a\nb"),
            "got {:?}",
            blocks[0]
        );
    }

    #[test]
    fn quote_wrap_counts_markup_delimiters_as_zero_width() {
        // A `**bold**` span must not consume column budget when wrapping a
        // quote: the delimiters are painted zero-width, so a quote whose
        // *visible* text fits on one line must not wrap early.
        let msg = TranscriptMessage::new(nuo_wire::Role::Assistant, "> **abcd** ef");
        let theme = Theme::default();
        let mut grid = nuotc::Grid::new(12, 4);
        let mut frame = nuotc::Frame::new(&mut grid);
        let mut layout_map = LayoutMap::new();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;
        let mut wrap = BlockWrapCache::default();
        draw_message_body(
            &mut frame,
            Rect::new(0, 0, 12, 4),
            &msg,
            0,
            &SelectionState::None,
            None,
            &theme,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            false,
            &mut wrap,
        );
        // Visible quote text is "abcd ef" (7 cols) inside a 7-col body budget
        // (12 - 5 prefix), so it fits on one row. Raw width (11) would wrap.
        assert_eq!(content_lines, 1);
    }

    #[test]
    fn quote_prefix_constant_matches_rendered_lead() {
        // The wrap budget, the painted lead, and the recorded hit-test
        // `prefix_cols` all derive from `QUOTE_PREFIX_COLS` / `QUOTE_PREFIX`.
        // If they ever disagree, selection columns drift from the painted text.
        use crate::design::{QUOTE_PREFIX, QUOTE_PREFIX_COLS};
        assert_eq!(QUOTE_PREFIX.width(), QUOTE_PREFIX_COLS);
        let char_cols: usize = QUOTE_PREFIX
            .chars()
            .map(|c| nuotc::text::grapheme_width(&c.to_string()) as usize)
            .sum();
        assert_eq!(char_cols, QUOTE_PREFIX_COLS);
    }

    #[test]
    fn rule_aligns_with_transcript_body_leading_indent() {
        let mut grid = nuotc::Grid::new(20, 3);
        let mut frame = nuotc::Frame::new(&mut grid);
        let msg = TranscriptMessage::new(nuo_wire::Role::Assistant, "---");
        let selection = SelectionState::None;
        let theme = Theme::default();
        let mut layout_map = LayoutMap::new();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;
        let mut wrap = BlockWrapCache::default();

        draw_message_body(
            &mut frame,
            Rect::new(0, 0, 20, 3),
            &msg,
            0,
            &selection,
            None,
            &theme,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            false,
            &mut wrap,
        );

        // Leading cells before TRANSCRIPT_BODY_LEADING_INDENT must be blank
        for x in 0..TRANSCRIPT_BODY_LEADING_INDENT {
            assert_eq!(grid.get(x, 0).unwrap().symbol, " ");
        }
        // Cell at TRANSCRIPT_BODY_LEADING_INDENT must be the horizontal rule glyph '─'
        assert_eq!(
            grid.get(TRANSCRIPT_BODY_LEADING_INDENT, 0).unwrap().symbol,
            "─"
        );
    }
}
