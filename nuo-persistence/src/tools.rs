//! Canonical persistence and role memory tools for cognitive agents.

use std::sync::Arc;
use async_trait::async_trait;
use nuo_tool::{Result as ToolResult, RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolScope};
use serde_json::{Value, json};

use crate::role_memory::RoleMemoryStore;

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
        "recall_memory"
    }

    fn description(&self) -> &str {
        "Recall past conversations, dialogues, and insights previously shared between the user and this role."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The topic, concept, question, or keyword to recall from past dialogues with the user"
                },
                "role": {
                    "type": "string",
                    "description": "The specific role boundary to search memories for (default: current role)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "description": "Maximum number of past dialogue memories to retrieve (default 5, max 10)"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::ReadOnly]
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> ToolResult<ToolOutput> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `query`"))?;

        let role = arguments
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or(&self.default_role);

        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(1, 10) as usize;

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

/// Registers persistence tools into a ToolRegistry.
pub fn register_persistence_tools(registry: &mut nuo_tool::ToolRegistry, store: Arc<RoleMemoryStore>) {
    for tool in create_persistence_tools(store) {
        registry.register_arc(tool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let res = tool
            .execute(&t_ctx, json!({"query": "rust tools"}))
            .await
            .unwrap();

        assert!(!res.is_error);
        assert!(res.content.contains("nuo_tool::Tool"));
    }
}
