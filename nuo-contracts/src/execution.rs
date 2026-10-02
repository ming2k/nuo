//! Execution environment and capability seams.
//!
//! Separates the agent's high-level intent (tools, prompts, ReAct loop) from
//! physical execution (local OS, Docker sandbox, remote E2B VM, or memory mock).

use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Physical containment required for shell commands in an execution environment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellIsolation {
    /// Direct host execution. Intended for explicit embedding/test environments,
    /// never the default product runtime.
    #[default]
    Host,
    /// Minimal read-only system runtime, writable workspace bind, isolated
    /// HOME/tmp/process namespaces, and scrubbed environment. Implementations
    /// must fail closed when unavailable.
    Workspace,
}

/// Errors produced by a [`FsProvider`] implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    NotFound(String),
    PermissionDenied(String),
    IsADirectory(String),
    NotADirectory(String),
    AlreadyExists(String),
    Io(String),
    Other(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(p) => {
                if p.is_empty() {
                    write!(f, "File or directory not found")
                } else if p.starts_with("No such file") {
                    write!(f, "{p}")
                } else {
                    write!(f, "File or directory not found: {p}")
                }
            }
            Self::PermissionDenied(p) => {
                if p.is_empty() {
                    write!(f, "Permission denied")
                } else if p.starts_with("Permission denied") {
                    write!(f, "{p}")
                } else {
                    write!(f, "Permission denied: {p}")
                }
            }
            Self::IsADirectory(p) => write!(f, "'{p}' is a directory, not a file"),
            Self::NotADirectory(p) => write!(f, "'{p}' is not a directory"),
            Self::AlreadyExists(p) => {
                if p.is_empty() {
                    write!(f, "File already exists")
                } else if p.starts_with("File exists") {
                    write!(f, "{p}")
                } else {
                    write!(f, "File already exists: {p}")
                }
            }
            Self::Io(e) => write!(f, "{e}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(String::new()),
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(String::new()),
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists(String::new()),
            _ => Self::Io(err.to_string()),
        }
    }
}

/// Metadata describing a file system node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsMetadata {
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub len: u64,
}

/// One directory entry returned by [`FsProvider::list_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub size_bytes: u64,
}

/// The file system capability seam.
///
/// Implementations can back onto the local OS file system, an in-memory VFS,
/// a Docker container overlay, or a remote cloud sandbox (e.g. E2B).
#[async_trait]
pub trait FsProvider: Send + Sync {
    /// Read entire file into raw bytes.
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError>;

    /// Read entire file as UTF-8 string.
    async fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        let bytes = self.read(path).await?;
        String::from_utf8(bytes).map_err(|e| FsError::Other(format!("Invalid UTF-8: {e}")))
    }

    /// Write byte contents to a file atomically or in-place.
    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError>;

    /// Write text string to a file.
    async fn write_str(&self, path: &Path, content: &str) -> Result<(), FsError> {
        self.write(path, content.as_bytes()).await
    }

    /// Check if a path exists.
    async fn exists(&self, path: &Path) -> bool;

    /// Check if a path is a directory.
    async fn is_dir(&self, path: &Path) -> bool;

    /// Check if a path is a regular file.
    async fn is_file(&self, path: &Path) -> bool;

    /// Read directory entries.
    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;

    /// Create directory recursively.
    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError>;

    /// Remove a file.
    async fn remove_file(&self, path: &Path) -> Result<(), FsError>;

    /// Query metadata for a path.
    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError>;
}

/// The outcome of running a subprocess command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// The process exit code, if terminated normally by exit code.
    pub exit_code: Option<i32>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    /// Whether execution was terminated because of timeout.
    pub timed_out: bool,
}

impl ProcessOutput {
    /// Format stdout as lossy UTF-8 string.
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    /// Format stderr as lossy UTF-8 string.
    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    /// Return combined output (stdout + stderr), preserving order where possible.
    pub fn combined_output_lossy(&self) -> String {
        let stdout = self.stdout_lossy();
        let stderr = self.stderr_lossy();
        if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{stdout}\n{stderr}")
        }
    }

    /// True if the process exited with code 0 and did not time out.
    pub fn is_success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// The subprocess execution capability seam.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Execute a shell/process command within a working directory, with optional
    /// environment overrides and a timeout deadline.
    async fn exec(
        &self,
        command: &str,
        cwd: &Path,
        env: Option<&HashMap<String, String>>,
        timeout: Duration,
    ) -> Result<ProcessOutput, String>;
}

