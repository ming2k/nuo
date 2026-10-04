//! Sticky pinned-step summary overlay for scrolled transcripts.

use nuotc::{Color, Frame, Paragraph, Rect};

use super::base::MARKER_EXPANDED;
use super::payloads::tool_summary_line;
use super::reasoning::reasoning_summary_line;
use crate::render::{StickyInfo, Theme};

/// Information needed to draw the sticky summary header for an expanded step
/// whose summary has scrolled above the viewport but whose body is still
/// visible.
#[derive(Clone, Debug)]
pub struct StickyStep {
    pub message_idx: usize,
    pub summary: String,
    pub color: Color,
    pub background: Option<Color>,
    pub summary_line: usize,
    pub body_end_line: usize,
}

pub fn draw_sticky_summary_if_needed(
    frame: &mut Frame,
    transcript_area: Rect,
    sticky_steps: &[StickyStep],
    scroll: u16,
    theme: &Theme,
) -> Option<StickyInfo> {
    let first_visible = scroll as usize;
    let step = sticky_steps
        .iter()
        .find(|c| c.summary_line < first_visible && c.body_end_line > first_visible)?;
    let summary_color = theme.fg();
    let line_rect = if let Some(bg) = step.background {
        let line_rect = Rect::new(
            transcript_area.x,
            transcript_area.y,
            transcript_area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(tool_summary_line(
                MARKER_EXPANDED,
                &step.summary,
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
                &step.summary,
                step.color,
                summary_color,
                transcript_area.width as usize,
            )),
            line_rect,
        );
        line_rect
    };
    Some(StickyInfo {
        message_idx: step.message_idx,
        rect: line_rect,
        summary_line: step.summary_line,
    })
}
