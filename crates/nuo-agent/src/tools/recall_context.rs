//! Native tool for recalling offstream observations and cognitive contexts (ADR-0069, ADR-0262).

use crate::token::observation::ObservationStore;
use async_trait::async_trait;
use nuo_tool::{Result, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::json;
use std::sync::Arc;

/// Tool allowing models to recall offstream offloaded observations.
#[derive(Clone)]
pub struct RecallContextTool {
    store: Arc<dyn ObservationStore>,
}

impl RecallContextTool {
    pub fn new(store: Arc<dyn ObservationStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for RecallContextTool {
    fn name(&self) -> &str {
        "recall_context"
    }

    fn description(&self) -> &str {
        "Recalls offstream tool execution observations and cognitive context. Supports demand-paged reading via offset and limit."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "The context handle to recall, e.g. 'sub:ses_xyz' or 'fold:node_xyz'"
                },
                "offset": {
                    "type": "integer",
                    "description": "Character offset to begin reading from (default: 0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of characters to read (default: 2000, max: 8000)"
                }
            },
            "required": ["target"],
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput> {
        let target = arguments
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::execution(self.name(), "missing required `target`"))?;

        let offset = arguments
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(2000)
            .min(8000) as usize;

        let total_len = self.store.get_length(target).await.unwrap_or(0);
        let slice = self
            .store
            .fetch_slice(target, offset, limit)
            .await
            .map_err(|err| ToolError::execution(self.name(), err.to_string()))?;

        let end = (offset + slice.len()).min(total_len);
        Ok(ToolOutput::success(format!(
            "[Recalling '{target}' (chars {offset}..{end} of {total_len} total)]\n\n{slice}"
        )))
    }
}
