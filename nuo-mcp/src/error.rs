use thiserror::Error;

pub type Result<T> = std::result::Result<T, McpError>;

/// Errors arising from Model Context Protocol (MCP) transport, serialization, or RPC dispatch.
#[derive(Error, Debug, Clone)]
pub enum McpError {
    #[error("I/O or process transport error: {0}")]
    Transport(String),

    #[error("MCP JSON-RPC error ({code}): {message}")]
    Protocol {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },

    #[error("JSON serialization/deserialization failed: {0}")]
    Serialization(String),

    #[error("handshake or initialization failed: {0}")]
    Initialization(String),

    #[error("MCP server process failed: {0}")]
    Process(String),

    #[error("MCP client connection is closed")]
    Closed,

    #[error("tool execution cancelled")]
    Cancelled,
}

impl McpError {
    pub fn transport(msg: impl Into<String>) -> Self {
        Self::Transport(msg.into())
    }

    pub fn protocol(
        code: i64,
        message: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self::Protocol {
            code,
            message: message.into(),
            data,
        }
    }

    pub fn serialization(msg: impl Into<String>) -> Self {
        Self::Serialization(msg.into())
    }

    pub fn initialization(msg: impl Into<String>) -> Self {
        Self::Initialization(msg.into())
    }

    pub fn process(msg: impl Into<String>) -> Self {
        Self::Process(msg.into())
    }
}

impl From<McpError> for nuo_tool::ToolError {
    fn from(err: McpError) -> Self {
        match err {
            McpError::Cancelled => nuo_tool::ToolError::Cancelled("mcp".into()),
            other => nuo_tool::ToolError::Execution {
                name: "mcp".into(),
                reason: other.to_string(),
            },
        }
    }
}
