//! Local host execution environment implementation.
//!
//! Executes filesystem operations and subprocess commands directly on the host OS.

use async_trait::async_trait;
use nuo_wire::execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
    ShellIsolation,
};
use nuo_wire::{SharedAdditionalRoots, SharedConfinement};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Local host filesystem provider.
#[derive(Debug, Default, Clone)]
pub struct LocalFsProvider;

impl LocalFsProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FsProvider for LocalFsProvider {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        tokio::fs::read(path).await.map_err(FsError::from)
    }

    async fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        tokio::fs::read_to_string(path).await.map_err(FsError::from)
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Atomic write via temp file and rename when possible
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let temp_name = format!(".tmp_{}_{}", std::process::id(), fastrand::u64(..));
        let temp_path = parent.join(temp_name);

        if let Err(e) = tokio::fs::write(&temp_path, content).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(FsError::from(e));
        }

        if let Err(error) = tokio::fs::rename(&temp_path, path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            // Never fall back to a direct write: besides losing atomicity, a
            // changed final path could be a symlink and reinterpret the
            // already-checked destination outside the admitted roots.
            return Err(FsError::from(error));
        }

        Ok(())
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

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let mut read_dir = tokio::fs::read_dir(path).await.map_err(FsError::from)?;
        let mut entries = Vec::new();

        while let Some(entry) = read_dir.next_entry().await.map_err(FsError::from)? {
            let entry_path = entry.path();
            let metadata = entry.metadata().await.map_err(FsError::from)?;
            let name = entry.file_name().to_string_lossy().to_string();

            entries.push(DirEntry {
                path: entry_path,
                name,
                is_dir: metadata.is_dir(),
                is_file: metadata.is_file(),
                size_bytes: metadata.len(),
            });
        }

        Ok(entries)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        tokio::fs::create_dir_all(path).await.map_err(FsError::from)
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        tokio::fs::remove_file(path).await.map_err(FsError::from)
    }

    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError> {
        let meta = tokio::fs::symlink_metadata(path)
            .await
            .map_err(FsError::from)?;
        Ok(FsMetadata {
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            is_symlink: meta.file_type().is_symlink(),
            len: meta.len(),
        })
    }
}

/// Local host subprocess runner.
#[derive(Debug, Default, Clone)]
pub struct LocalProcessRunner;

impl LocalProcessRunner {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProcessRunner for LocalProcessRunner {
    async fn exec(
        &self,
        command: &str,
        cwd: &Path,
        env: Option<&HashMap<String, String>>,
        timeout: Duration,
    ) -> Result<ProcessOutput, String> {
        let mut cmd = nuo_host::shell::native_shell(command);
        cmd.current_dir(cwd);
        if let Some(env_map) = env {
            cmd.envs(env_map);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (child, process_tree) = nuo_host::process::spawn_owned(&mut cmd)
            .map_err(|e| format!("failed to spawn and contain native shell: {e}"))?;

        let wait_fut = child.wait_with_output();
        match tokio::time::timeout(timeout, wait_fut).await {
            Ok(Ok(output)) => Ok(ProcessOutput {
                exit_code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
                timed_out: false,
            }),
            Ok(Err(e)) => Err(format!("Command execution failed: {e}")),
            Err(_) => {
                let _ = process_tree.terminate();

                Ok(ProcessOutput {
                    exit_code: None,
                    stdout: Vec::new(),
                    stderr: format!("Command timed out after {}s", timeout.as_secs()).into_bytes(),
                    timed_out: true,
                })
            }
        }
    }
}

/// Local execution environment tying together the local filesystem and local process runner.
#[derive(Debug, Clone)]
pub struct LocalExecutionEnvironment {
    fs: LocalFsProvider,
    process: LocalProcessRunner,
    workspace_root: PathBuf,
}

impl LocalExecutionEnvironment {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            fs: LocalFsProvider::new(),
            process: LocalProcessRunner::new(),
            workspace_root: workspace_root.into(),
        }
    }
}

