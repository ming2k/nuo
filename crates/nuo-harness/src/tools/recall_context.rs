//! The `recall_context` tool: demand-paged offstream epistemic memory retrieval (ADR-0069, ADR-0262).
//!
//! Exposes out-of-band context (subagent transcripts, folded causal subgraphs, and visual artifacts)
//! to the master model via a uniform, line-paginated interface.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::offstream::OffstreamRegistry;
use nuo_wire::{Tool, ToolOutput};

const RECALL_CONTEXT_DESCRIPTION: &str = "Recall offstream cognitive context that has exited the active model window (ADR-0069, ADR-0262, ADR-0285). \
Retrieve detailed historical context: subagent transcripts ('sub:<session_id>'), \
folded compaction checkpoints ('fold:<compaction_node_id>'), or visual media ('artifact:<blob_hash>'). \
Supports line-based deterministic pagination and optional query filtering. \
Use 'target' to specify the context handle, 'offset_lines' to page through content, and 'query' to filter lines.";

/// Tool enabling master agents to read offstream cognitive memory artifacts.
pub struct RecallContextTool {
    registry: Option<Arc<OffstreamRegistry>>,
    session_id: Option<String>,
}

impl RecallContextTool {
    pub fn new(registry: Arc<OffstreamRegistry>, session_id: impl Into<String>) -> Self {
        Self {
            registry: Some(registry),
            session_id: Some(session_id.into()),
        }
    }

    pub fn with_registry(registry: Arc<OffstreamRegistry>) -> Self {
        Self {
            registry: Some(registry),
            session_id: None,
        }
    }

    pub fn set_session_id(&mut self, session_id: impl Into<String>) {
        self.session_id = Some(session_id.into());
    }
}

#[derive(Deserialize)]
struct RecallContextArgs {
    target: String,
    offset_lines: Option<usize>,
    limit_lines: Option<usize>,
    query: Option<String>,
}

#[async_trait]
impl Tool for RecallContextTool {
    fn name(&self) -> &str {
        "recall_context"
    }

