//! Standardized cross-platform path and directory resolution.
//!
//! Provides platform-native directory resolution compliant with:
//! - Linux / BSD: XDG Base Directory Specification
//! - macOS: Standard Apple user library directories
//! - Windows: Known Folders (`%APPDATA%`, `%LOCALAPPDATA%`)

use std::path::{Path, PathBuf};

/// Base platform directory layout contract.
pub trait PlatformPaths {
    /// Directory for user configuration files.
    fn config_dir(&self) -> &Path;

    /// Directory for persistent user data.
    fn data_dir(&self) -> &Path;

    /// Directory for state files (logs, history, sockets).
    fn state_dir(&self) -> &Path;

    /// Directory for non-essential cached data.
    fn cache_dir(&self) -> &Path;

    /// Directory for runtime state (sockets, ephemeral locks, pidfiles).
    fn runtime_dir(&self) -> Option<&Path>;
}

/// Resolved standard directory layout for an application name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardLayout {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: Option<PathBuf>,
}

impl PlatformPaths for StandardLayout {
    fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn runtime_dir(&self) -> Option<&Path> {
        self.runtime_dir.as_deref()
    }
}

impl StandardLayout {
    /// Resolve the standard native directory layout for the given application name.
    #[must_use]
    pub fn for_app(app_name: &str) -> Self {
        let config_dir = resolve_config_dir(app_name);
        let data_dir = resolve_data_dir(app_name);
        let state_dir = resolve_state_dir(app_name);
        let cache_dir = resolve_cache_dir(app_name);
        let runtime_dir = resolve_runtime_dir(app_name);

        Self {
            config_dir,
            data_dir,
            state_dir,
            cache_dir,
            runtime_dir,
        }
    }

    /// Creates all standard directories on disk if they do not exist.
    pub fn ensure_all_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.state_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        if let Some(ref rt) = self.runtime_dir {
            std::fs::create_dir_all(rt)?;
        }
        Ok(())
    }
}

fn resolve_config_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::config_dir() {
            return base.join(app);
        }
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, XDG or ~/Library/Application Support/<app>
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".config").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".config").join(app);
        }
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_data_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::data_local_dir() {
            return base.join(app);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("share").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("share").join(app);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_state_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::data_local_dir() {
            return base.join(app).join("state");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("state").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("state").join(app);
        }
    }
    dirs::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_cache_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::cache_dir() {
            return base.join(app);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".cache").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".cache").join(app);
        }
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_runtime_dir(app: &str) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join(app));
        }
    }
    let _ = app;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_non_empty_layout() {
        let layout = StandardLayout::for_app("nuo-test");
        assert!(!layout.config_dir.as_os_str().is_empty());
        assert!(!layout.data_dir.as_os_str().is_empty());
        assert!(!layout.state_dir.as_os_str().is_empty());
        assert!(!layout.cache_dir.as_os_str().is_empty());
        assert!(layout.config_dir.ends_with("nuo-test"));
    }
}


// --- High-level application path resolution and Dirs ---

//  Centralised path resolution for nuo's on-disk footprint.
// 
//  Every persistent path the program writes flows through [`Dirs`]. Resolution
//  honours the XDG Base Directory Specification and layers overrides in this
//  precedence order (highest first):
// 
//  1. `NUO_CONFIG_DIR` / `NUO_DATA_DIR` / `NUO_STATE_DIR` /
//     `NUO_CACHE_DIR` environment variables (app-specific
//     per-category overrides; more specific than the root, so one
//     category can still be carved out of a sandbox).
//  2. `NUO_HOME` — the instance selector: one variable
//     moves the entire footprint (`<home>/nuo/{config,data,state,
//     cache}` plus the daemon's runtime files under `instance/`), so a
//     dev or test build can never touch the host installation's state.
//  3. `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_STATE_HOME` /
//     `XDG_CACHE_HOME` / `XDG_RUNTIME_DIR` environment variables
//     (standard XDG overrides; relative values are ignored per spec).
//  4. Platform-native defaults via the `directories` crate (`config_dir`,
//     `data_dir`, `state_dir`, `cache_dir`).
//  5. `$HOME/.config`, `$HOME/.local/share`, ... fallbacks when even the
//     `directories` crate cannot resolve a native location.
// 
//  On Linux `$XDG_RUNTIME_DIR` is honoured for the daemon's runtime files;
//  if it is unset macOS/Linux use the data directory and Windows uses a
//  machine-local state subdirectory. The daemon-facing derivation of that
//  rule lives in [`Dirs::instance_dir`].

use std::sync::OnceLock;
#[cfg(any(test, feature = "test-path-override"))]
use std::sync::RwLock;

use directories::ProjectDirs;

