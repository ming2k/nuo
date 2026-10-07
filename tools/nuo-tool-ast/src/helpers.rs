//! Shared helpers for built-in tools.

use std::path::{Path, PathBuf};

/// The directory workspace-relative tool operations resolve against.
///
/// Tools are session-scoped under the unified server (ADR-0096): one process
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
        tokio::fs::read(path).await.map_err(nuo_wire::execution::FsError::from)
    }
    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), nuo_wire::execution::FsError> {
        tokio::fs::write(path, content).await.map_err(nuo_wire::execution::FsError::from)
    }
    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }
    async fn is_dir(&self, path: &Path) -> bool {
        tokio::fs::metadata(path).await.map(|m| m.is_dir()).unwrap_or(false)
    }
    async fn is_file(&self, path: &Path) -> bool {
        tokio::fs::metadata(path).await.map(|m| m.is_file()).unwrap_or(false)
    }
    async fn list_dir(&self, _path: &Path) -> Result<Vec<nuo_wire::execution::DirEntry>, nuo_wire::execution::FsError> {
        Ok(Vec::new())
    }
    async fn create_dir_all(&self, path: &Path) -> Result<(), nuo_wire::execution::FsError> {
        tokio::fs::create_dir_all(path).await.map_err(nuo_wire::execution::FsError::from)
    }
    async fn remove_file(&self, path: &Path) -> Result<(), nuo_wire::execution::FsError> {
        tokio::fs::remove_file(path).await.map_err(nuo_wire::execution::FsError::from)
    }
    async fn metadata(&self, path: &Path) -> Result<nuo_wire::execution::FsMetadata, nuo_wire::execution::FsError> {
        let meta = tokio::fs::metadata(path).await.map_err(nuo_wire::execution::FsError::from)?;
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
    if let Some(env) =
        ctx.get::<std::sync::Arc<dyn nuo_wire::execution::ExecutionEnvironment>>()
    {
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
/// This is provenance for the freshness contract (ADR-0214 §4, ADR-0237): the
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

/// Enforce a caller-supplied content-version precondition before a mutation.
///
/// The optimistic-concurrency arm of the freshness contract (ADR-0237): a
/// caller that read a snapshot may hand its version back and refuse to write
/// over drift it never saw. Fails **closed** in every direction:
///
/// - `expected` is `None` → no precondition (the caller did not read a snapshot).
/// - `expected` is `Some`, `current` is `None` → the file vanished since it was
///   read; creating it fresh would silently resurrect deleted content.
/// - `expected` is `Some`, versions differ → the file changed; the caller's
///   `old_string` anchors or full-file intent describe a snapshot that is gone.
///
/// `content_version` is a whole-file digest, so this is a *coarser* check than
/// an `old_string` anchor (which pins the exact region). That is the point: the
/// anchor cannot see a change to a part of the file the edit does not touch.
pub(crate) fn check_expected_version(
    path: &str,
    expected: Option<&str>,
    current: Option<&[u8]>,
) -> Result<(), String> {
    let Some(expected) = expected else {
        return Ok(());
    };
    match current {
        None => Err(format!(
            "'expected_version' was supplied for '{path}', but the file no longer exists. \
             Re-read the path and retry, or omit 'expected_version' to create it."
        )),
        Some(bytes) => {
            let actual = content_version(bytes);
            if actual == expected {
                Ok(())
            } else {
                Err(format!(
                    "'{path}' has changed since version '{expected}' was read (current version \
                     '{actual}'). The write was rejected so it cannot overwrite content you have \
                     not seen. Re-read '{path}' and retry against its current version."
                ))
            }
        }
    }
}

/// Read a file's bytes for a precondition check: `None` means "does not exist",
/// while any other error is surfaced (an unreadable file must not look absent,
/// or a broken read would disable the check rather than fail closed).
pub(crate) async fn read_optional(
    env: &dyn nuo_wire::ExecutionEnvironment,
    resolved: &std::path::Path,
) -> Result<Option<Vec<u8>>, String> {
    match env.fs().read(resolved).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(nuo_wire::execution::FsError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Extract a string field from JSON arguments for `permission_scope`.
pub(crate) fn json_string(arguments: &str, key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get(key)?.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "*".to_string())
}

/// Deserialize either a single string or an array of strings into `Option<Vec<String>>`.
///
/// Models frequently supply `"patterns": "*.rs"` or `"include": "*.rs"` instead
/// of wrapping a single item in an array. This provides robust parameter ergonomics
/// without sacrificing type safety.
pub(crate) fn deserialize_optional_string_or_vec<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or an array of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec![trimmed.to_string()]))
            }
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element::<String>()? {
                let trimmed = elem.trim();
                if !trimmed.is_empty() {
                    vec.push(trimmed.to_string());
                }
            }
            if vec.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec))
            }
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}
