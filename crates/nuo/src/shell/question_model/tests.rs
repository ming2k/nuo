//! Pure state-machine tests: feed a script of actions, assert the model
//! and the emitted effects. No terminal, no async, no agent.
use super::*;
use nuo_tool::{UserQuestion, UserQuestionOption, UserQuestionRequest};


/// A single-select question with two labeled options.
fn single_select_req() -> UserQuestionRequest {
    UserQuestionRequest {
        id: "q1".into(),
        questions: vec![UserQuestion {
            header: Some("Style".into()),
            question: "Which error handling crate?".into(),
            options: vec![
                UserQuestionOption {
                    label: "anyhow".into(),
                    description: Some("Simple".into()),
                },
                UserQuestionOption {
                    label: "thiserror".into(),
                    description: Some("Structured".into()),
                },
            ],
            multi_select: false,
        }],
        origin: None,
    }
}

/// A multi-select question with two labeled options.
fn multi_select_req() -> UserQuestionRequest {
    UserQuestionRequest {
        id: "q2".into(),
        questions: vec![UserQuestion {
            header: None,
            question: "Which features?".into(),
            options: vec![
                UserQuestionOption {
                    label: "a".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "b".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "c".into(),
                    description: None,
                },
            ],
            multi_select: true,
        }],
        origin: None,
    }
}

fn two_question_req() -> UserQuestionRequest {
    UserQuestionRequest {
        id: "q3".into(),
        questions: vec![
            UserQuestion {
                header: None,
                question: "first?".into(),
                options: vec![UserQuestionOption {
                    label: "x".into(),
                    description: None,
                }],
                multi_select: false,
            },
            UserQuestion {
                header: None,
                question: "second?".into(),
                options: vec![
                    UserQuestionOption {
                        label: "y".into(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "z".into(),
                        description: None,
                    },
                ],
                multi_select: false,
            },
        ],
        origin: None,
    }
}

// Open / defaults

#[test]
fn single_select_open_defaults_to_first_option_selected() {
    let m = QuestionModel::open(single_select_req());
    assert_eq!(m.current(), 0);
    assert_eq!(m.highlight(), 0);
    assert_eq!(m.selected(), &[vec![0]]); // first option pre-selected
    assert!(m.other_text().iter().all(String::is_empty));
}

#[test]
fn multi_select_open_defaults_to_none_selected() {
    let m = QuestionModel::open(multi_select_req());
    assert_eq!(m.selected(), &[Vec::<usize>::new()]);
}

// Navigation (Up/Down)

#[test]
fn down_wraps_around() {
    let mut m = QuestionModel::open(single_select_req());
    // 2 options + Other = 3 rows. 0 -> 1 -> 2 -> 0.
    m = m.update(QuestionAction::Down).0;
    assert_eq!(m.highlight(), 1);
    m = m.update(QuestionAction::Down).0;
    assert_eq!(m.highlight(), 2);
    m = m.update(QuestionAction::Down).0;
    assert_eq!(m.highlight(), 0);
}

#[test]
fn up_wraps_around() {
    let mut m = QuestionModel::open(single_select_req());
    m = m.update(QuestionAction::Up).0; // 0 -> 2
    assert_eq!(m.highlight(), 2);
    m = m.update(QuestionAction::Up).0;
    assert_eq!(m.highlight(), 1);
}

#[test]
fn navigation_emits_no_effects() {
    let m = QuestionModel::open(single_select_req());
    let (_, eff) = m.update(QuestionAction::Down);
    assert!(eff.is_empty());
}

// Toggle / select (single-select)

#[test]
fn single_select_toggle_after_move_keeps_live_selection() {
    // Single-select is live: Down to "thiserror" already commits the
    // selection (highlight == selection). Space is now a harmless no-op
    // there, so the selection stays at [1] without any extra step.
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Down).0; // highlight -> 1, selected -> 1
    assert_eq!(m.selected(), &[vec![1]], "navigation commits the selection");
    let (m, eff) = m.update(QuestionAction::Toggle); // Space is a no-op now
    assert_eq!(m.selected(), &[vec![1]]);
    assert!(eff.is_empty());
}

