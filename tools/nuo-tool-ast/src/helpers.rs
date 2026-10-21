//! Shared helpers for built-in tools.
use std::path::{Path, PathBuf};

/// The directory workspace-relative tool operations resolve against.
///
/// Tools are session-scoped under the unified server: one process
/// hosts sessions for many projects, so the server's process cwd is whatever
/// directory the first client spawned it from. The assembling bootstrap
/// therefore registers the session's project root as a
/// [`WorkspaceRoot`](nuo_wire::WorkspaceRoot) service on the
/// [`ToolContext`](nuo_wire::ToolContext), and each path-taking tool
/// captures it at factory time into its `root` field.
///
/// `None` (unit tests, a context built without the service) means "use the
/// process cwd" — the historical behaviour, still correct wherever one
/// process serves exactly one project.
pub(crate) type WorkspaceBase = Option<PathBuf>;

/// Capture the workspace base from a tool-assembly context. Factories call
/// this once at build time; the returned value is immutable for the tool's
/// lifetime, matching the session whose bootstrap assembled it.
pub(crate) fn workspace_base(ctx: &nuo_wire::tool_registry::ToolContext) -> WorkspaceBase {
    ctx.workspace_root().map(Path::to_path_buf)
}

#[derive(Debug, Clone)]
struct SimpleFsProvider;

#[async_trait::async_trait]
impl nuo_wire::execution::FsProvider for SimpleFsProvider {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, nuo_wire::execution::FsError> {
        tokio::fs::read(path)
            .await
            .map_err(nuo_wire::execution::FsError::from)
    }
    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), nuo_wire::execution::FsError> {
        tokio::fs::write(path, content)
            .await
            .map_err(nuo_wire::execution::FsError::from)
    }
    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }
    async fn is_dir(&self, path: &Path) -> bool {
        tokio::fs::metadata(path)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }
    async fn is_file(&self, path: &Path) -> bool {
        tokio::fs::metadata(path)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
    }
    async fn list_dir(
        &self,
        _path: &Path,
    ) -> Result<Vec<nuo_wire::execution::DirEntry>, nuo_wire::execution::FsError> {
        Ok(Vec::new())
    }
    async fn create_dir_all(&self, path: &Path) -> Result<(), nuo_wire::execution::FsError> {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(nuo_wire::execution::FsError::from)
    }
    async fn remove_file(&self, path: &Path) -> Result<(), nuo_wire::execution::FsError> {
        tokio::fs::remove_file(path)
            .await
            .map_err(nuo_wire::execution::FsError::from)
    }
    async fn metadata(
        &self,
        path: &Path,
    ) -> Result<nuo_wire::execution::FsMetadata, nuo_wire::execution::FsError> {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(nuo_wire::execution::FsError::from)?;
        Ok(nuo_wire::execution::FsMetadata {
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            is_symlink: meta.is_symlink(),
            len: meta.len(),
        })
    }
}

#[derive(Debug, Clone)]
struct SimpleProcessRunner;

#[async_trait::async_trait]
impl nuo_wire::execution::ProcessRunner for SimpleProcessRunner {
    async fn exec(
        &self,
        _command: &str,
        _cwd: &Path,
        _env: Option<&std::collections::HashMap<String, String>>,
        _timeout: std::time::Duration,
    ) -> Result<nuo_wire::execution::ProcessOutput, String> {
        Err("Process execution not supported in code tools".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct SimpleExecutionEnvironment {
    root: PathBuf,
    fs: SimpleFsProvider,
    process: SimpleProcessRunner,
}

impl SimpleExecutionEnvironment {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fs: SimpleFsProvider,
            process: SimpleProcessRunner,
        }
    }
}

impl nuo_wire::execution::ExecutionEnvironment for SimpleExecutionEnvironment {
    fn fs(&self) -> &dyn nuo_wire::execution::FsProvider {
        &self.fs
    }
    fn process(&self) -> &dyn nuo_wire::execution::ProcessRunner {
        &self.process
    }
    fn workspace_root(&self) -> &Path {
        &self.root
    }
}

/// Capture or synthesize an [`ExecutionEnvironment`](nuo_wire::execution::ExecutionEnvironment)
/// from the tool build context.
pub(crate) fn execution_environment(
    ctx: &nuo_wire::tool_registry::ToolContext,
) -> std::sync::Arc<dyn nuo_wire::execution::ExecutionEnvironment> {
    if let Some(env) = ctx.get::<std::sync::Arc<dyn nuo_wire::execution::ExecutionEnvironment>>() {
        return env.clone();
    }
    let root = ctx
        .workspace_root()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::sync::Arc::new(SimpleExecutionEnvironment::new(root))
}

/// Synthesize an [`ExecutionEnvironment`](nuo_wire::execution::ExecutionEnvironment)
/// from an optional workspace root base.
pub(crate) fn env_from_root(
    root: &WorkspaceBase,
) -> std::sync::Arc<dyn nuo_wire::execution::ExecutionEnvironment> {
    let base = root.clone().unwrap_or_else(|| PathBuf::from("."));
    std::sync::Arc::new(SimpleExecutionEnvironment::new(base))
}

/// Resolve a user-supplied path argument against the tool's workspace base.
///
/// Leading `~` is expanded to the user's home directory. Absolute paths pass
/// through unchanged (`Path::join` semantics); a relative path is anchored to
/// the session's project root, never to the server's coincidental process cwd.
/// Model-facing argument text is untouched — only filesystem access goes
/// through the resolved value, so prompt/UI rendering keeps showing what the
/// model actually sent.
pub(crate) fn resolve_workspace_path(base: &WorkspaceBase, path: &str) -> PathBuf {
    let expanded = nuo_wire::execution::expand_tilde(Path::new(path));
    if expanded.is_absolute() {
        expanded
    } else {
        match base {
            Some(root) => root.join(expanded),
            None => expanded,
        }
    }
}

/// Directories that are almost never interesting to search or list and can be
/// enormous: VCS metadata, dependency trees, and build output. These are shared
/// by `find_files` and `search_text` so discovery and content search prune the
/// same set of directories and never disagree about the searchable tree.
pub(crate) const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".next",
    "dist",
    "build",
    ".venv",
    "venv",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "coverage",
    ".gradle",
    ".idea",
    ".vscode",
    "proc",
    "sys",
    "dev",
    "run",
];

/// Content-addressed version identity for a source snapshot.
///
/// Deterministic over the bytes actually read, so the same content always
/// yields the same short digest, and a same-size replacement still changes it.
/// This is provenance for the freshness contract: the
/// version a structural query reports is what a later mutation's
/// `expected_version` is checked against.
///
/// It is deliberately *not* a security boundary — it identifies content, it
/// does not authenticate it.
pub(crate) fn content_version(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(12);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest.iter().take(6) {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    hex
}