impl ExecutionEnvironment for LocalExecutionEnvironment {
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

/// Filesystem provider that physically confines every operation to the
/// admitted root set (ADR-0142: the workspace root plus any configured
/// additional roots).
///
/// The additional set lives behind a [`SharedAdditionalRoots`] handle so a
/// runtime trust decision (`/trust ex-workspace`, `/trust revoke`, `/settings
/// reload`) re-admits or collapses it live — every operation snapshots the
/// current set, so there is no stale clone and no restart needed.
#[derive(Debug, Clone)]
struct WorkspaceFsProvider {
    inner: LocalFsProvider,
    root: PathBuf,
    additional_roots: SharedAdditionalRoots,
    confinement: SharedConfinement,
}

impl WorkspaceFsProvider {
    fn new(root: PathBuf) -> Self {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Self {
            inner: LocalFsProvider::new(),
            root,
            additional_roots: SharedAdditionalRoots::empty(),
            confinement: SharedConfinement::default(),
        }
    }

    /// Widen admission to extra canonicalized roots (ADR-0142). The primary
    /// root keeps its exclusive duties — relative resolution and every
    /// project-bound decision; additional roots only expand containment.
    fn with_additional_roots(mut self, additional: Vec<PathBuf>) -> Self {
        let admitted = Self::sanitize(&self.root, additional);
        self.additional_roots = SharedAdditionalRoots::new(admitted);
        self
    }

    /// The canonical admitted subset of `additional`: existing directories,
    /// outside the primary, deduped. Shared by construction and by live
    /// trust-decision recomputes so both paths enforce identical rules.
    fn sanitize(root: &Path, additional: Vec<PathBuf>) -> Vec<PathBuf> {
        additional
            .into_iter()
            .filter_map(|extra| std::fs::canonicalize(extra).ok())
            .filter(|extra| extra.is_dir())
            .filter(|extra| *extra != root && !extra.starts_with(root))
            .fold(Vec::new(), |mut roots, extra| {
                if !roots.contains(&extra) {
                    roots.push(extra);
                }
                roots
            })
    }

    /// Canonicalized additional roots admitted alongside the primary.
    fn additional_roots(&self) -> Vec<PathBuf> {
        self.additional_roots.snapshot()
    }

    /// Human-readable summary of the admitted set for denial messages. The
    /// implicit temp admission appears as a `$TMPDIR` token rather than
    /// platform-specific spellings.
    fn admitted_set(&self) -> String {
        let additional = self.additional_roots.snapshot();
        let mut listed = self.root.display().to_string();
        for extra in &additional {
            listed.push_str(", ");
            listed.push_str(&extra.display().to_string());
        }
        listed.push_str(", $TMPDIR");
        listed
    }

    fn confined(&self, path: &Path) -> Result<PathBuf, FsError> {
        let expanded = nuo_wire::execution::expand_tilde(path);
        let candidate = if expanded.is_absolute() {
            expanded
        } else {
            self.root.join(expanded)
        };
        let resolved = resolve_existing_ancestor(&candidate).ok_or_else(|| {
            FsError::PermissionDenied(format!(
                "workspace sandbox could not resolve '{}'",
                path.display()
            ))
        })?;
        let admitted = !self.confinement.is_confined()
            || resolved.starts_with(&self.root)
            || nuo_wire::execution::admits_temp_path(&resolved)
            || nuo_wire::execution::admits_skills_path(&resolved)
            || self
                .additional_roots
                .snapshot()
                .iter()
                .any(|extra| resolved.starts_with(extra));
        if admitted {
            // Operate on the checked path, not the original spelling. This
            // collapses `..` and already-resolved symlink parents so the I/O
            // call cannot reinterpret a different lexical route.
            Ok(resolved)
        } else {
            let detail = format!(
                "'{}' is outside the admitted workspace roots [{}]",
                path.display(),
                self.admitted_set()
            );
            Err(FsError::PermissionDenied(detail))
        }
    }
}

fn resolve_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cursor = path;
    let mut suffix = Vec::new();
    // `Path::exists` follows symlinks and therefore treats a dangling link as
    // absent. `symlink_metadata` recognizes the directory entry itself; the
    // following canonicalization then rejects the dangling link instead of
    // rebuilding it lexically beneath an admitted parent.
    while std::fs::symlink_metadata(cursor).is_err() {
        suffix.push(cursor.file_name()?.to_os_string());
        cursor = cursor.parent()?;
    }
    let mut resolved = std::fs::canonicalize(cursor).ok()?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Some(resolved)
}

