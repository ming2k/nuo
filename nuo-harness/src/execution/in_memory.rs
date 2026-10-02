//! In-memory execution environment for fast, deterministic unit testing without OS/disk side effects.

use async_trait::async_trait;
use nuo_contracts::execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Thread-safe in-memory virtual file system.
#[derive(Debug, Default, Clone)]
pub struct InMemoryFsProvider {
    files: Arc<RwLock<BTreeMap<PathBuf, Vec<u8>>>>,
    dirs: Arc<RwLock<std::collections::BTreeSet<PathBuf>>>,
}

impl InMemoryFsProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn normalize_path(path: &Path) -> PathBuf {
        let mut components = Vec::new();
        for comp in path.components() {
            match comp {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    components.pop();
                }
                _ => components.push(comp.as_os_str()),
            }
        }
        components.iter().collect()
    }
}

#[async_trait]
impl FsProvider for InMemoryFsProvider {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        let normalized = Self::normalize_path(path);
        let files = self.files.read().await;
        if let Some(content) = files.get(&normalized) {
            Ok(content.clone())
        } else {
            let dirs = self.dirs.read().await;
            if dirs.contains(&normalized) {
                Err(FsError::IsADirectory(path.display().to_string()))
            } else {
                Err(FsError::NotFound(path.display().to_string()))
            }
        }
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError> {
        let normalized = Self::normalize_path(path);
        if let Some(parent) = normalized.parent()
            && !parent.as_os_str().is_empty()
        {
            self.create_dir_all(parent).await?;
        }
        let mut files = self.files.write().await;
        files.insert(normalized, content.to_vec());
        Ok(())
    }

    async fn exists(&self, path: &Path) -> bool {
        let normalized = Self::normalize_path(path);
        let files = self.files.read().await;
        if files.contains_key(&normalized) {
            return true;
        }
        let dirs = self.dirs.read().await;
        dirs.contains(&normalized)
    }

    async fn is_dir(&self, path: &Path) -> bool {
        let normalized = Self::normalize_path(path);
        let dirs = self.dirs.read().await;
        dirs.contains(&normalized)
    }

    async fn is_file(&self, path: &Path) -> bool {
        let normalized = Self::normalize_path(path);
        let files = self.files.read().await;
        files.contains_key(&normalized)
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let normalized = Self::normalize_path(path);
        let files = self.files.read().await;
        let dirs = self.dirs.read().await;

        let mut entries = Vec::new();

        for (file_path, content) in files.iter() {
            if let Some(parent) = file_path.parent()
                && parent == normalized
            {
                let name = file_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                entries.push(DirEntry {
                    path: file_path.clone(),
                    name,
                    is_dir: false,
                    is_file: true,
                    size_bytes: content.len() as u64,
                });
            }
        }

        for dir_path in dirs.iter() {
            if let Some(parent) = dir_path.parent()
                && parent == normalized
            {
                let name = dir_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                entries.push(DirEntry {
                    path: dir_path.clone(),
                    name,
                    is_dir: true,
                    is_file: false,
                    size_bytes: 0,
                });
            }
        }

        Ok(entries)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        let normalized = Self::normalize_path(path);
        let mut dirs = self.dirs.write().await;
        let mut current = PathBuf::new();
        for comp in normalized.components() {
            current.push(comp);
            dirs.insert(current.clone());
        }
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        let normalized = Self::normalize_path(path);
        let mut files = self.files.write().await;
        if files.remove(&normalized).is_some() {
            Ok(())
        } else {
            Err(FsError::NotFound(path.display().to_string()))
        }
    }

    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError> {
        let normalized = Self::normalize_path(path);
        let files = self.files.read().await;
        if let Some(content) = files.get(&normalized) {
            return Ok(FsMetadata {
                is_dir: false,
                is_file: true,
                is_symlink: false,
                len: content.len() as u64,
            });
        }
        let dirs = self.dirs.read().await;
        if dirs.contains(&normalized) {
            return Ok(FsMetadata {
                is_dir: true,
                is_file: false,
                is_symlink: false,
                len: 0,
            });
        }
        Err(FsError::NotFound(path.display().to_string()))
    }
}

/// Scripted mock subprocess runner for testing.
#[derive(Debug, Default, Clone)]
pub struct MockProcessRunner {
    canned_responses: Arc<RwLock<HashMap<String, ProcessOutput>>>,
}

impl MockProcessRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, command: impl Into<String>, output: ProcessOutput) {
        let mut map = self.canned_responses.write().await;
        map.insert(command.into(), output);
    }
}

#[async_trait]
impl ProcessRunner for MockProcessRunner {
    async fn exec(
        &self,
        command: &str,
        _cwd: &Path,
        _env: Option<&HashMap<String, String>>,
        _timeout: Duration,
    ) -> Result<ProcessOutput, String> {
        let map = self.canned_responses.read().await;
        if let Some(res) = map.get(command) {
            Ok(res.clone())
        } else {
            Ok(ProcessOutput {
                exit_code: Some(0),
                stdout: format!("mock execution of '{command}'").into_bytes(),
                stderr: Vec::new(),
                timed_out: false,
            })
        }
    }
}

/// In-memory execution environment bundling [`InMemoryFsProvider`] and [`MockProcessRunner`].
#[derive(Debug, Clone)]
pub struct InMemoryExecutionEnvironment {
    fs: InMemoryFsProvider,
    process: MockProcessRunner,
    workspace_root: PathBuf,
    confined: bool,
}

impl InMemoryExecutionEnvironment {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            fs: InMemoryFsProvider::new(),
            process: MockProcessRunner::new(),
            workspace_root: workspace_root.into(),
            confined: true,
        }
    }

    pub fn set_confined(&mut self, confined: bool) {
        self.confined = confined;
    }

    pub fn fs_provider(&self) -> &InMemoryFsProvider {
        &self.fs
    }

    pub fn process_runner(&self) -> &MockProcessRunner {
        &self.process
    }
}

impl ExecutionEnvironment for InMemoryExecutionEnvironment {
    fn is_confined(&self) -> bool {
        self.confined
    }

    fn fs(&self) -> &dyn FsProvider {
        &self.fs
    }

    fn process(&self) -> &dyn ProcessRunner {
        &self.process
    }

    fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}
