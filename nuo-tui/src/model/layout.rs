//! Layout engine: maps semantic blocks to screen coordinates.
//!
//! During rendering we record where each block lands on the terminal grid.
//! This allows mouse events to be resolved back to semantic positions.

use nuotc::Rect;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const TOOL_STEP_BLOCK_IDX: usize = usize::MAX;
pub const REASONING_BLOCK_IDX: usize = usize::MAX - 1;
/// ADR-0091 command-result header rows: same disclosure interaction as tool
/// steps, own block index so focus/click resolution routes to the command.
pub const COMMAND_RESULT_BLOCK_IDX: usize = usize::MAX - 3;
/// Expandable notice header rows (e.g. provider error with formatted JSON).
/// Live provider-retry entries render through the notice renderer and record
/// this sentinel too — a retry is a notice with a countdown, not a peer kind,
/// so it shares the notice's target rather than carrying a dead sentinel of
/// its own.
pub const NOTICE_BLOCK_IDX: usize = usize::MAX - 4;
/// Expandable compaction checkpoint card header rows (ADR-0296).
pub const COMPACTED_CARD_BLOCK_IDX: usize = usize::MAX - 6;
/// Sentinel message index for live composer input regions.
///
/// Distinct from every block sentinel above: these two id spaces are compared
/// in the same hit-test (`cursor.message_idx == INPUT_MSG_IDX`) and separated
/// by *value*, so a collision silently re-routes one kind of region into the
/// other.
pub const INPUT_MSG_IDX: usize = usize::MAX - 8;
/// Sentinel message index for text regions inside modal overlays.
pub const MODAL_DOC_MSG_IDX: usize = usize::MAX - 5;

/// Identifies a specific position inside the document model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemanticCursor {
    /// Index into the message list.
    pub message_idx: usize,
    /// Index into the message's block list.
    pub block_idx: usize,
    /// Byte offset inside the block's raw text. Hit-testing may place this
    /// inside a grapheme cluster; selection/copy consumers snap it to grapheme
    /// boundaries before slicing.
    pub byte_offset: usize,
}

impl SemanticCursor {
    pub fn new(message_idx: usize, block_idx: usize, byte_offset: usize) -> Self {
        Self {
            message_idx,
            block_idx,
            byte_offset,
        }
    }
}

/// User-activatable target recorded from the semantic layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveTarget {
    pub message_idx: usize,
    pub block_idx: usize,
    pub kind: InteractiveTargetKind,
}

/// Category of an activatable target.
///
/// Kept in lock-step with [`StepKind`](crate::step_interaction::StepKind): the
/// mapping here is what turns a message into a keyboard-focus target, while
/// `StepKind` turns a pointer hit into one. A variant present in only one of
/// the two is a target that either cannot be clicked or cannot be focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveTargetKind {
    ToolStep,
    Reasoning,
    CommandResult,
    Notice,
    CompactedCard,
}

impl InteractiveTarget {
    /// Resolve the target a recorded block sentinel represents, or `None` when
    /// the region is not an activatable entry (prose, code, a table cell, …).
    ///
    /// The single sentinel → target mapping: [`LayoutMap::interactive_targets`]
    /// filters through it so a sentinel that no arm claims can never be
    /// reported as visible-but-unfocusable.
    pub fn for_block(block_idx: usize, message_idx: usize) -> Option<Self> {
        Some(match block_idx {
            TOOL_STEP_BLOCK_IDX => Self::tool_step(message_idx),
            REASONING_BLOCK_IDX => Self::reasoning(message_idx),
            NOTICE_BLOCK_IDX => Self::notice(message_idx),
            COMMAND_RESULT_BLOCK_IDX => Self::command_result(message_idx),
            COMPACTED_CARD_BLOCK_IDX => Self::compacted_card(message_idx),
            _ => return None,
        })
    }

    pub fn tool_step(message_idx: usize) -> Self {
        Self {
            message_idx,
            block_idx: TOOL_STEP_BLOCK_IDX,
            kind: InteractiveTargetKind::ToolStep,
        }
    }

    pub fn reasoning(message_idx: usize) -> Self {
        Self {
            message_idx,
            block_idx: REASONING_BLOCK_IDX,
            kind: InteractiveTargetKind::Reasoning,
        }
    }