#[async_trait]
impl FsProvider for WorkspaceFsProvider {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.inner.read(&self.confined(path)?).await
    }

    async fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        self.inner.read_to_string(&self.confined(path)?).await
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError> {
        self.inner.write(&self.confined(path)?, content).await
    }

    async fn exists(&self, path: &Path) -> bool {
        match self.confined(path) {
            Ok(path) => self.inner.exists(&path).await,
            Err(_) => false,
        }
    }

    async fn is_dir(&self, path: &Path) -> bool {
        match self.confined(path) {
            Ok(path) => self.inner.is_dir(&path).await,
            Err(_) => false,
        }
    }

    async fn is_file(&self, path: &Path) -> bool {
        match self.confined(path) {
            Ok(path) => self.inner.is_file(&path).await,
            Err(_) => false,
        }
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        self.inner.list_dir(&self.confined(path)?).await
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        self.inner.create_dir_all(&self.confined(path)?).await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_file(&self.confined(path)?).await
    }

    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError> {
        self.inner.metadata(&self.confined(path)?).await
    }
}

#[derive(Debug, Clone, Default)]
struct WorkspaceProcessRunner;

#[async_trait]
impl ProcessRunner for WorkspaceProcessRunner {
    async fn exec(
        &self,
        _command: &str,
        _cwd: &Path,
        _env: Option<&HashMap<String, String>>,
        _timeout: Duration,
    ) -> Result<ProcessOutput, String> {
        Err("Direct process execution is disabled in the workspace sandbox; use the sandbox-aware shell capability.".to_string())
    }
}

/// Product runtime environment: filesystem tools are physically confined to
/// the workspace (and additional roots) and shell tools execute in host native mode,
/// governed by the Tool Hazard model.
#[derive(Debug, Clone)]
pub struct WorkspaceExecutionEnvironment {
    fs: WorkspaceFsProvider,
    process: WorkspaceProcessRunner,
    workspace_root: PathBuf,
}

impl WorkspaceExecutionEnvironment {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        let fs = WorkspaceFsProvider::new(workspace_root);
        Self {
            workspace_root: fs.root.clone(),
            fs,
            process: WorkspaceProcessRunner,
        }
    }

    /// Multi-root construction (ADR-0142): the primary root keeps its
    /// resolution/cwd duties, and each canonicalized additional root widens
    /// filesystem admission and sandbox mounts for cross-project workflows.
    pub fn with_additional_roots(
        workspace_root: impl Into<PathBuf>,
        additional_roots: Vec<PathBuf>,
    ) -> Self {
        let workspace_root = workspace_root.into();
        let fs = WorkspaceFsProvider::new(workspace_root).with_additional_roots(additional_roots);
        Self {
            workspace_root: fs.root.clone(),
            fs,
            process: WorkspaceProcessRunner,
        }
    }

    /// Canonicalized additional roots admitted alongside the primary (ADR-0142).
    pub fn additional_roots(&self) -> Vec<PathBuf> {
        self.fs.additional_roots()
    }

    /// The live admitted-root handle. Bootstrap provides a clone of this into
    /// the tool context so a runtime reload can swap the set without rebuilding
    /// the toolset; every confined operation snapshots it, so the change lands
    /// on the next tool call — no restart.
    pub fn shared_additional_roots(&self) -> SharedAdditionalRoots {
        self.fs.additional_roots.clone()
    }

    /// The live confinement handle.
    pub fn shared_confinement(&self) -> SharedConfinement {
        self.fs.confinement.clone()
    }
}

/// Probe the physical sandbox once. Existence is insufficient: distributions
/// may install bubblewrap while disabling unprivileged user namespaces.
pub fn workspace_sandbox_available() -> bool {
    nuo_host::workspace_sandbox::available()
}

impl ExecutionEnvironment for WorkspaceExecutionEnvironment {
    fn fs(&self) -> &dyn FsProvider {
        &self.fs
    }

    fn process(&self) -> &dyn ProcessRunner {
        &self.process
    }

    fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn additional_roots(&self) -> Vec<PathBuf> {
        self.fs.additional_roots()
    }

    fn is_confined(&self) -> bool {
        self.fs.confinement.is_confined()
    }

    fn shell_isolation(&self) -> ShellIsolation {
        ShellIsolation::Host
    }
}