/// App-specific override of the path roots supplied by the CLI or configuration.
///
/// Any field left as `None` falls back to env / native resolution.
#[derive(Debug, Clone, Default)]
pub struct PathsOverride {
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

/// The resolved on-disk layout. All paths are absolute and contain the `nuo`
/// segment as their final component (e.g. `~/.config/nuo`).
#[derive(Debug, Clone)]
pub struct Dirs {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    /// `$XDG_CACHE_HOME/nuo`. Written by the remote-skill cache (see
    /// [`Self::remote_skills_cache`]) and otherwise kept lazily by `fsutil`
    /// on first write.
    pub cache_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR/nuo` when set, otherwise `None` (callers fall
    /// back to `state_dir` for portability and to avoid surprising tmpfs
    /// use). For the daemon's runtime files prefer [`Self::instance_dir`],
    /// which folds this field together with the `NUO_HOME`
    /// override.
    pub runtime_dir: Option<PathBuf>,
}

impl Dirs {
    /// Resolve using the given overrides combined with env / native.
    pub fn resolve(overrides: &PathsOverride) -> Self {
        // A single application component is intentional. Supplying the app
        // name as both organization and application produces
        // `%APPDATA%\nuo\nuo` on Windows and an equally duplicated
        // macOS bundle path. Linux ignores those fields, which hid the bug.
        let project = ProjectDirs::from("", "", "nuo");
        // The instance root (ADR-0121): `NUO_HOME` is normalised to the `nuo`-suffixed
        // base once, so every category and the instance dir hang off one
        // location: `<home>/nuo/{config,data,state,cache,instance}`.
        // `app_dir_from_root` also tolerates a root that already ends in `nuo`.
        let home_base = nuo_home().map(app_dir_from_root);
        Self {
            config_dir: resolve_kind(
                Kind::Config,
                overrides.config_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            data_dir: resolve_kind(
                Kind::Data,
                overrides.data_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            state_dir: resolve_kind(
                Kind::State,
                overrides.state_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            cache_dir: resolve_kind(
                Kind::Cache,
                overrides.cache_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            runtime_dir: resolve_runtime(home_base.as_deref()),
        }
    }

    /// Resolve using only env / native defaults (no CLI overrides). Convenience
    /// for code paths that have not been plumbed through `main.rs`.
    pub fn system() -> Self {
        Self::resolve(&PathsOverride::default())
    }

    // well-known files

    /// User-edited configuration. `$XDG_CONFIG_HOME/nuo/config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// User-authored persistent roles (`$XDG_CONFIG_HOME/nuo/roles.toml`).
    /// A user-global registry of named agent roles, kept beside `config.toml`
    /// as user-edited config (never program state).
    pub fn roles_file(&self) -> PathBuf {
        self.config_dir.join("roles.toml")
    }

    /// Role-scoped config directory (`$XDG_CONFIG_HOME/nuo/roles/<name>/`, ADR-0253).
    pub fn role_config_dir(&self, role: &str) -> PathBuf {
        self.config_dir.join("roles").join(role)
    }

    /// Role-scoped MCP file (`$XDG_CONFIG_HOME/nuo/roles/<name>/mcp.json`, ADR-0253).
    pub fn role_mcp_file(&self, role: &str) -> PathBuf {
        self.role_config_dir(role).join("mcp.json")
    }

    /// Role-scoped data directory (`$XDG_DATA_HOME/nuo/roles/<name>/`, ADR-0253).
    pub fn role_data_dir(&self, role: &str) -> PathBuf {
        self.data_dir.join("roles").join(role)
    }

    /// Role-scoped skills directory (`$XDG_DATA_HOME/nuo/roles/<name>/skills/`, ADR-0253).
    pub fn role_skills_dir(&self, role: &str) -> PathBuf {
        self.role_data_dir(role).join("skills")
    }

    /// User-declared model provider customizations
    /// (`$XDG_CONFIG_HOME/nuo/model_providers.toml`, ADR-0199, ADR-0201).
    pub fn model_providers_file(&self) -> PathBuf {
        self.config_dir.join("model_providers.toml")
    }

    /// User-supplied color scheme files (`$XDG_CONFIG_HOME/nuo/themes`).
    /// Each `*.toml` in this directory defines a named theme with metadata
    /// and semantic palette / component overrides.
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    /// User-supplied ASCII logo for the empty-state hero.
    /// `$XDG_CONFIG_HOME/nuo/logo.txt`. When present, its lines replace the
    /// built-in figlet wordmark on the welcome screen (see `empty_state`).
    /// Optional and best-effort: missing/unreadable → built-in logo.
    pub fn logo_file(&self) -> PathBuf {
        self.config_dir.join("logo.txt")
    }

    /// Provider API keys, split out of `config.toml` into a file that holds
    /// **only** secrets. Written `rw-------` via [`crate::fsutil`] so keys
    /// never land on disk group- or world-readable. Keeping credentials here
    /// (rather than inline in `config.toml`) lets the config file be safely
    /// shared, screenshotted for support, or version-controlled, while
    /// `config.toml` keeps the provider *definitions* (id/name/transport/
    /// base_url/model). Resolution precedence — env var > credentials.toml >
    /// config inline — lives in the config layer.
    /// `$XDG_CONFIG_HOME/nuo/credentials.toml`.
    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join("credentials.toml")
    }

