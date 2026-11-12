//! Wrapping and intrinsic input-box growth.

use super::*;

#[test]
fn test_wrap_text() {
    let lines = wrap_text("hello world", 5);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "hello");
    assert_eq!(lines[1].text, "world");
}

#[test]
fn test_wrap_with_newlines() {
    let lines = wrap_text("hi\nthere", 10);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "hi");
    assert_eq!(lines[1].text, "there");
}

#[test]
fn wrap_avoids_cjk_punctuation_at_line_start() {
    let lines = wrap_text("人生需要坚持，才能前进。", 12);
    assert!(lines.len() > 1);
    assert!(lines.iter().skip(1).all(|line| {
        line.text
            .chars()
            .next()
            .is_none_or(|ch| !prohibited_line_start(ch))
    }));
    assert!(lines.iter().all(|line| {
        line.text
            .chars()
            .last()
            .is_none_or(|ch| !prohibited_line_end(ch))
    }));
}

/// The input box must reserve only a single content row for a short input
/// but grow to fit wrapped text when the input is long.
#[test]
fn input_box_grows_with_wrapped_content() {
    let theme = Theme::default();
    let messages: Vec<TranscriptMessage> = Vec::new();

    fn render_with(theme: &Theme, messages: &[TranscriptMessage], input: &str) -> Rect {
        let mut terminal = nuotc::TestTerminal::new(46, 24);
        let mut rect = Rect::default();
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let r = draw_transcript(
                f,
                &mut layout_map,
                TranscriptProps {
                    messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    backoff: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input,
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: Some(crate::shell::keymap::Key::CTRL_Q),
                    },
                    tasks_bar: Default::default(),
                    interrupt_key: None,
                    subagent_bar: None,
                    page_hints: None,
                    tab_bar: None,
                    thread_id: None,
                    thread_context: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_entry: None,
                    focus_plane: crate::shell::render::FocusPlane::Composer,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme,
                    layout: nuo_view::layout::Strategy::default(),
                    height_cache: None,
                },
            );
            rect = r.input_rect;
        });
        rect
    }

    // Short input: one content line + three chrome rows = 4.
    let short = render_with(&theme, &messages, "hi");
    assert_eq!(short.height, 4);

    // Long input wraps across many lines on a 40-wide terminal; the box
    // must grow beyond the single-line baseline.
    let long_input = "word ".repeat(40);
    let tall = render_with(&theme, &messages, &long_input);
    assert!(
        tall.height > 4,
        "wrapped input should grow the box, got height {}",
        tall.height
    );
    // ...but never more than half the terminal.
    assert!(tall.height <= 12);
}
