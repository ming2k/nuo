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

impl From<nuo_tool::Message> for Message {
    fn from(m: nuo_tool::Message) -> Self {
        let role = match m.role {
            nuo_tool::Role::System => Role::System,
            nuo_tool::Role::User => Role::User,
            nuo_tool::Role::Assistant => Role::Assistant,
            nuo_tool::Role::Tool => Role::Tool,
        };
        let tool_calls = m
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let arguments = serde_json::from_str(&tc.arguments)
                    .unwrap_or_else(|_| serde_json::json!({ "raw": tc.arguments }));
                ToolCall {
                    id: tc.id,
                    name: tc.name,
                    arguments,
                }
            })
            .collect();
        Self {
            role,
            content: m.content,
            thinking: m.reasoning_content,
            tool_calls,
            tool_results: Vec::new(),
            name: None,
        }
    }
}

impl From<Message> for nuo_tool::Message {
    fn from(m: Message) -> Self {
        let role = match m.role {
            Role::System => nuo_tool::Role::System,
            Role::User => nuo_tool::Role::User,
            Role::Assistant => nuo_tool::Role::Assistant,
            Role::Tool => nuo_tool::Role::Tool,
        };
        let tool_calls = if m.tool_calls.is_empty() {
            None
        } else {
            Some(
                m.tool_calls
                    .into_iter()
                    .map(|tc| nuo_tool::ToolCall {
                        id: tc.id,
                        name: tc.name,
                        arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                    })
                    .collect(),
            )
        };
        let mut msg = nuo_tool::Message::new(role, m.content);
        msg.reasoning_content = m.thinking;
        msg.tool_calls = tool_calls;
        msg
    }
}