    /// OAuth token sets, keyed by exact connection id (`auth.toml`, 0600).
    /// Stored in `$XDG_STATE_HOME/nuo/auth.toml` as dynamic runtime state.
    pub fn auth_file(&self) -> PathBuf {
        self.state_dir.join("auth.toml")
    }

    /// Connections (`$XDG_STATE_HOME/nuo/connections.toml`). The
    /// program-managed "who I connect to" records — deliberately NOT in the
    /// user-edited `config.toml`, which holds behavior only. See
    /// the connections store.
    pub fn connections_file(&self) -> PathBuf {
        self.state_dir.join("connections.toml")
    }

    /// Legacy web connections (`$XDG_STATE_HOME/nuo/web_connections.toml`).
    /// Read only as a migration source; current runtime state does not depend on it.
    pub fn web_connections_file(&self) -> PathBuf {
        self.state_dir.join("web_connections.toml")
    }

    /// Cached model discovery lists and capability metadata
    /// (`$XDG_STATE_HOME/nuo/remote_catalog.json`).
    ///
    /// Lives under state (not cache) because the contents — discovered model
    /// ids, ETag revalidation state, advertised capability metadata — are
    /// program-generated state the user expects to persist across sessions,
    /// not regenerable-from-scratch cache data. See ADR-0123 §"Derived state".
    pub fn remote_catalog_cache_file(&self) -> PathBuf {
        self.state_dir.join("remote_catalog.json")
    }

    /// Retired catalog-cache state filename (`models_discovery.json`).
    ///
    /// Read **only** as an input to the one-shot `route_settings` fold, which
    /// rescues user reasoning overrides (non-derivable user input) from a
    /// pre-rename file. The catalog payload in the same file is derivable and
    /// is deliberately *not* migrated (ADR-0203 §29: no shims for regenerable
    /// state). Nothing writes here.
    pub fn retired_remote_catalog_state_file(&self) -> PathBuf {
        self.state_dir.join("models_discovery.json")
    }

    /// Retired cache-dir catalog location (`$XDG_CACHE_HOME/nuo/`), read only
    /// as the source of the one-shot `route_settings` fold, which recovers
    /// **user reasoning overrides** (non-derivable user data) from the pre-split
    /// file. The on-disk name `models_discovery.json` is a frozen historical
    /// fact, not vocabulary: renaming it would orphan the very data this read
    /// exists to recover. Nothing writes here (ADR-0203 §29: no legacy writes,
    /// no vocabulary aliases).
    pub fn legacy_remote_catalog_cache_file(&self) -> PathBuf {
        self.cache_dir.join("models_discovery.json")
    }

