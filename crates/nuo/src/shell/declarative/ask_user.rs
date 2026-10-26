//! Declarative AskUser / UserQuestion Modal Component (ADR-0072 / ADR-0073).
//!
//! Encapsulates human-in-the-loop permission and question flows into
//! a pure, isolated TEA component with sovereign event transitions.

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use crate::shell::question_model::{QuestionAction, QuestionEffect, QuestionModel};
use crate::shell::ui::ComponentTree;
use nuo_ui_primitives::theme::Theme;
use nuo_wire::{UserQuestion, UserQuestionOption, UserQuestionRequest};
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

/// Discrete messages handled by AskUser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserMsg(pub QuestionAction);

/// Outward outcomes emitted to parent orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskUserCmd {
    Effects(Vec<QuestionEffect>),
}

/// The declarative AskUser modal component.
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

impl Component for AskUser {
    type Props = AskUserProps;
    type State = AskUserState;
    type Msg = AskUserMsg;
    type Cmd = AskUserCmd;

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        let mut hit_map = ComponentTree::default();
        hit_map.begin(area);
        Self::render_with_tree(props, state, frame, &mut hit_map, area, theme);
    }

    fn update(_props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        let effects = state.model.update_mut(msg.0);
        if !effects.is_empty() {
            Some(AskUserCmd::Effects(effects))
        } else {
            None
        }
    }
}

impl StorybookComponent for AskUser {
    fn category() -> &'static str {
        "Overlays"
    }

    fn name() -> &'static str {
        "AskUser"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        let req_simple = UserQuestionRequest {
            id: "req_simple".to_string(),
            questions: vec![UserQuestion {
                header: Some("Permission".to_string()),
                question: "Do you want to apply the suggested file edits?".to_string(),
                options: vec![
                    UserQuestionOption {
                        label: "Apply changes".to_string(),
                        description: Some("Write modifications to target files".to_string()),
                    },
                    UserQuestionOption {
                        label: "Discard".to_string(),
                        description: Some("Cancel and discard changes".to_string()),
                    },
                ],
                multi_select: false,
            }],
            origin: None,
        };

        let req_dangerous = UserQuestionRequest {
            id: "req_dangerous".to_string(),
            questions: vec![UserQuestion {
                header: Some("High-Risk Operation Detected".to_string()),
                question: "The agent requested execution of `rm -rf /tmp/scratch` outside the workspace boundary.\nAllowing this operation grants full host shell privileges for this process.".to_string(),
                options: vec![
                    UserQuestionOption {
                        label: "Deny (Recommended)".to_string(),
                        description: Some("Block execution immediately and notify agent".to_string()),
                    },
                    UserQuestionOption {
                        label: "Allow Once".to_string(),
                        description: Some("Authorize this specific command execution only".to_string()),
                    },
                    UserQuestionOption {
                        label: "Allow for entire session".to_string(),
                        description: Some("Trust all subsequent commands in this thread".to_string()),
                    },
                ],
                multi_select: false,
            }],
            origin: Some("subagent:security_guard".to_string()),
        };

        let req_multi = UserQuestionRequest {
            id: "req_multi".to_string(),
            questions: vec![UserQuestion {
                header: Some("Refactoring Scope Selection".to_string()),
                question: "Select all targets that should be included in the migration pass:"
                    .to_string(),
                options: vec![
                    UserQuestionOption {
                        label: "crates/nuo-tui".to_string(),
                        description: Some("Terminal presentation layer".to_string()),
                    },
                    UserQuestionOption {
                        label: "crates/nuo-server".to_string(),
                        description: Some("Headless container daemon".to_string()),
                    },
                    UserQuestionOption {
                        label: "crates/nuo-protocol".to_string(),
                        description: Some("East-West IPC wire format".to_string()),
                    },
                    UserQuestionOption {
                        label: "crates/nuo-harness".to_string(),
                        description: Some("Agent execution substrate".to_string()),
                    },
                ],
                multi_select: true,
            }],
            origin: None,
        };

        let req_long = UserQuestionRequest {
            id: "req_long".to_string(),
            questions: vec![UserQuestion {
                header: Some("Security Policy Arbitration".to_string()),
                question: "An automated patch attempted to modify the system invariant rulebook in `AGENTS.md`.\nBecause modifying operational directives can influence agent trajectory across all downstream rounds, human operator review is mandatory. Please confirm whether this edit complies with architectural guidance in ADR-0011.".to_string(),
                options: vec![
                    UserQuestionOption {
                        label: "Approve policy update".to_string(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "Reject policy update".to_string(),
                        description: None,
                    },
                ],
                multi_select: false,
            }],
            origin: None,
        };

        vec![
            Story::new(
                "Standard Confirmation",
                "Baseline single-select permission confirmation dialog",
                AskUserProps::new(req_simple.clone(), 0),
                AskUserState::from_request(req_simple),
            ),
            Story::new(
                "Dangerous Bash Operation",
                "Security-sensitive approval prompt with origin tag and multi-option ladder",
                AskUserProps::new(req_dangerous.clone(), 1),
                AskUserState::from_request(req_dangerous),
            ),
            Story::new(
                "Multi-Choice Branching",
                "Multi-select questionnaire allowing space-toggle selection across targets",
                AskUserProps::new(req_multi.clone(), 0),
                AskUserState::from_request(req_multi),
            ),
            Story::new(
                "Extreme Long Text Wrap",
                "Verifies vertical demand calculation and text wrapping for long multi-line rationale",
                AskUserProps::new(req_long.clone(), 2),
                AskUserState::from_request(req_long),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        let action = match *key {
            Key::Up | Key::Char('k') => QuestionAction::Up,
            Key::Down | Key::Char('j') => QuestionAction::Down,
            Key::Char(' ') => QuestionAction::Toggle,
            Key::Enter => QuestionAction::Submit,
            Key::Tab | Key::Right => QuestionAction::Next,
            Key::BackTab | Key::Left => QuestionAction::Previous,
            Key::Esc => QuestionAction::Cancel,
            Key::Char(c) if c.is_ascii_digit() && c != '0' => {
                QuestionAction::Select((c as usize) - ('0' as usize))
            }
            Key::Backspace => QuestionAction::Backspace,
            Key::Char(c) => QuestionAction::InsertChar(c),
            _ => return None,
        };
        Some(AskUserMsg(action))
    }
}
