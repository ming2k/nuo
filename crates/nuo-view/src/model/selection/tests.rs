use super::*;
use crate::model::document::{Block, Inline, TranscriptMessage};
use nuo_protocol::Role;

#[test]
fn test_block_selection() {
    let msg = TranscriptMessage::new(Role::Assistant, "Hello world");
    let messages = vec![msg];
    let sel = SelectionState::Block {
        message_idx: 0,
        block_idx: 0,
    };
    assert_eq!(
        get_selected_text(&sel, &messages, &|_, _| None, None),
        Some("Hello world".to_string())
    );
}

#[test]
fn active_selection_excludes_collapsed_range() {
    let cursor = SemanticCursor::new(0, 0, 0);
    assert!(!SelectionState::None.is_active());
    assert!(!SelectionState::start_range(cursor).is_active());
    assert_eq!(
        SelectionState::start_range(cursor).active_normalized_range(),
        None
    );
    assert!(
        SelectionState::Range {
            anchor: cursor,
            head: SemanticCursor::new(0, 0, 1),
        }
        .is_active()
    );
    assert!(
        SelectionState::Block {
            message_idx: 0,
            block_idx: 0,
        }
        .is_active()
    );
}

#[test]
fn range_drag_lifecycle_is_collapsed_until_head_moves() {
    let anchor = SemanticCursor::new(0, 0, 0);
    let head = SemanticCursor::new(0, 0, 1);
    let mut drag = SelectionDrag::default();
    let mut selection = SelectionState::None;

    drag.begin_range(&mut selection, anchor);
    assert!(drag.active);
    assert_eq!(selection, SelectionState::start_range(anchor));
    assert!(!selection.is_active());

    drag.update_to_cursor(&mut selection, head);
    assert!(selection.is_active());
    assert_eq!(selection.active_normalized_range(), Some((anchor, head)));

    drag.finish(&mut selection);
    assert!(!drag.active);
    assert_eq!(drag.anchor, Some(anchor));
    assert_eq!(selection.active_normalized_range(), Some((anchor, head)));
}

#[test]
fn collapsed_drag_finish_clears_selection_and_context() {
    let anchor = SemanticCursor::new(0, 0, 0);
    let mut drag = SelectionDrag::default();
    let mut selection = SelectionState::None;

    drag.begin_range(&mut selection, anchor);
    drag.finish(&mut selection);

    assert_eq!(selection, SelectionState::None);
    assert!(!drag.active);
    assert_eq!(drag.anchor, None);
    assert!(drag.cell_info.is_none());
}

#[test]
fn cell_drag_uses_range_lifecycle_and_clamps_updates() {
    let cell = CellDragInfo {
        message_idx: 2,
        block_idx: 3,
        cell_text: "abcdef".to_string(),
        segments: vec![TableCellSegment {
            rendered_range: (8, 18),
            content_range: (10, 16),
            source_range: (0, 6),
        }],
    };
    let mut drag = SelectionDrag::default();
    let mut selection = SelectionState::None;

    drag.begin_cell(&mut selection, SemanticCursor::new(2, 3, 8), cell);
    let anchor = SemanticCursor::new(2, 3, 10);
    assert_eq!(selection, SelectionState::start_range(anchor));
    assert!(!selection.is_active());

    drag.update_to_cursor(&mut selection, SemanticCursor::new(9, 9, 99));
    let head = SemanticCursor::new(2, 3, 16);
    assert_eq!(selection.active_normalized_range(), Some((anchor, head)));
    assert_eq!(
        drag.cell_info.as_ref().unwrap().extract_range_text(10, 16),
        "abcdef"
    );
}

#[test]
fn expanded_tool_step_copy_uses_semantic_detail() {
    let mut message = TranscriptMessage::tool_step("call_1", "read", r#"{"path":"README.md"}"#);
    message.finish_tool_step(
        "call_1",
        "file contents",
        nuo_protocol::ToolOutput::text("file contents"),
        42,
    );
    message.set_tool_step_expanded(true);
    // Block 0 = display arguments (key-value text), block 1 = output.
    let arg_index = message
        .blocks
        .iter()
        .position(
            |block| matches!(block, Block::Text(inline) if inline.content.contains("README.md")),
        )
        .unwrap();

    let copied = get_selected_text(
        &SelectionState::Block {
            message_idx: 0,
            block_idx: arg_index,
        },
        &[message],
        &|_, _| None,
        None,
    )
    .unwrap();

    assert!(copied.contains("README.md"));
}

#[test]
fn test_chinese_bold_middle_to_end() {
    let text = "**粗体段落中间复制到最后**";
    let msg = TranscriptMessage::new(Role::Assistant, text);
    let messages = vec![msg];
    // start at "中" (byte 14)
    for head_byte in 15..=40 {
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 14),
            head: SemanticCursor::new(0, 0, head_byte),
        };
        if let Some(res) = get_selected_text(&sel, &messages, &|_, _| None, None) {
            println!("head {head_byte}: {res:?}");
            assert!(!res.ends_with("**"), "failed at head {head_byte}: {res}");
        }
    }
}

