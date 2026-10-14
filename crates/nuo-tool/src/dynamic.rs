use crate::Tool;
use crate::context::ToolContext;
use crate::error::{Result, ToolError};
use crate::output::ToolOutput;
use crate::risk::RiskProfile;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::scope::ToolScope;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type ContextToolFn = Arc<
    dyn Fn(&ToolContext, serde_json::Value) -> BoxFuture<'static, Result<ToolOutput>> + Send + Sync,
>;
type ApprovalFn = Arc<dyn Fn(&ToolContext, &serde_json::Value) -> bool + Send + Sync>;

/// Dynamic tool constructed from closures or asynchronous functions.
#[derive(Clone)]
pub struct DynamicTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    risk: RiskProfile,
    scopes: Option<Vec<ToolScope>>,
    handler: ContextToolFn,
    approval_check: Option<ApprovalFn>,
}

impl DynamicTool {
    /// Creates a dynamic tool from a simple closure receiving only arguments.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: serde_json::Value,
        handler: F,
    ) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String>> + Send + 'static,
    {
        let tool_name = name.into();
        let tool_name_captured = tool_name.clone();
        let wrapped_handler: ContextToolFn = Arc::new(move |_ctx, args| {
            let fut = handler(args);
            let name_clone = tool_name_captured.clone();
            Box::pin(async move {
                fut.await.map(ToolOutput::success).map_err(|err| match err {
                    ToolError::Custom(s) => ToolError::execution(name_clone, s),
                    other => other,
                })
            })
        });

        Self {
            name: tool_name,
            description: description.into(),
            schema,
            risk: RiskProfile::ReadOnly,
            scopes: None,
            handler: wrapped_handler,
            approval_check: None,
        }
    }

    /// Creates a dynamic tool with access to the execution context.
    pub fn with_context<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: serde_json::Value,
        handler: F,
    ) -> Self
    where
        F: Fn(ToolContext, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput>> + Send + 'static,
    {
        let tool_name = name.into();
        let tool_name_captured = tool_name.clone();
        let wrapped_handler: ContextToolFn = Arc::new(move |ctx, args| {
            let fut = handler(ctx.clone(), args);
            let name_clone = tool_name_captured.clone();
            Box::pin(async move {
                fut.await.map_err(|err| match err {
                    ToolError::Custom(s) => ToolError::execution(name_clone, s),
                    other => other,
                })
            })
        });

        Self {
            name: tool_name,
            description: description.into(),
            schema,
            risk: RiskProfile::ReadOnly,
            scopes: None,
            handler: wrapped_handler,
            approval_check: None,
        }
    }

    /// Declares the risk profile for this dynamic tool.
    pub fn with_risk(mut self, risk: RiskProfile) -> Self {
        self.risk = risk;
        self
    }

    /// Declares an explicit operational scope for this dynamic tool.
    pub fn with_scope(mut self, scope: ToolScope) -> Self {
        let mut list = self.scopes.unwrap_or_default();
        list.push(scope);
        self.scopes = Some(list);
        self
    }

    /// Declares explicit operational scopes for this dynamic tool.
    pub fn with_scopes(mut self, scopes: Vec<ToolScope>) -> Self {
        self.scopes = Some(scopes);
        self
    }

    /// Sets an approval predicate based on arguments.
    pub fn with_approval_check<P>(mut self, predicate: P) -> Self
    where
        P: Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
    {
        self.approval_check = Some(Arc::new(move |_ctx, args| predicate(args)));
        self
    }

    /// Sets an approval predicate with full access to the execution context.
    pub fn with_context_approval_check<P>(mut self, predicate: P) -> Self
    where
        P: Fn(&ToolContext, &serde_json::Value) -> bool + Send + Sync + 'static,
    {
        self.approval_check = Some(Arc::new(predicate));
        self
    }
}

#[async_trait]
impl Tool for DynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    fn risk_profile(&self) -> RiskProfile {
        self.risk
    }

    fn scopes(&self) -> Vec<ToolScope> {
        if let Some(s) = &self.scopes {
            s.clone()
        } else {
            match self.risk {
                RiskProfile::ReadOnly => vec![ToolScope::ReadOnly],
                RiskProfile::IdempotentMutation => vec![ToolScope::Workspace],
                RiskProfile::NetworkAccess => vec![ToolScope::Network],
                RiskProfile::Destructive => vec![ToolScope::Workspace, ToolScope::Execution],
                RiskProfile::ArbitraryExecution => vec![ToolScope::Execution],
            }
        }
    }

    fn requires_approval(&self, ctx: &ToolContext, arguments: &serde_json::Value) -> bool {
        self.approval_check
            .as_ref()
            .is_some_and(|check| check(ctx, arguments))
    }

    async fn execute(&self, ctx: &ToolContext, arguments: serde_json::Value) -> Result<ToolOutput> {
        (self.handler)(ctx, arguments).await
    }
}