#[test]
fn single_select_toggle_on_default_keeps_it() {
    // Space on the already-highlighted default row is a no-op that keeps
    // the selection (single-select: the highlight is always the selection).
    let m = QuestionModel::open(single_select_req());
    let (m, _) = m.update(QuestionAction::Toggle); // highlight 0 already selected
    assert_eq!(m.selected(), &[vec![0]]);
}

#[test]
fn digit_select_jumps_and_selects() {
    let m = QuestionModel::open(single_select_req());
    let (m, _) = m.update(QuestionAction::Select(2)); // 1-based -> index 1
    assert_eq!(m.highlight(), 1);
    assert_eq!(m.selected(), &[vec![1]]);
}

#[test]
fn digit_select_out_of_range_is_ignored() {
    let m = QuestionModel::open(single_select_req()); // 3 rows
    let (m, _) = m.update(QuestionAction::Select(9));
    assert_eq!(m.highlight(), 0); // unchanged
    assert_eq!(m.selected(), &[vec![0]]); // unchanged
}

// Toggle / select (multi-select)

#[test]
fn multi_select_toggle_adds_and_sorts() {
    let m = QuestionModel::open(multi_select_req()); // rows 0,1,2 + Other(3)
    let m = m.update(QuestionAction::Select(3)).0; // select index 2 ("c")
    let m = m.update(QuestionAction::Select(1)).0; // select index 0 ("a")
    assert_eq!(m.selected(), &[vec![0, 2]]); // sorted
}

#[test]
fn multi_select_toggle_removes_if_present() {
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0; // add 0
    assert_eq!(m.selected(), &[vec![0]]);
    let (m, _) = m.update(QuestionAction::Select(1)); // toggle off 0
    assert_eq!(m.selected(), &[Vec::<usize>::new()]);
}

// "Other" free text

#[test]
fn insert_char_only_works_when_other_highlighted() {
    let mut m = QuestionModel::open(single_select_req()); // highlight 0
    let (m_, _) = m.update(QuestionAction::InsertChar('h')); // not on Other
    m = m_;
    assert_eq!(m.other_text(), &[""]);

    // Move to Other (index 2) then type.
    m = m.update(QuestionAction::Select(3)).0; // 1-based 3 -> index 2 (Other)
    let (m_, _) = m.update(QuestionAction::InsertChar('h'));
    let (m_, _) = m_.update(QuestionAction::InsertChar('i'));
    m = m_;
    assert_eq!(m.other_text(), &["hi"]);
}

#[test]
fn backspace_deletes_last_char() {
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(3)).0; // focus Other
    let m = m.update(QuestionAction::InsertChar('a')).0;
    let m = m.update(QuestionAction::InsertChar('b')).0;
    let (m, _) = m.update(QuestionAction::Backspace);
    assert_eq!(m.other_text(), &["a"]);
}

#[test]
fn backspace_deletes_last_grapheme_cluster() {
    let family = "👨‍👩‍👧‍👦";
    let mut m = QuestionModel::open(single_select_req());
    m = m.update(QuestionAction::Select(3)).0;
    for ch in family.chars() {
        m = m.update(QuestionAction::InsertChar(ch)).0;
    }
    m = m.update(QuestionAction::Backspace).0;
    assert_eq!(m.other_text(), &[""]);
}

#[test]
fn backspace_when_not_on_other_is_noop() {
    let m = QuestionModel::open(single_select_req()); // highlight 0
    let (m, _) = m.update(QuestionAction::Backspace);
    assert_eq!(m.other_text(), &[""]);
}

#[test]
fn paste_appends_to_other_field() {
    // Focus "Other", type one char, then paste a string — the paste
    // appends after the existing text (cursor is always at the end).
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(3)).0; // focus Other (idx 2)
    let m = m.update(QuestionAction::InsertChar('h')).0;
    let (m, _) = m.update(QuestionAction::Paste("ello world".to_string()));
    assert_eq!(m.other_text(), &["hello world"]);
}

