use async_trait::async_trait;
use nuo_wire::Tool;
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
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user structured questions to resolve ambiguity or choose trade-offs."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to ask the user. Each question is presented in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": {
                                "type": "string",
                                "description": "Very short label displayed as a chip/tag for the question (optional)."
                            },
                            "question": {
                                "type": "string",
                                "description": "The complete question to ask the user."
                            },
                            "options": {
                                "type": "array",
                                "description": "Available choices. Provide 2-4 options. Put the recommended option first and suffix its label with '(Recommended)'.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "Short choice label returned to you if selected."
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Optional longer explanation of the choice."
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
                                "description": "Whether the user may select more than one option."
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

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Err(
            "ask_user is handled by the agent harness and should not be called directly"
                .to_string(),
        )
    }
}

nuo_wire::register_tool!(AskUserFactory => AskUserTool);
