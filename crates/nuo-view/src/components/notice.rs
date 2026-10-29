//! Transcript notice component (System / Infrastructure notices as top-level
//! transcript entries).
//!
//! # One text, three consumers
//!
//! A notice's body is **stored**, not derived here: `TranscriptMessage::raw`
//! and its per-line `blocks` already hold the normalized display text
//! (`crate::model::notice_text`). This renderer therefore *paints* that text and
//! registers one [`BlockRegion`] per visual row against the same block bytes,
//! which is what makes the entry drag-selectable and copyable and keeps copy
//! byte-identical to what is on screen. It never re-parses, re-wraps, or
//! re-derives the body — the retired paint-time parse is what let "what you
//! see" and "what you copy" drift apart.
//!
//! # Chrome is not document
//!
//! The entry head (severity glyph, topic label, timestamp) is registered as the
//! entry's **sentinel** region: a click there focuses/activates the entry, and
//! none of its metadata can leak into a paste.
//!
//! A notice carries no live state at all. A provider retry's countdown used to
//! be painted here as a status label, which made a transcript row re-render
//! every second — a record that ticks is not a record. That countdown is now
//! chrome on the activity bar (`nuo_view::retry`), and what reaches this
//! component is either a settled diagnostic or a turn-outcome anchor.

use nuotc::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::disclosure::{Disclosure, Interaction, summary_text_color};
use crate::model::document::{Block, MessageKind, NoticeParts, NoticeSeverity, TranscriptMessage};
use crate::model::layout::{BlockRegion, LayoutMap, NOTICE_BLOCK_IDX};
use crate::model::selection::SelectionState;
use crate::text_layout::{BlockWrapCache, block_selection_range, line_selection};
use nuo_ui_primitives::components::row::{RowSegment, render_row_line};
use nuo_ui_primitives::design::{TRANSCRIPT_BODY_LEADING_INDENT, TURN_HEADER_BODY_GAP_ROWS};

use nuo_ui_primitives::theme::Theme;

pub struct NoticeProps<'a> {
    pub message: &'a TranscriptMessage,
}

impl<'a> NoticeProps<'a> {
    pub fn severity(&self) -> Option<NoticeSeverity> {
        match &self.message.kind {
            MessageKind::Notice { severity, .. } => Some(*severity),
            _ => None,
        }
    }

