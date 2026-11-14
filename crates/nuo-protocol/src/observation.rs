//! Tool-observation vocabulary shared by durable facts, request views, and UI
//! projections.
//!
//! These types are not a conversation model. They describe one tool execution's
//! physical shape (metrics), its retention lifecycle, and the typed reasons a
//! fact may be retired or terminated. They live here so `ThreadFact` and every
//! projection can name them without depending on the retired `SessionIR`.
use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Physical metrics captured for a tool execution observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObservationMetrics {
    pub original_bytes: usize,
    pub original_lines: usize,
    pub original_tokens: usize,
}

/// Lifecycle states of a tool execution observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ObservationLifecycle {
    /// State 1: raw payload retained in working context (within the token
    /// admission budget).
    Raw { content: String },
    /// State 2: active truncated (oversized at ingestion). Retains a bounded
    /// head and tail preview with elision metrics, plus the invariant invoice
    /// handle.
    ActiveTruncated {
        head_preview: String,
        tail_preview: String,
        retained_tokens: usize,
        omitted_bytes: usize,
    },
    /// State 3: retired (causally superseded or garbage-collected). The payload
    /// body is cleared to a lightweight tombstone, preserving causal intent and
    /// the invoice.
    Retired {
        reason: InvalidationReason,
        original_tokens: usize,
        original_lines: usize,
    },
}

impl ObservationLifecycle {
    /// Materialize the provider-facing tool message body.
    ///
    /// This is a projection step. It never mutates the observation fact.
    pub fn lower_to_message(&self, call_id: &str, tool_name: &str) -> Message {
        let content = match self {
            Self::Raw { content } => content.clone(),
            Self::ActiveTruncated {
                head_preview,
                tail_preview,
                omitted_bytes,
                ..
            } => {
                format!(
                    "{head_preview}\n\n[... {omitted_bytes} bytes elided to relieve context budget]\n\n{tail_preview}"
                )
            }
            Self::Retired {
                reason,
                original_tokens,
                original_lines,
            } => {
                format!(
                    "{} {tool_name} ({original_lines} lines, {original_tokens} tokens) — reason: {}]",
                    crate::pressure::CLEARED_TOOL_PREFIX,
                    reason.summary()
                )
            }
        };
        let mut msg = Message::new(crate::Role::Tool, content);
        msg.tool_call_id = Some(call_id.to_string());
        msg
    }
}

/// Causal invalidation reason for transitioning an observation to retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvalidationReason {
    /// File read invalidated by a subsequent write/edit to the same resource.
    SupersededByMutation {
        target_resource: String,
        mutator_node_id: String,
    },
    /// Diagnostic/test execution invalidated by a subsequent run.
    SupersededByExecution { successor_node_id: String },
    /// Reclaimed under quantum budget pressure after recency quarantine
    /// expiration.
    BudgetRelief,
}

impl InvalidationReason {
    pub fn summary(&self) -> &'static str {
        match self {
            Self::SupersededByMutation { .. } => "superseded by mutation",
            Self::SupersededByExecution { .. } => "superseded by execution",
            Self::BudgetRelief => "budget relief",
        }
    }
}

/// Causality classifier for execution terminations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminationReason {
    /// User intentionally stopped execution (Ctrl+C, cancel verb).
    UserInterrupt,
    /// Wall-clock or per-step timeout exceeded.
    Timeout,
    /// Fatal provider, network, or sandbox failure.
    FatalError { error: String },
    /// Superseded by a newer user turn before completion.
    Superseded,
}

/// Trust classification of a fact or blob.
///
/// Trust is a property of the source, never of the MIME type or the projection
/// that reads it. An untrusted blob is never promoted to trusted policy text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    #[default]
    Trusted,
    Untrusted,
}
