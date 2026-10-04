//! Model-View-Update core for the question (ask_user) modal.
//!
//! This is the single source of truth for the question modal's *interaction*
//! state and its *reply computation*, extracted out of the 2000-line
//! `event_loop.rs` match so the entire state machine can be unit-tested in
//! isolation — no terminal, no async, no agent channel.
//!
//! ## Architecture
//!
//! - **Model** ([`QuestionModel`]): the complete, self-contained state of an
//!   open question modal — the request, which question is active, the
//!   highlighted option, the per-question selected indices, and the per-question
//!   "Other" free-text. It is plain data: clone it, inspect it, render it.
//!
//! ## Single vs. multi select semantics
//!
//! - **Single-select** is *live*: the highlight is the selection. Moving with
//!   `↑`/`↓` or a digit jump immediately moves the selected index, so `Enter`
//!   advances with exactly what is highlighted (or submits on the final page)
//!   — there is no separate "commit" step and no radio-button marker is shown.
//! - **Multi-select** keeps a separate toggle set: `↑`/`↓` only moves the
//!   highlight, `Space` toggles a row on/off, and `Enter` advances to the next
//!   question or submits the whole set on the final page. The `[x]`/`[ ]`
//!   marker stays.
//! - **View**: already pure and lives in `paint::draw_question_modal`, which
//!   reads straight off the model via the [`QuestionModel`] accessors.
//! - **Update** ([`QuestionModel::update`]): the pure state transition. It
//!   takes an [`QuestionAction`] (an input-event already mapped by `input.rs`)
//!   and returns `(updated model, Vec<QuestionEffect>)`. It performs **no I/O**:
//!   every side effect — replying to the agent, advancing the queue — is
//!   described as a [`QuestionEffect`] value that the event loop executes.
//!
//! Because `update` is pure, a test can feed a script of keystrokes and assert
//! both the final model *and* the emitted effects, then render each
//! intermediate state to a snapshot. That is the "see the interaction" debug
//! loop the old inline arms made impossible.

use nuo_wire::{UserQuestion, UserQuestionRequest};

/// The "Other" free-text option label emitted in a reply when the user
/// selected the synthetic "Other" row but left its text field blank — matches
/// the original inline behavior in `event_loop.rs`.
const OTHER_LABEL: &str = "Other";

/// Index reserved for the synthetic "Other" free-text option, which always
/// sits one past the last real option of a question.
fn other_index(q: &UserQuestion) -> usize {
    q.options.len()
}

/// Total selectable rows for a question: the real options plus the "Other"
/// row. Clamped to ≥ 1 so the up/down wrap modulo never divides by zero.
fn option_rows(q: &UserQuestion) -> usize {
    q.options.len() + 1
}

/// A reduced input-event the modal cares about.
///
/// This mirrors the `input::InputAction::Question*` variants but is its own
/// enum so the model has zero coupling to the terminal input layer: tests
/// construct actions directly, and the event loop is the one place that
/// translates `InputAction` → `QuestionAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAction {
    /// Move the highlight up, wrapping past the top to the last row.
    Up,
    /// Move the highlight down, wrapping past the last row to the top.
    Down,
    /// Toggle the highlighted row. Multi-select flips the row on/off; for
    /// single-select this is a no-op selection-wise (the highlight already
    /// *is* the live selection), but it still maps to the Space key so the
    /// old reflex of "navigate then Space" does nothing harmful rather than
    /// nothing at all.
    Toggle,
    /// Jump the highlight to the 1-based `n`-th row AND select it.
    Select(usize),
    /// Type a character into the "Other" field (no-op unless "Other" is
    /// highlighted for the active question).
    InsertChar(char),
    /// Delete the last character from the "Other" field.
    Backspace,
    /// Paste a string into the "Other" field at the end. Newlines are stripped
    /// first by the caller so the field stays single-line, matching the
    /// inline-edit modal fields. No-op unless "Other" is highlighted.
    Paste(String),
    /// Advance to the next question, or submit all answers from the final
    /// question (Enter).
    Submit,
    /// Advance to the next question without submitting (Tab / Right). No-op on the final page.
    Next,
    /// Return to the previous question (Shift+Tab / Left). No-op on the first page.
    Previous,
    /// Cancel the modal (Esc).
    Cancel,
}

