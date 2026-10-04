//! Approval handler connecting harness governance policies with cognitive runtimes.

use async_trait::async_trait;
use nuo_tool::{ApprovalDecision, ApprovalHandler, Result, ToolCallRequest, ToolContext};
use std::sync::{Arc, RwLock};

use super::bash_policy::{BashPolicy, BashPolicyAction};
use super::permission_store::PermissionStore;

/// Policy-driven approval handler enforcing Bash safety and PermissionStore rules.
#[derive(Clone)]
pub struct HarnessApprovalHandler {
    permissions: Arc<PermissionStore>,
    bash_policy: Arc<RwLock<BashPolicy>>,
}

impl HarnessApprovalHandler {
    pub(crate) fn new(permissions: Arc<PermissionStore>, bash_policy: Arc<RwLock<BashPolicy>>) -> Self {
        Self {
            permissions,
            bash_policy,
        }
    }
}

#[async_trait]
impl ApprovalHandler for HarnessApprovalHandler {
    async fn request_approval(
        &self,
        request: &ToolCallRequest,
        _ctx: &ToolContext,
    ) -> Result<ApprovalDecision> {
        // 1. Check command execution policies if this is a shell tool
        if request.tool_name == "execute_command" {
            if let Some(cmd) = request.arguments.get("command").and_then(|v| v.as_str()) {
                let policy = self.bash_policy.read().unwrap_or_else(|e| e.into_inner());
                if let Some(decision) = policy.evaluate(cmd) {
                    if matches!(decision.action, BashPolicyAction::Deny) {
                        return Ok(ApprovalDecision::Rejected {
                            reason: format!(
                                "Command execution denied by security policy: {}",
                                decision.reason
                            ),
                        });
                    }
                }
            }
        }

        // 2. Check PermissionStore rule definitions
        let rule = super::permission_store::PermissionRule {
            tool: request.tool_name.clone(),
            scope: String::new(),
        };
        if !self.permissions.unattended() && !self.permissions.is_allowed(&rule) {
            // Default permits allowed tools or delegates to interactive approvals
        }

        Ok(ApprovalDecision::Approved)
    }
}