    fn description(&self) -> &str {
        RECALL_CONTEXT_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Target context handle (e.g. 'sub:ses_123', 'fold:node_xyz', 'artifact:hash'). Required."
                },
                "query": {
                    "type": "string",
                    "description": "Optional substring or regex pattern to pre-filter matching lines before pagination."
                },
                "offset_lines": {
                    "type": "integer",
                    "description": "0-based line offset to start reading from (default: 0)."
                },
                "limit_lines": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: 80, min: 20, max: 200)."
                }
            },
            "required": ["target"],
            "additionalProperties": false
        })
    }

    fn permission_label(&self) -> String {
        "Recall offstream context".to_string()
    }

    fn permission_description(&self) -> String {
        "Read offstream context (subagent transcripts, compacted history, visual artifacts).".to_string()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: RecallContextArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid recall_context arguments: {e}"))?;

        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| "OffstreamRegistry not configured for RecallContextTool".to_string())?;

        let handle = &args.target;

        // ADR-0069 [INV-RECALL-03]: Clamped budget to prevent context blowup
        let limit = args.limit_lines.unwrap_or(80).clamp(20, 200);
        let budget = (limit * 20).clamp(400, 2000);
        let cursor = args.offset_lines.map(|o| o.to_string());

        let paged = registry
            .read(
                handle,
                cursor.as_deref(),
                args.query.as_deref(),
                budget,
            )
            .await?;

        // ADR-0285: Multimodal rehydration. If the retrieved artifact carries media,
        // rehydrate it directly as ToolOutput::Image so the active round sees the pixels.
        if let Some(media) = paged.media {
            return Ok(ToolOutput::Image {
                mime: media.mime,
                data: media.data,
            });
        }

        let mut content = paged.text;
        if let Some(next_cursor) = paged.next_cursor {
            content.push_str(&format!(
                "\n\n[... Truncated to fit limit ({} total lines). More content available. Re-invoke recall_context with target = {:?} and offset_lines = {} to continue ...]",
                paged.total_lines, handle, next_cursor
            ));
        }

        Ok(ToolOutput::Text(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OffstreamEntry, OffstreamSource, OffstreamStatus, PagedOffstreamContent};

    struct DummySource;

    #[async_trait]
    impl OffstreamSource for DummySource {
        fn scheme(&self) -> &'static str {
            "test"
        }

        async fn enumerate(&self, _session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
            Ok(vec![OffstreamEntry {
                handle: "test:entry_1".to_string(),
                label: "Dummy test entry".to_string(),
                status: OffstreamStatus::Ready,
                size_tokens: Some(42),
            }])
        }

        async fn read(
            &self,
            key: &str,
            _cursor: Option<&str>,
            query: Option<&str>,
            _budget: usize,
        ) -> Result<PagedOffstreamContent, String> {
            let base = format!("Content for key '{key}'\nLine 1: apple\nLine 2: banana");
            if let Some(q) = query {
                let filtered: Vec<&str> = base.lines().filter(|l| l.contains(q)).collect();
                Ok(PagedOffstreamContent::new(filtered.join("\n"), None, 3))
            } else {
                Ok(PagedOffstreamContent::new(base, None, 3))
            }
        }
    }

    #[tokio::test]
    async fn test_recall_context_read_and_filter() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(DummySource)]));
        let tool = RecallContextTool::new(registry, "session-test");

        // 1. Test read
        let read_res = tool
            .call(r#"{"target":"test:entry_1"}"#)
            .await
            .unwrap();
        assert!(read_res.contains("Content for key 'entry_1'"));

        // 2. Test read with query
        let query_res = tool
            .call(r#"{"target":"test:entry_1","query":"banana"}"#)
            .await
            .unwrap();
        assert!(query_res.contains("Line 2: banana"));
        assert!(!query_res.contains("Line 1: apple"));
    }

    #[tokio::test]
    async fn test_recall_context_missing_target() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(DummySource)]));
        let tool = RecallContextTool::new(registry, "session-test");

        let err = tool.call(r#"{}"#).await.unwrap_err();
        assert!(err.contains("missing field `target`"));
    }

    struct TruncatingSource;

    #[async_trait]
    impl OffstreamSource for TruncatingSource {
        fn scheme(&self) -> &'static str {
            "trunc"
        }

        async fn enumerate(&self, _session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
            Ok(vec![])
        }

        async fn read(
            &self,
            _key: &str,
            _cursor: Option<&str>,
            _query: Option<&str>,
            _budget: usize,
        ) -> Result<PagedOffstreamContent, String> {
            Ok(PagedOffstreamContent::new("Page 1 content", Some("50".to_string()), 50))
        }
    }

    struct VisualMediaSource;

    #[async_trait]
    impl OffstreamSource for VisualMediaSource {
        fn scheme(&self) -> &'static str {
            "media"
        }

        async fn enumerate(&self, _session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
            Ok(vec![])
        }

        async fn read(
            &self,
            _key: &str,
            _cursor: Option<&str>,
            _query: Option<&str>,
            _budget: usize,
        ) -> Result<PagedOffstreamContent, String> {
            Ok(PagedOffstreamContent::new(
                "[Visual Media]",
                None,
                1,
            ).with_media(nuo_wire::ImagePart {
                mime: "image/png".to_string(),
                data: "base64_rehydrated_png_data".to_string(),
            }))
        }
    }

    #[tokio::test]
    async fn test_recall_context_continuation_notice() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(TruncatingSource)]));
        let tool = RecallContextTool::new(registry, "session-test");

        let res = tool
            .call(r#"{"target":"trunc:item_1"}"#)
            .await
            .unwrap();
        assert!(res.contains("Page 1 content"));
        assert!(res.contains("Truncated to fit limit (50 total lines)"));
        assert!(res.contains("offset_lines = 50"));
    }

    #[tokio::test]
    async fn test_recall_context_rehydrates_visual_media() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(VisualMediaSource)]));
        let tool = RecallContextTool::new(registry, "session-test");

        let out = tool
            .call_structured(r#"{"target":"media:screenshot_1"}"#)
            .await
            .unwrap();

        match out {
            ToolOutput::Image { mime, data } => {
                assert_eq!(mime, "image/png");
                assert_eq!(data, "base64_rehydrated_png_data");
            }
            other => panic!("expected ToolOutput::Image, got {:?}", other),
        }
    }
}
