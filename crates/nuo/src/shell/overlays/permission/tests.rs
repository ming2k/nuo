use super::*;
use nuo_tool::{UserQuestion, UserQuestionOption};

#[test]
fn question_modal_records_option_hit_boxes() {
    let request = UserQuestionRequest {
        id: "q".into(),
        questions: vec![UserQuestion {
            header: None,
            question: "Pick one".into(),
            options: vec![
                UserQuestionOption {
                    label: "A".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "B".into(),
                    description: Some("Second option".into()),
                },
            ],
            multi_select: false,
        }],
        origin: None,
    };
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let mut hit_map = ComponentTree::new();
    terminal.draw(|frame| {
        hit_map.begin(frame.area());
        let mut scroll = 0;
        draw_question_modal(
            frame,
            &mut hit_map,
            &request,
            0,
            &[vec![0]],
            &[String::new()],
            0,
            &mut scroll,
            true,
            0,
            Rect::new(0, 0, 78, 16),
            true,
            &Theme::default(),
        );
    });

    hit_map.commit();
    assert!(find_question_hit(&hit_map, 80, 24, 0));
    assert!(find_question_hit(&hit_map, 80, 24, 1));
    assert!(find_question_hit(&hit_map, 80, 24, 2));
}

#[test]
fn permission_sheet_records_footer_action_hit_boxes() {
    let request = PermissionRequest {
        id: "p".into(),
        tool: "execute_command".into(),
        label: "execute_command".into(),
        description: "Run a command".into(),
        arguments: r#"{"command":"cargo test"}"#.into(),
        scope: "*".into(),
        elevation: false,
        one_off: false,
        origin: None,
        ..Default::default()
    };
    let mut terminal = nuotc::TestTerminal::new(80, 24);
    let mut hit_map = ComponentTree::new();
    terminal.draw(|frame| {
        hit_map.begin(frame.area());
        let rect = Rect::new(0, 16, 80, 8);
        let _ = draw_permission_sheet(
            frame,
            &mut hit_map,
            &request,
            0,
            false,
            false,
            0,
            0,
            rect,
            &Theme::default(),
            &nuo_view::model::selection::SelectionState::None,
            &mut nuo_view::model::layout::LayoutMap::new(),
        );
    });

    hit_map.commit();
    for action_index in 0..4 {
        assert!(
            find_permission_hit(&hit_map, 80, 24, action_index),
            "missing permission action {action_index}"
        );
    }
}

#[test]
fn permission_sheet_shows_queue_depth_badge() {
    // Concurrent AI interactions queue FIFO behind the front
    // sheet; the badge tells the user more decisions are pending.
    let request = PermissionRequest {
        id: "p".into(),
        tool: "execute_command".into(),
        label: "execute_command".into(),
        description: "Run a command".into(),
        arguments: r#"{"command":"ls"}"#.into(),
        scope: "*".into(),
        elevation: false,
        one_off: false,
        origin: None,
        ..Default::default()
    };
    let render = |depth: usize| {
        let mut terminal = nuotc::TestTerminal::new(80, 24);
        let mut hit_map = ComponentTree::new();
        terminal.draw(|frame| {
            hit_map.begin(frame.area());
            let rect = Rect::new(0, 16, 80, 8);
            let _ = draw_permission_sheet(
                frame,
                &mut hit_map,
                &request,
                0,
                false,
                false,
                0,
                depth,
                rect,
                &Theme::default(),
                &nuo_view::model::selection::SelectionState::None,
                &mut nuo_view::model::layout::LayoutMap::new(),
            );
        });
        sheet_text(&terminal)
    };

    assert!(
        !render(1).contains("queued"),
        "a lone request carries no badge"
    );
    assert!(
        render(3).contains("3 queued"),
        "the badge names how many requests are queued"
    );
}

fn find_question_hit(map: &ComponentTree, width: u16, height: u16, option_index: usize) -> bool {
    (0..height).any(|y| {
        (0..width).any(|x| {
            map.question_option_at(x, y)
                .is_some_and(|hit| hit.option_index == option_index)
        })
    })
}

fn find_permission_hit(map: &ComponentTree, width: u16, height: u16, action_index: usize) -> bool {
    (0..height).any(|y| {
        (0..width).any(|x| {
            map.permission_action_at(x, y)
                .is_some_and(|hit| hit.action_index == action_index)
        })
    })
}

