//! The turn-band transcript layout: each tool-bearing ReAct turn is grouped into
//! a labelled band with a header row (`> turn N  model effort  HH:MM`), so history reads
//! as discrete model-request chunks instead of one flush stream.
//!
//! ## Grouping model
//! A "turn group" is a maximal run of consecutive assistant-side messages
//! (tool steps, reasoning traces, subagent tasks, assistant text) that share the
//! same `(round, turn)` stamp and contain at least one tool-like step. User messages
//! and notices are *not* grouped and act as group terminators — with one
//! exception: a **steer insert** (a user entry staged for, or admitted at, an
//! inner boundary of the running turn). It is typed *while* the turn is still
//! producing output, so it lands mid-turn in append order; it is therefore
//! absorbed into the turn it interrupted, and the band spans it, instead of
//! splitting that turn into two bands carrying the same `> turn N` header.
//! See [`super::is_steer_insert`].
//!
//! Assistant-side components carry a 1-indexed ReAct `turn`, plus the enclosing
//! user `round`. When a position is unknown (legacy sessions predating the
//! stamps), it falls back to ordinary unbanded flow,
//! without a band, so old transcripts stay readable.
//!
//! ## Visual form
//! Each group with a *known* turn gets, immediately before its first message,
//! a single-line header:
//!
//! ```text
//! > turn 2  sonnet
//! > turn 3  glm-5.3 xhigh        (channel exposing a reasoning effort)
//! ```
//!
//! rendered in an info-tone bold for the `> turn N` anchor and muted for the
//! rest (model info, send time), using foreground color only — no background
//! band. Model and reasoning depth form a single identity component
//! (`glm-5.3 xhigh`), and components on the header row are separated entirely
//! by spatial distance (two columns of whitespace, R2 enumeration) without `·`.
//! The effort detail appears only when the turn actually ran with one
//! (thinking-gated per protocol), so non-reasoning channels keep the shorter
//! form. The header is composed from the shared `MetaStrip` component
//! (`render/components/meta_strip.rs`), keeping this metadata treatment shared
//! with transcript chrome while letting spatial distance express the semantic
//! relationship.

use nuotc::Rect;

use crate::components::meta_strip::{MetaStrip, MetaTone};
use crate::design::{AI_OUTPUT_LEAD_GLYPH, JOIN_ENUMERATE_COLS, STREAM_BOTTOM_GAP_ROWS};
use crate::model::document::TranscriptMessage;
use crate::time::sent_time_label;

use super::{
    Stream, TranscriptLayout, default_boundary_gap, default_gap_before, default_group_end,
};

/// Turn-banded layout. See module docs.
pub struct TurnBand;

impl TranscriptLayout for TurnBand {
    fn run(&mut self, stream: &mut Stream<'_, '_>) {
        let messages_len = stream.message_end;
        let mut mi = stream.message_start;

        while mi < messages_len {
            let msg = &stream.messages[mi];

            // Detect the start of a turn group
            // The group planner can start at optional thinking/assistant text,
            // then look forward for the tool step that makes this a tool turn.
            if let (Some(group_end), Some(turn)) =
                (default_group_end(stream.messages, mi), msg.turn)
            {
                stream.gap(default_gap_before(stream.messages, mi));
                draw_turn_header(stream, turn, msg);
                stream.gap(super::TURN_HEADER_BODY_GAP_ROWS);

                for gj in mi..group_end {
                    if gj > mi {
                        stream.gap(default_boundary_gap(
                            &stream.messages[gj - 1],
                            &stream.messages[gj],
                        ));
                    }
                    stream.badge(gj);
                    stream.dispatch(gj);
                }

                mi = group_end;
                continue;
            }

            // Non-grouped message: legacy behavior
            stream.gap(default_gap_before(stream.messages, mi));
            stream.badge(mi);
            stream.dispatch(mi);

            mi += 1;
        }

        if mi == stream.messages.len() {
            stream.gap(STREAM_BOTTOM_GAP_ROWS);
        }

        stream.finish_virtual();
    }
}

/// Paint the turn header row: `> turn N  model (effort)  HH:MM`,
/// info-tone bold anchor with muted metadata, no background band. Components
/// are separated by spatial distance (R2 enumeration whitespace, no `·`), and
/// model + effort form a single identity component. The caller inserts the
/// standard header-to-body gap before the group's first component.
fn draw_turn_header(stream: &mut Stream<'_, '_>, turn: u64, msg: &TranscriptMessage) {
    // Always account for one content line even when scrolled out of view, so
    // scroll height stays faithful to what a user scrolling back would see.
    stream.content_lines += 1;
    if stream.skip_rows > 0 {
        stream.skip_rows -= 1;
        return;
    }
    if stream.current_y >= stream.viewport_bottom() {
        return;
    }

    let band = stream.band;

    // Two-tone label, no background band: `> turn N` is the info-tone
    // anchor, the rest (model + effort component, send time) reads as muted metadata on
    // the same line. Components are separated by plain whitespace (R2 enumeration,
    // `JOIN_ENUMERATE_COLS`), never `·`.
    let lead = format!("{AI_OUTPUT_LEAD_GLYPH} ");
    let mut strip = MetaStrip::new()
        .separator(" ".repeat(JOIN_ENUMERATE_COLS))
        .lead(lead, MetaTone::Accent)
        .anchor(format!("turn {}", turn));

    // Model and reasoning effort form a single identity component: `model effort`
    // (e.g. `glm-5.3 xhigh`). Absent for non-reasoning channels — nothing to claim.
    let model = msg.model.as_deref().filter(|m| !m.is_empty());
    let effort = msg
        .effort
        .as_deref()
        .filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case("none"));

    let model_info = match (model, effort) {
        (Some(m), Some(e)) => Some(format!("{m} {e}")),
        (Some(m), None) => Some(m.to_string()),
        (None, Some(e)) => Some(e.to_string()),
        (None, None) => None,
    };

    if let Some(info) = model_info {
        strip = strip.detail(info);
    }
    if let Some(sent_at_ms) = msg.sent_at_ms {
        strip = strip.trailing_detail(sent_time_label(sent_at_ms));
    }

    let rect = Rect::new(band.x, stream.current_y, band.width, 1);
    strip.render(stream.frame, rect, stream.theme);
    stream.current_y += 1;
}