    pub fn command_result(message_idx: usize) -> Self {
        Self {
            message_idx,
            block_idx: COMMAND_RESULT_BLOCK_IDX,
            kind: InteractiveTargetKind::CommandResult,
        }
    }

    pub fn notice(message_idx: usize) -> Self {
        Self {
            message_idx,
            block_idx: NOTICE_BLOCK_IDX,
            kind: InteractiveTargetKind::Notice,
        }
    }

    pub fn compacted_card(message_idx: usize) -> Self {
        Self {
            message_idx,
            block_idx: COMPACTED_CARD_BLOCK_IDX,
            kind: InteractiveTargetKind::CompactedCard,
        }
    }
}

/// A rectangular region on screen that corresponds to a slice of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRegion {
    pub message_idx: usize,
    pub block_idx: usize,
    /// Byte offset of the first character displayed in this region.
    pub start_byte: usize,
    /// Byte offset of the first character *after* this region.
    pub end_byte: usize,
    /// The exact text slice rendered in this region (no indent/prefix).
    pub text: String,
    /// Display columns occupied by decoration before the text (indent, `│ `).
    pub prefix_cols: u16,
    /// Screen rectangle (inclusive start, exclusive end in x; y is absolute row).
    pub rect: Rect,
    /// Byte ranges within `text` that are rendered as zero-width (visually
    /// elided) — e.g. the `**` bold marker delimiters. [`Self::text`] still
    /// holds the original bytes (so copy, which resolves against the block's
    /// raw `content`, yields the exact `**bold**` source), but these ranges
    /// occupy no display columns, so [`LayoutMap::cursor_at`] must skip them
    /// when mapping a screen column back to a byte offset. Empty for blocks
    /// with no elided markup.
    pub hidden_ranges: Vec<(usize, usize)>,
}

/// Records the layout of rendered blocks for a single frame.
#[derive(Debug, Clone, Default)]
pub struct LayoutMap {
    regions: Vec<BlockRegion>,
    /// The displayed grid text for each `Block::Table`, keyed by
    /// `(message_idx, block_idx)`. Stored at render time because table
    /// columns are reshaped to fit the viewport, so this grid can differ
    /// from the width-independent `rendered` field stored on the block.
    /// Whole-table copy (middle-click) resolves against this text.
    table_grids: std::collections::HashMap<(usize, usize), String>,
    /// Hit boxes for individual table cells, so a click inside a cell resolves
    /// to that cell (row-major index: `row * ncols + col`, header is row 0)
    /// rather than to the whole grid line.
    table_cell_hits: Vec<TableCellHit>,
    /// Hit boxes for visible hyperlink labels.
    link_hits: Vec<LinkHit>,
    /// The visible transcript content rect for the frame: the horizontal band
    /// (inside the `TRANSCRIPT_H_INSET` gutters) spanning only the rows where
    /// transcript content was actually drawn. A click that doesn't resolve to
    /// any region but lands inside this rect still focuses the nearest
    /// transcript step, so gap rows between messages behave like the content
    /// they separate rather than dead zones. The outer
    /// gutters are excluded on purpose: clicks there are not transcript clicks.
    transcript_content_rect: Option<Rect>,
    /// Screen rectangle enclosing the entire composer component.
    composer_rect: Option<Rect>,
}

/// A visible row range belonging to one selectable question option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionOptionHit {
    /// Zero-based option index for the active question. The synthetic `Other`
    /// row uses `question.options.len()`.
    pub option_index: usize,
    pub rect: Rect,
}

/// Permission-sheet footer action hit box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionActionHit {
    pub action_index: usize,
    pub rect: Rect,
}

/// A clickable region belonging to one logical table cell.
///
/// One rendered line segment belonging to a logical table cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableCellSegment {
    /// Absolute byte range of the padded cell content within the rendered table
    /// grid. This is the range selection rendering understands.
    pub rendered_range: (usize, usize),
    /// Absolute byte range of the actual cell text within the rendered table
    /// grid, excluding alignment padding. Drag endpoints clamp here so padding
    /// clicks resolve to the nearest text boundary.
    pub content_range: (usize, usize),
    /// Byte range in the original, unwrapped cell text represented by this
    /// rendered line segment.
    pub source_range: (usize, usize),
}

