//! Unified interaction router: classifies every screen point into one of a
//! fixed set of [`ClickTarget`] variants, so the event loop can dispatch
//! behaviour with a single `match` instead of inline `if-let` chains.
//!
//! # Design
//!
//! The router owns the priority cascade (modal chrome → activity bar →
//! sticky header → input box → step summary → table cell → content → gap →
//! dead) that was previously inlined across multiple handler arms in
//! `event_loop.rs`. It depends only on the layout layer and
//! `step_interaction` — no app-state or render dependency — so it stays
//! unit-testable and free of layering cycles.
//!
//! The router does **not** handle pre-cascade checks (overlay backdrop,
//! activity bar, sticky header) that consume the click before any content
//! resolution. Those remain in the event loop because they gate on
//! transient app state (`App::surfaces`, `sticky_step`).
//!
//! # Relationship to `step_interaction`
//!
//! `step_interaction` classifies a *resolved cursor* as a step summary or
//! not. This module is the next layer up: it resolves the cursor (via
//! `LayoutMap::cursor_at`), then delegates to `step_interaction::summary_at`
//! when the cursor lands on a summary, or to `LayoutMap::table_cell_at` for
//! table cells, or falls through to generic content.

use crate::model::layout::{LayoutMap, SemanticCursor, TableCellSegment};
use crate::render::INPUT_MSG_IDX;
use crate::step_interaction::{self, StepKind};

/// The type of clickable region a screen point resolved to.
///
/// Variants are ordered by priority — the router returns the first match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickTarget {
    /// Click landed inside the live input (composer) box.
    InputBox { cursor: SemanticCursor },
    /// Click landed on a step summary (tool step or reasoning trace).
    StepSummary { message_idx: usize, kind: StepKind },
    /// Click landed inside a table cell. Drag selections are clamped to the
    /// cell's `│` boundaries but the user can select a substring of the
    /// original `cell_text`.
    TableCell {
        message_idx: usize,
        block_idx: usize,
        cell_idx: usize,
        cursor: SemanticCursor,
        /// Original cell text (from headers/rows, before padding/wrapping).
        cell_text: String,
        /// Render/source mappings for every visible line segment belonging to
        /// this logical cell.
        cell_segments: Vec<TableCellSegment>,
    },
    /// Click landed on a rendered hyperlink label.
    Link {
        message_idx: usize,
        block_idx: usize,
        url: String,
    },
    /// Click landed on regular content — prose, code, heading, quote, list,
    /// etc. — with a resolved semantic cursor.
    Content { cursor: SemanticCursor },
    /// Click landed inside the transcript content rect but did not hit any
    /// region (gap rows between messages, spacing inside expanded steps).
    ContentGap,
    /// Click landed outside all known areas (outer gutters, below content,
    /// chrome that doesn't have its own handler).
    Dead,
}

