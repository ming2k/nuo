//! AskUser / UserQuestion overlay.
//!
//! This is the production rendering path for human-in-the-loop questions.
//! The former declarative storybook component was retired (ADR-0185).

use crate::shell::question_model::QuestionModel;
use crate::shell::ui::ComponentTree;
use nuo_tool::{UserQuestion, UserQuestionOption, UserQuestionRequest};

use nuo_ui_primitives::theme::Theme;
use nuotc::{Frame, Rect};

/// Pure immutable properties for the AskUser component.
#[derive(Debug, Clone)]
pub struct AskUserProps {
    pub request: UserQuestionRequest,
    pub queue_depth: usize,
    pub follow_highlight: bool,
    pub show_caret: bool,
}

impl AskUserProps {
    pub fn new(request: UserQuestionRequest, queue_depth: usize) -> Self {
        Self {
            request,
            queue_depth,
            follow_highlight: true,
            show_caret: true,
        }
    }
}

/// Sovereign volatile interaction state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserState {
    pub model: QuestionModel,
    pub scroll: usize,
}

impl Default for AskUserState {
    fn default() -> Self {
        Self {
            model: QuestionModel::open(UserQuestionRequest {
                id: "default".to_string(),
                questions: vec![UserQuestion {
                    header: Some("Approval".to_string()),
                    question: "Proceed with operation?".to_string(),
                    options: vec![
                        UserQuestionOption {
                            label: "Yes, proceed".to_string(),
                            description: Some("Grant permission to continue".to_string()),
                        },
                        UserQuestionOption {
                            label: "No, abort".to_string(),
                            description: Some("Reject operation and stop".to_string()),
                        },
                    ],
                    multi_select: false,
                }],
                origin: None,
            }),
            scroll: 0,
        }
    }
}

impl AskUserState {
    pub fn from_request(request: UserQuestionRequest) -> Self {
        let model = QuestionModel::open(request);
        Self { model, scroll: 0 }
    }
}

/// The AskUser overlay renderer.
pub struct AskUser;

impl AskUser {
    /// Render with an external component tree (production runtime integration).
    pub fn render_with_tree(
        props: &AskUserProps,
        state: &AskUserState,
        frame: &mut Frame<'_>,
        hit_map: &mut ComponentTree,
        area: Rect,
        theme: &Theme,
    ) {
        let mut scroll = state.scroll;
        let model = &state.model;

        crate::shell::overlays::permission::draw_question_modal(
            frame,
            hit_map,
            model.request(),
            model.current(),
            model.selected(),
            model.other_text(),
            model.highlight(),
            &mut scroll,
            props.follow_highlight,
            props.queue_depth,
            area,
            props.show_caret,
            theme,
        );
    }
}
