//! Tinted-panel hint row, overflow indicators, and history badges.

use super::helpers::*;
use super::*;

/// The composer is a tinted panel laid out as four rows for a one-line
/// draft: blank breathing row, the text row (opened by `›`), a blank gap
/// row, and the meta row carrying the Enter sentence. The right side of
/// the meta row carries nothing while every wrapped row is visible — the
/// position readout only exists once the box clips rows (see
/// `composer_overflow_indicates_hidden_lines_and_position`).

#[test]
fn composer_panel_carries_the_hint_row() {
    let mut terminal = draw_frame_composer("hello", true, Default::default());

    let row1 = frame_row_text(&mut terminal, 0);
    assert!(
        row1.trim().is_empty(),
        "row 1 is a blank breathing row: {row1:?}"
    );

    let row2 = frame_row_text(&mut terminal, 1);
    assert!(row2.starts_with("› hello"), "prompt + text: {row2:?}");

    let row3 = frame_row_text(&mut terminal, 2);
    assert!(row3.trim().is_empty(), "row 3 is a blank gap row: {row3:?}");

    let row4 = frame_row_text(&mut terminal, 3);
    assert!(
        row4.contains("Enter send"),
        "hint row carries the Enter sentence: {row4:?}"
    );
    assert!(
        !row4.contains("lines") && !row4.contains("chars"),
        "a fully visible draft carries no right-side readout: {row4:?}"
    );
}

/// The hint sentence names the delivery group the buffer will land in, and
/// mid-round it also carries the Tab toggle.
///
/// The panel here is 60 columns — a `Full`-density band — but the row's own
/// widths (`Tab follow-up mode   Shift-Tab transcript` left, `Enter send steer`
/// right) overrun it, so the row steps down one rung instead of collapsing to
/// `F1 more`: compact drops the toggle verb and keeps the crossing. Naming the
/// crossing matters more than naming the toggle, which `Tab` still performs
/// whether or not it is advertised (ADR-0054 §2, ADR-0145 §6).
#[test]
fn composer_hint_sentence_names_the_delivery_group() {
    use crate::shell::composer_hints::{ComposeTarget, ComposerHints};

    let hints = ComposerHints {
        compose_target: ComposeTarget::Running(crate::shell::app::ComposerSendMode::Steer),
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("wait", true, hints);
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(row4.contains("send steer"), "steer verb: {row4:?}");
    assert!(
        row4.contains("Shift-Tab transcript"),
        "the crossing survives width pressure: {row4:?}"
    );
    assert!(
        !row4.contains("F1 more"),
        "a denser rung beats the F1 collapse: {row4:?}"
    );

    let hints_followup = ComposerHints {
        compose_target: ComposeTarget::Running(crate::shell::app::ComposerSendMode::FollowUp),
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("wait", true, hints_followup);
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(row4.contains("queue follow-up"), "follow-up verb: {row4:?}");
    assert!(
        row4.contains("Shift-Tab transcript"),
        "the crossing survives width pressure: {row4:?}"
    );
}

/// At a width where even the compact row cannot fit, the row collapses to the
/// Floor-1 `F1 more` affordance rather than clipping — discovery is never
/// destroyed, only folded behind `F1` (ADR-0054 `[INV-AFFORDANCE-02]`).
#[test]
fn composer_hint_row_collapses_to_f1_when_even_compact_overflows() {
    use crate::shell::composer_hints::{ComposeTarget, ComposerHints};

    let hints = ComposerHints {
        compose_target: ComposeTarget::Running(crate::shell::app::ComposerSendMode::Steer),
        ..Default::default()
    };
    let mut terminal = nuotc::TestTerminal::new(30, 6);
    let theme = Theme::default();
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 30, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: "wait",
                byte_cursor: 4,
            },
            true,
            false,
            false,
            0,
            0,
            hints,
        );
    });
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(
        row4.contains("F1"),
        "the overflow affordance must appear, not a clipped row: {row4:?}"
    );
    assert!(
        row4.contains("send steer"),
        "the submit verb outranks navigation: {row4:?}"
    );
}