#[test]
fn test_range_selection_within_block() {
    let msg = TranscriptMessage::new(Role::Assistant, "Hello world");
    let messages = vec![msg];
    // Head points at the last char; inclusive semantics select it.
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 6),
        head: SemanticCursor::new(0, 0, 10),
    };
    assert_eq!(
        get_selected_text(&sel, &messages, &|_, _| None, None),
        Some("world".to_string())
    );
}

#[test]
fn test_range_selection_strips_markdown_formatting_delimiters() {
    // "This is **bold text** and `code`."
    // Bold: "bold text" is at bytes 10..19, delimiters are at 8..10 and 19..21.
    // Selecting from visual start of 'bold' (byte 10) to visual end (byte 20, including trailing **)
    // must yield clean "bold text" without the trailing '**' or leading '**'.
    let msg = TranscriptMessage::new(Role::Assistant, "This is **bold text** and `code`.");
    let messages = vec![msg];

    // 1. Selecting the bold content exactly (as cursor_at resolves screen coordinates):
    let sel_bold = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 10), // start of 'b'
        head: SemanticCursor::new(0, 0, 20),   // lands on trailing '*'
    };
    assert_eq!(
        get_selected_text(&sel_bold, &messages, &|_, _| None, None),
        Some("bold text".to_string())
    );

    // 2. Selecting a substring within bold ("bold"):
    let sel_sub = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 10), // start of 'b'
        head: SemanticCursor::new(0, 0, 13),   // end of 'd'
    };
    assert_eq!(
        get_selected_text(&sel_sub, &messages, &|_, _| None, None),
        Some("bold".to_string())
    );

    // 3. Selecting the inline code span:
    // "This is **bold text** and `code`."
    // `code` is at 26..32. Content is 27..31.
    let sel_code = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 27), // 'c'
        head: SemanticCursor::new(0, 0, 31),   // trailing '`'
    };
    assert_eq!(
        get_selected_text(&sel_code, &messages, &|_, _| None, None),
        Some("code".to_string())
    );
}

#[test]
fn test_range_selection_strips_triple_asterisk_formatting_delimiters() {
    let msg = TranscriptMessage::new(Role::Assistant, "Here is ***bold and italic*** note.");
    let messages = vec![msg];

    // Triple asterisks: delimiters at 8..11 and 26..29.
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 11), // 'b'
        head: SemanticCursor::new(0, 0, 28),   // trailing '*'
    };
    assert_eq!(
        get_selected_text(&sel, &messages, &|_, _| None, None),
        Some("bold and italic".to_string())
    );
}

#[test]
fn test_range_cross_blocks() {
    let mut msg = TranscriptMessage::new(Role::Assistant, "");
    msg.blocks = vec![
        Block::Text(Inline::plain("First")),
        Block::Text(Inline::plain("Second")),
    ];
    let messages = vec![msg];
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 2),
        head: SemanticCursor::new(0, 1, 3),
    };
    // Head at byte 3 ('o') is included.
    assert_eq!(
        get_selected_text(&sel, &messages, &|_, _| None, None),
        Some("rst\nSeco".to_string())
    );
}

#[test]
fn multibyte_selection_never_panics_and_includes_head_char() {
    let msg = TranscriptMessage::new(Role::Assistant, "😀😃😄😁");
    let messages = vec![msg];

    // Head in the middle of 😄 (byte 10 is not a boundary) — must not panic.
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 1),
        head: SemanticCursor::new(0, 0, 10),
    };
    assert_eq!(
        get_selected_text(&sel, &messages, &|_, _| None, None),
        Some("😀😃😄".to_string())
    );
}

