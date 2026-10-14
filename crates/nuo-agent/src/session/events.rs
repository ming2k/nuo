use crate::provider::TokenUsage;
use serde::{Deserialize, Serialize};

/// Real-time lifecycle events emitted by an agent session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A new cognitive reasoning round has begun
    RoundStarted { round: u32 },

    /// Incremental content chunk from assistant
    ContentDelta { delta: String },

    /// Incremental extended thinking / reasoning block chunk
    ThinkingDelta { delta: String },

    /// Incremental tool call streaming delta
    ToolCallDelta {
        index: usize,
        name: Option<String>,
        arguments_delta: Option<String>,
    },

    /// Assistant has issued a tool call
    ToolCallStarted {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },

    /// Tool execution completed
    ToolCallFinished {
        call_id: String,
        name: String,
        output: String,
        is_error: bool,
    },

    /// Agent dispatched a task delegation to another peer agent
    DelegationStarted { target: String, task: String },

    /// Peer agent completed task delegation
    DelegationResolved { target: String, output: String },

    /// A round was modified by inbound steering.
    Steered {
        /// Round the guidance was applied at.
        round: u32,
        /// The instruction text.
        instruction: String,
        /// One of `note`, `redirect`, `cancel`.
        action: String,
    },

    /// Context history compaction was executed to relieve context pressure
    Compacted {
        before_tokens: usize,
        after_tokens: usize,
    },

    /// Session turn concluded with final response
    Done {
        final_content: String,
        total_rounds: u32,
        total_usage: TokenUsage,
    },
}
