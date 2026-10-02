use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            name: None,
        }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            thinking: None,
            tool_calls,
            tool_results: Vec::new(),
            name: None,
        }
    }

    pub fn with_thinking(mut self, thinking: impl Into<String>) -> Self {
        self.thinking = Some(thinking.into());
        self
    }

    pub fn tool_response(
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        let name_str = name.into();
        let out_str = output.into();
        Self {
            role: Role::Tool,
            content: out_str.clone(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                name: name_str.clone(),
                output: out_str,
                is_error: false,
            }],
            name: Some(name_str),
        }
    }

    pub fn tool_error(
        call_id: impl Into<String>,
        name: impl Into<String>,
        error_msg: impl Into<String>,
    ) -> Self {
        let name_str = name.into();
        let err_str = error_msg.into();
        Self {
            role: Role::Tool,
            content: err_str.clone(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                name: name_str.clone(),
                output: err_str,
                is_error: true,
            }],
            name: Some(name_str),
        }
    }
}
