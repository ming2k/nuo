use async_trait::async_trait;
use tool::{BuiltinTool, Tool};

use serde_json::json;

/// Ask the user one or more multiple-choice questions mid-task.
///
/// The actual blocking user interaction is handled by the agent harness (see
/// `Agent::execute_tool`). The tool implementation itself only provides the
/// schema and description exposed to the model.
pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        BuiltinTool::AskUser.as_str()
    }

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::AskUser.aliases()
    }

    fn description(&self) -> &str {
        "Ask the user structured multiple-choice questions to resolve ambiguity."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to present in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": {
                                "type": "string",
                                "description": "Short chip/tag label"
                            },
                            "question": {
                                "type": "string",
                                "description": "Question text"
                            },
                            "options": {
                                "type": "array",
                                "description": "2-4 choices; put recommended first with '(Recommended)' suffix",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "Choice label"
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Optional choice explanation"
                                        }
                                    },
                                    "required": ["label"]
                                },
                                "minItems": 2,
                                "maxItems": 4
                            },
                            "multi_select": {
                                "type": "boolean",
                                "default": false,
                                "description": "Allow selecting multiple options"
                            }
                        },
                        "required": ["question", "options"]
                    },
                    "minItems": 1,
                    "maxItems": 5
                }
            },
            "required": ["questions"]
        })
    }

    /// `ask_user` blocks on a live human answer; subagents (which have no
    /// user reachable) must be excluded from it by their profile.
    fn requires_user(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        Err(::tool::ToolError::execution(
            self.name(),
            "ask_user is handled by the agent harness and should not be called directly",
        ))
    }
}

protocol::register_tool!(AskUserFactory => AskUserTool);