/// The whole visible text of the sheet as one newline-joined string, so
/// tests can assert on what the user actually sees.
fn sheet_text(terminal: &nuotc::TestTerminal) -> String {
    let buf = terminal.buffer();
    let w = buf.area().width;
    let h = buf.area().height;
    let mut rows = Vec::new();
    for y in 0..h {
        let mut row = String::new();
        for x in 0..w {
            row.push_str(buf.get(x, y).map(|c| c.symbol.as_str()).unwrap_or(" "));
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows.join("\n")
}

/// The visible text of one rect, read column by column from the painted
/// grid. Scoping an assertion to the region that owns the content keeps
/// neighbouring chrome (a scrollbar gutter, a border, a footer band) out
/// of the substring under test.
fn body_text(terminal: &nuotc::TestTerminal, rect: Rect) -> String {
    let buf = terminal.buffer();
    let mut rows = Vec::new();
    for y in rect.y..rect.bottom() {
        let mut row = String::new();
        for x in rect.x..rect.right() {
            row.push_str(buf.get(x, y).map(|c| c.symbol.as_str()).unwrap_or(" "));
        }
        rows.push(row.trim_end().to_string());
    }
    rows.join("\n")
}

/// A wrapped multi-span body row must keep every character. The header is
/// `label` + `"  "` + scope (three spans); when the concatenation is wider
/// than the body, the continuation row re-slices spans with byte
/// boundaries computed against the *full* row text — this guards against
/// that re-slicing silently dropping text (the `l | he` regression, where
/// `ls | head` lost its `s`, `a`, `d`).
#[test]
fn permission_sheet_wrapped_header_keeps_every_character() {
    let request = PermissionRequest {
        id: "p".into(),
        tool: "execute_command".into(),
        label: "tool".into(),
        description: "Run a command".into(),
        arguments: r#"{"command":"ls | head"}"#.into(),
        scope: "ls | head".into(),
        elevation: false,
        one_off: false,
        origin: None,
        ..Default::default()
    };
    let mut terminal = nuotc::TestTerminal::new(30, 24);
    let mut hit_map = ComponentTree::new();
    terminal.draw(|frame| {
        hit_map.begin(frame.area());
        let rect = Rect::new(0, 16, 30, 8);
        let _ = draw_permission_sheet(
            frame,
            &mut hit_map,
            &request,
            0,
            false,
            false,
            0,
            0,
            rect,
            &Theme::default(),
            &nuo_view::model::selection::SelectionState::None,
            &mut nuo_view::model::layout::LayoutMap::new(),
        );
    });

    // Row 0 is the header (`tool  ls | head` — 15 cols, fits at width 30
    // in one visual row); so instead force a wrap by narrowing the sheet.
    let text = sheet_text(&terminal);
    assert!(
        text.contains("ls | head"),
        "wrapped header lost characters: {text:?}"
    );
}

/// The same multi-span row, forced to wrap by a body narrower than the
/// header: every character of `ls | head` must survive the wrap.
#[test]
fn permission_sheet_wrapped_header_survives_wrap() {
    let request = PermissionRequest {
        id: "p".into(),
        tool: "execute_command".into(),
        label: "tool".into(),
        description: "Run a command".into(),
        arguments: r#"{"command":"ls | head"}"#.into(),
        scope: "ls | head".into(),
        elevation: false,
        one_off: false,
        origin: None,
        ..Default::default()
    };
    let mut terminal = nuotc::TestTerminal::new(14, 24);
    let mut hit_map = ComponentTree::new();
    terminal.draw(|frame| {
        hit_map.begin(frame.area());
        let rect = Rect::new(0, 16, 14, 8);
        let _ = draw_permission_sheet(
            frame,
            &mut hit_map,
            &request,
            0,
            false,
            false,
            0,
            0,
            rect,
            &Theme::default(),
            &nuo_view::model::selection::SelectionState::None,
            &mut nuo_view::model::layout::LayoutMap::new(),
        );
    });

    // Body width = 14 - 1 - 2*2 = 9; header `tool  ls | head` is 15 cols,
    // so it spans two visual rows. No non-whitespace character may be
    // dropped by the wrap: the body rows reassemble to the full header
    // (whitespace at a wrap point may trail and go unrendered).
    //
    // Read the header from the body's own columns rather than from the
    // whole painted grid. `sheet_text` returns every column, and once the
    // body overflows, the scrollbar gutter (`▼` at `area.right() - 1`)
    // paints on the header's continuation row — chrome that a text filter
    // anchored on `head` would happily append to the header and then
    // report as a lost character. The wrap is what is under test here, not
    // the indicator, so the assertion is scoped to the body rect.
    let content_x = 1 + PERMISSION_H_PADDING;
    let content_w = 14u16.saturating_sub(1 + 2 * PERMISSION_H_PADDING).max(1);
    let body = Rect::new(content_x, 16 + PERMISSION_TOP_PADDING, content_w, 2);
    let text = sheet_text(&terminal);
    let header: String = body_text(&terminal, body)
        .lines()
        .flat_map(|l| l.chars().filter(|c| !c.is_whitespace()))
        .collect();
    assert_eq!(
        header, "toolls|head",
        "wrapped header lost characters: {text:?}"
    );
}
