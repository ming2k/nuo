//! Concrete [`OffstreamSource`] implementations for `nuo-server` (ADR-0069).
//!
//! Provides lock-free offstream readers for:
//! - `sub:`: Subagent session transcripts and trajectories
//! - `fold:`: Folded causal subgraphs behind compaction horizons
//! - `artifact:`: Multimodal media and visual artifacts
use std::sync::Arc;

use async_trait::async_trait;
use nuo_harness::runtime::offstream::{
    OffstreamEntry, OffstreamRegistry, OffstreamSource, OffstreamStatus, PagedOffstreamContent,
};
use nuo_persistence::blobs::BlobStore;
use nuo_persistence::db::get_persistence_handle;
use nuo_protocol::{FactPayload, Role};

/// Pagination and text filtering helper for offstream sources.
pub fn paginate_text(
    full_text: &str,
    cursor: Option<&str>,
    query: Option<&str>,
    budget_tokens: usize,
) -> PagedOffstreamContent {
    let lines: Vec<&str> = full_text.lines().collect();
    let total_lines = lines.len();

    // 1. Filter lines if query is specified
    let filtered_lines: Vec<(usize, &str)> = if let Some(q) = query.filter(|s| !s.trim().is_empty())
    {
        let q_lower = q.to_lowercase();
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&q_lower))
            .map(|(idx, line)| (idx + 1, *line))
            .collect()
    } else {
        lines
            .iter()
            .enumerate()
            .map(|(idx, line)| (idx + 1, *line))
            .collect()
    };

    // 2. Resolve cursor offset
    let start_idx = cursor.and_then(|c| c.parse::<usize>().ok()).unwrap_or(0);

    let mut accumulated_tokens = 0;
    let mut collected = Vec::new();
    let mut next_cursor = None;

    for (pos, (line_num, line)) in filtered_lines.iter().enumerate().skip(start_idx) {
        let line_fmt = if query.is_some() {
            format!("L{line_num}: {line}")
        } else {
            line.to_string()
        };

        let line_tokens = nuo_protocol::tokenizer::count_tokens(&line_fmt).max(1);
        if accumulated_tokens + line_tokens > budget_tokens && !collected.is_empty() {
            next_cursor = Some(pos.to_string());
            break;
        }

        accumulated_tokens += line_tokens;
        collected.push(line_fmt);
    }

    PagedOffstreamContent::new(collected.join("\n"), next_cursor, total_lines)
}

/// Source for inspecting subagent session lineages (`sub:<session_id>`).
pub struct SubagentSource {
    session_id: String,
}

impl SubagentSource {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

#[async_trait]
impl OffstreamSource for SubagentSource {
    fn scheme(&self) -> &'static str {
        "sub"
    }

    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
        let sid = if session_id.is_empty() {
            &self.session_id
        } else {
            session_id
        };

        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let subagents = reader
            .list_subagent_sessions(sid)
            .map_err(|e| e.to_string())?;

        let mut entries = Vec::new();
        for sub in subagents {
            let mut status = OffstreamStatus::Ready;
            let mut size_tokens = None;

            // Inspect child session termination status dynamically without modifying SubagentRef
            if let Ok(Some(child)) = reader.load_thread(&sub.id) {
                let total_text: usize = child
                    .all_facts()
                    .iter()
                    .map(|fact| match &fact.payload {
                        FactPayload::Message { message } => message.content.len(),
                        FactPayload::Observation { lifecycle, .. } => match lifecycle {
                            nuo_protocol::ObservationLifecycle::Raw { content } => content.len(),
                            _ => 0,
                        },
                        FactPayload::Compaction { summary, .. } => summary.len(),
                        _ => 0,
                    })
                    .sum();
                size_tokens = Some(total_text.div_ceil(4));

                for fact in child.all_facts() {
                    if let FactPayload::Termination { reason, .. } = &fact.payload {
                        status = match reason {
                            nuo_protocol::TerminationReason::UserInterrupt => {
                                OffstreamStatus::Interrupted
                            }
                            nuo_protocol::TerminationReason::FatalError { .. } => {
                                OffstreamStatus::Failed
                            }
                            _ => OffstreamStatus::Ready,
                        };
                        break;
                    }
                }
            }

            let label = sub
                .title
                .or(sub.last_user_prompt)
                .unwrap_or_else(|| "Subagent session".to_string());

            entries.push(OffstreamEntry {
                handle: format!("sub:{}", sub.id),
                label,
                status,
                size_tokens,
            });
        }