/// A unified execution environment bundling filesystem access, process
/// execution, and a canonical workspace root.
pub trait ExecutionEnvironment: Send + Sync {
    /// The filesystem provider for this environment.
    fn fs(&self) -> &dyn FsProvider;

    /// The process runner for this environment.
    fn process(&self) -> &dyn ProcessRunner;

    /// The canonical root directory for this environment.
    fn workspace_root(&self) -> &Path;

    /// Canonicalized additional roots admitted alongside the primary
    /// (ADR-0142). Empty for the default single-root environment; shell
    /// sandboxes bind each entry read-write and path confinement admits
    /// anything under them.
    ///
    /// Returns an owned snapshot rather than a borrowed slice: the admitted
    /// set is live state (ADR-0147 trust decisions recompute it mid-session),
    /// so implementers backed by the [`super::SharedAdditionalRoots`] handle
    /// must clone under a short lock instead of exposing one.
    fn additional_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Whether path confinement is enforced for this environment.
    /// By default `true` (file tools are strictly confined to workspace roots).
    /// When disabled (`false`), file tools resolve any absolute path on the host,
    /// bounded only by host OS / daemon user permissions.
    fn is_confined(&self) -> bool {
        true
    }

    /// Required containment for shell-capable tools.
    fn shell_isolation(&self) -> ShellIsolation {
        ShellIsolation::Host
    }

    /// Resolve a model- or caller-supplied path (relative or absolute) against
    /// this environment's admitted workspace roots.
    ///
    /// Relative paths resolve against `workspace_root()`. Leading `~` is expanded
    /// to the user's home directory. Returns the resolved path if confinement is disabled,
    /// if it falls within `workspace_root()`, or within any entry in `additional_roots()`
    /// (plus the implicit platform temp roots — scratch files must be readable),
    /// or `FsError::PermissionDenied` if it attempts to escape containment.
    fn resolve_path(&self, raw: &str) -> Result<PathBuf, FsError> {
        let supplied = expand_tilde(Path::new(raw));
        let target = if supplied.is_absolute() {
            supplied
        } else {
            self.workspace_root().join(supplied)
        };

        let normalized = lexical_normalize(&target);
        let root_norm = lexical_normalize(self.workspace_root());
        let admitted = !self.is_confined()
            || normalized.starts_with(&root_norm)
            || admits_temp_path(&target)
            || admits_skills_path(&target)
            || self
                .additional_roots()
                .iter()
                .any(|extra| normalized.starts_with(lexical_normalize(extra)));

        if admitted {
            Ok(target)
        } else {
            Err(FsError::PermissionDenied(format!(
                "access to '{raw}' is outside the admitted workspace roots"
            )))
        }
    }
}

/// Expands a leading `~` or `~/` to the user's home directory if available.
///
/// If the path does not start with `~`, or if the home directory cannot be determined,
/// returns the original path unchanged.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let path_str = match path.to_str() {
        Some(s) => s,
        None => return path.to_path_buf(),
    };

    if path_str == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
    } else if let Some(stripped) = path_str.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            home.join(stripped)
        } else {
            path.to_path_buf()
        }
    } else if let Some(stripped) = path_str.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            home.join(stripped)
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    }
}

