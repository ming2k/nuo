//! Bidirectional bridge between `nuo_tool::Tool` (substrate standard) and
//! `crate::domain::Tool` (application session standard), satisfying [INV-TOOL-01]
//! and ADR-0002.

use std::sync::Arc;
use async_trait::async_trait;
use crate::{HazardLevel, Tool, ToolOutput};

/// Adapts a canonical substrate [`nuo_tool::Tool`] into an application [`Tool`].
pub struct NousToolBridge {
    inner: Arc<dyn nuo_tool::Tool>,
}

pub type NuoToolBridge = NousToolBridge;

impl NousToolBridge {
    pub fn new(inner: Arc<dyn nuo_tool::Tool>) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &Arc<dyn nuo_tool::Tool> {
        &self.inner
    }
}

#[async_trait]
impl Tool for NousToolBridge {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    fn hazard_level(&self) -> HazardLevel {
        self.inner.risk_profile().into()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args_val = if arguments.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(arguments).map_err(|e| format!("invalid JSON arguments: {e}"))?
        };
        let ctx = nuo_tool::ToolContext::default();
        let output = self.inner.execute(&ctx, args_val).await
            .map_err(|e| e.to_string())?;
        if output.is_error {
            Err(output.content)
        } else {
            Ok(output.content)
        }
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let text = self.call(arguments).await?;
        Ok(ToolOutput::text(text))
    }
}

/// Convenience helper to bridge a list of `nuo_tool::Tool` into application `crate::domain::Tool`.
pub fn bridge_substrate_tools(
    tools: impl IntoIterator<Item = Arc<dyn nuo_tool::Tool>>,
) -> Vec<Arc<dyn Tool>> {
    tools
        .into_iter()
        .map(|t| Arc::new(NousToolBridge::new(t)) as Arc<dyn Tool>)
        .collect()
}

/// Adapts an application [`Tool`] into a canonical substrate [`nuo_tool::Tool`].
pub struct NuoToNousToolBridge {
    inner: Arc<dyn Tool>,
}

impl NuoToNousToolBridge {
    pub fn new(inner: Arc<dyn Tool>) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &Arc<dyn Tool> {
        &self.inner
    }
}

#[async_trait]
impl nuo_tool::Tool for NuoToNousToolBridge {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    fn risk_profile(&self) -> nuo_tool::RiskProfile {
        self.inner.hazard_level().into()
    }

    async fn execute(
        &self,
        _ctx: &nuo_tool::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<nuo_tool::ToolOutput, nuo_tool::ToolError> {
        let args_str = serde_json::to_string(&arguments)
            .map_err(|e| nuo_tool::ToolError::invalid_args(self.name(), e.to_string()))?;
        match self.inner.call(&args_str).await {
            Ok(content) => Ok(nuo_tool::ToolOutput::success(content)),
            Err(content) => Ok(nuo_tool::ToolOutput::error(content)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_tool::{RiskProfile, ToolOutput as NousOutput};

    struct DummyNousTool;

    #[async_trait]
    impl nuo_tool::Tool for DummyNousTool {
        fn name(&self) -> &str {
            "dummy_nous"
        }
        fn description(&self) -> &str {
            "A test nous tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn risk_profile(&self) -> RiskProfile {
            RiskProfile::ReadOnly
        }
        async fn execute(
            &self,
            _ctx: &nuo_tool::ToolContext,
            _args: serde_json::Value,
        ) -> Result<NousOutput, nuo_tool::ToolError> {
            Ok(NousOutput::success("hello from nuo_tool"))
        }
    }

    #[tokio::test]
    async fn bridge_adapts_nuo_tool_to_nuo_tool() {
        let nuo_tool = Arc::new(DummyNousTool);
        let bridge = NousToolBridge::new(nuo_tool);

        assert_eq!(bridge.name(), "dummy_nous");
        assert_eq!(bridge.description(), "A test nous tool");
        assert_eq!(bridge.hazard_level(), HazardLevel::Safe);

        let result = bridge.call("{}").await.unwrap();
        assert_eq!(result, "hello from nuo_tool");
    }

    #[tokio::test]
    async fn reverse_bridge_adapts_nuo_tool_to_nuo_tool() {
        let nuo_tool = Arc::new(DummyNousTool);
        let nuo_bridge: Arc<dyn Tool> = Arc::new(NousToolBridge::new(nuo_tool));
        let reverse_bridge = NuoToNousToolBridge::new(nuo_bridge);

        use nuo_tool::Tool as _;
        assert_eq!(reverse_bridge.name(), "dummy_nous");
        assert_eq!(reverse_bridge.risk_profile(), RiskProfile::ReadOnly);

        let ctx = nuo_tool::ToolContext::default();
        let res = reverse_bridge.execute(&ctx, serde_json::json!({})).await.unwrap();
        assert_eq!(res.content, "hello from nuo_tool");
        assert!(!res.is_error);
    }
}