    /// The architecture-agreed two-part split (topic + title/detail), when
    /// this notice carries one. Core notices keep their structure across the
    /// boundary; local notices have no parts and report the generic topic.
    fn parts(&self) -> Option<&'a NoticeParts> {
        self.message.notice_parts()
    }

    /// The subsystem label painted in the entry head.
    fn topic(&self) -> &'a str {
        self.parts()
            .and_then(|parts| parts.topic.as_deref())
            .unwrap_or("notification")
    }

    /// The right-aligned chrome label: the wall-clock time the notice was
    /// stamped.
    ///
    /// A notice is a settled record, so this is the *only* volatile-adjacent
    /// fact it carries and it is frozen at construction. It used to also render
    /// a live retry countdown; that countdown is chrome with its own typed state
    /// (`nuo_view::retry`) and lives on the activity bar, because a transcript
    /// entry that re-renders every second is not a record.
    fn status_label(&self) -> Option<String> {
        self.message
            .sent_at_ms
            .map(nuo_ui_primitives::time::sent_time_label)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_notice_view(
    frame: &mut Frame,
    area: Rect,
    notice: NoticeProps<'_>,
    mi: usize,
    layout_map: &mut LayoutMap,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    wrap: &mut BlockWrapCache,
    selection: &SelectionState,
    theme: &Theme,
    hovered: bool,
    focused: bool,
) {
    let Some(severity) = notice.severity() else {
        return;
    };
    let full_width = area.width as usize;

    // A notice is an **always-open** entry (ADR-0020 "fully disclosed
    // entries"): its severity, title, and detail *are* the message, so nothing
    // gates them and it carries no disclosure marker — the severity glyph is
    // its whole marker vocabulary, matching the marker-free rule for command
    // entries.
    //
    // The `expanded` field remains part of the record contract (`y`-copy,
    // `pin_notice_expanded`, restore). A future collapsible notice — one whose
    // detail is a long body worth folding — must honor it in the body *and*
    // the header marker together, exactly as `compacted.rs` does.
    let (lead_symbol, tag_color) = match severity {
        NoticeSeverity::Error => ("! ", theme.err()),
        NoticeSeverity::Warning => ("▲ ", theme.warn()),
        NoticeSeverity::Info => ("ℹ ", theme.info()),
    };
    // The interaction channel still tints the topic label, so a hovered or
    // keyboard-focused notice is visibly the component attention is on
    // (affordance hue) even though its body never folds.
    let topic_color = summary_text_color(
        Some(tag_color),
        Disclosure::Expanded,
        Interaction::from_hover_focused(hovered, focused),
        theme,
    );
    let header_line = notice_header_line(
        lead_symbol,
        notice.topic(),
        topic_color,
        notice.status_label().as_deref(),
        theme.muted(),
        full_width,
    );

    // 1. Entry head — chrome, registered as the entry's sentinel region.
    *content_lines += 1;
    if *skip_rows > 0 {
        *skip_rows -= 1;
    } else if *current_y < area.y + area.height {
        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(&mut Paragraph::new(header_line), line_rect);
        layout_map.push(BlockRegion::entry_sentinel(mi, NOTICE_BLOCK_IDX, line_rect));
        *current_y += 1;
    }

    // 2. One-row blank gap between entry head and body.
    for _ in 0..TURN_HEADER_BODY_GAP_ROWS {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
        } else if *current_y < area.y + area.height {
            *current_y += 1;
        }
    }

    // 3. Body: the stored display text, painted verbatim and registered so it
    //    is selectable. The lead paragraph (everything before the first blank
    //    line) is the entry's bold lead; the rest is muted diagnostic detail.
    let indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
    let body_wrap_width = full_width.saturating_sub(indent.width()).max(1);
    let mut in_lead = true;

    for (bi, block) in notice.message.blocks.iter().enumerate() {
        if matches!(block, Block::Break) {
            in_lead = false;
            *content_lines += 1;
            if *skip_rows > 0 {
                *skip_rows -= 1;
            } else if *current_y < area.y + area.height {
                *current_y += 1;
            }
            continue;
        }
        let text = block.raw_text();
        if text.is_empty() {
            continue;
        }
        let style = if in_lead {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };
        let sel_range = block_selection_range(selection, mi, bi);
        let lines = wrap.wrap_text(text, body_wrap_width);
        // `render_row_line` works in the **logical row's** coordinate space:
        // `base_offset` is where this wrapped slice starts inside the row, and
        // each segment's byte range is measured from the row's start too. The
        // whole run therefore carries one segment spanning the full row — the
        // row is single-styled. Handing it the *slice* as the segment (with a
        // row-relative `base_offset`) left continuation rows with no segment
        // covering them, and `render_row_line` correctly painted nothing: every
        // notice longer than one visual line silently lost its body.
        let segments = [RowSegment::styled(text.to_string(), style)];
        for wl in lines.iter() {
            *content_lines += 1;
            if *skip_rows > 0 {
                *skip_rows -= 1;
                continue;
            }
            if *current_y >= area.y + area.height {
                continue;
            }
            let line_rect = Rect::new(area.x, *current_y, area.width, 1);
            let mut line = render_row_line(
                &wl.text,
                wl.start_byte,
                &segments,
                line_selection(sel_range, wl),
                theme,
            );
            line.spans
                .insert(0, Span::styled(indent.clone(), Style::default()));
            frame.render_widget(&mut Paragraph::new(line), line_rect);
            layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx: bi,
                start_byte: wl.start_byte,
                end_byte: wl.end_byte,
                text: wl.text.clone(),
                prefix_cols: indent.width() as u16,
                rect: line_rect,
                hidden_ranges: Vec::new(),
            });
            *current_y += 1;
        }
    }
}

/// Build the one-row header of a notification entry: topic left, chrome right.
///
/// The lead glyph (`! `, `▲ `, `ℹ `) and the topic label (`▲ trust`,
/// `▲ provider`, …; generic `notification` when no topic is known) are
/// rendered in the severity indicator tone (BOLD), followed by the optional
/// right-aligned chrome label (timestamp, and a retry's live attempt/countdown).
///
/// A notice is an **always-open** entry (see [`draw_notice_view`]), so it
/// carries no disclosure marker: the severity glyph is the whole marker
/// vocabulary, exactly as keeps command entries marker-free.
fn notice_header_line(
    lead_symbol: &str,
    topic: &str,
    severity_tone: Color,
    status_label: Option<&str>,
    muted: Color,
    full_width: usize,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);
    let mut used = 0usize;

    // Indicator tag: `! trust`, `▲ provider`, `ℹ command` in severity_tone + BOLD.
    let tag = format!("{lead_symbol}{topic}");
    used += tag.width();
    spans.push(Span::styled(
        tag,
        Style::default()
            .fg(severity_tone)
            .add_modifier(Modifier::BOLD),
    ));

    // Trailing chrome: right-aligned, muted.
    if let Some(status) = status_label {
        let status_width = status.width();
        if used + 2 + status_width <= full_width {
            spans.push(Span::styled(
                " ".repeat(full_width - used - status_width),
                Style::default(),
            ));
            spans.push(Span::styled(status.to_string(), Style::default().fg(muted)));
        }
    }

    Line::from(spans)
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
