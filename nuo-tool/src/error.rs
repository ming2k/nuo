use thiserror::Error;

pub type Result<T> = std::result::Result<T, ToolError>;

/// Canonical error type for tool invocation, validation, and execution.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    #[error("tool `{0}` not found in registry")]
    NotFound(String),

    #[error("invalid arguments for tool `{name}`: {reason}")]
    InvalidArguments { name: String, reason: String },

    #[error("tool execution failed for `{name}`: {reason}")]
    Execution { name: String, reason: String },

    #[error("tool `{0}` timed out")]
    Timeout(String),

    #[error("tool `{0}` execution was cancelled")]
    Cancelled(String),

    #[error("tool `{name}` execution was rejected: {reason}")]
    Rejected { name: String, reason: String },

    #[error("tool error: {0}")]
    Custom(String),
}

impl ToolError {
    pub fn not_found(name: impl Into<String>) -> Self {
        Self::NotFound(name.into())
    }

    pub fn invalid_args(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidArguments {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn execution(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Execution {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn timeout(name: impl Into<String>) -> Self {
        Self::Timeout(name.into())
    }

    pub fn cancelled(name: impl Into<String>) -> Self {
        Self::Cancelled(name.into())
    }

    pub fn rejected(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Rejected {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn custom(reason: impl Into<String>) -> Self {
        Self::Custom(reason.into())
    }
}

impl From<String> for ToolError {
    fn from(s: String) -> Self {
        Self::Custom(s)
    }
}

impl From<&str> for ToolError {
    fn from(s: &str) -> Self {
        Self::Custom(s.to_string())
    }
}