#[test]
fn paste_when_not_on_other_is_noop() {
    // A real option is highlighted, so paste must not touch other_text.
    let m = QuestionModel::open(single_select_req()); // highlight 0
    let (m, _) = m.update(QuestionAction::Paste("nope".to_string()));
    assert_eq!(m.other_text(), &[""]);
}

#[test]
fn paste_then_submit_carries_the_text() {
    // End-to-end: paste into "Other", submit — the reply carries the
    // pasted text rather than the literal "Other".
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(3)).0; // focus Other
    let m = m.update(QuestionAction::Paste("custom".to_string())).0;
    let (_, eff) = m.update(QuestionAction::Submit);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q1".into(),
                answers: vec![vec!["custom".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q1".into()
            },
        ]
    );
}

#[test]
fn paste_empty_string_is_noop() {
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(3)).0; // focus Other
    let (m, _) = m.update(QuestionAction::Paste(String::new()));
    assert_eq!(m.other_text(), &[""]);
}

// Submit / cancel effects

#[test]
fn submit_emits_reply_and_closed_with_labels() {
    let m = QuestionModel::open(single_select_req());
    let (m, eff) = m.update(QuestionAction::Submit);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q1".into(),
                answers: vec![vec!["anyhow".to_string()]], // default selection kept
            },
            QuestionEffect::Closed {
                request_id: "q1".into()
            },
        ]
    );
    // Model is returned (the event loop drops it via the Closed effect).
    assert_eq!(m.current(), 0);
}

#[test]
fn submit_with_other_blank_emits_literal_other() {
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(3)).0; // select Other (index 2)
    let (_, eff) = m.update(QuestionAction::Submit);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q1".into(),
                answers: vec![vec!["Other".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q1".into()
            },
        ]
    );
}

#[test]
fn submit_with_other_text_emits_the_text() {
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(3)).0;
    let m = m.update(QuestionAction::InsertChar('c')).0;
    let m = m.update(QuestionAction::InsertChar('u')).0;
    let m = m.update(QuestionAction::InsertChar('s')).0;
    let m = m.update(QuestionAction::InsertChar('t')).0;
    let m = m.update(QuestionAction::InsertChar('o')).0;
    let m = m.update(QuestionAction::InsertChar('m')).0;
    let (_, eff) = m.update(QuestionAction::Submit);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q1".into(),
                answers: vec![vec!["custom".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q1".into()
            },
        ]
    );
}

#[test]
fn cancel_emits_cancelled_then_closed() {
    let m = QuestionModel::open(single_select_req());
    let (_, eff) = m.update(QuestionAction::Cancel);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Cancelled {
                request_id: "q1".into()
            },
            QuestionEffect::Closed {
                request_id: "q1".into()
            },
        ]
    );
}

// Multi-question answers

#[test]
fn next_and_previous_navigate_pages_without_early_submit() {
    let m = QuestionModel::open(two_question_req());
    assert_eq!(m.current(), 0);

    // Previous on first question is a no-op
    let (m, effects) = m.update(QuestionAction::Previous);
    assert_eq!(m.current(), 0);
    assert!(effects.is_empty());

    // Next moves to page 1
    let (m, effects) = m.update(QuestionAction::Next);
    assert_eq!(m.current(), 1);
    assert!(effects.is_empty());

    // Next on last question does NOT submit; it stays at the end
    let (m, effects) = m.update(QuestionAction::Next);
    assert_eq!(m.current(), 1);
    assert!(effects.is_empty());

    // Previous steps back to page 0
    let (m, effects) = m.update(QuestionAction::Previous);
    assert_eq!(m.current(), 0);
    assert!(effects.is_empty());
}