    /// Authoritative unified SQLite database file (ADR-0163 / ADR-0168).
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("nuo.db")
    }

    /// Role dialogue memory database file (for role-scoped RAG and memory decay).
    pub fn role_memory_db(&self) -> PathBuf {
        self.data_dir.join("role_memory.db")
    }

    /// Content-addressed blob store root. Large payloads are stored under
    /// `<root>/<2-char-prefix>/<hash>`.
    pub fn blobs_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    /// Persistent, program-generated data lives under here.
    pub fn projects_dir(&self) -> PathBuf {
        self.data_dir.join("projects")
    }

    /// Per-project bucket directory: `projects/<sha256(cwd)[..16]>`. Each
    /// project's sessions, current pointer, and metadata live under their own
    /// bucket, so different working directories never see each other's
    /// sessions. The hash is truncated to 16 hex chars (64 bits) — enough to
    /// make accidental collision astronomically unlikely across a single
    /// user's projects while keeping the directory name short and ASCII-safe.
    pub fn project_dir(&self, project_root: &Path) -> PathBuf {
        self.projects_dir().join(project_bucket_name(project_root))
    }

    /// User-global skills (`$XDG_DATA_HOME/nuo/skills`). Per-project skills
    /// still live under the project's working directory (`.nuo/skills/`)
    /// and are not stored here.
    pub fn user_skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }

    /// Cached remote skills (`$XDG_CACHE_HOME/nuo/skills/remote`). Safe to
    /// delete; repopulated on next `fetch_remote_repo`.
    pub fn remote_skills_cache(&self) -> PathBuf {
        self.cache_dir.join("skills").join("remote")
    }

    /// User-global slash commands (`$XDG_DATA_HOME/nuo/commands`). Project
    /// commands still live under `.nuo/commands/` in the working directory.
    pub fn user_commands_dir(&self) -> PathBuf {
        self.data_dir.join("commands")
    }

    /// Per-project directory holding every session file. As of ADR-0018 each
    /// live `nuo` instance pins its own `sessions/<id>.json` plus
    /// `sessions/<id>.jsonl` here, so concurrent instances never share a
    /// mutable file. Replaces the legacy single project-root `session.json`.
    pub fn project_sessions_dir(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("sessions")
    }

    /// Directory bucket for a workspace-free (unbound) session set:
    /// `projects/<hash(key)>`. Unbound sessions have no workspace path, so
    /// their stable bucket derives from a fixed key (ADR-0226).
    pub fn bucket_dir(&self, bucket_key: &str) -> PathBuf {
        self.projects_dir().join(bucket_name(bucket_key))
    }

    /// Sessions directory for a workspace-free (unbound) session set.
    pub fn bucket_sessions_dir(&self, bucket_key: &str) -> PathBuf {
        self.bucket_dir(bucket_key).join("sessions")
    }

    /// Per-project `/debug trace` capture directory: `projects/<bucket>/network`.
    /// Each provider round-trip is written here as one owner-only JSON file
    /// while tracing is armed. Mirror of the `sessions/` layout; the
    /// directory is created lazily on first write by `atomic_write_bytes`.
    pub fn project_network_dir(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("network")
    }

    /// Per-project `/debug preview` directory: `projects/<bucket>/debug`.
    /// One owner-only JSON file is written here per `/debug preview` invocation —
    /// a dry-run of the request that *would* be sent (rebuilt system message +
    /// auto-loaded skills + message list + tool schemas + token pressure),
    /// without calling the provider. The directory is created lazily on first
    /// write by `atomic_write_bytes`. Mirror of the `network/` layout.
    pub fn project_debug_dir(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("debug")
    }

    /// One session's snapshot path: `sessions/<id>.json`. The matching event
    /// log lives at `sessions/<id>.jsonl` (derived via `with_extension`).
    pub fn project_session_file(&self, project_root: &Path, id: &str) -> PathBuf {
        self.project_sessions_dir(project_root)
            .join(format!("{id}.json"))
    }

    /// Per-project persistent "always allow" permission rules. The cached
    /// rules from `PermissionDecision::Always` are mirrored here so a new
    /// session in the same project inherits prior approvals instead of
    /// re-prompting for the same operations. Best-effort; absence or parse
    /// failure is non-fatal (the agent just asks the user again).
    pub fn project_permissions(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("permissions.json")
    }

    /// Structured log directory for the rolling appender, under
    /// `$XDG_STATE_HOME/nuo/log`. Used by `init_tracing` at startup
    /// and by `Self::ensure` in tests.
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("log")
    }

    // helpers

    /// The **daemon instance directory**: the one directory holding the
    /// per-daemon runtime files — `daemon.json` (discovery), `daemon.sock`
    /// (control plane), `daemon.lock` (single-instance flock), `serve/`
    /// (legacy records) — and nothing else (ADR-0121).
    ///
    /// It is exactly [`Self::runtime_dir`] when a runtime location resolves
    /// (`NUO_HOME` → `<home>/nuo/instance`, else
    /// `$XDG_RUNTIME_DIR/nuo`), else the data dir as the portable
    /// fallback. Windows instead uses `state_dir/instance`, keeping process
    /// coordination out of the roaming profile. The rule is named once here.
    ///
    /// Code that touches daemon runtime files must use this method — never
    /// `runtime_dir` directly — so every daemon-facing path observes the
    /// same override stack. `runtime_dir` stays public as the raw resolved
    /// location for diagnostics and tests that anchor other ephemeral files.
    pub fn instance_dir(&self) -> PathBuf {
        self.runtime_dir.clone().unwrap_or_else(|| {
            #[cfg(windows)]
            {
                return self.state_dir.join("instance");
            }
            #[cfg(not(windows))]
            self.data_dir.clone()
        })
    }

    /// Best-effort initial creation of every directory nuo may write to.
    /// Idempotent. Errors are surfaced as a single aggregate `String`. Used by
    /// tests; production creates directories lazily via `fsutil` on first write.
    #[cfg(test)]
    pub fn ensure(&self) -> Result<(), String> {
        for path in [
            &self.config_dir,
            &self.data_dir,
            &self.state_dir,
            &self.cache_dir,
            &self.projects_dir(),
            &self.user_skills_dir(),
            &self.user_commands_dir(),
            &self.remote_skills_cache(),
            &self.log_dir(),
        ] {
            std::fs::create_dir_all(path)
                .map_err(|e| format!("could not create directory {}: {e}", path.display()))?;
        }
        if let Some(runtime) = &self.runtime_dir {
            // Best-effort: the runtime directory is ephemeral and may not be
            // writable in sandboxes or when an unrelated test set
            // `XDG_RUNTIME_DIR`. Do not let this prevent data/state creation.
            let _ = std::fs::create_dir_all(runtime);
        }
        Ok(())
    }
}

