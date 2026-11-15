use crate::Tool;
use crate::context::ToolContext;
use crate::error::{Result, ToolError};
use crate::output::ToolOutput;
use crate::risk::RiskProfile;
use async_trait::async_trait;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type McpCallHandler =
    Arc<dyn Fn(&str, Value) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

pub type ContextMcpCallHandler = Arc<
    dyn Fn(&ToolContext, &str, Value) -> Pin<Box<dyn Future<Output = Result<ToolOutput>> + Send>>
        + Send
        + Sync,
>;

/// Adapter enabling external Model Context Protocol (MCP) tools to be hosted
/// natively within the Nous tool architecture.
#[derive(Clone)]
pub struct McpTool {
    server_name: String,
    tool_name: String,
    description: String,
    parameters_schema: Value,
    risk: RiskProfile,
    handler: ContextMcpCallHandler,
}

impl McpTool {
    /// Creates an MCP tool adapter using a simple function pointer or closure.
    pub fn new(
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        handler: McpCallHandler,
    ) -> Self {
        let tool_str = tool_name.into();
        let tool_name_captured = tool_str.clone();
        let wrapped_handler: ContextMcpCallHandler = Arc::new(move |_ctx, name, args| {
            let fut = handler(name, args);
            let tool_name_cloned = tool_name_captured.clone();
            Box::pin(async move {
                fut.await.map(ToolOutput::success).map_err(|err| match err {
                    ToolError::Custom(s) => ToolError::execution(tool_name_cloned, s),
                    other => other,
                })
            })
        });

        Self {
            server_name: server_name.into(),
            tool_name: tool_str,
            description: description.into(),
            parameters_schema,
            risk: RiskProfile::NetworkAccess,
            handler: wrapped_handler,
        }
    }

    /// Creates an MCP tool adapter with cancellation and context awareness.
    pub fn with_context_handler(
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        handler: ContextMcpCallHandler,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            tool_name: tool_name.into(),
            description: description.into(),
            parameters_schema,
            risk: RiskProfile::NetworkAccess,
            handler,
        }
    }

    pub fn with_risk(mut self, risk: RiskProfile) -> Self {
        self.risk = risk;
        self
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    fn risk_profile(&self) -> RiskProfile {
        self.risk
    }

    async fn execute(&self, ctx: &ToolContext, arguments: Value) -> Result<ToolOutput> {
        ctx.check_cancelled(self.name())?;
        (self.handler)(ctx, &self.tool_name, arguments).await
    }
}