#[test]
fn compute_answers_covers_all_questions() {
    let m = QuestionModel::open(two_question_req());
    // Defaults: q0 -> [x] (index 0), q1 -> [y] (index 0).
    assert_eq!(
        m.compute_answers(),
        vec![vec!["x".to_string()], vec!["y".to_string()]]
    );
}

// Single-select: switch selection back and forth

#[test]
fn single_select_switches_selection_on_repeated_jump() {
    // Select "thiserror" (2), then switch back to "anyhow" (1) via digit
    // jumps. Single-select is "replace", so each jump overwrites the
    // selection — and because it is live, the highlight and selection
    // move together.
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(2)).0; // -> thiserror (idx 1)
    assert_eq!(m.selected(), &[vec![1]]);
    let m = m.update(QuestionAction::Select(1)).0; // -> anyhow (idx 0)
    assert_eq!(m.selected(), &[vec![0]]);
}

#[test]
fn single_select_arrow_commits_space_is_noop() {
    // The keyboard path is now one step: Down to highlight "thiserror"
    // already commits it (the highlight is the selection). Space adds
    // nothing, and Enter then submits [1].
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Down).0; // highlight 1 -> selected
    assert_eq!(m.selected(), &[vec![1]]);
    let (m, eff) = m.update(QuestionAction::Toggle); // Space is a no-op
    assert_eq!(m.selected(), &[vec![1]]);
    assert!(eff.is_empty(), "selecting emits no effect");
}

#[test]
fn single_select_arrow_then_arrow_moves_live_selection() {
    // Discontinuous: move away from the default, keep moving — the
    // committed selection follows the highlight at every step.
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Down).0; // highlight thiserror -> selected
    assert_eq!(m.selected(), &[vec![1]]);
    let m = m.update(QuestionAction::Down).0; // move to Other -> selected
    assert_eq!(m.highlight(), 2);
    assert_eq!(m.selected(), &[vec![2]], "selection follows the highlight");
}

// Single-select: discontinuous keystrokes (move, then pick)

#[test]
fn single_select_arrows_commit_the_selection_live() {
    // Single-select is *live*: navigating with arrows moves the selection
    // along with the highlight — there is no separate "commit" step, so
    // the selected index always tracks the highlight.
    let m = QuestionModel::open(single_select_req()); // anyhow selected
    let m = m.update(QuestionAction::Down).0; // highlight thiserror -> selected
    assert_eq!(m.highlight(), 1);
    assert_eq!(m.selected(), &[vec![1]], "arrows commit the selection");
    let m = m.update(QuestionAction::Down).0; // highlight Other -> selected
    assert_eq!(m.highlight(), 2);
    assert_eq!(m.selected(), &[vec![2]]);
    let m = m.update(QuestionAction::Up).0; // back to thiserror -> selected
    assert_eq!(m.highlight(), 1);
    assert_eq!(m.selected(), &[vec![1]]);
}

#[test]
fn single_select_arrow_moves_live_without_commit() {
    // Moving around continuously leaves the selection equal to the final
    // highlight — no stale default lingers once the user has navigated.
    let m = QuestionModel::open(single_select_req()); // anyhow selected
    let m = m.update(QuestionAction::Down).0; // thiserror
    let m = m.update(QuestionAction::Down).0; // Other
    let m = m.update(QuestionAction::Up).0; // thiserror
    assert_eq!(m.highlight(), 1);
    assert_eq!(m.selected(), &[vec![1]], "selection tracks the highlight");
}

#[test]
fn single_select_jump_back_and_forth_between_two() {
    // Select option 2, then 1, then 2 again via digit keys — the
    // single-select slot replaces each time.
    let m = QuestionModel::open(single_select_req());
    let m = m.update(QuestionAction::Select(2)).0; // thiserror
    assert_eq!(m.selected(), &[vec![1]]);
    assert_eq!(m.highlight(), 1);
    let m = m.update(QuestionAction::Select(1)).0; // anyhow
    assert_eq!(m.selected(), &[vec![0]]);
    assert_eq!(m.highlight(), 0);
    let m = m.update(QuestionAction::Select(2)).0; // thiserror again
    assert_eq!(m.selected(), &[vec![1]]);
}