/// Global process-wide [`Dirs`] instance. `main` installs it once via
/// [`set_default`] (the `NUO_HOME` override, ADR-0121); every other
/// module reads via [`get`].
///
/// Implementation: a `std::sync::OnceLock` holds the production value (set
/// exactly once at startup, never replaced, so production code can rely on
/// stability). A separate `std::sync::RwLock` layered on top is used **only
/// by tests** to swap in isolated `Dirs` per test, since tests cannot reset a
/// `OnceLock`. Production reads ([`get`]) check the test override first; if it
/// is empty they fall back to the `OnceLock`, then to a fresh
/// [`Dirs::system`] resolution.
static DEFAULT: OnceLock<Dirs> = OnceLock::new();
/// Test-only override. Marked `allow(dead_code)` because a production build
/// compiles the static but never reads it (every accessor sits behind the
/// same `test` / `test-path-override` gate).
#[cfg(any(test, feature = "test-path-override"))]
static TEST_OVERRIDE: RwLock<Option<Dirs>> = RwLock::new(None);

/// Single process-wide lock that **every** test touching [`set_test_default`]
/// must hold for the duration of its override. Without this, tests in
/// different modules each used their own per-module lock (`config`'s
/// `PATHS_GUARD`, `session`'s `GLOBAL_GUARD`), so two such tests ran
/// concurrently and stomped the shared `TEST_OVERRIDE` — a flaky cross-test
/// race. Routing all of them through one lock serialises the critical section
/// regardless of which module the test lives in.
#[cfg(any(test, feature = "test-path-override"))]
pub static TEST_OVERRIDE_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Install the process-wide [`Dirs`]. Idempotent: subsequent calls in the same
/// process are no-ops (the first value wins), matching production semantics.
/// Returns `Ok(None)` on first install or `Ok(Some(previous))` if a value was
/// already set (the new value is NOT stored in that case).
///
/// `mutx`'s `main` calls this once at startup to install the
/// `NUO_HOME` override (ADR-0121) before any path is resolved; library
/// code that runs outside `main` (tests, examples) falls back to
/// [`Dirs::system`] via [`get`].
pub fn set_default(dirs: Dirs) -> Result<Option<Dirs>, Dirs> {
    match DEFAULT.set(dirs) {
        Ok(()) => Ok(None),
        Err(existing) => Ok(Some(existing)),
    }
}

/// Test-only override of the process-wide [`Dirs`]. Pass `None` to clear.
/// Production code MUST NOT call this — it exists purely so unit tests can run
/// with isolated `data_dir`/`state_dir` roots without polluting the real
/// filesystem or racing the `OnceLock`.
///
/// Compiled under `#[cfg(any(test, feature = "test-path-override"))]`: the
/// `test-path-override` feature exists so *other crates'* test suites (which
/// cannot see this crate's `cfg(test)`) can install the same sandbox. A
/// dev-dependency with `features = ["test-path-override"]` opts a crate into
/// it without leaking the hook into production builds.
#[cfg(any(test, feature = "test-path-override"))]
pub fn set_test_default(dirs: Option<Dirs>) {
    *TEST_OVERRIDE.write().unwrap_or_else(|e| e.into_inner()) = dirs;
}

/// Access the process-wide [`Dirs`]. Falls back to [`Dirs::system`] when
/// [`set_default`] has not been called yet (e.g. in tests, or in library code
/// invoked outside of `main`). When a test override is installed (via the
/// test-only `set_test_default`), that value wins over the production install.
pub fn get() -> Dirs {
    #[cfg(any(test, feature = "test-path-override"))]
    if let Some(d) = TEST_OVERRIDE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return d;
    }
    match DEFAULT.get() {
        Some(d) => d.clone(),
        None => Dirs::system(),
    }
}

// Internals

#[derive(Clone, Copy)]
enum Kind {
    Config,
    Data,
    State,
    Cache,
}

impl Kind {
    fn app_env_var(self) -> &'static str {
        match self {
            Kind::Config => "NUO_CONFIG_DIR",
            Kind::Data => "NUO_DATA_DIR",
            Kind::State => "NUO_STATE_DIR",
            Kind::Cache => "NUO_CACHE_DIR",
        }
    }

    fn xdg_env_var(self) -> &'static str {
        match self {
            Kind::Config => "XDG_CONFIG_HOME",
            Kind::Data => "XDG_DATA_HOME",
            Kind::State => "XDG_STATE_HOME",
            Kind::Cache => "XDG_CACHE_HOME",
        }
    }

    fn fallback_segment(self) -> &'static str {
        match self {
            Kind::Config => ".config",
            Kind::Data => ".local/share",
            Kind::State => ".local/state",
            Kind::Cache => ".cache",
        }
    }

    /// The subdirectory under an instance root (ADR-0121). Plain names,
    /// not XDG segments: the instance root is not an XDG hierarchy and
    /// `app_dir_from_root` appends the `nuo` segment once.
    fn home_segment(self) -> &'static str {
        match self {
            Kind::Config => "config",
            Kind::Data => "data",
            Kind::State => "state",
            Kind::Cache => "cache",
        }
    }

    fn native(self, project: Option<&ProjectDirs>) -> Option<PathBuf> {
        let p = project?;
        Some(match self {
            Kind::Config => p.config_dir().to_path_buf(),
            Kind::Data => p.data_dir().to_path_buf(),
            Kind::State => p.state_dir().map(Path::to_path_buf).unwrap_or_else(|| {
                #[cfg(windows)]
                {
                    // State is machine-local and must not roam with the user
                    // profile. `data_local_dir` ends in `data`; use its app
                    // parent to produce `%LOCALAPPDATA%\nuo\state`.
                    return p
                        .data_local_dir()
                        .parent()
                        .unwrap_or_else(|| p.data_local_dir())
                        .join("state");
                }
                #[cfg(not(windows))]
                {
                    // macOS has no separate state directory. Keep it namespaced
                    // under this application's Application Support directory.
                    p.data_dir().join("state")
                }
            }),
            Kind::Cache => p.cache_dir().to_path_buf(),
        })
    }
}

