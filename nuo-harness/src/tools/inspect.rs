//! The `inspect` tool: demand-paged offstream epistemic memory (ADR-0262).
//!
//! Exposes out-of-band context (subagent transcripts, pruned tool results, and
//! folded causal subgraphs) to the master model via a uniform, paginated interface.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use nuo_contracts::{OffstreamRegistry, Tool, ToolOutput};

const INSPECT_DESCRIPTION: &str = "Inspect offstream epistemic memory (ADR-0262, ADR-0285). \
Retrieve detailed historical context that has exited the active model window: \
subagent transcripts ('sub:<session_id>'), pruned tool results ('call:<tool_call_id>'), \
folded compaction subgraphs ('fold:<compaction_node_id>'), or visual artifacts ('artifact:<blob_hash>'). \
Supports automatic multimodal visual rehydration for pruned image observations. \
Use action 'list' to see available artifacts, or 'read' to page through content.";

/// Tool enabling master agents to read offstream memory artifacts.
pub struct InspectTool {
    registry: Option<Arc<OffstreamRegistry>>,
    session_id: Option<String>,
}

impl InspectTool {
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
struct InspectArgs {
    action: String,
    handle: Option<String>,
    cursor: Option<String>,
    query: Option<String>,
    budget_tokens: Option<usize>,
}

#[async_trait]
impl Tool for InspectTool {
    fn name(&self) -> &str {
        "inspect"
    }