/// A side effect the event loop must perform after an update. The pure
/// `update` function never touches the channel or the queue — it only
/// *describes* what should happen, and the loop carries it out. This is what
/// makes the state transition testable without a live agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionEffect {
    /// Send the computed answers back to the agent. Carries the request id,
    /// one array of selected option labels per question, and the optional
    /// subagent parent tool-call id the reply must be tagged with for routing.
    Reply {
        request_id: String,
        answers: Vec<Vec<String>>,
    },
    /// Resolve the parked agent request as cancelled. The event loop sends an
    /// empty outer answers array, which is the transport-level cancellation
    /// sentinel; legitimate multi-select answers retain one inner array per
    /// question even when no options were chosen.
    Cancelled { request_id: String },
    /// The modal closed (submit or cancel). The loop should drop the current
    /// request from the pending queue and, if the queue is now empty, clear
    /// the modal.
    Closed { request_id: String },
}

/// The self-contained state of an open question modal.
///
/// Built from a [`UserQuestionRequest`] via [`QuestionModel::open`]. Held by
/// `App` (replacing the four scattered `question_*` fields) for the lifetime
/// of the modal and consumed back into `None` on close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionModel {
    /// The original request (id + questions). Kept by value so `submit` can
    /// compute replies and the renderer can read questions without borrowing.
    request: UserQuestionRequest,
    /// Which question the user is currently answering (0-based). Multiple
    /// questions are presented one at a time; this is the page cursor.
    current: usize,
    /// Per-question highlighted option rows (0-based, where `options.len()` is
    /// the synthetic "Other" row). Keeping one cursor per page makes backward
    /// navigation lossless.
    highlights: Vec<usize>,
    /// Per-question selected option indices. Parallels `request.questions`.
    /// Multi-select questions may hold several (and never include the
    /// highlight unless toggled); single-select questions always hold exactly
    /// the highlighted index, kept in sync by [`QuestionModel::sync_selection`].
    selected: Vec<Vec<usize>>,
    /// Per-question free text for the "Other" row. Parallels
    /// `request.questions`; only meaningful when that row is selected.
    other_text: Vec<String>,
}

impl QuestionModel {
    /// Initialize a model from a freshly-arriving request, applying the
    /// default-selection rule: multi-select questions start with nothing
    /// selected, single-select questions start with the first option selected
    /// (matching the initial highlight on row 0 — single-select is *live*,
    /// so the highlight is always the selection). The highlight begins on the
    /// first row.
    pub fn open(request: UserQuestionRequest) -> Self {
        let selected = request
            .questions
            .iter()
            .map(|q| if q.multi_select { Vec::new() } else { vec![0] })
            .collect();
        let highlights = request.questions.iter().map(|_| 0).collect();
        let other_text = request.questions.iter().map(|_| String::new()).collect();
        Self {
            request,
            current: 0,
            highlights,
            selected,
            other_text,
        }
    }

    // Accessors used by the renderer and by tests

    pub fn request(&self) -> &UserQuestionRequest {
        &self.request
    }
    pub fn current(&self) -> usize {
        self.current
    }
    pub fn highlight(&self) -> usize {
        self.highlights.get(self.current).copied().unwrap_or(0)
    }
    pub fn selected(&self) -> &[Vec<usize>] {
        &self.selected
    }

    /// Pre-select all options for question `q` (used for multi-select defaults).
    pub fn select_all(&mut self, q: usize) {
        if let Some(question) = self.request.questions.get(q)
            && let Some(sel) = self.selected.get_mut(q)
        {
            sel.clear();
            for i in 0..question.options.len() {
                sel.push(i);
            }
        }
    }

    pub fn other_text(&self) -> &[String] {
        &self.other_text
    }

    /// Whether the synthetic "Other" free-text row is the highlighted row for
    /// the active question. This is the *only* state in which the modal has a
    /// text-input surface, so it is the one condition under which the modal
    /// owns the terminal cursor — `App::caret_owner` reads this to decide
    /// whether to keep the real cursor visible (so the host IME can anchor its
    /// composition window to the field) or hide it (a plain option pick has no
    /// text surface). See `QuestionModel::other_index_of`.
    pub fn is_other_highlighted(&self) -> bool {
        self.highlight() == self.other_index_of(self.current)
    }

    /// The active question, or `None` if the model is somehow empty. The
    /// renderer and update logic treat the non-empty case as the norm.
    fn active_question(&self) -> Option<&UserQuestion> {
        self.request.questions.get(self.current)
    }

