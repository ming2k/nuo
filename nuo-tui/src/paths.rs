//! Centralised path resolution for the `mutx` TUI application.
//!
//! Follows XDG Base Directory Specification and decouples client TUI paths
//! (`$XDG_CONFIG_HOME/mutx`, `$XDG_STATE_HOME/mutx`) from the core daemon
//! (`$XDG_CONFIG_HOME/muta`, `$XDG_STATE_HOME/muta`).

use std::path::PathBuf;
use std::sync::OnceLock;

/// The resolved on-disk layout for mutx.
#[derive(Debug, Clone)]
pub struct MutxPaths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl MutxPaths {
    pub fn resolve() -> Self {
        let config_dir = resolve_config_dir();
        let state_dir = resolve_state_dir();
        Self {
            config_dir,
            state_dir,
        }
    }

    /// User-edited TUI configuration: `$XDG_CONFIG_HOME/mutx/config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// User-supplied color scheme files: `$XDG_CONFIG_HOME/mutx/themes`.
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    /// User-supplied ASCII logo: `$XDG_CONFIG_HOME/mutx/logo.txt`.
    pub fn logo_file(&self) -> PathBuf {
        self.config_dir.join("logo.txt")
    }
}

static PATHS: OnceLock<MutxPaths> = OnceLock::new();

pub fn get() -> &'static MutxPaths {
    PATHS.get_or_init(MutxPaths::resolve)
}

fn resolve_config_dir() -> PathBuf {
    if let Some(val) = std::env::var_os("NUOX_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(val);
    }
    if let Some(home) = std::env::var_os("NUOX_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("config");
    }
    if let Some(home) = std::env::var_os("MUTA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("mutx").join("config");
    }
    dirs::config_dir()
        .map(|d| d.join("mutx"))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".config").join("mutx"))
                .unwrap_or_else(|| PathBuf::from(".config/mutx"))
        })
}

fn resolve_state_dir() -> PathBuf {
    if let Some(val) = std::env::var_os("NUOX_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(val);
    }
    if let Some(home) = std::env::var_os("NUOX_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("state");
    }
    if let Some(home) = std::env::var_os("MUTA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("mutx").join("state");
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("mutx"))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".local").join("state").join("mutx"))
                .unwrap_or_else(|| PathBuf::from(".local/state/mutx"))
        })
}
