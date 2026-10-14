//! Offstream epistemic memory contracts and registry (ADR-0262).
//!
//! Defines the pluggable [`OffstreamSource`] trait, entry/pagination DTOs,
//! and [`OffstreamRegistry`] for retrieving historical context (subagent
//! trajectories, pruned tool outputs, and folded causal subgraphs) that has
//! exited the active model window.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Operational status of an offstream artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffstreamStatus {
    /// Artifact settled successfully and available for full inspection.
    Ready,
    /// Artifact archived in long-term cold storage.
    Archived,
    /// Tool result whose original payload was pruned to relieve context pressure.
    Pruned,
    /// Dialogue turns folded behind a causal compaction horizon.
    Compacted,
    /// Subagent execution or background task failed with an error.
    Failed,
    /// Execution was interrupted by the user (Ctrl+C).
    Interrupted,
}

impl std::fmt::Display for OffstreamStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready => write!(f, "ready"),
            Self::Archived => write!(f, "archived"),
            Self::Pruned => write!(f, "pruned"),
            Self::Compacted => write!(f, "compacted"),
            Self::Failed => write!(f, "failed"),
            Self::Interrupted => write!(f, "interrupted"),
        }
    }
}

/// Catalog entry describing an addressable offstream artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffstreamEntry {
    /// Canonical URI handle, e.g. "sub:ses_123", "call:call_abc", "fold:node_xyz".
    pub handle: String,
    /// Human/model-readable label describing the content.
    pub label: String,
    /// Operational status of the artifact.
    pub status: OffstreamStatus,
    /// Estimated size in tokens to guide model recall decisions.
    pub size_tokens: Option<usize>,
}

/// Paginated content returned by an offstream read operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagedOffstreamContent {
    /// Extracted text content.
    pub text: String,
    /// Opaque continuation cursor. `None` indicates end of content.
    pub next_cursor: Option<String>,
    /// Total lines in the retrieved segment.
    pub total_lines: usize,
    /// Optional rehydrated media payload (e.g. visual image) for multimodal artifacts (ADR-0285).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<crate::ImagePart>,
}

impl PagedOffstreamContent {
    pub fn new(text: impl Into<String>, next_cursor: Option<String>, total_lines: usize) -> Self {
        Self {
            text: text.into(),
            next_cursor,
            total_lines,
            media: None,
        }
    }

    pub fn with_media(mut self, media: crate::ImagePart) -> Self {
        self.media = Some(media);
        self
    }
}

/// Pluggable offstream data source provider (ADR-0262).
#[async_trait]
pub trait OffstreamSource: Send + Sync {
    /// The canonical scheme prefix handled by this source (e.g. "sub", "call", "fold").
    fn scheme(&self) -> &'static str;

    /// Enumerate all available entries within the scope of the given session.
    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String>;

    /// Read and optionally filter/paginate content identified by key.
    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String>;
}

/// Registry coordinating registered [`OffstreamSource`] implementations.
#[derive(Clone, Default)]
pub struct OffstreamRegistry {
    sources: Vec<Arc<dyn OffstreamSource>>,
}

impl OffstreamRegistry {
    pub fn new(sources: Vec<Arc<dyn OffstreamSource>>) -> Self {
        Self { sources }
    }

    pub fn empty() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    pub fn register(&mut self, source: Arc<dyn OffstreamSource>) {
        self.sources.push(source);
    }

    pub fn get_source(&self, scheme: &str) -> Option<Arc<dyn OffstreamSource>> {
        self.sources.iter().find(|s| s.scheme() == scheme).cloned()
    }

    pub async fn enumerate_all(&self, session_id: &str) -> Vec<OffstreamEntry> {
        let mut all = Vec::new();
        for source in &self.sources {
            if let Ok(entries) = source.enumerate(session_id).await {
                all.extend(entries);
            }
        }
        all
    }

    pub async fn read(
        &self,
        handle: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let (scheme, key) = handle
            .split_once(':')
            .ok_or_else(|| format!("Invalid handle '{handle}': expected '<scheme>:<key>'"))?;

        let source = self
            .get_source(scheme)
            .ok_or_else(|| format!("No offstream source registered for scheme '{scheme}'"))?;

        source.read(key, cursor, query, budget_tokens).await
    }
}