/// Classify a screen point `(x, y)` into a [`ClickTarget`].
///
/// The priority cascade:
///
/// ```text
/// cursor_at(x,y)?
///   ├─ InputBox    (message_idx == INPUT_MSG_IDX)
///   ├─ StepSummary (block_idx is TOOL_STEP or THINKING sentinel)
///   ├─ TableCell   (table_cell_at hits)
///   └─ Content     (any other resolved cursor)
/// transcript_content_rect?
///   ├─ ContentGap  (inside content band, no cursor)
///   └─ Dead        (nothing)
/// ```
///
/// This function is pure: it only reads from `layout_map` and returns a value.
pub fn classify_click(layout_map: &LayoutMap, x: u16, y: u16) -> ClickTarget {
    if let Some(cursor) = layout_map.cursor_at(x, y) {
        // Input box regions carry the INPUT_MSG_IDX sentinel.
        if cursor.message_idx == INPUT_MSG_IDX {
            return ClickTarget::InputBox { cursor };
        }

        // Step summaries carry sentinel block_idx values.
        if let Some((message_idx, kind)) = step_interaction::summary_at(&cursor) {
            return ClickTarget::StepSummary { message_idx, kind };
        }

        // Table cells have their own parallel hit-test layer.
        if let Some(hit) = layout_map.table_cell_at(x, y) {
            return ClickTarget::TableCell {
                message_idx: hit.message_idx,
                block_idx: hit.block_idx,
                cell_idx: hit.cell_idx,
                cursor,
                cell_text: hit.cell_text.clone(),
                cell_segments: layout_map.table_cell_segments(
                    hit.message_idx,
                    hit.block_idx,
                    hit.cell_idx,
                ),
            };
        }

        if let Some(hit) = layout_map.link_at(x, y) {
            return ClickTarget::Link {
                message_idx: hit.message_idx,
                block_idx: hit.block_idx,
                url: hit.url.clone(),
            };
        }

        return ClickTarget::Content { cursor };
    }

    // Composer component hit-test: any click inside the physical bounds of the
    // composer (e.g. top chrome row, breathing margins, bottom gap row, or
    // hint bar) routes focus directly to the composer input box.
    if let Some(rect) = layout_map.composer_rect()
        && rect.x <= x
        && x < rect.x + rect.width
        && rect.y <= y
        && y < rect.y + rect.height
    {
        let cursor = layout_map.composer_fallback_cursor(y);
        return ClickTarget::InputBox { cursor };
    }

    // No region hit. Check whether the point falls inside the content band
    // (gap rows between messages / inside expanded steps).
    if layout_map
        .transcript_content_rect()
        .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        return ClickTarget::ContentGap;
    }

    ClickTarget::Dead
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::layout::{BlockRegion, LayoutMap, LinkHit, TableCellHit, TableCellSegment};
    use nuotc::Rect;

    fn push_region(map: &mut LayoutMap, text: &str, msg_idx: usize, block_idx: usize, y: u16) {
        map.push(BlockRegion {
            message_idx: msg_idx,
            block_idx,
            start_byte: 0,
            end_byte: text.len(),
            text: text.to_string(),
            prefix_cols: 0,
            rect: Rect::new(0, y, text.len() as u16, 1),
            hidden_ranges: Vec::new(),
        });
    }

    #[test]
    fn dead_when_no_regions() {
        let map = LayoutMap::new();
        assert_eq!(classify_click(&map, 5, 5), ClickTarget::Dead);
    }

    #[test]
    fn content_when_region_hit() {
        let mut map = LayoutMap::new();
        push_region(&mut map, "hello", 0, 0, 0);
        assert_eq!(
            classify_click(&map, 2, 0),
            ClickTarget::Content {
                cursor: SemanticCursor::new(0, 0, 2)
            }
        );
    }

    #[test]
    fn input_box_when_msg_idx_is_input_sentinel() {
        let mut map = LayoutMap::new();
        push_region(&mut map, "prompt text", INPUT_MSG_IDX, 0, 0);
        assert!(matches!(
            classify_click(&map, 2, 0),
            ClickTarget::InputBox { .. }
        ));
    }

    #[test]
    fn step_summary_when_block_idx_is_sentinel() {
        use crate::model::layout::TOOL_STEP_BLOCK_IDX;
        let mut map = LayoutMap::new();
        push_region(&mut map, "[bash] $ echo hi", 3, TOOL_STEP_BLOCK_IDX, 5);
        assert_eq!(
            classify_click(&map, 2, 5),
            ClickTarget::StepSummary {
                message_idx: 3,
                kind: StepKind::ToolStep,
            }
        );
    }

    #[test]
    fn table_cell_when_hit_box_matches() {
        let mut map = LayoutMap::new();
        // A plain-text region for the grid line (needed so cursor_at passes).
        push_region(&mut map, "│ hello │ world │", 1, 2, 3);
        // A cell hit box covering column 3..8 ("hello").
        map.push_table_cell_hit(TableCellHit {
            message_idx: 1,
            block_idx: 2,
            cell_idx: 0,
            rect: Rect::new(3, 3, 5, 1),
            cell_text: "hello".into(),
            segment: TableCellSegment {
                rendered_range: (2, 7),
                content_range: (2, 7),
                source_range: (0, 5),
            },
        });
        let result = classify_click(&map, 5, 3);
        assert!(matches!(
            result,
            ClickTarget::TableCell {
                message_idx: 1,
                block_idx: 2,
                cell_idx: 0,
                ..
            }
        ));
    }

    #[test]
    fn priority_input_box_beats_step_summary() {
        use crate::model::layout::TOOL_STEP_BLOCK_IDX;
        let mut map = LayoutMap::new();
        // Both a step-summary sentinel AND input sentinel on the same region.
        // Input box must win (checked first).
        push_region(&mut map, "x", INPUT_MSG_IDX, TOOL_STEP_BLOCK_IDX, 0);
        assert!(matches!(
            classify_click(&map, 0, 0),
            ClickTarget::InputBox { .. }
        ));
    }

    #[test]
    fn priority_step_summary_beats_table_cell() {
        use crate::model::layout::TOOL_STEP_BLOCK_IDX;
        let mut map = LayoutMap::new();
        push_region(&mut map, "step", 0, TOOL_STEP_BLOCK_IDX, 0);
        map.push_table_cell_hit(TableCellHit {
            message_idx: 0,
            block_idx: TOOL_STEP_BLOCK_IDX,
            cell_idx: 0,
            cell_text: String::new(),
            segment: TableCellSegment {
                rendered_range: (0, 0),
                content_range: (0, 0),
                source_range: (0, 0),
            },
            rect: Rect::new(0, 0, 4, 1),
        });
        assert!(matches!(
            classify_click(&map, 2, 0),
            ClickTarget::StepSummary { .. }
        ));
    }

    #[test]
    fn priority_table_cell_beats_content() {
        let mut map = LayoutMap::new();
        push_region(&mut map, "│ a │ b │", 0, 0, 0);
        map.push_table_cell_hit(TableCellHit {
            message_idx: 0,
            block_idx: 0,
            cell_idx: 0,
            cell_text: String::new(),
            segment: TableCellSegment {
                rendered_range: (2, 3),
                content_range: (2, 3),
                source_range: (0, 1),
            },
            rect: Rect::new(2, 0, 1, 1),
        });
        assert!(matches!(
            classify_click(&map, 2, 0),
            ClickTarget::TableCell { .. }
        ));
    }

    #[test]
    fn link_beats_generic_content() {
        let mut map = LayoutMap::new();
        push_region(&mut map, "Rust", 0, 0, 0);
        map.push_link_hit(LinkHit {
            message_idx: 0,
            block_idx: 0,
            range: (0, 4),
            url: "https://www.rust-lang.org".into(),
            rect: Rect::new(0, 0, 4, 1),
        });
        assert_eq!(
            classify_click(&map, 2, 0),
            ClickTarget::Link {
                message_idx: 0,
                block_idx: 0,
                url: "https://www.rust-lang.org".into(),
            }
        );
    }

    #[test]
    fn content_gap_when_inside_content_rect_but_no_region() {
        let mut map = LayoutMap::new();
        map.set_transcript_content_rect(Rect::new(2, 4, 10, 20));
        // A point inside the band but no region registered there.
        assert_eq!(classify_click(&map, 5, 10), ClickTarget::ContentGap);
    }

    #[test]
    fn dead_when_outside_content_rect_and_no_region() {
        let mut map = LayoutMap::new();
        map.set_transcript_content_rect(Rect::new(2, 4, 10, 20));
        assert_eq!(classify_click(&map, 100, 100), ClickTarget::Dead);
    }

    #[test]
    fn composer_rect_hit_on_top_chrome_snaps_to_start() {
        let mut map = LayoutMap::new();
        // Composer rect at y=10..14 (height 4). Text line at y=11.
        map.set_composer_rect(Rect::new(0, 10, 40, 4));
        push_region(&mut map, "typed prompt text", INPUT_MSG_IDX, 0, 11);

        // Click on top chrome (y=10, above text at y=11)
        assert_eq!(
            classify_click(&map, 5, 10),
            ClickTarget::InputBox {
                cursor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0)
            }
        );
    }

    #[test]
    fn composer_rect_hit_on_bottom_gap_or_hints_snaps_to_end() {
        let mut map = LayoutMap::new();
        // Composer rect at y=10..14 (height 4). Text line at y=11.
        map.set_composer_rect(Rect::new(0, 10, 40, 4));
        push_region(&mut map, "typed prompt text", INPUT_MSG_IDX, 0, 11);

        // Click on bottom gap (y=12)
        assert_eq!(
            classify_click(&map, 5, 12),
            ClickTarget::InputBox {
                cursor: SemanticCursor::new(INPUT_MSG_IDX, 0, 17)
            }
        );

        // Click on hint bar (y=13)
        assert_eq!(
            classify_click(&map, 20, 13),
            ClickTarget::InputBox {
                cursor: SemanticCursor::new(INPUT_MSG_IDX, 0, 17)
            }
        );
    }

    #[test]
    fn composer_rect_hit_with_empty_input_snaps_to_zero() {
        let mut map = LayoutMap::new();
        map.set_composer_rect(Rect::new(0, 10, 40, 4));
        // No regions pushed (or 0-length region)
        assert_eq!(
            classify_click(&map, 5, 10),
            ClickTarget::InputBox {
                cursor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0)
            }
        );
        assert_eq!(
            classify_click(&map, 5, 13),
            ClickTarget::InputBox {
                cursor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0)
            }
        );
    }
}