/// Once the draft outgrows the box, the three-part overflow affordance
/// lights up: the breathing row above the text carries `↑ N lines`, the gap
/// row below carries `↓ N lines`, and the meta row's right side swaps in a
/// `first–last/total lines` position readout. The box height never changes
/// crossing the threshold — the indicators reuse existing chrome rows.
#[test]
fn composer_overflow_indicates_hidden_lines_and_position() {
    let theme = Theme::default();
    // 12 words ≈ 65 cols: three wrapped rows on a 60-col panel whose height
    // leaves room for exactly one visible text row.
    let input = "one two three four five six seven eight nine ten eleven twelve ";
    let draw = |byte_cursor: usize, scroll: &mut usize| {
        let mut terminal = nuotc::TestTerminal::new(60, 6);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, 60, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: scroll,
                    selection: &SelectionState::None,
                },
                ComposerText { input, byte_cursor },
                true,
                false,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        terminal
    };

    // Caret at the end: the caret-follow clamp scrolls the last row into
    // view, hiding the rows above → the ↑ indicator + position readout.
    let mut scroll = 0usize;
    let mut terminal = draw(input.len(), &mut scroll);
    assert!(scroll > 0, "caret-follow scroll hides rows above: {scroll}");
    let top = frame_row_text(&mut terminal, 0);
    assert!(
        top.contains('↑') && top.trim_end().ends_with("lines"),
        "breathing row carries the ↑ indicator: {top:?}"
    );
    let gap = frame_row_text(&mut terminal, 2);
    assert!(
        gap.trim().is_empty(),
        "nothing hides below when the caret rests on the last row: {gap:?}"
    );
    let meta = frame_row_text(&mut terminal, 3);
    assert!(
        meta.contains(&format!("–{}/{} lines", scroll + 1, scroll + 1)),
        "position readout names the visible span: {meta:?}"
    );

    // Caret at the start instead: the viewport stays at the top, hiding
    // rows below → the ↓ indicator, and the readout names the first row.
    let mut scroll_top = 0usize;
    let mut terminal = draw(0, &mut scroll_top);
    assert_eq!(
        scroll_top, 0,
        "viewport rests at the top with the caret there"
    );
    let top = frame_row_text(&mut terminal, 0);
    assert!(
        top.trim().is_empty(),
        "no ↑ while nothing hides above: {top:?}"
    );
    let gap = frame_row_text(&mut terminal, 2);
    assert!(
        gap.contains('↓') && gap.trim_end().ends_with("lines"),
        "gap row carries the ↓ indicator: {gap:?}"
    );
    let meta = frame_row_text(&mut terminal, 3);
    assert!(
        meta.contains("1–1/"),
        "readout names the first visible row: {meta:?}"
    );
}

/// While blurred the panel keeps its tint but sheds the hint sentence — the
/// meta text never competes with a step-focused transcript.
#[test]
fn composer_panel_blurred_sheds_the_hint_row() {
    let mut terminal = draw_frame_composer("hello", false, Default::default());
    let row4 = frame_row_text(&mut terminal, 3);
    assert!(!row4.contains("Enter"), "blurred row sheds the keys");
    assert!(
        !row4.contains("lines"),
        "blurred row sheds the position readout"
    );
    let buffer = terminal.buffer();
    assert_eq!(
        buffer.get(0, 3).unwrap().style.bg,
        Theme::default().input_surface_inactive(),
        "blurred panel keeps its recessed tint"
    );
}