/// The platform temporary directory, admitting both spellings when they differ.
///
/// macOS reports `TMPDIR` under `/var/folders/…/T/` while the same directory is
/// canonically spelled `/private/var/folders/…/T/` (and `/tmp` vs `/private/tmp`
/// on the default install). Both spellings of each root are admitted so a
/// caller may use either the well-known name or the resolved physical path;
/// see [`admits_temp_path`] / [`temp_roots`].
pub fn temp_roots() -> Vec<std::path::PathBuf> {
    let mut roots = vec![std::env::temp_dir(), std::path::PathBuf::from("/tmp")];
    // Canonical spellings too (macOS: /tmp → /private/tmp,
    // /var/folders/… → /private/var/folders/…; Linux: usually identical, so
    // the dedup below collapses them). Best-effort — an unresolvable dir just
    // contributes its raw spelling.
    for raw in [std::env::temp_dir(), std::path::PathBuf::from("/tmp")] {
        if let Ok(canon) = std::fs::canonicalize(&raw) {
            roots.push(canon);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Lexical containment check against the implicit temporary-root set.
///
/// Roots carry both the raw [`std::env::temp_dir`] / `/tmp` spellings and
/// their canonicalized forms (macOS: `/private/tmp`), so a path spelled
/// either way is admitted. Pure `starts_with` over [`lexical_normalize`] —
/// no syscalls on the caller's hot path beyond the one-time root resolution
/// inside [`temp_roots`].
pub fn admits_temp_path(path: &Path) -> bool {
    let normalized = lexical_normalize(path);
    temp_roots()
        .iter()
        .any(|root| normalized.starts_with(lexical_normalize(root)))
}

/// Global skills roots that are admitted for progressive disclosure skill reading.
pub fn global_skills_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(muta_home) = std::env::var("MUTA_HOME") {
        let base = PathBuf::from(muta_home);
        roots.push(base.join("data").join("skills"));
        roots.push(base.join("cache").join("skills"));
    }
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
        roots.push(PathBuf::from(xdg_data).join("muta").join("skills"));
    }
    if let Ok(xdg_cache) = std::env::var("XDG_CACHE_HOME") {
        roots.push(PathBuf::from(xdg_cache).join("nuo").join("skills"));
    }
    if let Some(data) = dirs::data_local_dir().or_else(dirs::data_dir) {
        roots.push(data.join("nuo").join("skills"));
    }
    if let Some(cache) = dirs::cache_dir() {
        roots.push(cache.join("nuo").join("skills"));
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".nuo").join("skills"));
        roots.push(
            home.join(".local")
                .join("share")
                .join("nuo")
                .join("skills"),
        );
        roots.push(home.join(".cache").join("nuo").join("skills"));
    }
    let raw_roots = roots.clone();
    for raw in raw_roots {
        if let Ok(canon) = std::fs::canonicalize(&raw) {
            roots.push(canon);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Lexical containment check against global user and remote cached skills roots.
pub fn admits_skills_path(path: &Path) -> bool {
    let normalized = lexical_normalize(path);
    global_skills_roots()
        .iter()
        .any(|root| normalized.starts_with(lexical_normalize(root)))
}

/// Normalize `.` and `..` without consulting a host filesystem.
///
/// This deliberately works in terms of `Path::components`, so prefixes and
/// root components retain their native Windows/Unix meaning.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

/// Interceptor middleware for the tool execution pipeline.
///
/// Enables cross-cutting concerns like large output spilling (Spill-to-Disk),
/// secret scrubbing, and permission guards without modifying individual tool bodies.
#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    /// Pre-execution inspection: inspect or reject a tool call before invocation.
    async fn pre_execute(
        &self,
        _tool: &str,
        _arguments: &serde_json::Value,
        _env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Post-execution transformation: scrub secrets, spill large outputs, or append metadata.
    async fn post_execute(
        &self,
        _tool: &str,
        _output: &mut crate::ToolOutput,
        _env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEnv {
        root: PathBuf,
        additional: Vec<PathBuf>,
        confined: bool,
    }

    struct MockFs;
    #[async_trait]
    impl FsProvider for MockFs {
        async fn read(&self, _path: &Path) -> Result<Vec<u8>, FsError> {
            Ok(Vec::new())
        }
        async fn write(&self, _path: &Path, _content: &[u8]) -> Result<(), FsError> {
            Ok(())
        }
        async fn exists(&self, _path: &Path) -> bool {
            true
        }
        async fn is_dir(&self, _path: &Path) -> bool {
            true
        }
        async fn is_file(&self, _path: &Path) -> bool {
            false
        }
        async fn list_dir(&self, _path: &Path) -> Result<Vec<DirEntry>, FsError> {
            Ok(Vec::new())
        }
        async fn create_dir_all(&self, _path: &Path) -> Result<(), FsError> {
            Ok(())
        }
        async fn remove_file(&self, _path: &Path) -> Result<(), FsError> {
            Ok(())
        }
        async fn metadata(&self, _path: &Path) -> Result<FsMetadata, FsError> {
            Ok(FsMetadata {
                is_dir: true,
                is_file: false,
                is_symlink: false,
                len: 0,
            })
        }
    }

    struct MockProcess;
    #[async_trait]
    impl ProcessRunner for MockProcess {
        async fn exec(
            &self,
            _command: &str,
            _cwd: &Path,
            _env: Option<&HashMap<String, String>>,
            _timeout: Duration,
        ) -> Result<ProcessOutput, String> {
            Ok(ProcessOutput {
                exit_code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out: false,
            })
        }
    }

    impl ExecutionEnvironment for MockEnv {
        fn fs(&self) -> &dyn FsProvider {
            &MockFs
        }
        fn process(&self) -> &dyn ProcessRunner {
            &MockProcess
        }
        fn workspace_root(&self) -> &Path {
            &self.root
        }
        fn additional_roots(&self) -> Vec<PathBuf> {
            self.additional.clone()
        }
        fn is_confined(&self) -> bool {
            self.confined
        }
    }

    #[test]
    fn test_lexical_normalize() {
        assert_eq!(
            lexical_normalize(Path::new("/workspace/src/../target")),
            PathBuf::from("/workspace/target")
        );
        assert_eq!(
            lexical_normalize(Path::new("/workspace/../optics")),
            PathBuf::from("/optics")
        );
        assert_eq!(
            lexical_normalize(Path::new("/workspace/./src/./main.rs")),
            PathBuf::from("/workspace/src/main.rs")
        );
    }

    #[test]
    fn test_resolve_path_admitted_roots() {
        let env = MockEnv {
            root: PathBuf::from("/home/user/project"),
            additional: vec![PathBuf::from("/home/user/optics")],
            confined: true,
        };

        // Relative path inside primary root
        assert_eq!(
            env.resolve_path("src/main.rs").unwrap(),
            PathBuf::from("/home/user/project/src/main.rs")
        );

        // Relative path into additional root
        assert_eq!(
            env.resolve_path("../optics/Cargo.toml").unwrap(),
            PathBuf::from("/home/user/project/../optics/Cargo.toml")
        );

        // Absolute path into additional root
        assert_eq!(
            env.resolve_path("/home/user/optics/Cargo.toml").unwrap(),
            PathBuf::from("/home/user/optics/Cargo.toml")
        );

        // Escape attempts rejected
        assert!(env.resolve_path("../../etc/passwd").is_err());
        assert!(env.resolve_path("/etc/shadow").is_err());
        assert!(env.resolve_path("../other_unadmitted").is_err());
    }

    #[test]
    fn test_resolve_path_confinement_disabled() {
        let env = MockEnv {
            root: PathBuf::from("/home/user/project"),
            additional: Vec::new(),
            confined: false,
        };

        // Any absolute path is admitted when confinement is disabled
        assert_eq!(
            env.resolve_path("/etc/hosts").unwrap(),
            PathBuf::from("/etc/hosts")
        );
        assert_eq!(
            env.resolve_path("/var/log/syslog").unwrap(),
            PathBuf::from("/var/log/syslog")
        );
    }

    #[test]
    fn temp_roots_admit_platform_scratch_dirs() {
        let roots = temp_roots();
        // The process temp dir (whatever it is) is always admitted, as is
        // the well-known /tmp spelling on Unix. On Linux both spellings
        // usually collapse to one entry.
        assert!(roots.iter().any(|r| r == &std::env::temp_dir()));
        if cfg!(unix) {
            assert!(roots.iter().any(|r| r == Path::new("/tmp")));
        }

        // Containment: anything under temp is admitted, everything else is not.
        let probe = std::env::temp_dir().join("muta-probe/scratch.txt");
        assert!(admits_temp_path(&probe));
        assert!(admits_temp_path(Path::new("/tmp/anything")));
        assert!(!admits_temp_path(Path::new("/etc/passwd")));
        assert!(!admits_temp_path(Path::new(
            "/home/user/project/src/main.rs"
        )));
        // Traversal cannot escape a temp root.
        assert!(!admits_temp_path(Path::new("/tmp/../etc/passwd")));
    }

    #[test]
    fn resolve_path_admits_temp_paths() {
        let env = MockEnv {
            root: PathBuf::from("/home/user/project"),
            additional: Vec::new(),
            confined: true,
        };
        let tmp = std::env::temp_dir().join("muta-resolve-probe/build.log");
        env.resolve_path(tmp.to_str().unwrap()).unwrap();
        env.resolve_path("/tmp/scratch.txt").unwrap();
        // Non-temp absolute paths stay denied.
        assert!(env.resolve_path("/etc/shadow").is_err());
    }

    #[test]
    fn test_expand_tilde() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde(Path::new("~")), home);
            assert_eq!(
                expand_tilde(Path::new("~/.local/state/muta/auth.toml")),
                home.join(".local/state/muta/auth.toml")
            );
        }
        assert_eq!(
            expand_tilde(Path::new("/var/log")),
            PathBuf::from("/var/log")
        );
        assert_eq!(
            expand_tilde(Path::new("src/main.rs")),
            PathBuf::from("src/main.rs")
        );
    }
}
