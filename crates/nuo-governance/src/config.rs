//! Declarative repository governance configuration.

use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GovernanceConfig {
    #[serde(default)]
    pub docs: DocsConfig,
    #[serde(default)]
    pub adr: AdrConfig,
    #[serde(default)]
    pub invariants: InvariantsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocsConfig {
    #[serde(default = "default_doc_roots")]
    pub doc_roots: Vec<String>,
    #[serde(default = "default_root_files")]
    pub root_files: Vec<String>,
    #[serde(default = "default_user_owned_surfaces")]
    pub user_owned_surfaces: Vec<String>,
    #[serde(default)]
    pub firewall: FirewallConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FirewallConfig {
    #[serde(default = "default_public_surfaces")]
    pub public_surfaces: Vec<String>,
    #[serde(default = "default_internal_space")]
    pub internal_space: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdrConfig {
    #[serde(default = "default_adr_dir")]
    pub directory: String,
    #[serde(default = "default_string_allowlist")]
    pub string_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvariantsConfig {
    #[serde(default = "default_invariants_dir")]
    pub directory: String,
}

fn default_doc_roots() -> Vec<String> {
    vec!["docs".to_string()]
}

fn default_root_files() -> Vec<String> {
    vec!["README.md".to_string(), "CHANGELOG.md".to_string()]
}

fn default_user_owned_surfaces() -> Vec<String> {
    vec!["AGENTS.md".to_string()]
}

fn default_public_surfaces() -> Vec<String> {
    vec![
        "docs/tutorials/".to_string(),
        "docs/how-to/".to_string(),
        "docs/reference/".to_string(),
        "docs/explanation/".to_string(),
    ]
}

fn default_internal_space() -> String {
    "docs/dev/".to_string()
}

fn default_adr_dir() -> String {
    "docs/adr".to_string()
}

fn default_string_allowlist() -> Vec<String> {
    vec!["crates/nuo-persistence/src/db/migrations.rs".to_string()]
}

fn default_invariants_dir() -> String {
    "docs".to_string()
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            doc_roots: default_doc_roots(),
            root_files: default_root_files(),
            user_owned_surfaces: default_user_owned_surfaces(),
            firewall: FirewallConfig::default(),
        }
    }
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            public_surfaces: default_public_surfaces(),
            internal_space: default_internal_space(),
        }
    }
}

impl Default for AdrConfig {
    fn default() -> Self {
        Self {
            directory: default_adr_dir(),
            string_allowlist: default_string_allowlist(),
        }
    }
}

impl Default for InvariantsConfig {
    fn default() -> Self {
        Self {
            directory: default_invariants_dir(),
        }
    }
}

impl GovernanceConfig {
    pub fn load_from_workspace(root: &Path) -> Self {
        let config_path = root.join("docs/governance.toml");
        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(cfg) = toml::from_str::<GovernanceConfig>(&content) {
                return cfg;
            }
        }
        Self::default()
    }
}
