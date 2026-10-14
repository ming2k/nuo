use crate::context::ToolContext;
use crate::error::Result;
use crate::risk::RiskProfile;
use async_trait::async_trait;
use serde_json::Value;

/// Outcome of an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Tool execution is permitted.
    Approved,
    /// Tool execution is denied with an explanatory reason.
    Rejected { reason: String },
}

impl ApprovalDecision {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved)
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
        }
    }
}

/// Metadata about a tool invocation presented to the approval handler.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub risk_profile: RiskProfile,
}

/// Handler for authorizing sensitive or high-risk tool execution.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Evaluates whether `request` is permitted under `ctx`.
    async fn request_approval(
        &self,
        request: &ToolCallRequest,
        ctx: &ToolContext,
    ) -> Result<ApprovalDecision>;
}

/// Default approval handler that permits all tool executions.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysApprove;

#[async_trait]
impl ApprovalHandler for AlwaysApprove {
    async fn request_approval(
        &self,
        _request: &ToolCallRequest,
        _ctx: &ToolContext,
    ) -> Result<ApprovalDecision> {
        Ok(ApprovalDecision::Approved)
    }
}
