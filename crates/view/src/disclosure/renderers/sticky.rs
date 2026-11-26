//! Sticky disclosure context anchor overlay for scrolled transcripts (ADR-0062 [INV-DISCLOSE-01]).

use graticule_canvas::{Frame, Paragraph, Rect};

use super::base::MARKER_EXPANDED;
use super::payloads::tool_summary_line;
use super::reasoning::reasoning_summary_line;
use ui_primitives::Theme;

pub use crate::layout::StickyAnchor;

/// A sticky pinned step summary (returned to the app for click handling).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StickyInfo {
    pub message_idx: usize,
    pub rect: Rect,
    /// The content-line index of the real summary inside the stream. The app
    /// uses this to re-anchor the scroll offset when the user collapses the
    /// pinned step, so the real summary takes the sticky's place at the top of
    /// the viewport instead of jumping to unrelated content.
    pub summary_line: usize,
}

pub fn draw_sticky_anchor_if_needed(
    frame: &mut Frame,
    transcript_area: Rect,
    sticky_anchors: &[StickyAnchor],
    scroll: usize,
    theme: &Theme,
) -> Option<StickyInfo> {
    let first_visible = scroll;
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
            &mut Paragraph::new(tool_summary_line(
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
            &mut Paragraph::new(reasoning_summary_line(
                MARKER_EXPANDED,
                &anchor.summary,
                anchor.color,
                summary_color,
                None,
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
