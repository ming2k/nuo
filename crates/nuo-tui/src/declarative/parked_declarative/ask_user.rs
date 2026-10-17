//! Declarative AskUser / UserQuestion Modal Component (ADR-0072 / ADR-0073).
//!
//! Encapsulates human-in-the-loop permission and question flows into
//! a pure, isolated TEA component with sovereign event transitions.

use nuotc::{Frame, Rect};
use crossterm::event::{KeyCode, KeyEvent};
use nuo_wire::{UserQuestion, UserQuestionOption, UserQuestionRequest};
use crate::question_model::{QuestionAction, QuestionEffect, QuestionModel};
use crate::theme::Theme;
use crate::ui::ComponentTree;
use super::{Component, story::{Story, StorybookComponent}};

/// Pure immutable properties for the AskUser component.
#[derive(Debug, Clone)]
pub struct AskUserProps {
    pub request: UserQuestionRequest,
    pub queue_depth: usize,
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
        let mut scroll = state.scroll;
        let model = &state.model;

        crate::overlays::permission::draw_question_modal(
            frame,
            &mut hit_map,
            model.request(),
            model.current(),
            model.selected(),
            model.other_text(),
            model.highlight(),
            &mut scroll,
            true, // follow_highlight
            props.queue_depth,
            area,
            true, // show_caret
            theme,
        );
    }

    fn update(
        _props: &Self::Props,
        state: &mut Self::State,
        msg: Self::Msg,
    ) -> Option<Self::Cmd> {
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
                question: "Select all targets that should be included in the migration pass:".to_string(),
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
                AskUserProps {
                    request: req_simple.clone(),
                    queue_depth: 0,
                },
                AskUserState::from_request(req_simple),
            ),
            Story::new(
                "Dangerous Bash Operation",
                "Security-sensitive approval prompt with origin tag and multi-option ladder",
                AskUserProps {
                    request: req_dangerous.clone(),
                    queue_depth: 1,
                },
                AskUserState::from_request(req_dangerous),
            ),
            Story::new(
                "Multi-Choice Branching",
                "Multi-select questionnaire allowing space-toggle selection across targets",
                AskUserProps {
                    request: req_multi.clone(),
                    queue_depth: 0,
                },
                AskUserState::from_request(req_multi),
            ),
            Story::new(
                "Extreme Long Text Wrap",
                "Verifies vertical demand calculation and text wrapping for long multi-line rationale",
                AskUserProps {
                    request: req_long.clone(),
                    queue_depth: 2,
                },
                AskUserState::from_request(req_long),
            ),
        ]
    }

    fn map_key_to_msg(
        key: &KeyEvent,
        _props: &Self::Props,
        _state: &Self::State,
    ) -> Option<Self::Msg> {
        let action = match key.code {
            KeyCode::Up | KeyCode::Char('k') => QuestionAction::Up,
            KeyCode::Down | KeyCode::Char('j') => QuestionAction::Down,
            KeyCode::Char(' ') => QuestionAction::Toggle,
            KeyCode::Enter => QuestionAction::Submit,
            KeyCode::Tab | KeyCode::Right => QuestionAction::Next,
            KeyCode::BackTab | KeyCode::Left => QuestionAction::Previous,
            KeyCode::Esc => QuestionAction::Cancel,
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                QuestionAction::Select((c as usize) - ('0' as usize))
            }
            KeyCode::Backspace => QuestionAction::Backspace,
            KeyCode::Char(c) => QuestionAction::InsertChar(c),
            _ => return None,
        };
        Some(AskUserMsg(action))
    }
}
