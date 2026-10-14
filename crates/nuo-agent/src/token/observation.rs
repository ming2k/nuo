//! Content-addressed observation backing store and claim-check invoice persistence.
//!
//! When tool execution outputs exceed context watermarks, the raw payload is offloaded
//! into an [`ObservationStore`] and replaced in working context with an immutable
//! invoice handle (`call:<call_id>`).

use crate::error::{AgentError, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Persistence interface for offstream raw tool execution payloads.
#[async_trait]
pub trait ObservationStore: Send + Sync {
    /// Stores the raw observation payload under an immutable invoice handle (e.g. `call:<call_id>`).
    async fn store(&self, handle: &str, raw_output: String) -> Result<()>;

    /// Fetches a character slice of the stored observation starting at `offset` up to `limit` chars.
    async fn fetch_slice(&self, handle: &str, offset: usize, limit: usize) -> Result<String>;

    /// Returns the total length (in characters) of the stored observation if it exists.
    async fn get_length(&self, handle: &str) -> Option<usize>;

    /// Checks whether an observation is available for the given handle.
    async fn contains(&self, handle: &str) -> bool;
}

/// In-memory implementation of [`ObservationStore`] backed by `Arc<RwLock<HashMap>>`.
#[derive(Clone, Default)]
pub struct InMemoryObservationStore {
    store: Arc<RwLock<HashMap<String, String>>>,
}

impl InMemoryObservationStore {
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl ObservationStore for InMemoryObservationStore {
    async fn store(&self, handle: &str, raw_output: String) -> Result<()> {
        let mut guard = self.store.write().await;
        guard.insert(handle.to_string(), raw_output);
        Ok(())
    }

    async fn fetch_slice(&self, handle: &str, offset: usize, limit: usize) -> Result<String> {
        let guard = self.store.read().await;
        let content = guard.get(handle).ok_or_else(|| {
            AgentError::Tool(
                "inspect".into(),
                format!("invoice handle `{handle}` not found"),
            )
        })?;

        if offset >= content.len() {
            return Ok(String::new());
        }

        let slice = content.chars().skip(offset).take(limit).collect::<String>();

        Ok(slice)
    }

    async fn get_length(&self, handle: &str) -> Option<usize> {
        self.store.read().await.get(handle).map(String::len)
    }

    async fn contains(&self, handle: &str) -> bool {
        self.store.read().await.contains_key(handle)
    }
}

/// Zero-external-dependency local filesystem backing store for observation payloads.
#[derive(Clone)]
pub struct FileObservationStore {
    root_dir: PathBuf,
}

impl FileObservationStore {
    /// Creates a file store rooted at `dir`. Creates the directory if it does not exist.
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(|e| {
            AgentError::Session(format!("failed to initialize observation directory: {e}"))
        })?;
        Ok(Self { root_dir: root })
    }

    /// Creates a file store in the system temp directory.
    pub fn in_temp_dir() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("nuo_observations_{}", uuid::Uuid::new_v4()));
        Self::new(dir)
    }

    fn file_path_for(&self, handle: &str) -> PathBuf {
        let sanitized: String = handle
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root_dir.join(format!("{sanitized}.blob"))
    }
}

#[async_trait]
impl ObservationStore for FileObservationStore {
    async fn store(&self, handle: &str, raw_output: String) -> Result<()> {
        let path = self.file_path_for(handle);
        tokio::fs::write(&path, raw_output)
            .await
            .map_err(|e| AgentError::Session(format!("failed to write observation blob: {e}")))?;
        Ok(())
    }

    async fn fetch_slice(&self, handle: &str, offset: usize, limit: usize) -> Result<String> {
        let path = self.file_path_for(handle);
        let content = tokio::fs::read_to_string(&path).await.map_err(|_| {
            AgentError::Tool(
                "inspect".into(),
                format!("invoice handle `{handle}` not found on disk"),
            )
        })?;

        if offset >= content.len() {
            return Ok(String::new());
        }

        let slice = content.chars().skip(offset).take(limit).collect::<String>();

        Ok(slice)
    }

    async fn get_length(&self, handle: &str) -> Option<usize> {
        let path = self.file_path_for(handle);
        tokio::fs::read_to_string(&path).await.ok().map(|s| s.len())
    }

    async fn contains(&self, handle: &str) -> bool {
        self.file_path_for(handle).exists()
    }
}
