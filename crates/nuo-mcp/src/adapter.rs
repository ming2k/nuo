use crate::client::McpClient;
use crate::protocol::McpToolDefinition;
use async_trait::async_trait;
use nuo_tool::{RiskProfile, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::Value;
use std::sync::Arc;

/// Bridges an external MCP-discovered tool to the native `nuo_tool::Tool` contract.
pub struct McpNativeTool {
    client: Arc<McpClient>,
    def: McpToolDefinition,
}

impl McpNativeTool {
    pub fn new(client: Arc<McpClient>, def: McpToolDefinition) -> Self {
        Self { client, def }
    }
}

#[async_trait]
impl Tool for McpNativeTool {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        self.def.description.as_deref().unwrap_or("")
    }

    fn parameters_schema(&self) -> Value {
        self.def.input_schema.clone()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::NetworkAccess
    }

    async fn execute(&self, ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        ctx.check_cancelled(self.name())?;

        let call_fut = self.client.call_tool(self.name(), arguments);

        let call_res = tokio::select! {
            _ = ctx.cancel_token.cancelled() => {
                return Err(ToolError::cancelled(self.name()));
            }
            res = call_fut => {
                res.map_err(|err| ToolError::execution(self.name(), err.to_string()))?
            }
        };

        let combined = call_res.to_combined_text();
        if call_res.is_error == Some(true) {
            Ok(ToolOutput::error(combined))
        } else {
            Ok(ToolOutput::success(combined))
        }
    }
}