/// Narrow panels degrade instead of overflowing: the counter goes first,
/// then the sentence's optional clauses, and every row stays exactly the
/// panel width.
#[test]
fn composer_panel_degrades_gracefully_on_narrow_widths() {
    let theme = Theme::default();
    for width in [12u16, 9, 7] {
        let mut terminal = nuotc::TestTerminal::new(width, 6);
        terminal.draw(|f| {
            draw_composer(
                ComposerProps {
                    frame: f,
                    input_rect: Rect::new(0, 0, width, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input: "hi",
                    byte_cursor: 2,
                },
                true,
                false,
                false,
                0,
                0,
                crate::shell::composer_hints::ComposerHints::default(),
            );
        });
        for y in 0..4u16 {
            let row = frame_row_text(&mut terminal, y);
            assert_eq!(
                row.chars().count(),
                width as usize,
                "w={width} y={y}: rows never exceed the panel width"
            );
        }
        // The recessed/active tint still covers the full box on every row.
        let buffer = terminal.buffer();
        for y in 0..4u16 {
            assert_eq!(
                buffer.get(width - 1, y).unwrap().style.bg,
                theme.input_surface(),
                "w={width} y={y}: panel tint reaches the last column"
            );
        }
    }
}

#[test]
fn composer_top_row_renders_history_mode_badges() {
    use crate::shell::composer_hints::{ComposeTarget, ComposerHints};

    // HistorySearch mode badge on row 0 right side
    let search_hints = ComposerHints {
        compose_target: ComposeTarget::HistorySearch,
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("query", true, search_hints);
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[history search | draft saved]"),
        "top row has history search badge: {row0:?}"
    );
}

/// The inline ↑/↓ recall badge: when the recall pointer sits on a
/// history row the top breathing row declares it — 1-based position over the
/// session slice, the `draft saved` reassurance while the stash is intact,
/// the `edited` clause once the buffer forks from the loaded row. Draft mode
/// renders the plain breathing row (no badge, zero indication tax).
#[test]
fn composer_top_row_declares_the_history_recall_pointer() {
    use crate::shell::composer_hints::{ComposeTarget, ComposerHints};

    let mut hints = ComposerHints {
        compose_target: ComposeTarget::HistoryRecall,
        history_recall: Some((1, 17, false)),
        recall_draft_saved: true,
        ..Default::default()
    };
    let mut terminal = draw_frame_composer("recalled prompt", true, hints);
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[history 1/17 | draft saved]"),
        "full-width badge names position, total, and the draft reassurance: {row0:?}"
    );

    // The edited clause replaces the reassurance once the buffer forks.
    hints.history_recall = Some((3, 17, true));
    let mut terminal = draw_frame_composer("recalled prompt EDITED", true, hints);
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[history 3/17 | edited]"),
        "an edited buffer is declared on the badge: {row0:?}"
    );

    // Draft mode: no badge at all.
    let mut terminal = draw_frame_composer("live draft", true, ComposerHints::default());
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.trim().is_empty(),
        "draft mode renders the plain breathing row, no badge: {row0:?}"
    );
}

/// The badge's width ladder: the full clause set on comfortable panels, the
/// bare `position/total` pointer when space runs out — the pointer is the
/// last element to go because it is the badge's entire point.
#[test]
fn composer_recall_badge_degrades_with_width() {
    use crate::shell::composer_hints::{ComposeTarget, ComposerHints};

    let hints = ComposerHints {
        compose_target: ComposeTarget::HistoryRecall,
        history_recall: Some((2, 9, false)),
        recall_draft_saved: true,
        ..Default::default()
    };

    // Narrow panel: pointer only.
    let mut terminal = nuotc::TestTerminal::new(20, 6);
    let theme = Theme::default();
    terminal.draw(|f| {
        draw_composer(
            ComposerProps {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: "row",
                byte_cursor: 3,
            },
            true,
            false,
            false,
            0,
            0,
            hints,
        );
    });
    let row0 = frame_row_text(&mut terminal, 0);
    assert!(
        row0.contains("[2/9]"),
        "narrow panel degrades to the bare pointer: {row0:?}"
    );
    assert!(
        !row0.contains("draft saved"),
        "the reassurance clause drops first: {row0:?}"
    );
}
