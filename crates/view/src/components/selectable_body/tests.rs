use super::*;
use crate::model::selection::{SelectionDrag, SelectionState};

fn body_rect(w: u16, h: u16) -> Rect {
    Rect::new(0, 0, w, h)
}

/// Every logical row — including one that soft-wraps — registers exactly
/// one region per *visual* row, under the MODAL_DOC sentinel, with byte
/// ranges that tile the row's text.
#[test]
fn registers_one_region_per_visual_row() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(20, 10);
    let rows = vec![SelectableRow::styled(
        "abcdefghij klmnopqrst uvwxyz", // wraps at 20 cols
        nuotc::Style::default(),
    )];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(20, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });
    assert_eq!(map.region_at(0, 0).map(|r| r.block_idx), Some(0));
    // Two wrapped rows, same logical row idx, disjoint byte windows that
    // reassemble to the source text.
    let r1 = map.region_at(0, 0).expect("wrapped row 1");
    let r2 = map.region_at(0, 1).expect("wrapped row 2");
    assert_eq!(r1.block_idx, r2.block_idx);
    assert_eq!(
        format!("{}{}", r1.text, r2.text),
        "abcdefghij klmnopqrst uvwxyz"
    );
    assert!(r1.end_byte <= r2.start_byte || r2.end_byte <= r1.start_byte);
}

/// An empty spacer row still occupies one visual row with an empty region,
/// so layout accounting matches `render_body`'s one-line-per-`Line`.
#[test]
fn empty_row_occupies_one_visual_row() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(20, 10);
    let rows = vec![
        SelectableRow::styled("one", nuotc::Style::default()),
        SelectableRow::empty(),
        SelectableRow::styled("two", nuotc::Style::default()),
    ];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(20, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });
    let middle = map.region_at(0, 1).expect("spacer region");
    assert_eq!(middle.text, "");
    assert_eq!(map.region_at(0, 2).map(|r| r.text.as_str()), Some("two"));
}

/// A selection spanning two logical rows resolves through
/// `extract_text_for_range` to the wrapped text of both, exactly what the
/// copy action runs for MODAL_DOC regions.
#[test]
fn selection_extract_spans_rows() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(40, 10);
    let rows = vec![
        SelectableRow::styled("hello world", nuotc::Style::default()),
        SelectableRow::styled("second row", nuotc::Style::default()),
    ];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    // Drag from (0,0) to end of row 1.
    let sel = {
        terminal.draw(|f| {
            render_selectable_body(
                f,
                body_rect(40, 5),
                &rows,
                &mut scroll,
                None,
                &theme,
                &SelectionState::None,
                &mut map,
            );
        });
        let anchor = map.cursor_at(0, 0).expect("anchor");
        let head = map.cursor_at(6, 1).expect("head");
        SelectionState::Range { anchor, head }
    };
    // Drag from (0,0) to (6,1): column 6 of "second row" is the space
    // after "second", so the inclusive-end extraction stops there.
    let text = map.extract_text_for_range(&sel).expect("extracted");
    assert_eq!(text, "hello world\nsecond ");
}

/// Hit-testing maps a screen column to the byte offset inside the *row's*
/// text, so a click past the end of a short row lands at end-of-row, not
/// in a neighbouring row.
#[test]
fn cursor_at_clamps_to_row_end() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(40, 10);
    let rows = vec![SelectableRow::styled("ab", nuotc::Style::default())];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(40, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });
    let cursor = map.cursor_at(39, 0).expect("cursor past end");
    assert_eq!(cursor.byte_offset, 2);
}

/// Multi-segment rows (label + value, the usage-stats / session-detail
/// shape) flatten to their concatenated text; selection painting splits
/// at segment boundaries without disturbing hit-testing.
#[test]
fn multi_segment_row_round_trips() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(40, 10);
    let rows = vec![SelectableRow::from_segments(vec![
        RowSegment::styled("Total  ", nuotc::Style::default()),
        RowSegment::styled("12345", nuotc::Style::default()),
    ])];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(40, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });
    let region = map.region_at(0, 0).expect("region");
    assert_eq!(region.text, "Total  12345");
    // Click on the digits (column 7) resolves inside the value segment.
    let cursor = map.cursor_at(7, 0).expect("cursor");
    assert_eq!(cursor.byte_offset, 7);
}

/// A multi-segment row that soft-wraps must render every character on the
/// continuation row. Segment byte boundaries are computed against the
/// *full* row text while the painted text is the *wrapped slice*: slicing
/// the slice with full-row offsets skips any range that runs past the
/// slice's end (`hi > text.len()`), which silently dropped mid-row text —
/// e.g. a permission header `bash  ls | head` rendering as `l | he`.
#[test]
fn wrapped_multi_segment_row_keeps_every_segment() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(12, 10);
    // Two segments: label (4 cols) + separator (2) + value (9) = 15 cols,
    // so the row wraps at the 12-col body width.
    let rows = vec![SelectableRow::from_segments(vec![
        RowSegment::styled("bash", nuotc::Style::default()),
        RowSegment::styled("  ", nuotc::Style::default()),
        RowSegment::styled("ls | head", nuotc::Style::default()),
    ])];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(12, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });

    // Grid text: every non-whitespace character of the row survives.
    let buf = terminal.buffer();
    let mut painted = String::new();
    for y in 0..5 {
        for x in 0..12 {
            painted.push_str(buf.get(x, y).map(|c| c.symbol.as_str()).unwrap_or(" "));
        }
    }
    let non_ws = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(
        non_ws(&painted),
        non_ws("bash  ls | head"),
        "wrapped row dropped characters: {painted:?}"
    );

    // Regions: the wrapped rows tile the full document text.
    let r1 = map.region_at(0, 0).expect("wrapped row 1");
    let r2 = map.region_at(0, 1).expect("wrapped row 2");
    assert_eq!(format!("{}{}", r1.text, r2.text), "bash  ls | head");
}