/// The `NUO_HOME` env layer of the instance-root selector (ADR-0121).
/// Returns the raw value; the caller normalises it to the `nuo`-suffixed
/// base exactly once. The value must be absolute and non-empty; a relative
/// value is ignored (with a warning) because an instance root only isolates
/// when both processes see the same absolute location.
fn nuo_home() -> Option<PathBuf> {
    let value = std::env::var_os("NUO_HOME")?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        tracing::warn!(
            value = %path.display(),
            "NUO_HOME must be absolute; ignoring it (no sandbox active)"
        );
        return None;
    }
    Some(path)
}

/// Resolve the daemon's runtime location (ADR-0121): the instance root's
/// `instance/` subdirectory when one is active, else `$XDG_RUNTIME_DIR/
/// nuo` (pre-0121 behaviour, unchanged). A relative or empty env value
/// is ignored with a warning: an instance root only isolates when both
/// processes see the same absolute location.
fn resolve_runtime(home_base: Option<&Path>) -> Option<PathBuf> {
    if let Some(base) = home_base {
        return Some(base.join("instance"));
    }
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(x) => {
            let p = PathBuf::from(x);
            if p.is_absolute() {
                return Some(p.join("nuo"));
            }
            tracing::warn!(value = %p.display(), "XDG_RUNTIME_DIR must be absolute; ignoring it");
            None
        }
        None => None,
    }
}

fn resolve_kind(
    kind: Kind,
    override_path: Option<PathBuf>,
    home: Option<&Path>,
    project: Option<&ProjectDirs>,
) -> PathBuf {
    // 1. CLI flag
    if let Some(p) = override_path {
        return app_dir_from_root(p);
    }
    // 2. NUO_* env (per-category, the most specific selector)
    if let Some(p) = std::env::var_os(kind.app_env_var()).filter(|v| !v.is_empty()) {
        return app_dir_from_root(PathBuf::from(p));
    }
    // 3. Instance root (ADR-0121): `NUO_HOME` env.
    //    `home` is already the `nuo`-suffixed base, so only the category
    //    segment appends.
    if let Some(base) = home {
        return base.join(kind.home_segment());
    }
    // 4. XDG_* env (must be absolute per spec, otherwise ignored)
    if let Some(p) = std::env::var_os(kind.xdg_env_var()).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            return app_dir_from_root(p);
        }
    }
    // 5. Native
    if let Some(p) = kind.native(project) {
        // `directories` already returns the app-suffixed path
        return p;
    }
    // 6. Home fallback
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        if home.is_absolute() {
            return home.join(kind.fallback_segment()).join("nuo");
        }
    }
    // Last resort: cwd. Better than panicking.
    app_dir_from_root(PathBuf::from("."))
}

/// Given a root directory (e.g. `--data-dir=/tmp/x` or `$XDG_DATA_HOME=/foo`),
/// append the `nuo` segment unless the caller already named a directory that
/// ends in `nuo` (so `--data-dir=~/.local/share/nuo` and
/// `--data-dir=~/.local/share` both do the right thing).
fn app_dir_from_root(root: PathBuf) -> PathBuf {
    if root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "nuo")
        .unwrap_or(false)
    {
        root
    } else {
        root.join("nuo")
    }
}

/// Find the project root by walking upward from `start` looking for common
/// repository and project markers (`.nuo`, `.git`, `Cargo.toml`, `package.json`, etc.).
/// Falls back to `start` if no marker is found.
pub fn find_project_root(start: &Path) -> PathBuf {
    const MARKERS: &[&str] = &[".nuo", ".git", "Cargo.toml", "package.json", "pyproject.toml", "go.mod"];
    let temp_dir = std::env::temp_dir();
    for ancestor in start.ancestors() {
        if ancestor == temp_dir && ancestor != start {
            break;
        }
        for marker in MARKERS {
            if ancestor.join(marker).exists() {
                return ancestor.to_path_buf();
            }
        }
    }
    start.to_path_buf()
}