        Ok(entries)
    }

    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        if let Ok(Some(thread)) = reader.load_thread(key) {
            let branch = thread.state.active_branch.clone();
            let facts = thread.path(&branch).unwrap_or_default();

            let mut formatted = format!("# Subagent Session Transcript ({key})\n\n");
            for fact in facts {
                match &fact.payload {
                    FactPayload::Message { message } => {
                        let role_label = match message.role {
                            Role::User => "User",
                            Role::Assistant => "Assistant",
                            Role::Tool => "Tool Result",
                            Role::System => "System",
                        };
                        formatted.push_str(&format!("### [{role_label}]\n{}\n\n", message.content));
                        if let Some(calls) = &message.tool_calls {
                            for c in calls {
                                formatted.push_str(&format!(
                                    "-> Tool Call: {}({})\n\n",
                                    c.name, c.arguments
                                ));
                            }
                        }
                    }
                    FactPayload::Observation {
                        call_id,
                        tool_name,
                        lifecycle,
                        ..
                    } => {
                        let message = lifecycle.lower_to_message(call_id, tool_name);
                        formatted.push_str(&format!(
                            "### [Tool Result ({tool_name})]\n{}\n\n",
                            message.content
                        ));
                    }
                    FactPayload::Compaction { summary, .. } => {
                        formatted.push_str(&format!("### [Compacted Checkpoint]\n{summary}\n\n"));
                    }
                    FactPayload::Termination {
                        reason,
                        partial_output,
                        ..
                    } => {
                        formatted.push_str(&format!(
                            "### [Execution Terminated: {:?}]\n{}\n\n",
                            reason,
                            partial_output.as_deref().unwrap_or("(no partial output)")
                        ));
                    }
                    FactPayload::SystemNotice {
                        content, source, ..
                    } => {
                        formatted.push_str(&format!("### [Notice from {source}]\n{content}\n\n"));
                    }
                }
            }
            return Ok(paginate_text(&formatted, cursor, query, budget_tokens));
        }

        Err(format!("Subagent session '{key}' not found"))
    }
}

/// Source for inspecting folded causal subgraphs (`fold:<compaction_node_id>`).
pub struct FoldedCausalSource {
    session_id: String,
}

impl FoldedCausalSource {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

#[async_trait]
impl OffstreamSource for FoldedCausalSource {
    fn scheme(&self) -> &'static str {
        "fold"
    }

    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
        let sid = if session_id.is_empty() {
            &self.session_id
        } else {
            session_id
        };

        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let thread = match reader.load_thread(sid) {
            Ok(Some(thread)) => thread,
            _ => return Ok(Vec::new()),
        };

        let mut entries = Vec::new();
        for fact in thread.all_facts() {
            if let FactPayload::Compaction {
                summary,
                tokens_before,
                ..
            } = &fact.payload
            {
                let first_line = summary.lines().next().unwrap_or("Compaction checkpoint");
                entries.push(OffstreamEntry {
                    handle: format!("fold:{}", fact.id.0),
                    label: first_line.chars().take(80).collect(),
                    status: OffstreamStatus::Compacted,
                    size_tokens: Some(*tokens_before),
                });
            }
        }

        Ok(entries)
    }

    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let thread = reader
            .load_thread(&self.session_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session '{}' not found", self.session_id))?;

        let target_fact = thread
            .graph
            .get(&nuo_protocol::FactId::from_raw(key))
            .ok_or_else(|| format!("Compaction fact '{key}' not found"))?;

        let mut formatted = format!("# Folded Causal Checkpoint ({key})\n\n");
        if let FactPayload::Compaction {
            summary,
            tokens_before,
            read_files,
            modified_files,
            ..
        } = &target_fact.payload
        {
            formatted.push_str(&format!(
                "Tokens before compaction: ~{tokens_before}\n\n## Summary\n{summary}\n\n"
            ));
            if !modified_files.is_empty() {
                formatted.push_str(&format!("Modified Files: {}\n", modified_files.join(", ")));
            }
            if !read_files.is_empty() {
                formatted.push_str(&format!("Consulted Files: {}\n\n", read_files.join(", ")));
            }
        }

        // Trace upstream parent nodes that were folded into this compaction
        let mut curr_parent = target_fact.parent_id.as_ref();
        let mut folded_nodes = Vec::new();
        while let Some(pid) = curr_parent {
            if let Some(parent) = thread.graph.get(pid) {
                if matches!(parent.payload, FactPayload::Compaction { .. }) {
                    break; // stop at previous compaction boundary
                }
                folded_nodes.push(parent);
                curr_parent = parent.parent_id.as_ref();
            } else {
                break;
            }
        }
        folded_nodes.reverse();

        if !folded_nodes.is_empty() {
            formatted.push_str("## Folded Dialogue History\n\n");
            for fact in folded_nodes {
                match &fact.payload {
                    FactPayload::Message { message } => {
                        let role_str = match message.role {
                            Role::User => "User",
                            Role::Assistant => "Assistant",
                            Role::Tool => "Tool Result",
                            Role::System => "System",
                        };
                        formatted.push_str(&format!("### [{role_str}]\n{}\n\n", message.content));
                    }
                    FactPayload::Observation {
                        call_id,
                        tool_name,
                        lifecycle,
                        ..
                    } => {
                        let message = lifecycle.lower_to_message(call_id, tool_name);
                        formatted.push_str(&format!(
                            "### [Tool Result ({tool_name})]\n{}\n\n",
                            message.content
                        ));
                    }
                    _ => {}
                }
            }
        }

        Ok(paginate_text(&formatted, cursor, query, budget_tokens))
    }
}