// Multi-select: add several, deselect, reselect

#[test]
fn multi_select_select_all_three_then_deselect_middle() {
    // Pick all three real options, then toggle the middle one off.
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0; // a
    let m = m.update(QuestionAction::Select(2)).0; // b
    let m = m.update(QuestionAction::Select(3)).0; // c
    assert_eq!(m.selected(), &[vec![0, 1, 2]]);
    let (m, _) = m.update(QuestionAction::Select(2)); // deselect b
    assert_eq!(m.selected(), &[vec![0, 2]]);
}

#[test]
fn multi_select_toggle_same_option_twice_is_idempotent() {
    // Toggle on then off returns to empty.
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0;
    assert_eq!(m.selected(), &[vec![0]]);
    let (m, _) = m.update(QuestionAction::Select(1));
    assert_eq!(m.selected(), &[Vec::<usize>::new()]);
    let (m, _) = m.update(QuestionAction::Select(1)); // reselect
    assert_eq!(m.selected(), &[vec![0]]);
}

#[test]
fn multi_select_plus_other_text_all_in_reply() {
    // Select two real options AND "Other" with typed text; the reply
    // must list all three labels in index order.
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0; // a (idx 0)
    let m = m.update(QuestionAction::Select(3)).0; // c (idx 2)
    let m = m.update(QuestionAction::Select(4)).0; // Other (idx 3)
    let m = m.update(QuestionAction::InsertChar('z')).0;
    assert_eq!(m.selected(), &[vec![0, 2, 3]]);
    let (_, eff) = m.update(QuestionAction::Submit);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q2".into(),
                answers: vec![vec!["a".to_string(), "c".to_string(), "z".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q2".into()
            },
        ]
    );
}

// "Other" edge cases

#[test]
fn multi_select_other_blank_in_reply_emits_literal_other() {
    // Selecting "Other" with no typed text resolves to "Other" in the
    // reply, even alongside real selections.
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0; // a
    let m = m.update(QuestionAction::Select(4)).0; // Other (blank)
    let (_, eff) = m.update(QuestionAction::Submit);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q2".into(),
                answers: vec![vec!["a".to_string(), "Other".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q2".into()
            },
        ]
    );
}

#[test]
fn typing_into_other_does_not_affect_a_real_option_highlight() {
    // When a real option is highlighted, InsertChar/Backspace are no-ops
    // on the other_text field (they never write there).
    let m = QuestionModel::open(single_select_req()); // highlight 0
    let (m, _) = m.update(QuestionAction::InsertChar('x'));
    let (m, _) = m.update(QuestionAction::Backspace);
    assert_eq!(m.other_text(), &[""]);
}

// Multi-question (paged) interaction

#[test]
fn two_question_reply_carries_one_array_per_question() {
    // Enter on the first page advances without effects. Enter on the final
    // page submits one answer array per question, in question order.
    let m = QuestionModel::open(two_question_req());
    let (m, effects) = m.update(QuestionAction::Submit);
    assert!(effects.is_empty());
    assert_eq!(m.current(), 1);
    let (_, eff) = m.update(QuestionAction::Submit);
    // q0 single-select defaults [x]; q1 single-select defaults [y].
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q3".into(),
                answers: vec![vec!["x".to_string()], vec!["y".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q3".into()
            },
        ]
    );
}

#[test]
fn cancel_discards_pending_selections() {
    // Even after making selections, Cancel emits an explicit cancellation
    // before closing, so the agent's parked request always resolves.
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0;
    let m = m.update(QuestionAction::Select(2)).0;
    assert_eq!(m.selected(), &[vec![0, 1]]);
    let (_, eff) = m.update(QuestionAction::Cancel);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Cancelled {
                request_id: "q2".into()
            },
            QuestionEffect::Closed {
                request_id: "q2".into()
            },
        ]
    );
}

