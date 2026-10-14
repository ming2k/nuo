//! Presenter for the `ask_user` clarifying-question tool.
//!
//! Unlike file/command tools, `ask_user` has two information sources that both
//! matter to the reader and live in *different* places:
//!
//! * the call's **arguments** carry the full question metadata — each
//!   `header`, `question` text, option list, and `multi_select` flag;
//! * the call's **result** carries only the *selected labels* (the harness
//!   serializes the answers to a JSON array of arrays), or a cancellation note.
//!
//! The old implementation ignored this split: it reported only the first
//! question in the collapsed header (`Ask 3 questions: <q1>`) and let the body
//! fall through to the generic code renderer, which dumped the answer JSON as a
//! line-numbered blob. The questions were unrecoverable and the answers
//! unreadable. This module owns the reader-facing summary and declares
//! [`ResultKind::Questions`] so the body is drawn as a question→answer list
//! instead.

use super::{ResultKind, ToolPresenter, ToolView, truncate};
use serde_json::Value;

/// Budget the collapsed summary is clamped to before the registry applies its
/// own harder cap. Kept as a named token so the presenter and its tests agree
/// on the same number instead of two literals drifting apart.
const SUMMARY_BUDGET: usize = 68;

/// The optional short `header` chip of each question, in order. Malformed or
/// absent input yields an empty list, never a panic — a restored / truncated
/// call must still render.
fn parse_headers(view: &ToolView) -> Vec<Option<String>> {
    view.args
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| {
            questions
                .iter()
                .map(|q| {
                    q.get("header")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|h| !h.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub struct AskUserPresenter;

impl ToolPresenter for AskUserPresenter {
    /// Count-led, no-question-privileged header.
    ///
    /// The old `Ask 3 questions: <first question>` led with one arbitrary
    /// question and buried the count. The count is the honest top-level fact —
    /// it tells the reader "the agent paused for N decisions" without picking a
    /// favourite — so it leads. The questions' own short `header` chips follow
    /// as a scannable topic trail (a request authored without headers falls back
    /// to count-only), and the registry clamps the whole line to its budget.
    fn summary(&self, view: &ToolView) -> String {
        let headers = parse_headers(view);
        let count = headers.len().max(1);
        let noun = if count == 1 { "question" } else { "questions" };
        let mut summary = format!("Ask {count} {noun}");
        let topics: Vec<&str> = headers.iter().filter_map(Option::as_deref).collect();
        if !topics.is_empty() {
            summary.push_str(crate::design::QUESTION_META_SEPARATOR);
            summary.push_str(&topics.join(crate::design::QUESTION_META_SEPARATOR));
        }
        truncate(&summary, SUMMARY_BUDGET)
    }

    /// The questions live in the arguments and the answers in the result text;
    /// only a dedicated renderer can rejoin them, so this steers the body away
    /// from the code default.
    fn result_kind(&self) -> ResultKind {
        ResultKind::Questions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolPresenter, ToolView};
    use serde_json::json;

    fn summary(args: &Value) -> String {
        let view = ToolView {
            name: "ask_user",
            args: args.as_object().unwrap(),
            profile: None,
            workspace_root: None,
        };
        AskUserPresenter.summary(&view)
    }

    #[test]
    fn declares_the_questions_result_kind() {
        assert_eq!(AskUserPresenter.result_kind(), ResultKind::Questions);
    }

    #[test]
    fn multi_question_summary_leads_with_count_and_header_chips() {
        let args = json!({
            "questions": [
                { "header": "Scope", "question": "How big?", "options": [{"label":"a"},{"label":"b"}] },
                { "header": "Layout", "question": "Which pieces?", "options": [{"label":"x"},{"label":"y"}], "multi_select": true }
            ]
        });
        assert_eq!(summary(&args), "Ask 2 questions · Scope · Layout");
    }

    #[test]
    fn single_question_uses_singular_noun() {
        let args = json!({
            "questions": [ { "question": "Proceed?", "options": [{"label":"yes"},{"label":"no"}] } ]
        });
        // No header chip authored -> count only, never a privileged question.
        assert_eq!(summary(&args), "Ask 1 question");
    }

    #[test]
    fn summary_is_clamped_to_the_header_budget() {
        let long_header = "H".repeat(120);
        let args = json!({
            "questions": [
                { "header": long_header, "question": "q", "options": [{"label":"a"},{"label":"b"}] }
            ]
        });
        let summary = summary(&args);
        // `truncate` appends a 3-char ellipsis past the take budget.
        assert!(
            summary.chars().count() <= SUMMARY_BUDGET + 3,
            "summary leaked past the header budget: {summary:?}"
        );
        assert!(
            summary.ends_with("..."),
            "a clipped header must signal elision: {summary:?}"
        );
    }

    #[test]
    fn malformed_arguments_never_panic() {
        assert_eq!(summary(&json!({ "questions": "not an array" })), "Ask 1 question");
        assert_eq!(summary(&json!({})), "Ask 1 question");
    }
}