    fn description(&self) -> &str {
        INSPECT_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "read"],
                    "description": "Operation to perform: 'list' to enumerate available offstream artifacts, or 'read' to fetch paginated content."
                },
                "handle": {
                    "type": "string",
                    "description": "Target artifact handle (e.g. 'sub:ses_123', 'call:call_abc', 'fold:node_xyz'). Required for 'read'."
                },
                "cursor": {
                    "type": "string",
                    "description": "Opaque continuation cursor returned from a previous truncated 'read'. Pass verbatim to fetch the next page."
                },
                "query": {
                    "type": "string",
                    "description": "Optional substring or regex pattern to pre-filter matching lines/turns before pagination."
                },
                "budget_tokens": {
                    "type": "integer",
                    "description": "Max token budget for this read operation (default: 4096, min: 512, max: 8192)."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn permission_label(&self) -> String {
        "Inspect offstream memory".to_string()
    }

    fn permission_description(&self) -> String {
        "Read offstream context (subagent transcripts, pruned tool results, compacted history).".to_string()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: InspectArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid inspect arguments: {e}"))?;

        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| "OffstreamRegistry not configured for InspectTool".to_string())?;

        match args.action.as_str() {
            "list" => {
                let session_id = self.session_id.as_deref().unwrap_or("");
                let entries = registry.enumerate_all(session_id).await;
                if entries.is_empty() {
                    return Ok(ToolOutput::Text(
                        "No offstream artifacts recorded for this session yet.".to_string(),
                    ));
                }

                let mut out = String::from(
                    "| Handle | Status | Size | Label |\n| :--- | :--- | :--- | :--- |\n",
                );
                for entry in entries {
                    let size = entry
                        .size_tokens
                        .map(|s| format!("~{} tokens", s))
                        .unwrap_or_else(|| "unknown".to_string());
                    out.push_str(&format!(
                        "| `{}` | {} | {} | {} |\n",
                        entry.handle, entry.status, size, entry.label
                    ));
                }
                Ok(ToolOutput::Text(out))
            }
            "read" => {
                let handle = args
                    .handle
                    .as_deref()
                    .ok_or_else(|| "Missing required 'handle' for action 'read'".to_string())?;

                let budget = args.budget_tokens.unwrap_or(4096).clamp(512, 8192);

                let paged = registry
                    .read(
                        handle,
                        args.cursor.as_deref(),
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
                        "\n\n[... Truncated to fit budget ({} lines). More content available. Re-invoke inspect with handle = {:?} and cursor = {:?} to continue ...]",
                        paged.total_lines, handle, next_cursor
                    ));
                }

                Ok(ToolOutput::Text(content))
            }
            other => Err(format!(
                "Unknown action '{other}': expected 'list' or 'read'"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_contracts::{OffstreamEntry, OffstreamSource, OffstreamStatus, PagedOffstreamContent};

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
                size_tokens: Some(150),
            }])
        }

        async fn read(
            &self,
            key: &str,
            _cursor: Option<&str>,
            query: Option<&str>,
            _budget: usize,
        ) -> Result<PagedOffstreamContent, String> {
            let mut text = format!("Content for key '{key}'\nLine 1: apple\nLine 2: banana");
            if let Some(q) = query {
                text = text
                    .lines()
                    .filter(|l| l.contains(q))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            Ok(PagedOffstreamContent::new(text, None, 2))
        }
    }

    #[tokio::test]
    async fn test_inspect_list_and_read() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(DummySource)]));
        let tool = InspectTool::new(registry, "session-test");

        // 1. Test list
        let list_res = tool.call(r#"{"action":"list"}"#).await.unwrap();
        assert!(list_res.contains("`test:entry_1`"));
        assert!(list_res.contains("Dummy test entry"));

        // 2. Test read
        let read_res = tool
            .call(r#"{"action":"read","handle":"test:entry_1"}"#)
            .await
            .unwrap();
        assert!(read_res.contains("Content for key 'entry_1'"));

        // 3. Test read with query
        let query_res = tool
            .call(r#"{"action":"read","handle":"test:entry_1","query":"banana"}"#)
            .await
            .unwrap();
        assert!(query_res.contains("Line 2: banana"));
        assert!(!query_res.contains("Line 1: apple"));
    }

    #[tokio::test]
    async fn test_inspect_missing_handle_for_read() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(DummySource)]));
        let tool = InspectTool::new(registry, "session-test");

        let err = tool.call(r#"{"action":"read"}"#).await.unwrap_err();
        assert!(err.contains("Missing required 'handle'"));
    }

    #[tokio::test]
    async fn test_inspect_unknown_action() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(DummySource)]));
        let tool = InspectTool::new(registry, "session-test");

        let err = tool.call(r#"{"action":"destroy"}"#).await.unwrap_err();
        assert!(err.contains("Unknown action 'destroy'"));
    }

    #[tokio::test]
    async fn test_inspect_empty_list() {
        let registry = Arc::new(OffstreamRegistry::empty());
        let tool = InspectTool::new(registry, "session-empty");

        let res = tool.call(r#"{"action":"list"}"#).await.unwrap();
        assert!(res.contains("No offstream artifacts recorded"));
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
            Ok(PagedOffstreamContent::new("Page 1 content", Some("cursor_page_2".to_string()), 50))
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
            ).with_media(nuo_contracts::ImagePart {
                mime: "image/png".to_string(),
                data: "base64_rehydrated_png_data".to_string(),
            }))
        }
    }

    #[tokio::test]
    async fn test_inspect_continuation_notice() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(TruncatingSource)]));
        let tool = InspectTool::new(registry, "session-test");

        let res = tool
            .call(r#"{"action":"read","handle":"trunc:item_1"}"#)
            .await
            .unwrap();
        assert!(res.contains("Page 1 content"));
        assert!(res.contains("Truncated to fit budget (50 lines)"));
        assert!(res.contains("cursor = \"cursor_page_2\""));
    }

    #[tokio::test]
    async fn test_inspect_rehydrates_visual_media() {
        let registry = Arc::new(OffstreamRegistry::new(vec![Arc::new(VisualMediaSource)]));
        let tool = InspectTool::new(registry, "session-test");

        let out = tool
            .call_structured(r#"{"action":"read","handle":"media:screenshot_1"}"#)
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