#[cfg(test)]
pub(crate) mod workspace_tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A directory **outside** the workspace that is *not* under the implicit
    /// temp roots, shared by every crate-internal denial fixture (including
    /// `crate::tools::tests`). `tempfile::tempdir()` stopped qualifying as
    /// "outside" once temp dirs became implicitly admitted, so denial fixtures
    /// live under the repo's gitignored `target/test-scratch` area instead.
    pub(crate) fn workspace_tests_outside_scratch(tag: &str) -> PathBuf {
        // `<manifest>/../..` is the workspace root whose `/target/` is
        // gitignored; anchoring there keeps fixture bytes out of both the
        // temp set and any tracked tree, regardless of the test's cwd.
        let repo_target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("manifest has a workspace-root ancestor")
            .join("target/test-scratch");
        let dir = repo_target.join(format!(
            "{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn workspace_fs_allows_inside_and_denies_outside() {
        let root = scratch();
        let outside_root = workspace_tests_outside_scratch("denied-fs");
        let outside = outside_root.join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        let env = WorkspaceExecutionEnvironment::new(root.path());

        env.fs()
            .write(Path::new("inside.txt"), b"ok")
            .await
            .unwrap();
        assert_eq!(
            env.fs()
                .read(&root.path().join("inside.txt"))
                .await
                .unwrap(),
            b"ok"
        );
        assert!(matches!(
            env.fs().read(&outside).await,
            Err(FsError::PermissionDenied(_))
        ));
        assert_eq!(env.shell_isolation(), ShellIsolation::Host);
    }

    #[tokio::test]
    async fn temp_dir_paths_are_implicitly_admitted() {
        let root = scratch();
        let env = WorkspaceExecutionEnvironment::new(root.path());

        // Absolute paths under the platform temp dir are admitted without any
        // configured additional root, and both read and write flow through.
        let scratch_file = std::env::temp_dir().join(format!(
            "nuo-temp-admission-{}-scratch.txt",
            uuid::Uuid::new_v4().simple()
        ));
        env.fs().write(&scratch_file, b"temp").await.unwrap();
        assert_eq!(env.fs().read(&scratch_file).await.unwrap(), b"temp");

        // The canonical spelling (macOS: /private/…) is admitted too.
        let canonical = scratch_file.canonicalize().unwrap();
        assert_eq!(env.fs().read(&canonical).await.unwrap(), b"temp");

        std::fs::remove_file(&scratch_file).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn additional_roots_widen_admission_without_losing_primary() {
        let root = scratch();
        let sibling = scratch(); // a separate project root
        let stranger = workspace_tests_outside_scratch("denied-roots");
        let sibling_file = sibling.path().join("api.rs");
        std::fs::write(&sibling_file, b"sibling").unwrap();
        let stranger_file = stranger.join("secret.txt");
        std::fs::write(&stranger_file, b"secret").unwrap();

        let env = WorkspaceExecutionEnvironment::with_additional_roots(
            root.path(),
            vec![sibling.path().canonicalize().unwrap()],
        );

        // Relative resolution still binds to the primary root only.
        env.fs()
            .write(Path::new("primary.txt"), b"ok")
            .await
            .unwrap();
        assert_eq!(
            env.fs()
                .read(&root.path().join("primary.txt"))
                .await
                .unwrap(),
            b"ok"
        );
        // The admitted sibling root is now readable and writable...
        assert_eq!(env.fs().read(&sibling_file).await.unwrap(), b"sibling");
        env.fs()
            .write(&sibling.path().join("note.md"), b"cross")
            .await
            .unwrap();
        // ...while an unadmitted third location still fails closed.
        assert!(matches!(
            env.fs().read(&stranger_file).await,
            Err(FsError::PermissionDenied(_))
        ));
        // The sandbox env carries the roots for the bash tool to bind.
        assert_eq!(
            env.additional_roots(),
            &[sibling.path().canonicalize().unwrap()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn trust_decision_rewires_admission_live() {
        // ADR-0147: the boundary takes shape at trust-decision time. The
        // same environment instance must admit a linked root after a grant
        // handle-store and close it again after a revoke — with no rebuild
        // between — because every confined operation snapshots the live set.
        let root = scratch();
        // The revocable sibling must live outside the implicit temp roots:
        // a tempdir sibling stays admitted regardless of the trust decision,
        // making the revoke-deny assertion unreachable.
        let sibling = workspace_tests_outside_scratch("trust-sibling");
        let sibling_file = sibling.join("api.rs");
        std::fs::write(&sibling_file, b"sibling").unwrap();
        let env = WorkspaceExecutionEnvironment::new(root.path());
        let shared = env.shared_additional_roots();

        // Pre-grant: denied when not admitted.
        assert!(matches!(
            env.fs().read(&sibling_file).await,
            Err(FsError::PermissionDenied(_))
        ));

        // Grant: swap in the canonicalized root; same instance admits it.
        let granted = sibling.canonicalize().unwrap();
        shared.store(vec![granted.clone()]);
        assert_eq!(env.fs().read(&sibling_file).await.unwrap(), b"sibling");

        // Revoke: collapse back to primary-only; deny returns.
        shared.store(Vec::new());
        assert!(matches!(
            env.fs().read(&sibling_file).await,
            Err(FsError::PermissionDenied(_))
        ));
    }

    #[tokio::test]
    async fn additional_root_inside_workspace_is_ignored() {
        // A root nested inside the primary adds no authority; the loader
        // rejects it, and the environment defensively drops it too.
        let root = scratch();
        let nested = root.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let env = WorkspaceExecutionEnvironment::with_additional_roots(
            root.path(),
            vec![nested.canonicalize().unwrap()],
        );
        assert!(env.additional_roots().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_fs_denies_symlink_escape() {
        let root = scratch();
        let outside_root = workspace_tests_outside_scratch("denied-symlink");
        let outside = outside_root.join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.path().join("escape")).unwrap();
        let env = WorkspaceExecutionEnvironment::new(root.path());
        assert!(matches!(
            env.fs().read(&root.path().join("escape")).await,
            Err(FsError::PermissionDenied(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_fs_denies_dangling_symlink_destination() {
        let root = scratch();
        let outside_root = workspace_tests_outside_scratch("denied-dangling");
        let outside = outside_root.join("not-created.txt");
        std::os::unix::fs::symlink(&outside, root.path().join("dangling")).unwrap();
        let env = WorkspaceExecutionEnvironment::new(root.path());

        assert!(matches!(
            env.fs()
                .write(&root.path().join("dangling"), b"blocked")
                .await,
            Err(FsError::PermissionDenied(_))
        ));
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn additional_roots_are_canonicalized_and_missing_roots_are_dropped() {
        let root = scratch();
        let sibling = scratch();
        let missing = sibling.path().join("missing");
        let env = WorkspaceExecutionEnvironment::with_additional_roots(
            root.path(),
            vec![sibling.path().join("."), missing],
        );

        assert_eq!(
            env.additional_roots(),
            &[sibling.path().canonicalize().unwrap()]
        );
    }

    #[tokio::test]
    async fn workspace_process_subagent_fails_closed() {
        let root = scratch();
        let env = WorkspaceExecutionEnvironment::new(root.path());
        let error = env
            .process()
            .exec(
                "echo unsafe",
                env.workspace_root(),
                None,
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(error.contains("disabled in the workspace sandbox"));
    }

    #[tokio::test]
    async fn every_builtin_file_discovery_path_rejects_workspace_escape() {
        let root = workspace_tests_outside_scratch("denied-discovery-ws");
        let outside_root = workspace_tests_outside_scratch("denied-discovery-out");
        let outside_text = outside_root.join("secret.txt");
        let outside_image = outside_root.join("secret.png");
        std::fs::write(&outside_text, "secret").unwrap();
        std::fs::write(&outside_image, b"not actually an image").unwrap();
        let env: std::sync::Arc<dyn ExecutionEnvironment> =
            std::sync::Arc::new(WorkspaceExecutionEnvironment::new(&root));

        assert!(env.fs().read_to_string(&outside_text).await.is_err());
        assert!(env.fs().metadata(&outside_text).await.is_err());
        assert!(env.fs().list_dir(&outside_root).await.is_err());
    }

    #[tokio::test]
    async fn workspace_fs_confinement_disabled_admits_outside_path() {
        let root = scratch();
        let outside_root = workspace_tests_outside_scratch("unconfined-outside");
        let outside_file = outside_root.join("outside.txt");
        std::fs::write(&outside_file, "unconfined content").unwrap();

        let env = WorkspaceExecutionEnvironment::new(root.path());
        let shared = env.shared_confinement();

        // Confined by default
        assert!(matches!(
            env.fs().read(&outside_file).await,
            Err(FsError::PermissionDenied(_))
        ));

        // Disable confinement
        shared.set_confined(false);
        assert!(!env.is_confined());
        assert_eq!(
            env.fs().read_to_string(&outside_file).await.unwrap(),
            "unconfined content"
        );

        // Switch back to confined
        shared.set_confined(true);
        assert!(env.is_confined());
        assert!(matches!(
            env.fs().read(&outside_file).await,
            Err(FsError::PermissionDenied(_))
        ));
    }

    #[tokio::test]
    async fn workspace_shell_isolation_is_host_native() {
        let root = scratch();
        let env = WorkspaceExecutionEnvironment::new(root.path());

        assert_eq!(env.shell_isolation(), ShellIsolation::Host);
    }
}
