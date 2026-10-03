//! `recall_memory`: the model's way to reach the host's role-scoped dialogue
//! memory (ADR-0248).
//!
//! The tool holds a [`RoleMemory`] port rather than opening a store: *where*
//! memory lives is the host's business (ADR-0300 §1, ADR-0303 §1), and a tool
//! that resolved a database path would make the kernel own one. An embedding
//! that supplies no memory gets [`NoRoleMemory`], whose `recall` is an empty set
//! — which the tool renders as the honest "nothing found", not as an error.

use std::sync::Arc;

use async_trait::async_trait;
use nuo_wire::Tool;
use serde_json::json;

use crate::host::{NoRoleMemory, RoleMemory};

/// The port-injected tool. Constructed with the host's memory at assembly.
pub struct RecallMemoryTool {
    memory: Arc<dyn RoleMemory>,
}

impl RecallMemoryTool {
    pub fn new(memory: Arc<dyn RoleMemory>) -> Self {
        Self { memory }
    }
}

impl Default for RecallMemoryTool {
    fn default() -> Self {
        Self::new(Arc::new(NoRoleMemory))
    }
}

/// The dialogue memory, as a tool-assembly service.
///
/// A newtype rather than `Arc<dyn RoleMemory>` directly: the tool context keys
/// services by `TypeId`, and a bare `Arc<dyn Trait>` would make this port
/// collide with any other trait-object service a future tool provides.
#[derive(Clone)]
pub struct RecallMemoryService(pub Arc<dyn RoleMemory>);

impl RecallMemoryService {
    pub fn new(memory: Arc<dyn RoleMemory>) -> Self {
        Self(memory)
    }
}

impl std::fmt::Debug for RecallMemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecallMemoryService")
    }
}

#[async_trait]
impl Tool for RecallMemoryTool {
    fn name(&self) -> &str {
        "recall_memory"
    }

    fn description(&self) -> &str {
        "Recall past conversations, dialogues, and philosophical insights previously shared \
         between the user and this role. Use this to remember previous discussions, established \
         positions, user perspectives, or topics explored in earlier sessions."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query — keywords, phrases, or concepts to recall from past dialogues."
                },
                "role": {
                    "type": "string",
                    "description": "The role whose memory to search. Defaults to the current role."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (1-10, default 5).",
                    "minimum": 1,
                    "maximum": 10
                }
            },
            "required": ["query"]
        })
    }

    fn permission_label(&self) -> String {
        "Recall dialogue memory".to_string()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let query = parsed
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("Missing required parameter: query")?;

        let role = parsed
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("philosophist");

        let limit = parsed
            .get("limit")
            .and_then(|l| l.as_u64())
            .unwrap_or(5)
            .clamp(1, 10) as usize;

        let memories = self
            .memory
            .recall(role, query, limit)
            .map_err(|e| format!("failed to recall memory: {e}"))?;

        if memories.is_empty() {
            return Ok(format!(
                "No past dialogues matching \"{query}\" were found in memory for role '{role}'."
            ));
        }

        let mut output = format!(
            "### Recalled {} Dialogue Memory Entries for Role ({role}):\n\n",
            memories.len()
        );

        for (idx, mem) in memories.iter().enumerate() {
            output.push_str(&format!(
                "#### Memory Entry #{} ({} old · Recalled {})\n",
                idx + 1,
                humanize_age(mem.age_s),
                mem.score
            ));
            output.push_str(&format!("**User:** {}\n\n", mem.user_prompt.trim()));
            output.push_str(&format!(
                "**{}:** {}\n\n",
                capitalize(role),
                mem.role_response.trim()
            ));
            output.push_str("---\n\n");
        }

        Ok(output)
    }
}

/// A memory's age in the units a reader cares about, from the host's measurement.
fn humanize_age(age_s: i64) -> String {
    match age_s {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

nuo_wire::register_tool!(RecallMemoryFactory => |ctx| {
    // The host's dialogue memory arrives as a service (ADR-0300 §1); a context
    // that provides none gets the no-memory tool, whose recall is an empty set.
    let memory = ctx
        .shared::<RecallMemoryService>()
        .map(|service| Arc::clone(&service.0))
        .unwrap_or_else(|| Arc::new(NoRoleMemory));
    RecallMemoryTool::new(memory)
});

#[cfg(test)]
mod tests {
    use super::*;

    /// A host memory that answers deterministically, so the tool's rendering is
    /// exercised without a database.
    struct StubMemory(Vec<crate::host::RecalledMemory>);

    impl RoleMemory for StubMemory {
        fn record(
            &self,
            _role: &str,
            _session_id: Option<&str>,
            _prompt: &str,
            _response: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn recall(
            &self,
            _role: &str,
            _query: &str,
            _limit: usize,
        ) -> Result<Vec<crate::host::RecalledMemory>, String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn schema_is_advertised() {
        let tool = RecallMemoryTool::default();
        assert_eq!(tool.name(), "recall_memory");
        assert_eq!(tool.permission_label(), "Recall dialogue memory");
        assert!(
            tool.parameters()
                .get("properties")
                .and_then(|p| p.get("query"))
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_host_with_no_memory_reports_nothing_found_rather_than_erroring() {
        let tool = RecallMemoryTool::default();
        let text = tool
            .call(r#"{"query": "eternal recurrence"}"#)
            .await
            .expect("an empty memory is not a failure");
        assert!(text.contains("No past dialogues"), "{text}");
    }

    #[tokio::test]
    async fn a_missing_query_is_rejected() {
        let tool = RecallMemoryTool::default();
        let error = tool.call("{}").await.expect_err("query is required");
        assert!(error.contains("query"), "{error}");
    }

    #[tokio::test]
    async fn recalled_entries_render_with_their_age_and_text() {
        let tool = RecallMemoryTool::new(Arc::new(StubMemory(vec![crate::host::RecalledMemory {
            role: "philosophist".into(),
            user_prompt: "What of eternal recurrence?".into(),
            role_response: "It is the ultimate existential test.".into(),
            age_s: 7_200,
            score: 0.87,
        }])));
        let text = tool.call(r#"{"query": "recurrence"}"#).await.unwrap();
        assert!(text.contains("2h old"), "age comes from the host: {text}");
        assert!(text.contains("eternal recurrence"), "{text}");
        assert!(text.contains("ultimate existential test"), "{text}");
    }

    #[test]
    fn age_units_follow_the_scale() {
        assert_eq!(humanize_age(30), "30s");
        assert_eq!(humanize_age(120), "2m");
        assert_eq!(humanize_age(7_200), "2h");
        assert_eq!(humanize_age(172_800), "2d");
    }
}