/// Map a project root (cwd) to a stable, ASCII-safe bucket name. Uses the first
/// 16 hex chars of SHA-256 so the layout is reproducible across processes,
/// Rust versions, and platforms, and so the cwd is not leaked in the path
/// structure (paths may contain sensitive directory names).
pub fn project_bucket_name(project_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let normalised = normalise_project_root(project_root);
    let mut hasher = Sha256::new();
    hasher.update(normalised.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Map a workspace-free bucket key to a stable, ASCII-safe bucket name.
/// Mirrors [`project_bucket_name`] (first 16 hex chars of SHA-256) so the
/// unbound session set has one reproducible on-disk bucket.
pub fn bucket_name(bucket_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bucket_key.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Canonicalise a project root for hashing. Redundant trailing slashes are
/// stripped, and on POSIX `..`/`.` segments are collapsed via
/// [`Path::canonicalize`] when the path actually exists; otherwise the raw path
/// is used (so a not-yet-created `--project` still produces a stable name).
fn normalise_project_root(path: &Path) -> String {
    let trimmed = path
        .to_str()
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_default();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    trimmed
}

#[cfg(test)]
mod dir_tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests that mutate process-wide env vars (`XDG_*`, `MUTA_*`, `HOME`)
    /// cannot run in parallel with each other or with tests that read those
    /// vars. We serialise them through this global lock. Tests that don't touch
    /// env vars omit the guard and can still run in parallel.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn absolute_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join("nuo-path-tests").join(name)
    }

    macro_rules! env_locked {
        ($body:block) => {{
            let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            $body
        }};
    }

    #[test]
    fn app_dir_from_root_appends_muta_segment() {
        let p = app_dir_from_root(PathBuf::from("/tmp/foo"));
        assert_eq!(p, PathBuf::from("/tmp/foo/nuo"));
    }

    #[test]
    fn app_dir_from_root_does_not_double_append() {
        let p = app_dir_from_root(PathBuf::from("/tmp/foo/nuo"));
        assert_eq!(p, PathBuf::from("/tmp/foo/nuo"));
    }

    #[test]
    fn native_project_identity_has_one_application_component() {
        let project = ProjectDirs::from("", "", "nuo").expect("native project dirs");
        assert_eq!(project.project_path(), Path::new("nuo"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_is_machine_local_and_app_scoped() {
        let project = ProjectDirs::from("", "", "nuo").expect("native project dirs");
        let state = Kind::State
            .native(Some(&project))
            .expect("native state dir");
        assert!(state.ends_with(Path::new("nuo").join("state")));
        assert!(state.starts_with(project.data_local_dir().parent().unwrap()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_state_stays_inside_application_support_namespace() {
        let project = ProjectDirs::from("", "", "nuo").expect("native project dirs");
        let state = Kind::State
            .native(Some(&project))
            .expect("native state dir");
        assert_eq!(state, project.data_dir().join("state"));
    }

    #[test]
    fn resolve_honours_nuo_env_over_xdg_env() {
        env_locked!({
            let muta_data = absolute_test_root("nuo-data");
            let xdg_data = absolute_test_root("xdg-data");
            unsafe {
                std::env::set_var("NUO_DATA_DIR", &muta_data);
            }
            unsafe {
                std::env::set_var("XDG_DATA_HOME", xdg_data);
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(dirs.data_dir, muta_data.join("nuo"));
            unsafe {
                std::env::remove_var("NUO_DATA_DIR");
            }
            unsafe {
                std::env::remove_var("XDG_DATA_HOME");
            }
        });
    }

    #[test]
    fn resolve_cli_override_beats_env() {
        env_locked!({
            let env_data = absolute_test_root("env-loses");
            let cli_data = absolute_test_root("cli-wins");
            unsafe {
                std::env::set_var("NUO_DATA_DIR", env_data);
            }
            let dirs = Dirs::resolve(&PathsOverride {
                data_dir: Some(cli_data.clone()),
                ..Default::default()
            });
            assert_eq!(dirs.data_dir, cli_data.join("nuo"));
            unsafe {
                std::env::remove_var("NUO_DATA_DIR");
            }
        });
    }

    #[test]
    fn resolve_ignores_relative_xdg_var() {
        env_locked!({
            unsafe {
                std::env::set_var("XDG_CACHE_HOME", "relative/path");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            // The documented precedence (ADR-0014 §3): a relative XDG var is
            // ignored and resolution falls through to the native base dir
            // (`directories`) — an absolute path under the real cache root.
            // The disjunction this replaces (`absolute || starts_with(".")`)
            // accepted both outcomes and pinned neither.
            assert!(dirs.cache_dir.is_absolute());
            assert!(
                !dirs.cache_dir.ends_with("relative/path"),
                "relative XDG value must be ignored, got {}",
                dirs.cache_dir.display()
            );
            unsafe {
                std::env::remove_var("XDG_CACHE_HOME");
            }
        });
    }

    #[test]
    fn runtime_dir_only_when_xdg_runtime_dir_set() {
        env_locked!({
            let runtime = absolute_test_root("runtime");
            unsafe {
                std::env::remove_var("NUO_HOME");
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(dirs.runtime_dir.is_none());
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", &runtime);
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.runtime_dir.as_deref(),
                Some(runtime.join("nuo").as_path())
            );
            unsafe {
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
        });
    }

    // NUO_HOME instance root (ADR-0121)

    #[test]
    fn nuo_home_redirects_every_category_and_the_instance_dir() {
        env_locked!({
            let runtime = absolute_test_root("runtime-priority");
            let home = absolute_test_root("home");
            for var in [
                "NUO_HOME",
                "NUO_CONFIG_DIR",
                "NUO_DATA_DIR",
                "NUO_STATE_DIR",
                "NUO_CACHE_DIR",
            ] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", &runtime);
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.instance_dir(),
                runtime.join("nuo"),
                "without NUO_HOME the XDG runtime dir still wins"
            );

            unsafe {
                std::env::set_var("NUO_HOME", &home);
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            let app_home = home.join("nuo");
            assert_eq!(dirs.config_dir, app_home.join("config"));
            assert_eq!(dirs.data_dir, app_home.join("data"));
            assert_eq!(dirs.state_dir, app_home.join("state"));
            assert_eq!(dirs.cache_dir, app_home.join("cache"));
            assert_eq!(
                dirs.instance_dir(),
                app_home.join("instance"),
                "the instance dir must follow the sandbox root, not the host XDG runtime"
            );

            for var in ["NUO_HOME", "XDG_RUNTIME_DIR"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
        });
    }

    #[test]
    fn nuo_home_leaves_headroom_for_per_category_overrides() {
        env_locked!({
            let home = absolute_test_root("root-home");
            let data = absolute_test_root("explicit-data");
            let runtime = absolute_test_root("ignored-runtime");
            unsafe {
                std::env::set_var("NUO_HOME", &home);
                std::env::set_var("NUO_DATA_DIR", &data);
                std::env::set_var("XDG_RUNTIME_DIR", runtime);
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.data_dir,
                data.join("nuo"),
                "a per-category env var is more specific than the instance root"
            );
            assert_eq!(
                dirs.instance_dir(),
                home.join("nuo").join("instance"),
                "the daemon runtime files follow the root"
            );
            assert_eq!(
                dirs.config_dir,
                home.join("nuo").join("config"),
                "categories without an explicit override keep following the root"
            );
            for var in ["NUO_HOME", "NUO_DATA_DIR", "XDG_RUNTIME_DIR"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
        });
    }

    #[test]
    fn relative_or_empty_nuo_home_is_ignored() {
        env_locked!({
            unsafe {
                std::env::set_var("NUO_HOME", "relative/home");
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(
                dirs.runtime_dir.is_none(),
                "a relative sandbox root must not half-apply"
            );
            unsafe {
                std::env::set_var("NUO_HOME", "");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(dirs.runtime_dir.is_none(), "an empty root is unset");
            unsafe {
                std::env::remove_var("NUO_HOME");
            }
        });
    }

    #[test]
    fn instance_dir_uses_the_native_fallback_without_a_runtime_location() {
        env_locked!({
            for var in ["XDG_RUNTIME_DIR", "NUO_HOME"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            #[cfg(windows)]
            assert_eq!(dirs.instance_dir(), dirs.state_dir.join("instance"));
            #[cfg(not(windows))]
            assert_eq!(dirs.instance_dir(), dirs.data_dir);
        });
    }

    #[test]
    fn project_bucket_name_is_stable_and_ascii_safe() {
        let n1 = project_bucket_name(Path::new("/home/user/code/muta"));
        let n2 = project_bucket_name(Path::new("/home/user/code/muta"));
        assert_eq!(n1, n2, "must be stable for the same input");
        assert_eq!(n1.len(), 16, "must be 16 hex chars (8 bytes)");
        assert!(n1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn project_bucket_name_normalises_trailing_slash() {
        let a = project_bucket_name(Path::new("/foo/bar"));
        let b = project_bucket_name(Path::new("/foo/bar/"));
        assert_eq!(a, b, "trailing slash must not change the bucket");
    }

    #[test]
    fn project_bucket_name_distinguishes_different_roots() {
        let a = project_bucket_name(Path::new("/foo/aaa"));
        let b = project_bucket_name(Path::new("/foo/bbb"));
        assert_ne!(a, b);
    }

    #[test]
    fn project_dir_under_projects_root() {
        let dirs = Dirs::resolve(&PathsOverride {
            data_dir: Some(PathBuf::from("/tmp/nd")),
            ..Default::default()
        });
        let project_root = Path::new("/home/me/proj");
        let bucket = project_bucket_name(project_root);
        assert_eq!(
            dirs.project_dir(project_root),
            PathBuf::from(format!("/tmp/nd/nuo/projects/{bucket}"))
        );
    }

    #[test]
    fn project_permissions_under_project_bucket() {
        let dirs = Dirs::resolve(&PathsOverride {
            data_dir: Some(PathBuf::from("/tmp/nd")),
            ..Default::default()
        });
        let project_root = Path::new("/home/me/proj");
        let bucket = project_bucket_name(project_root);
        assert_eq!(
            dirs.project_permissions(project_root),
            PathBuf::from(format!("/tmp/nd/nuo/projects/{bucket}/permissions.json"))
        );
    }
}
