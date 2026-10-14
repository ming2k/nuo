//! Canonical persistence and role memory tools for cognitive agents.

use std::sync::Arc;
use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolScope, ToolSchema,
};
use serde::Deserialize;
use serde_json::Value;

use crate::role_memory::RoleMemoryStore;

/// Typed parameters for [`RecallMemoryTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct RecallMemoryArgs {
    #[tool(desc = "The topic, concept, question, or keyword to recall from past dialogues with the user")]
    pub query: String,
    #[tool(desc = "The specific role boundary to search memories for (default: current role)")]
    pub role: Option<String>,
    #[tool(desc = "Maximum number of past dialogue memories to retrieve (default 5, max 10)")]
    pub limit: Option<usize>,
}

/// Queries the agent's long-term role dialogue memory.
pub struct RecallMemoryTool {
    store: Arc<RoleMemoryStore>,
    default_role: String,
}

impl RecallMemoryTool {
    pub fn new(store: Arc<RoleMemoryStore>) -> Self {
        Self {
            store,
            default_role: "assistant".to_string(),
        }
    }

    pub fn with_default_role(store: Arc<RoleMemoryStore>, default_role: impl Into<String>) -> Self {
        Self {
            store,
            default_role: default_role.into(),
        }
    }
}

#[async_trait]
impl Tool for RecallMemoryTool {
    fn name(&self) -> &str {
        BuiltinTool::RecallMemory.as_str()
    }

    fn description(&self) -> &str {
        "Recall past conversations, dialogues, and insights previously shared between the user and this role."
    }

    fn parameters_schema(&self) -> Value {
        RecallMemoryArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: RecallMemoryArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let query = &args.query;
        let role = args.role.as_deref().unwrap_or(&self.default_role);
        let limit = args.limit.unwrap_or(5).clamp(1, 10);

        let memories = self.store.recall(role, query, limit).map_err(|err| {
            ToolError::execution(self.name(), format!("failed to recall memory: {err}"))
        })?;

        if memories.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No past memories found for query `{query}` in role `{role}`."
            )));
        }

        let mut lines = vec![format!("Recalled {} relevant dialogue memories:", memories.len())];
        for (idx, mem) in memories.iter().enumerate() {
            lines.push(format!(
                "[{}] (retention: {:.0}%, score: {:.2})\nUser: {}\nAssistant: {}",
                idx + 1,
                mem.retention * 100.0,
                mem.score,
                mem.user_prompt,
                mem.role_response
            ));
        }

        Ok(ToolOutput::success(lines.join("\n\n")))
    }
}

/// Creates the standard persistence tools.
pub fn create_persistence_tools(store: Arc<RoleMemoryStore>) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(RecallMemoryTool::new(store))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_recall_memory_tool() {
        let store = Arc::new(RoleMemoryStore::open_in_memory().unwrap());
        store
            .record_dialogue(
                "developer",
                Some("session-1"),
                "How do we write rust tools?",
                "We implement nuo_tool::Tool with async execute",
            )
            .unwrap();

        let tool = RecallMemoryTool::with_default_role(store, "developer");
        let t_ctx = ToolContext::default();

        // Validate derived schema
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["query"]));
        assert_eq!(schema["additionalProperties"], false);

        let res = tool
            .execute(&t_ctx, json!({"query": "rust tools"}))
            .await
            .unwrap();

        assert!(!res.is_error());
        assert!(res.content().contains("nuo_tool::Tool"));

        // Validate typed rejection on missing query
        let err = tool.execute(&t_ctx, json!({})).await.unwrap_err();
        assert!(err.to_string().contains("invalid arguments"));
    }
}