/// Source for inspecting CAS blobs and visual artifacts (`artifact:<sha256>`).
pub struct ArtifactSource {
    _session_id: String,
    blob_store: BlobStore,
}

impl ArtifactSource {
    pub fn new(session_id: impl Into<String>, blob_store: BlobStore) -> Self {
        Self {
            _session_id: session_id.into(),
            blob_store,
        }
    }
}

#[async_trait]
impl OffstreamSource for ArtifactSource {
    fn scheme(&self) -> &'static str {
        "artifact"
    }

    async fn enumerate(&self, _session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
        Ok(Vec::new())
    }

    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let Some(bytes) = self.blob_store.get(key) else {
            return Err(format!("Blob artifact '{key}' not found in CAS store"));
        };

        // Detect visual media format (PNG, JPEG, WebP, GIF)
        if let Some(mime) = detect_image_mime(&bytes) {
            use base64::{Engine, engine::general_purpose::STANDARD};
            let b64 = STANDARD.encode(&bytes);
            // Rehydrated screenshots are replayed through every subsequent
            // provider request; bound them at the same ingest boundary as a
            // pasted attachment. Non-raster or already-small payloads fall
            // through unchanged.
            let (mime, b64) = nuo_host::image::normalize_base64_image(&b64, mime)
                .map(|(data, mime)| (mime, data))
                .unwrap_or_else(|| (mime.to_string(), b64));
            let raw_len = b64.len() * 3 / 4;
            return Ok(PagedOffstreamContent::new(
                format!(
                    "[Rehydrated visual artifact: mime={mime} (~{raw_len} bytes) | handle: artifact:{key}]"
                ),
                None,
                1,
            )
            .with_media(nuo_protocol::ImagePart {
                mime,
                data: b64,
            }));
        }

        // Fallback: try UTF-8 text
        if let Ok(text) = String::from_utf8(bytes.clone()) {
            return Ok(paginate_text(&text, cursor, query, budget_tokens));
        }

        // Binary non-image
        let summary = format!(
            "[Binary artifact: {} bytes | handle: artifact:{key}]",
            bytes.len()
        );
        Ok(PagedOffstreamContent::new(summary, None, 1))
    }
}

fn detect_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Helper to build an [`OffstreamRegistry`] for a session.
pub fn build_offstream_registry(session_id: &str, blob_store: BlobStore) -> Arc<OffstreamRegistry> {
    let mut registry = OffstreamRegistry::empty();
    registry.register(Arc::new(SubagentSource::new(session_id)));
    registry.register(Arc::new(ArtifactSource::new(session_id, blob_store)));
    registry.register(Arc::new(FoldedCausalSource::new(session_id)));
    Arc::new(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paginate_text_filtering_and_cursor() {
        let text = "Line 1: setup\nLine 2: error in test\nLine 3: clean\nLine 4: error in build\nLine 5: done";

        // Query filter: only matching lines
        let res = paginate_text(text, None, Some("error"), 1000);
        assert_eq!(res.total_lines, 5);
        assert!(res.text.contains("L2: Line 2: error in test"));
        assert!(res.text.contains("L4: Line 4: error in build"));
        assert!(!res.text.contains("Line 1: setup"));
        assert!(res.next_cursor.is_none());

        // Low budget triggering cursor pagination
        let paged1 = paginate_text(text, None, None, 4);
        assert!(paged1.next_cursor.is_some());
        let next = paged1.next_cursor.as_deref().unwrap();

        let paged2 = paginate_text(text, Some(next), None, 1000);
        assert!(!paged2.text.is_empty());
    }

    #[tokio::test]
    async fn test_artifact_source_reads_and_rehydrates_image() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_store = BlobStore::new(tmp.path().to_path_buf());
        let fake_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfakeimagebytes";
        let hash = blob_store.put(fake_png).unwrap();

        let source = ArtifactSource::new("test-session", blob_store);
        let res = source.read(&hash, None, None, 1000).await.unwrap();

        assert!(res.media.is_some());
        let media = res.media.unwrap();
        assert_eq!(media.mime, "image/png");
        assert!(!media.data.is_empty());
    }
}