#[test]
fn paged_navigation_preserves_each_question_cursor_and_selection() {
    let m = QuestionModel::open(two_question_req());
    let m = m.update(QuestionAction::Submit).0;
    assert_eq!(m.current(), 1);
    let m = m.update(QuestionAction::Select(2)).0;
    assert_eq!(m.highlight(), 1);
    assert_eq!(m.selected(), &[vec![0], vec![1]]);

    let m = m.update(QuestionAction::Previous).0;
    assert_eq!(m.current(), 0);
    assert_eq!(m.highlight(), 0);
    let m = m.update(QuestionAction::Submit).0;
    assert_eq!(m.current(), 1);
    assert_eq!(m.highlight(), 1);
    assert_eq!(m.selected(), &[vec![0], vec![1]]);
}

// Full interaction script (regression)

#[test]
fn full_script_multi_select_then_other_then_submit() {
    // A realistic script: open a multi-select, pick two, switch to Other,
    // type a custom answer, submit — and assert both final model and the
    // emitted reply in one go. This is the "see the interaction" test the
    // old inline arms could never express.
    let m = QuestionModel::open(multi_select_req());
    let m = m.update(QuestionAction::Select(1)).0; // "a"
    let m = m.update(QuestionAction::Down).0; // highlight -> "b"
    let m = m.update(QuestionAction::Toggle).0; // toggle "b" on
    let m = m.update(QuestionAction::Select(4)).0; // Other (1-based 4 -> idx 3)
    let m = m.update(QuestionAction::InsertChar('z')).0;
    let (m, eff) = m.update(QuestionAction::Submit);

    assert_eq!(m.selected(), &[vec![0, 1, 3]]); // a, b, and Other
    assert_eq!(m.other_text(), &["z".to_string()]);
    assert_eq!(
        eff,
        vec![
            QuestionEffect::Reply {
                request_id: "q2".into(),
                answers: vec![vec!["a".to_string(), "b".to_string(), "z".to_string()]],
            },
            QuestionEffect::Closed {
                request_id: "q2".into()
            },
        ]
    );
}

// Rendering film: see the interaction, frame by frame
//
// The pure `update` lets a test feed a *script* of actions and render every
// intermediate state. Each frame is snapshotted, so `cargo insta review`
// reads like a flip-book of the modal responding to keystrokes — the
// "can I see this component behave correctly?" debug loop the old inline
// arms made impossible. Regenerate after an intentional visual change:
//   INSTA_UPDATE=always cargo test -p nuo-ui question_modal_film

