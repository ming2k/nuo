use serde::{Deserialize, Serialize};
use std::fmt;

/// Result returned from a tool execution.
///
/// Holds the textual output observed by the model, an explicit error flag,
/// and optional structured metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Textual representation of the observation consumed by the language model.
    pub content: String,
    /// Whether the tool execution resulted in an error observation.
    pub is_error: bool,
    /// Optional structured data or epistemic claim-check payload.
    pub metadata: Option<serde_json::Value>,
}

impl ToolOutput {
    /// Creates a successful tool execution output.
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            metadata: None,
        }
    }

    /// Creates an error tool execution output.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            metadata: None,
        }
    }

    /// Creates an output from a JSON value.
    pub fn json(value: &serde_json::Value) -> Self {
        let content = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        Self {
            content,
            is_error: false,
            metadata: Some(value.clone()),
        }
    }

    /// Attaches structured metadata to the output.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Access the textual content of this output.
    pub fn to_text(&self) -> &str {
        &self.content
    }

    /// Whether this execution was an error.
    pub fn is_error(&self) -> bool {
        self.is_error
    }
}

impl fmt::Display for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.content)
    }
}

impl std::ops::Deref for ToolOutput {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl AsRef<str> for ToolOutput {
    fn as_ref(&self) -> &str {
        &self.content
    }
}

impl PartialEq<str> for ToolOutput {
    fn eq(&self, other: &str) -> bool {
        self.content == other
    }
}

impl PartialEq<&str> for ToolOutput {
    fn eq(&self, other: &&str) -> bool {
        self.content == *other
    }
}

impl PartialEq<String> for ToolOutput {
    fn eq(&self, other: &String) -> bool {
        &self.content == other
    }
}

impl From<String> for ToolOutput {
    fn from(s: String) -> Self {
        Self::success(s)
    }
}

impl From<&str> for ToolOutput {
    fn from(s: &str) -> Self {
        Self::success(s)
    }
}
