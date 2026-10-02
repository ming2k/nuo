use crate::error::{Result, ToolError};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Execution context passed to every tool invocation.
///
/// Encapsulates conversation coordinates, cooperative cancellation tokens,
/// tracing metadata, and correlation handles.
#[derive(Clone)]
pub struct ToolContext {
    pub session_id: Option<String>,
    pub call_id: String,
    pub cancel_token: CancellationToken,
    pub correlation_id: Option<Uuid>,
    pub metadata: HashMap<String, String>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            session_id: None,
            call_id: Uuid::new_v4().to_string(),
            cancel_token: CancellationToken::new(),
            correlation_id: None,
            metadata: HashMap::new(),
        }
    }
}

impl ToolContext {
    /// Creates a new context with a specific tool call ID.
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            session_id: None,
            call_id: call_id.into(),
            cancel_token: CancellationToken::new(),
            correlation_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Sets the enclosing session ID.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Attaches an existing cooperative cancellation token.
    pub fn with_cancel_token(mut self, cancel_token: CancellationToken) -> Self {
        self.cancel_token = cancel_token;
        self
    }

    /// Sets the correlation UUID.
    pub fn with_correlation_id(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    /// Inserts a metadata key-value pair.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Checks if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Returns `Err(ToolError::Cancelled)` if cooperative cancellation was triggered.
    pub fn check_cancelled(&self, tool_name: &str) -> Result<()> {
        if self.is_cancelled() {
            Err(ToolError::cancelled(tool_name))
        } else {
            Ok(())
        }
    }
}