/// A decoration prefix (indent / rail) paints on every visual row but is
/// excluded from the document text: copy yields the content only, and
/// hit-testing skips the prefix columns via `prefix_cols`.
#[test]
fn prefix_excluded_from_copy_and_hit_testing() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(20, 10);
    let rows = vec![
        SelectableRow::styled(
            "indented content that is long enough to wrap once or twice",
            nuotc::Style::default(),
        )
        .with_prefix(RowSegment::styled("  ", nuotc::Style::default())),
    ];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(20, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });
    let region = map.region_at(0, 0).expect("region");
    assert_eq!(region.prefix_cols, 2);
    assert!(!region.text.starts_with(' '), "copy text has no indent");
    // Column 1 is inside the prefix: the cursor resolves to the content
    // start (byte 0), never a negative/hijacked offset.
    let cursor = map.cursor_at(1, 0).expect("cursor in prefix");
    assert_eq!(cursor.byte_offset, 0);
}

/// The full mouse-drag interaction chain the event loop runs for a
/// selectable modal: press (arm via `begin_range`), move
/// (`update_from_point`), release (`finish`), copy
/// (`extract_text_for_range`). A press on the backdrop *outside* every
/// region resolves to no cursor — the signal the dismiss branch keys on.
#[test]
fn drag_chain_from_press_to_copy() {
    use crate::model::selection::SelectionDrag;

    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(40, 10);
    let rows = vec![
        SelectableRow::styled("alpha", nuotc::Style::default()),
        SelectableRow::styled("beta", nuotc::Style::default()),
        SelectableRow::styled("gamma", nuotc::Style::default()),
    ];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(40, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });

    // Press inside the document: resolves and arms.
    let press = map.cursor_at(2, 0).expect("press resolves");
    assert_eq!(press.message_idx, MODAL_DOC_MSG_IDX);
    let mut drag = SelectionDrag::default();
    let mut selection = SelectionState::None;
    drag.begin_range(&mut selection, press);

    // Drag to the third row: head follows the pointer.
    drag.update_from_point(&mut selection, &map, 3, 2);
    drag.finish(&mut selection);
    assert!(selection.is_active(), "drag must leave a live selection");

    let text = map
        .extract_text_for_range(&selection)
        .expect("copy extracts");
    // The head character is included (the transcript's inclusive-end
    // selection rule), so column 3 of "gamma" copies through "gamm".
    assert_eq!(text, "pha\nbeta\ngamm");

    // Press on the backdrop (below the last painted row of this 5-row
    // body): no region there, so the dismiss branch keeps ownership.
    assert!(
        map.cursor_at(10, 4).is_none(),
        "empty panel area must not resolve to a document cursor"
    );
}

#[test]
fn visual_row_count_and_desired_rows_match_wrapped_rendering() {
    let mut grid = nuotc::CellGrid::new(80, 24);
    let mut damage = nuotc::Damage::new(24);
    let frame = nuotc::Frame::new(&mut grid, &mut damage);
    let geometry = ContentModalSpec::PERMISSIONS;

    let rows = vec![
        SelectableRow::styled("Header", nuotc::Style::default()),
        SelectableRow::styled(
            "This is a long line of text that is expected to wrap across multiple visual lines when given a standard content modal width.",
            nuotc::Style::default(),
        ),
        SelectableRow::empty(),
        SelectableRow::styled("Footer", nuotc::Style::default()),
    ];

    let probe = content_modal_probe(&frame, geometry);
    let body_w = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let visual_rows = selectable_body_visual_rows(&rows, body_w);
    assert!(visual_rows >= 4);

    let desired = selectable_body_desired_rows(&frame, geometry, &rows);
    assert_eq!(
        desired,
        visual_rows as u16 + modal_chrome_rows(geometry.modal_spec())
    );
}

#[test]
fn selection_extract_wrapped_single_row_url() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(30, 10);
    let url = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=123456";
    let rows = vec![SelectableRow::styled(url, nuotc::Style::default())];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(30, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });

    // The URL wraps across multiple visual rows with the same logical block_idx = 0.
    // Start selection on the second line (y = 1) and drag across to line 3 (y = 2).
    let start_cursor = map.cursor_at(0, 1).expect("line 1 cursor");

    let mut drag = SelectionDrag::default();
    let mut selection = SelectionState::None;
    drag.begin_range(&mut selection, start_cursor);
    drag.update_from_point(&mut selection, &map, 10, 2);
    drag.finish(&mut selection);

    assert!(selection.is_active());
    let text = map
        .extract_text_for_range(&selection)
        .expect("must extract text across wrapped lines of a single row");
    assert!(!text.is_empty());
    assert!(url.contains(&text));
}

#[test]
fn selectable_body_registers_link_hits() {
    let theme = Theme::default();
    let mut terminal = nuotc::TestTerminal::new(50, 10);
    let url = "https://example.com/oauth";
    let rows = vec![SelectableRow::styled(
        format!("Click {url} to authenticate"),
        nuotc::Style::default(),
    )];
    let mut scroll = 0;
    let mut map = LayoutMap::new();
    terminal.draw(|f| {
        render_selectable_body(
            f,
            body_rect(50, 5),
            &rows,
            &mut scroll,
            None,
            &theme,
            &SelectionState::None,
            &mut map,
        );
    });

    // Link hit should be registered at column 6 (where "https://..." starts)
    let link_hit = map.link_at(8, 0).expect("link hit should be found");
    assert_eq!(link_hit.url, url);
}