#[test]
fn selection_head_past_text_end_is_clamped() {
    let msg = TranscriptMessage::new(Role::Assistant, "abc😀");
    let messages = vec![msg];
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 0),
        head: SemanticCursor::new(0, 0, 999),
    };
    assert_eq!(
        get_selected_text(&sel, &messages, &|_, _| None, None),
        Some("abc😀".to_string())
    );
}

#[test]
fn whole_block_copy_matches_range_copy_and_strips_markup() {
    // Regression: whole-block (middle-click) copy used to resolve through
    // `block.raw_text()`, leaking `**bold**`, `` `code` `` and
    // `[label](url)` into the clipboard while a drag over the same block
    // returned clean text. Both paths must now agree.
    let msg = TranscriptMessage::new(
        Role::Assistant,
        "Text with **bold** and `code` and a [link](https://example.com).",
    );
    let messages = vec![msg];
    let clean = "Text with bold and code and a link.";
    let whole = get_selected_text(
        &SelectionState::Block {
            message_idx: 0,
            block_idx: 0,
        },
        &messages,
        &|_, _| None,
        None,
    );
    let range = get_selected_text(
        &SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 0),
            head: SemanticCursor::new(0, 0, usize::MAX),
        },
        &messages,
        &|_, _| None,
        None,
    );
    assert_eq!(whole.as_deref(), Some(clean));
    assert_eq!(range.as_deref(), Some(clean));
}

#[test]
fn cross_message_copy_extracts_clean_text_from_every_message() {
    // Regression: interior messages of a multi-message selection were
    // copied as `msg.raw`, splicing the original markdown source into the
    // middle of an otherwise clean selection.
    let msgs = vec![
        TranscriptMessage::new(Role::Assistant, "first **bold**"),
        TranscriptMessage::new(Role::Assistant, "> middle **quote**"),
        TranscriptMessage::new(Role::Assistant, "# last `code`"),
    ];
    let copied = get_selected_text(
        &SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 0),
            head: SemanticCursor::new(2, 0, usize::MAX),
        },
        &msgs,
        &|_, _| None,
        None,
    );
    assert_eq!(
        copied.as_deref(),
        Some("first bold\nmiddle quote\nlast code")
    );
}

#[test]
fn paragraph_rhythm_copies_as_single_blank_line() {
    // A `Break` between two paragraphs is one blank line ("\n\n"), not the
    // block's own "\n" stacked on the separator (which produced "\n\n\n").
    let msg = TranscriptMessage::new(Role::Assistant, "one\n\ntwo");
    let messages = vec![msg];
    let copied = get_selected_text(
        &SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 0),
            head: SemanticCursor::new(0, messages[0].blocks.len() - 1, usize::MAX),
        },
        &messages,
        &|_, _| None,
        None,
    );
    assert_eq!(copied.as_deref(), Some("one\n\ntwo"));
}

#[test]
fn table_block_copy_strips_borders() {
    let grid = "┌─────┬──────┐\n│ a   │ b    │\n├─────┼──────┤\n│ c   │ d    │\n└─────┴──────┘";
    let grid_fn = |mi: usize, bi: usize| {
        assert_eq!((mi, bi), (0, 0));
        Some(grid)
    };
    let mut msg = TranscriptMessage::new(Role::Assistant, "");
    msg.blocks = vec![Block::Table {
        headers: vec!["a".to_string(), "b".to_string()],
        rows: vec![vec!["c".to_string(), "d".to_string()]],
        aligns: vec![crate::model::document::TableAlignment::None; 2],
        rendered: String::new(),
    }];
    let messages = vec![msg];
    let sel = SelectionState::Block {
        message_idx: 0,
        block_idx: 0,
    };
    let copied = get_selected_text(&sel, &messages, &grid_fn, None).unwrap();
    assert_eq!(copied, "a b\nc d");
}

#[test]
fn table_range_copy_strips_borders() {
    // Whole first data line selected via byte range over the grid.
    let grid = "┌─────┬──────┐\n│ hello │ world │\n└─────┴──────┘";
    let grid_fn = |_: usize, _: usize| Some(grid);
    let mut msg = TranscriptMessage::new(Role::Assistant, "");
    msg.blocks = vec![Block::Table {
        headers: vec!["hello".to_string(), "world".to_string()],
        rows: vec![],
        aligns: vec![crate::model::document::TableAlignment::None; 2],
        rendered: String::new(),
    }];
    let messages = vec![msg];
    // The data line starts after the first '\n' (byte 13) and runs to the
    // next '\n'. Selecting [13, end_of_line) grabs the whole data row.
    let data_start = grid.find('\n').unwrap() + 1;
    let data_end = grid[data_start..].find('\n').unwrap() + data_start;
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, data_start),
        head: SemanticCursor::new(0, 0, data_end),
    };
    let copied = get_selected_text(&sel, &messages, &grid_fn, None).unwrap();
    assert_eq!(copied, "hello world");
}