    /// Whether the active question is multi-select. The showcase footer (and
    /// any future caller that needs to know whether to advertise a `Space`
    /// toggle) reads this; `false` for single-select, whose selection is the
    /// live highlight.
    pub fn active_multi_select(&self) -> bool {
        self.active_question().is_some_and(|q| q.multi_select)
    }

    /// Enforce the single-select invariant: the selected index is the
    /// highlighted index. No-op for multi-select (whose selection is an
    /// independent toggle set). This is what makes single-select *live* —
    /// navigation immediately commits the selection, so `Enter` submits the
    /// highlighted row with no separate "Space to confirm" step. Bound-checks
    /// the question slot.
    fn sync_selection(&mut self, q: usize, multi: bool) {
        if multi {
            return;
        }
        let highlight = self.highlights.get(q).copied().unwrap_or(0);
        if let Some(sel) = self.selected.get_mut(q) {
            sel.clear();
            sel.push(highlight);
        }
    }

    /// Compute the reply answers from the current selections. One array of
    /// option labels per question. The synthetic "Other" index resolves to its
    /// typed text, or the literal `"Other"` when left blank — exactly the
    /// legacy `event_loop.rs` behavior.
    pub fn compute_answers(&self) -> Vec<Vec<String>> {
        self.request
            .questions
            .iter()
            .enumerate()
            .map(|(q_idx, q)| {
                let other_idx = other_index(q);
                let other_text = self.other_text.get(q_idx).cloned().unwrap_or_default();
                self.selected
                    .get(q_idx)
                    .map(|sel| {
                        sel.iter()
                            .map(|&opt_idx| {
                                if opt_idx == other_idx {
                                    if other_text.is_empty() {
                                        OTHER_LABEL.to_string()
                                    } else {
                                        other_text.clone()
                                    }
                                } else {
                                    q.options
                                        .get(opt_idx)
                                        .map(|o| o.label.clone())
                                        .unwrap_or_default()
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    /// The pure state transition. Consumes the model and an action, returns
    /// the next model plus any effects to perform. The model is taken by value
    /// so the caller (`App`) replaces its field cleanly with the return value;
    /// within a no-op action the model is returned unchanged.
    ///
    /// Actions are ignored gracefully when they don't apply (e.g. typing into
    /// "Other" while a real option is highlighted) — this mirrors the original
    /// arms, which silently no-oped in the same situations rather than erroring.
    pub fn update(mut self, action: QuestionAction) -> (Self, Vec<QuestionEffect>) {
        // No active question means there is nothing to do; bail unchanged.
        let Some(q) = self.active_question() else {
            return (self, Vec::new());
        };
        let rows = option_rows(q);
        let q = self.current;
        let highlight = self.highlights.get(q).copied().unwrap_or(0);
        let multi = self
            .request
            .questions
            .get(q)
            .map(|x| x.multi_select)
            .unwrap_or(false);

        let effects = match action {
            QuestionAction::Up => {
                let next = if highlight == 0 {
                    rows.saturating_sub(1)
                } else {
                    highlight - 1
                };
                self.highlights[q] = next;
                self.sync_selection(q, multi);
                Vec::new()
            }
            QuestionAction::Down => {
                self.highlights[q] = (highlight + 1) % rows.max(1);
                self.sync_selection(q, multi);
                Vec::new()
            }
            QuestionAction::Toggle => {
                // Multi-select flips the highlighted row on/off. Single-select
                // is live, so Space is a harmless no-op — the highlight is
                // already the selection, but syncing keeps the invariant
                // bulletproof if anything ever leaves them out of step.
                if multi {
                    self.toggle(highlight, q, multi);
                } else {
                    self.sync_selection(q, multi);
                }
                Vec::new()
            }
            QuestionAction::Select(n) => {
                if n > 0 && n <= rows {
                    self.highlights[q] = n - 1;
                    self.toggle(n - 1, q, multi);
                    // single-select: `toggle` already replaced the selection
                    // with n-1 == highlight, so we are synced.
                }
                Vec::new()
            }
            QuestionAction::InsertChar(c) => {
                let other_idx = self.other_index_of(q);
                if highlight == other_idx
                    && let Some(text) = self.other_text.get_mut(q)
                {
                    text.push(c);
                }
                Vec::new()
            }
            QuestionAction::Backspace => {
                let other_idx = self.other_index_of(q);
                if highlight == other_idx
                    && let Some(text) = self.other_text.get_mut(q)
                    && !text.is_empty()
                {
                    let start = nuotc::text::floor_grapheme_boundary(text, text.len() - 1);
                    text.truncate(start);
                }
                Vec::new()
            }
            QuestionAction::Paste(pasted) => {
                // Same gating as InsertChar/Backspace: only the "Other" row
                // has a text surface. The caller (event loop) has already
                // stripped newlines, so this is a plain append.
                let other_idx = self.other_index_of(q);
                if highlight == other_idx
                    && let Some(text) = self.other_text.get_mut(q)
                {
                    text.push_str(&pasted);
                }
                Vec::new()
            }
            QuestionAction::Submit => {
                if self.current + 1 < self.request.questions.len() {
                    self.current += 1;
                    Vec::new()
                } else {
                    // Selections are already committed (single-select is live,
                    // and multi-select was toggled explicitly), so the final
                    // page computes and submits the complete reply.
                    let request_id = self.request.id.clone();
                    let answers = self.compute_answers();
                    vec![
                        QuestionEffect::Reply {
                            request_id: request_id.clone(),
                            answers,
                        },
                        QuestionEffect::Closed { request_id },
                    ]
                }
            }
            QuestionAction::Next => {
                if self.current + 1 < self.request.questions.len() {
                    self.current += 1;
                }
                Vec::new()
            }
            QuestionAction::Previous => {
                self.current = self.current.saturating_sub(1);
                Vec::new()
            }
            QuestionAction::Cancel => {
                let request_id = self.request.id.clone();
                vec![
                    QuestionEffect::Cancelled {
                        request_id: request_id.clone(),
                    },
                    QuestionEffect::Closed { request_id },
                ]
            }
        };

        (self, effects)
    }

    /// Shared toggle logic. Multi-select removes the index if already present
    /// (and sorts for stable ordering), otherwise appends; single-select
    /// replaces with the given index. Bound-checks the question slot. This is
    /// used by the multi-select `Toggle` action and by the digit-jump `Select`
    /// action (for both modes). For single-select arrow navigation use
    /// [`QuestionModel::sync_selection`], which keys off `highlight` directly.
    fn toggle(&mut self, idx: usize, q: usize, multi: bool) {
        let Some(sel) = self.selected.get_mut(q) else {
            return;
        };
        if multi {
            if let Some(pos) = sel.iter().position(|&x| x == idx) {
                sel.remove(pos);
            } else {
                sel.push(idx);
                sel.sort();
            }
        } else {
            sel.clear();
            sel.push(idx);
        }
    }

    /// In-place variant of [`update`](Self::update) for callers that hold the
    /// model behind a mutable reference (e.g. inside a borrow-constrained
    /// showcase loop). Applies the transition in place and returns just the
    /// effects. Equivalent to taking the value, calling `update`, and writing
    /// it back.
    #[cfg(debug_assertions)]
    pub fn update_mut(&mut self, action: QuestionAction) -> Vec<QuestionEffect> {
        // We need to move `self` into `update`, so clone the request as a
        // stand-in, swap, then write the result back. This is cheap because the
        // model is small (a request + a few indices/vecs).
        let placeholder = self.request.clone();
        let me = std::mem::replace(self, QuestionModel::open(placeholder));
        let (next, effects) = me.update(action);
        *self = next;
        effects
    }

    /// The synthetic "Other" index for question `q` (its real option count).
    fn other_index_of(&self, q: usize) -> usize {
        self.request
            .questions
            .get(q)
            .map(|x| x.options.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    //! Pure state-machine tests: feed a script of actions, assert the model
    //! and the emitted effects. No terminal, no async, no agent.

    use super::*;
    use nuo_wire::{UserQuestion, UserQuestionOption, UserQuestionRequest};

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
    //   INSTA_UPDATE=always cargo test -p mutx question_modal_film

    /// Render a question model into a trimmed grid of cell symbols at a fixed
    /// size, mirroring `paint::snapshot_tests::render_grid` but for the modal
    /// instead of a tool step. Returns only the painted rows so the snapshot
    /// stays compact and diffable.
    fn render_question_grid(model: &QuestionModel, width: u16, height: u16) -> String {
        use crate::render::{Theme, draw_question_modal};

        let mut terminal = nuotc::TestTerminal::new(width, height);
        terminal.draw(|f| {
            let mut hit_map = crate::ui::ComponentTree::new();
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
                // Slot-anchored sheet (ADR-0173 §3): the bottom band of the
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
                row.push_str(buf.content[y * bw + x].symbol());
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
}