/// Render a question model into a trimmed grid of cell symbols at a fixed
/// size, mirroring `paint::snapshot_tests::render_grid` but for the modal
/// instead of a tool step. Returns only the painted rows so the snapshot
/// stays compact and diffable.
fn render_question_grid(model: &QuestionModel, width: u16, height: u16) -> String {
    use crate::shell::render::{Theme, draw_question_modal};

    let mut terminal = nuotc::TestTerminal::new(width, height);
    terminal.draw(|f| {
        let mut hit_map = crate::shell::ui::ComponentTree::new();
        hit_map.begin(f.area());
        let mut scroll = 0;
        draw_question_modal(
            f,
            &mut hit_map,
            model.request(),
            model.current(),
            model.selected(),
            model.other_text(),
            model.highlight(),
            &mut scroll,
            true,
            0,
            // Slot-anchored sheet: the bottom band of the
            // terminal, as if the composer slot it replaces sat there.
            nuotc::Rect::new(0, height * 5 / 8, width, height * 3 / 8),
            true,
            &Theme::default(),
        );
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            row.push_str(buf.cells()[y * bw + x].symbol.as_str());
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows.join("\n")
}

/// A flip-book: open a multi-select, then walk through a realistic
/// keystroke script, snapshotting the rendered modal at each step. The
/// combined snapshot is the whole interaction as text — review it to see
/// the highlight move, selections toggle, and the "Other" field fill in.
#[test]
fn question_modal_film_multiselect_interaction() {
    let m = QuestionModel::open(multi_select_req());
    let mut film = String::new();
    film.push_str("=== open ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    // ↓ highlight moves to "b" (row 1)
    let m = m.update(QuestionAction::Down).0;
    film.push_str("\n\n=== down → highlight 'b' ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    // Space toggles "b" on
    let m = m.update(QuestionAction::Toggle).0;
    film.push_str("\n\n=== space → toggle 'b' selected ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    // 1 selects "a"
    let m = m.update(QuestionAction::Select(1)).0;
    film.push_str("\n\n=== '1' → select 'a' ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    // ↓ to "Other" (index 3), type "custom"
    let m = m.update(QuestionAction::Select(4)).0;
    let m = m.update(QuestionAction::InsertChar('c')).0;
    let m = m.update(QuestionAction::InsertChar('u')).0;
    let m = m.update(QuestionAction::InsertChar('s')).0;
    let m = m.update(QuestionAction::InsertChar('t')).0;
    film.push_str("\n\n=== '4' + 'custom' → focus 'Other', type ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    insta::assert_snapshot!(film);
}

/// Single-select film: no marker is shown — the highlight *is* the
/// selection, so navigating with ↓ moves the brand-colored highlight and
/// commits the selection live (Enter then submits it). No radio dot, no
/// Space step.
#[test]
fn question_modal_film_single_select_jump() {
    let m = QuestionModel::open(single_select_req());
    let mut film = String::new();
    film.push_str("=== open (first row highlighted = selected) ===\n");
    film.push_str(&render_question_grid(&m, 64, 16));

    // ↓ moves the highlight (and, live, the selection) to "thiserror"
    let m = m.update(QuestionAction::Down).0;
    film.push_str("\n\n=== down → highlight 'thiserror' (selected, no marker) ===\n");
    film.push_str(&render_question_grid(&m, 64, 16));

    // Enter would submit now; instead jump to "Other" to show the field
    let m = m.update(QuestionAction::Select(3)).0;
    film.push_str("\n\n=== '3' → jump to 'Other' (now the live selection) ===\n");
    film.push_str(&render_question_grid(&m, 64, 16));

    insta::assert_snapshot!(film);
}

/// Multi-select film: pick all three, then deselect the middle one — the
/// checkbox markers flip from `[x]` to `[ ]` and back, and the highlight
/// ring follows the cursor through `↑`/`↓`.
#[test]
fn question_modal_film_multiselect_checkboxes() {
    let m = QuestionModel::open(multi_select_req());
    let mut film = String::new();
    film.push_str("=== open (none selected) ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    // 1, 2, 3 → select a, b, c
    let m = m.update(QuestionAction::Select(1)).0;
    let m = m.update(QuestionAction::Select(2)).0;
    let m = m.update(QuestionAction::Select(3)).0;
    film.push_str("\n\n=== 1,2,3 → select all three ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    // 2 again → deselect b (the middle checkbox flips off)
    let m = m.update(QuestionAction::Select(2)).0;
    film.push_str("\n\n=== '2' again → deselect 'b' ===\n");
    film.push_str(&render_question_grid(&m, 72, 26));

    insta::assert_snapshot!(film);
}

/// Multi-page film: open a two-question request, make a selection, then
/// submit — the footer shows "Question 1/2" and the title advances. This
/// pins the paged header rendering.
#[test]
fn question_modal_film_two_question_header() {
    let m = QuestionModel::open(two_question_req());
    let mut film = String::new();
    film.push_str("=== open: Question 1/2 ===\n");
    film.push_str(&render_question_grid(&m, 64, 16));

    let m = m.update(QuestionAction::Submit).0;
    film.push_str("\n\n=== Enter → Question 2/2 ===\n");
    film.push_str(&render_question_grid(&m, 64, 16));

    let m = m.update(QuestionAction::Previous).0;
    film.push_str("\n\n=== Shift+Tab → Question 1/2 ===\n");
    film.push_str(&render_question_grid(&m, 64, 16));

    insta::assert_snapshot!(film);
}