/// `cell_text` is the *original* cell text (from `headers` / `rows`, before
/// padding/wrapping). `segment` maps this visible table line back to that
/// source text.
#[derive(Debug, Clone)]
pub struct TableCellHit {
    pub message_idx: usize,
    pub block_idx: usize,
    pub cell_idx: usize,
    pub rect: Rect,
    /// Original cell text, copied from the `Block::Table` headers/rows.
    pub cell_text: String,
    pub segment: TableCellSegment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub message_idx: usize,
    pub block_idx: usize,
    pub range: (usize, usize),
    pub url: String,
    pub rect: Rect,
}

impl LayoutMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a portion of a block occupies a screen rectangle.
    pub fn push(&mut self, region: BlockRegion) {
        self.regions.push(region);
    }

    /// Record the visible transcript content rect for this frame, drawn inside
    /// the horizontal gutters. Called once at the end of `draw_transcript`.
    pub fn set_transcript_content_rect(&mut self, rect: Rect) {
        self.transcript_content_rect = Some(rect);
    }

    /// Record the screen rectangle enclosing the composer component.
    pub fn set_composer_rect(&mut self, rect: Rect) {
        self.composer_rect = Some(rect);
    }

    /// The screen rectangle of the composer component, if rendered this frame.
    pub fn composer_rect(&self) -> Option<Rect> {
        self.composer_rect
    }

    /// Resolve a semantic cursor for a click within the composer component that
    /// landed outside any specific text line (e.g. on the top breathing row,
    /// mode indicator, bottom gap row, or hint bar).
    pub fn composer_fallback_cursor(&self, y: u16) -> SemanticCursor {
        let input_regions: Vec<&BlockRegion> = self
            .regions
            .iter()
            .filter(|r| r.message_idx == INPUT_MSG_IDX)
            .collect();

        if let Some(first) = input_regions.first() {
            let last = input_regions.last().unwrap_or(first);
            if y < first.rect.y {
                SemanticCursor::new(INPUT_MSG_IDX, 0, first.start_byte)
            } else {
                SemanticCursor::new(INPUT_MSG_IDX, 0, last.end_byte)
            }
        } else {
            SemanticCursor::new(INPUT_MSG_IDX, 0, 0)
        }
    }

    /// The visible transcript content rect, if any content was drawn this frame.
    /// Extract the rendered text covered by a selection range from recorded regions.
    pub fn extract_text_for_range(
        &self,
        sel: &crate::model::selection::SelectionState,
    ) -> Option<String> {
        let (start, end) = sel.active_normalized_range()?;
        let mut result = Vec::new();
        let mut current_block: Option<usize> = None;
        let mut block_text = String::new();

        for region in &self.regions {
            let here = (region.message_idx, region.block_idx);
            if here < (start.message_idx, start.block_idx)
                || here > (end.message_idx, end.block_idx)
            {
                continue;
            }

            let s_byte = if here == (start.message_idx, start.block_idx) {
                start.byte_offset
            } else {
                0
            };
            let e_byte = if here == (end.message_idx, end.block_idx) {
                Some(end.byte_offset)
            } else {
                None
            };

            if let Some(e) = e_byte
                && e <= region.start_byte
                && !(e == region.start_byte && region.text.is_empty())
            {
                continue;
            }
            if s_byte >= region.end_byte && !(s_byte == region.start_byte && region.text.is_empty())
            {
                continue;
            }

            let lo = crate::model::selection::floor_grapheme_boundary(
                &region.text,
                s_byte
                    .saturating_sub(region.start_byte)
                    .min(region.text.len()),
            );
            let hi = match e_byte {
                Some(e) if e < region.end_byte => crate::model::selection::inclusive_grapheme_end(
                    &region.text,
                    e.saturating_sub(region.start_byte),
                ),
                _ => region.text.len(),
            };
            let hi = hi.min(region.text.len());

            if lo < hi {
                if current_block != Some(region.block_idx) {
                    if current_block.is_some() && !block_text.is_empty() {
                        result.push(std::mem::take(&mut block_text));
                    }
                    current_block = Some(region.block_idx);
                }
                block_text.push_str(&region.text[lo..hi]);
            }
        }

        if !block_text.is_empty() {
            result.push(block_text);
        }

        if result.is_empty() {
            None
        } else {
            Some(result.join("\n"))
        }
    }

    /// Clicks inside this rect that don't resolve to a specific region still
    /// switch keyboard focus to Browse (see the `SelectionStart` handler).
    pub fn transcript_content_rect(&self) -> Option<Rect> {
        self.transcript_content_rect
    }

    /// Record the displayed grid text for a table block.
    pub fn record_table_grid(&mut self, message_idx: usize, block_idx: usize, text: String) {
        self.table_grids.insert((message_idx, block_idx), text);
    }

    /// Look up the displayed grid text previously recorded for a table block.
    pub fn table_grid(&self, message_idx: usize, block_idx: usize) -> Option<&str> {
        self.table_grids
            .get(&(message_idx, block_idx))
            .map(String::as_str)
    }

    /// Record a clickable hit box for one table cell.
    pub fn push_table_cell_hit(&mut self, hit: TableCellHit) {
        self.table_cell_hits.push(hit);
    }

    /// Resolve a screen point to the table cell it lies inside, if any.
    pub fn table_cell_at(&self, x: u16, y: u16) -> Option<&TableCellHit> {
        self.table_cell_hits.iter().find(|h| {
            h.rect.x <= x
                && x < h.rect.x + h.rect.width
                && h.rect.y <= y
                && y < h.rect.y + h.rect.height
        })
    }

    pub fn push_link_hit(&mut self, hit: LinkHit) {
        self.link_hits.push(hit);
    }

    pub fn link_at(&self, x: u16, y: u16) -> Option<&LinkHit> {
        self.link_hits.iter().find(|h| {
            h.rect.x <= x
                && x < h.rect.x + h.rect.width
                && h.rect.y <= y
                && y < h.rect.y + h.rect.height
        })
    }

    pub fn table_cell_segments(
        &self,
        message_idx: usize,
        block_idx: usize,
        cell_idx: usize,
    ) -> Vec<TableCellSegment> {
        self.table_cell_hits
            .iter()
            .filter(|hit| {
                hit.message_idx == message_idx
                    && hit.block_idx == block_idx
                    && hit.cell_idx == cell_idx
            })
            .map(|hit| hit.segment)
            .collect()
    }

    /// Find the semantic cursor at a given screen coordinate.
    ///
    /// The column is resolved against the region's actual text using Unicode
    /// display width, so multi-byte and wide (CJK) characters map to the
    /// correct byte offset. For non-leading columns of a wide grapheme, the
    /// result intentionally sits inside that grapheme: a collapsed click still
    /// compares equal to itself, while an actual drag can distinguish "moved
    /// across this glyph" without jumping to the next glyph. Consumers that
    /// slice text must snap to grapheme boundaries first.
    pub fn cursor_at(&self, x: u16, y: u16) -> Option<SemanticCursor> {
        let region = self.region_at(x, y)?;

        let col_in_rect = x.saturating_sub(region.rect.x);
        let col = col_in_rect.saturating_sub(region.prefix_cols) as usize;

        // Walk the rendered text, accumulating display width until we reach
        // the clicked column. The cursor lands at the start of the character
        // occupying that column. Bytes that fall inside a `hidden_ranges`
        // entry are visually elided (zero-width, e.g. `**` bold markers), so
        // they advance the byte cursor but contribute no display columns —
        // keeping the screen-column → byte-offset mapping in lockstep with
        // what the user actually sees.
        let mut acc_width = 0usize;
        for (byte_idx, grapheme) in region.text.grapheme_indices(true) {
            if region
                .hidden_ranges
                .iter()
                .any(|&(lo, hi)| byte_idx >= lo && byte_idx < hi)
            {
                continue;
            }
            let w = if grapheme == "\n" {
                0
            } else {
                grapheme.width().max(1)
            };
            if col < acc_width + w {
                let target_byte = if col == acc_width || grapheme.len() <= 1 {
                    byte_idx
                } else {
                    byte_idx + 1
                };
                return Some(SemanticCursor::new(
                    region.message_idx,
                    region.block_idx,
                    region.start_byte + target_byte,
                ));
            }
            acc_width += w;
        }

        // Past the end of the line: cursor sits after the last character.
        Some(SemanticCursor::new(
            region.message_idx,
            region.block_idx,
            region.end_byte.max(region.start_byte),
        ))
    }

    /// Find the region containing a screen point (x, y).
    pub fn region_at(&self, x: u16, y: u16) -> Option<&BlockRegion> {
        self.regions.iter().find(|r| {
            r.rect.x <= x
                && x < r.rect.x + r.rect.width
                && r.rect.y <= y
                && y < r.rect.y + r.rect.height
        })
    }

    /// Return the first block region recorded for a given message index.
    pub fn first_region_for_message(&self, message_idx: usize) -> Option<&BlockRegion> {
        self.regions.iter().find(|r| r.message_idx == message_idx)
    }

    /// Return visible activatable targets in screen order.
    pub fn interactive_targets(&self) -> Vec<InteractiveTarget> {
        let mut regions: Vec<&BlockRegion> = self
            .regions
            .iter()
            .filter(|region| InteractiveTarget::for_block(region.block_idx, 0).is_some())
            .collect();
        regions.sort_by_key(|region| (region.rect.y, region.rect.x));

        let mut targets = Vec::new();
        for region in regions {
            let Some(target) =
                InteractiveTarget::for_block(region.block_idx, region.message_idx)
            else {
                continue;
            };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block sentinels and the message-index sentinels live in **one**
    /// numeric space and are compared in the same hit-test
    /// (`cursor.message_idx == INPUT_MSG_IDX`), so a collision silently
    /// re-routes one kind of region into another. Every sentinel must be
    /// distinct, and none may collide with a real message index.
    #[test]
    fn sentinels_are_distinct_and_never_collide() {
        let sentinels = [
            ("TOOL_STEP_BLOCK_IDX", TOOL_STEP_BLOCK_IDX),
            ("REASONING_BLOCK_IDX", REASONING_BLOCK_IDX),
            ("COMMAND_RESULT_BLOCK_IDX", COMMAND_RESULT_BLOCK_IDX),
            ("NOTICE_BLOCK_IDX", NOTICE_BLOCK_IDX),
            ("COMPACTED_CARD_BLOCK_IDX", COMPACTED_CARD_BLOCK_IDX),
            ("INPUT_MSG_IDX", INPUT_MSG_IDX),
            ("MODAL_DOC_MSG_IDX", MODAL_DOC_MSG_IDX),
        ];
        for (i, (name_a, a)) in sentinels.iter().enumerate() {
            for (name_b, b) in sentinels.iter().skip(i + 1) {
                assert_ne!(a, b, "{name_a} and {name_b} share the sentinel value {a}");
            }
            assert!(
                *a > 1024,
                "{name_a} must stay clear of real message/block indices"
            );
        }
    }

    /// Every sentinel a renderer records for an activatable entry must resolve
    /// to a target, and a non-entry sentinel must resolve to nothing. This is
    /// what keeps a click on a marked entry from silently degrading into a
    /// text selection (ADR-0020 §6).
    #[test]
    fn for_block_maps_every_entry_sentinel_and_nothing_else() {
        assert_eq!(
            InteractiveTarget::for_block(TOOL_STEP_BLOCK_IDX, 3),
            Some(InteractiveTarget::tool_step(3))
        );
        assert_eq!(
            InteractiveTarget::for_block(REASONING_BLOCK_IDX, 3),
            Some(InteractiveTarget::reasoning(3))
        );
        assert_eq!(
            InteractiveTarget::for_block(NOTICE_BLOCK_IDX, 3),
            Some(InteractiveTarget::notice(3))
        );
        assert_eq!(
            InteractiveTarget::for_block(COMMAND_RESULT_BLOCK_IDX, 3),
            Some(InteractiveTarget::command_result(3))
        );
        assert_eq!(
            InteractiveTarget::for_block(COMPACTED_CARD_BLOCK_IDX, 3),
            Some(InteractiveTarget::compacted_card(3))
        );
        // Prose / code / table-cell regions are not entries.
        assert_eq!(InteractiveTarget::for_block(0, 3), None);
        assert_eq!(InteractiveTarget::for_block(7, 3), None);
    }

    fn region(text: &str, start_byte: usize, prefix_cols: u16, rect: Rect) -> BlockRegion {
        BlockRegion {
            message_idx: 0,
            block_idx: 0,
            start_byte,
            end_byte: start_byte + text.len(),
            text: text.to_string(),
            prefix_cols,
            rect,
            hidden_ranges: Vec::new(),
        }
    }

    #[test]
    fn test_cursor_at_basic() {
        let mut map = LayoutMap::new();
        map.push(region("hello", 0, 0, Rect::new(0, 0, 10, 1)));

        let cursor = map.cursor_at(2, 0).unwrap();
        assert_eq!(cursor.message_idx, 0);
        assert_eq!(cursor.block_idx, 0);
        assert_eq!(cursor.byte_offset, 2);
    }

    #[test]
    fn test_cursor_at_miss() {
        let map = LayoutMap::new();
        assert!(map.cursor_at(0, 0).is_none());
    }

    #[test]
    fn cursor_at_subtracts_prefix_columns() {
        let mut map = LayoutMap::new();
        map.push(region("hello", 0, 3, Rect::new(0, 0, 20, 1)));

        // Column 3 is the first text column.
        assert_eq!(map.cursor_at(3, 0).unwrap().byte_offset, 0);
        assert_eq!(map.cursor_at(5, 0).unwrap().byte_offset, 2);
        // Inside the prefix clamps to the line start.
        assert_eq!(map.cursor_at(1, 0).unwrap().byte_offset, 0);
    }

    #[test]
    fn cursor_at_handles_wide_and_multibyte_chars() {
        // "😀😃a" — 😀/😃 are 4 bytes, 2 columns each.
        let mut map = LayoutMap::new();
        map.push(region("😀😃a", 0, 0, Rect::new(0, 0, 20, 1)));

        // The leading column resolves to the glyph start; the trailing column
        // resolves inside the glyph so inclusive selection can cover exactly
        // this glyph without spilling into the next one.
        assert_eq!(map.cursor_at(0, 0).unwrap().byte_offset, 0);
        assert_eq!(map.cursor_at(1, 0).unwrap().byte_offset, 1);
        // 😃 starts at byte 4 (columns 2-3).
        assert_eq!(map.cursor_at(2, 0).unwrap().byte_offset, 4);
        assert_eq!(map.cursor_at(3, 0).unwrap().byte_offset, 5);
        // 'a' at byte 8, column 4.
        assert_eq!(map.cursor_at(4, 0).unwrap().byte_offset, 8);
        // Past the end clamps to end_byte — always a char boundary.
        assert_eq!(map.cursor_at(15, 0).unwrap().byte_offset, 9);
    }

    #[test]
    fn cursor_at_respects_wrapped_line_offsets() {
        // Second wrapped line of a block starting at byte 10.
        let mut map = LayoutMap::new();
        map.push(region("world", 10, 3, Rect::new(0, 4, 20, 1)));

        assert_eq!(map.cursor_at(4, 4).unwrap().byte_offset, 11);
    }

    #[test]
    fn interactive_targets_are_visible_ordered_and_deduplicated() {
        let mut map = LayoutMap::new();
        map.push(BlockRegion {
            message_idx: 2,
            block_idx: TOOL_STEP_BLOCK_IDX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect: Rect::new(0, 5, 10, 1),
            hidden_ranges: Vec::new(),
        });
        map.push(BlockRegion {
            message_idx: 2,
            block_idx: TOOL_STEP_BLOCK_IDX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect: Rect::new(0, 6, 10, 1),
            hidden_ranges: Vec::new(),
        });
        map.push(BlockRegion {
            message_idx: 3,
            block_idx: REASONING_BLOCK_IDX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect: Rect::new(0, 7, 10, 1),
            hidden_ranges: Vec::new(),
        });

        assert_eq!(
            map.interactive_targets(),
            vec![
                InteractiveTarget::tool_step(2),
                InteractiveTarget::reasoning(3)
            ]
        );
    }
}
