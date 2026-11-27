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
use nuo_tool::{UserQuestion, UserQuestionRequest};

/// The "Other" free-text option label emitted in a reply when the user
/// selected the synthetic "Other" row but left its text field blank — matches
/// the original inline behavior in `event_loop.rs`.
const OTHER_LABEL: &str = "Other";

/// Whether a question offers the synthetic free-text row.
///
/// `ask_user` questions do: the model may ask an open question, so "none of
/// these" has to be typeable. A *structured* question does not: the workspace
/// trust gate offers a closed set of assets, and a free-text row there is not an
/// answer to the question — it is a row the user can land on, type into, and
/// submit that grants nothing while looking like a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeText {
    /// The trailing "Other" row exists (AI-authored `ask_user`).
    Allowed,
    /// The question is a closed set; navigation stops at the last real option.
    Suppressed,
}

/// Index reserved for the synthetic "Other" free-text option, which always
/// sits one past the last real option of a question.
fn other_index(q: &UserQuestion) -> usize {
    q.options.len()
}

/// Total selectable rows for a question: its options, plus the synthetic
/// "Other" row when `free_text` allows one. Never zero (a question with no
/// options still has the "Other" row), so the up/down wrap modulo cannot divide
/// by zero.
fn option_rows(q: &UserQuestion, free_text: FreeText) -> usize {
    let extra = usize::from(free_text == FreeText::Allowed);
    (q.options.len() + extra).max(1)
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
    /// Whether this question carries the synthetic free-text row at all.
    ///
    /// Not a property of the request: the same `UserQuestion` shape serves an
    /// AI-authored question (where "none of these, here is my answer" has to be
    /// typeable) and the workspace trust gate (where the answer is a selection
    /// from a closed set of assets, and a free-text row is not an answer at
    /// all). The caller that knows which question it is asking says so.
    free_text: FreeText,
}

impl QuestionModel {
    /// Initialize a model from a freshly-arriving request, applying the
    /// default-selection rule: multi-select questions start with nothing
    /// selected, single-select questions start with the first option selected
    /// (matching the initial highlight on row 0 — single-select is *live*,
    /// so the highlight is always the selection). The highlight begins on the
    /// first row.
    ///
    /// The question carries the synthetic "Other" row — this is the
    /// AI-authored `ask_user` shape. A question whose answer is a choice from a
    /// closed set opens with [`QuestionModel::open_closed`].
    pub fn open(request: UserQuestionRequest) -> Self {
        Self::build(request, FreeText::Allowed)
    }

    /// [`QuestionModel::open`] for a question whose answer is one of its
    /// options and nothing else.
    ///
    /// The free-text row is suppressed rather than merely unpainted: painting a
    /// closed decision without it and still letting `↓` land on it left the
    /// surface with *no* highlighted row (nothing to paint) while `Enter` there
    /// submitted a decision the user could not see. A row the surface does not
    /// paint must not exist in the state machine either.
    pub fn open_closed(request: UserQuestionRequest) -> Self {
        Self::build(request, FreeText::Suppressed)
    }

    fn build(request: UserQuestionRequest, free_text: FreeText) -> Self {
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
            free_text,
        }
    }

    /// Whether this question carries the synthetic free-text row.
    pub fn free_text(&self) -> FreeText {
        self.free_text
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
    /// the active question. Always `false` when the question is closed.
    ///
    /// This is the *only* state in which the modal has a
    /// text-input surface, so it is the one condition under which the modal
    /// owns the terminal cursor — `App::caret_owner` reads this to decide
    /// whether to keep the real cursor visible (so the host IME can anchor its
    /// composition window to the field) or hide it (a plain option pick has no
    /// text surface). See `QuestionModel::other_index_of`.
    pub fn is_other_highlighted(&self) -> bool {
        self.other_index_of(self.current) == Some(self.highlight())
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
                let other_idx = (self.free_text == FreeText::Allowed).then(|| other_index(q));
                let other_text = self.other_text.get(q_idx).cloned().unwrap_or_default();
                self.selected
                    .get(q_idx)
                    .map(|sel| {
                        sel.iter()
                            .map(|&opt_idx| {
                                if Some(opt_idx) == other_idx {
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
        let rows = option_rows(q, self.free_text);
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
                if self.other_index_of(q) == Some(highlight)
                    && let Some(text) = self.other_text.get_mut(q)
                {
                    text.push(c);
                }
                Vec::new()
            }
            QuestionAction::Backspace => {
                if self.other_index_of(q) == Some(highlight)
                    && let Some(text) = self.other_text.get_mut(q)
                    && !text.is_empty()
                {
                    let start = graticule::floor_grapheme_boundary(text, text.len() - 1);
                    text.truncate(start);
                }
                Vec::new()
            }
            QuestionAction::Paste(pasted) => {
                // Same gating as InsertChar/Backspace: only the "Other" row
                // has a text surface. The caller (event loop) has already
                // stripped newlines, so this is a plain append.
                if self.other_index_of(q) == Some(highlight)
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

    /// The in-place variant of [`update`](Self::update) for callers that hold
    /// the model behind a mutable reference. Applies the transition in place
    /// and returns just the effects. Equivalent to taking the value, calling
    /// `update`, and writing it back.
    pub fn update_mut(&mut self, action: QuestionAction) -> Vec<QuestionEffect> {
        // We need to move `self` into `update`, so clone the request as a
        // stand-in, swap, then write the result back. This is cheap because the
        // model is small (a request + a few indices/vecs).
        let placeholder = self.request.clone();
        let free_text = self.free_text;
        let me = std::mem::replace(self, QuestionModel::build(placeholder, free_text));
        let (next, effects) = me.update(action);
        *self = next;
        effects
    }

    /// The synthetic "Other" index for question `q`, or `None` when the
    /// question is closed and has no such row.
    fn other_index_of(&self, q: usize) -> Option<usize> {
        (self.free_text == FreeText::Allowed)
            .then(|| self.request.questions.get(q).map(|x| x.options.len()))
            .flatten()
    }
}

#[cfg(test)]
mod tests;
