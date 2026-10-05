//! Shared configuration schema for skills.
//!
//! Lives in `nuo-wire` for the same reason [`crate::WebSearchConfig`] and
//! [`crate::McpServerConfig`] do: the app-layer `Config` owns the `[skills]`
//! table and the loader in `nuo-skills` needs to read it, while the store
//! does not depend on that implementation crate.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Skill configuration stored under `[skills]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillsConfig {
    /// Additional local directories to scan for skills.
    pub paths: Vec<String>,
    /// Remote skill repositories to fetch and cache.
    pub urls: Vec<String>,
    /// Skill names to disable (case-sensitive).
    pub disabled: Vec<String>,
    /// Project root the project-local skill sources (`.nuo/skills`,
    /// `skills/`) resolve from. Runtime-populated
    /// by the session bootstrap — never deserialized from `config.toml`
    /// (a config file must not name a workspace) — and `None` in contexts
    /// without a designated project (tests, `nuo config`), where
    /// discovery falls back to the process cwd. Under the unified daemon
    /// (ADR-0096) one process hosts sessions for many projects, so this
    /// field is what keeps each session's skill catalog scoped to its own
    /// project instead of whichever directory first spawned the daemon.
    #[serde(skip)]
    pub project_root: Option<PathBuf>,
    /// Active agent role name (e.g. "philosophist", "developer").
    /// Runtime-populated by the session bootstrap (ADR-0253).
    #[serde(skip)]
    pub role: Option<String>,
}

impl SkillsConfig {
    /// True when no skill configuration is present.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.urls.is_empty() && self.disabled.is_empty()
    }

    /// True when the given skill name is disabled.
    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.iter().any(|n| n == name)
    }
}