#[test]
fn table_cell_copy_returns_cell_text() {
    // header row 0: "name" (cell 0), "value" (cell 1)
    // body  row 1: "a"    (cell 2), "b"     (cell 3)
    let mut msg = TranscriptMessage::new(Role::Assistant, "");
    msg.blocks = vec![Block::Table {
        headers: vec!["name".to_string(), "value".to_string()],
        rows: vec![vec!["a".to_string(), "b".to_string()]],
        aligns: vec![crate::model::document::TableAlignment::None; 2],
        rendered: String::new(),
    }];
    let messages = vec![msg];

    let copy = |cell_idx| {
        get_selected_text(
            &SelectionState::TableCell {
                message_idx: 0,
                block_idx: 0,
                cell_idx,
            },
            &messages,
            &|_, _| None,
            None,
        )
    };
    assert_eq!(copy(0), Some("name".to_string())); // header col 0
    assert_eq!(copy(1), Some("value".to_string())); // header col 1
    assert_eq!(copy(2), Some("a".to_string())); // body row 0, col 0
    assert_eq!(copy(3), Some("b".to_string())); // body row 0, col 1
}

#[test]
fn table_cell_copy_includes_wrapped_text() {
    // A cell whose source text is long (would wrap when rendered) is copied
    // in full — the selection is logical-cell-granular, not line-granular.
    let mut msg = TranscriptMessage::new(Role::Assistant, "");
    let long = "the quick brown fox jumps over the lazy dog";
    msg.blocks = vec![Block::Table {
        headers: vec!["desc".to_string()],
        rows: vec![vec![long.to_string()]],
        aligns: vec![crate::model::document::TableAlignment::None],
        rendered: String::new(),
    }];
    let messages = vec![msg];
    // cell 0 = header, cell 1 = body (the long text).
    let copied = get_selected_text(
        &SelectionState::TableCell {
            message_idx: 0,
            block_idx: 0,
            cell_idx: 1,
        },
        &messages,
        &|_, _| None,
        None,
    )
    .unwrap();
    assert_eq!(copied, long);
}

#[test]
fn cell_drag_copy_is_grapheme_safe_and_inclusive() {
    let cell = CellDragInfo {
        message_idx: 0,
        block_idx: 0,
        cell_text: "中文测".to_string(),
        segments: vec![TableCellSegment {
            rendered_range: (2, 11),
            content_range: (2, 11),
            source_range: (0, 9),
        }],
    };

    // Offsets are grid-line byte offsets, so subtracting `lo` lands inside
    // the first and third CJK grapheme. Extraction must not panic, and the
    // inclusive head includes the grapheme under the drag head.
    assert_eq!(cell.extract_range_text(3, 8), "中文测");
}

#[test]
fn cell_drag_copy_maps_rendered_padding_to_source_text() {
    let cell = CellDragInfo {
        message_idx: 0,
        block_idx: 0,
        cell_text: "abcdef".to_string(),
        segments: vec![TableCellSegment {
            rendered_range: (20, 30),
            content_range: (22, 28),
            source_range: (0, 6),
        }],
    };

    assert_eq!(
        cell.clamp_cursor(SemanticCursor::new(9, 9, 20)),
        SemanticCursor::new(0, 0, 22)
    );
    assert_eq!(
        cell.clamp_cursor(SemanticCursor::new(9, 9, 29)),
        SemanticCursor::new(0, 0, 28)
    );
    assert_eq!(cell.extract_range_text(22, 24), "abc");
}

#[test]
fn cell_drag_clamps_between_wrapped_segments_to_nearest_boundary() {
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
        cell.clamp_cursor(SemanticCursor::new(9, 9, 15)),
        SemanticCursor::new(0, 0, 13)
    );
    assert_eq!(
        cell.clamp_cursor(SemanticCursor::new(9, 9, 39)),
        SemanticCursor::new(0, 0, 40)
    );
}
