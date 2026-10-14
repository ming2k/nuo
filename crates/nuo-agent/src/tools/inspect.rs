//! Native tool for rehydrating offstream observations via claim-check invoice handles.

use crate::token::observation::ObservationStore;
use async_trait::async_trait;
use nuo_tool::{Result, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::json;
use std::sync::Arc;

/// Tool allowing models to inspect offstream offloaded tool observations via invoice handles.
#[derive(Clone)]
pub struct InspectTool {
    store: Arc<dyn ObservationStore>,
}

impl InspectTool {
    pub fn new(store: Arc<dyn ObservationStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for InspectTool {
    fn name(&self) -> &str {
        "inspect"
    }

    fn description(&self) -> &str {
        "Inspects offstream tool execution outputs using their claim-check invoice handle (e.g. 'call:<tool_call_id>'). Supports demand-paged reading via offset and limit."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "The invoice handle to inspect, e.g. 'call:call_xyz'"
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
            "[Inspecting '{target}' (chars {offset}..{end} of {total_len} total)]\n\n{slice}"
        )))
    }
}
