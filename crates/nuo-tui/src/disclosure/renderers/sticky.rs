//! Sticky disclosure context anchor overlay for scrolled transcripts (ADR-0062 [INV-DISCLOSE-01]).

use nuotc::{Color, Frame, Paragraph, Rect};

use super::base::MARKER_EXPANDED;
use super::payloads::tool_summary_line;
use super::reasoning::reasoning_summary_line;
use crate::render::{StickyInfo, Theme};

/// Information needed to draw the sticky anchor header for an expanded disclosure card
/// whose summary has scrolled above the viewport but whose body is still visible.
#[derive(Clone, Debug)]
pub struct StickyAnchor {
    pub message_idx: usize,
    pub summary: String,
    pub color: Color,
    pub background: Option<Color>,
    pub summary_line: usize,
    pub body_end_line: usize,
}

pub fn draw_sticky_anchor_if_needed(
    frame: &mut Frame,
    transcript_area: Rect,
    sticky_anchors: &[StickyAnchor],
    scroll: u16,
    theme: &Theme,
) -> Option<StickyInfo> {
    let first_visible = scroll as usize;
    let anchor = sticky_anchors
        .iter()
        .find(|c| c.summary_line < first_visible && c.body_end_line > first_visible)?;
    let summary_color = theme.fg();
    let line_rect = if let Some(bg) = anchor.background {
        let line_rect = Rect::new(
            transcript_area.x,
            transcript_area.y,
            transcript_area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(tool_summary_line(
                MARKER_EXPANDED,
                &anchor.summary,
                summary_color,
                bg,
                transcript_area.width as usize,
            )),
            line_rect,
        );
        line_rect
    } else {
        let line_rect = Rect::new(
            transcript_area.x,
            transcript_area.y,
            transcript_area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(reasoning_summary_line(
                MARKER_EXPANDED,
                &anchor.summary,
                anchor.color,
                summary_color,
                transcript_area.width as usize,
            )),
            line_rect,
        );
        line_rect
    };
    Some(StickyInfo {
        message_idx: anchor.message_idx,
        summary_line: anchor.summary_line,
        rect: line_rect,
    })
}
