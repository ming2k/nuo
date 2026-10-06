use nuo_wire::MainAgentRole;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use nuo_wire::policy_schema::{CustomRole, RoleWorkspace};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RolesConfig {
    #[serde(default)]
    pub roles: BTreeMap<String, CustomRole>,
}

impl RolesConfig {
    /// Load `roles.toml` from the resolved config directory.
    /// Absent file = no roles. A parse or validation error is logged and
    /// degrades to an empty registry rather than failing startup.
    pub fn load() -> Self {
        let path = crate::paths::get().roles_file();
        match std::fs::read_to_string(&path) {
            Ok(raw) => match Self::from_toml_str(&raw) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "roles.toml invalid; ignored");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Load with optional workspace override (ADR-0243).
    /// If `workspace_root` contains `.nuo/roles.toml` or `.nuo/config.toml` with `[roles.<name>]`,
    /// those definitions override or extend the global roles.
    pub fn load_for_workspace(workspace_root: Option<&std::path::Path>) -> Self {
        let mut config = Self::load();
        if let Some(root) = workspace_root {
            for dir in &[".nuo"] {
                for candidate in &["roles.toml", "config.toml"] {
                    let project_cfg_path = root.join(dir).join(candidate);
                if project_cfg_path.exists()
                    && let Ok(raw) = std::fs::read_to_string(&project_cfg_path)
                    && let Ok(override_cfg) = Self::from_toml_str(&raw)
                {
                    for (id, role) in override_cfg.roles {
                        config.roles.insert(id, role);
                    }
                }
            }
            }
        }
        config
    }

    /// Parse and validate a `roles.toml` body.
    pub fn from_toml_str(raw: &str) -> Result<Self, String> {
        let mut config: Self =
            toml::from_str(raw).map_err(|error| format!("roles.toml parse error: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&mut self) -> Result<(), String> {
        for (id, role) in self.roles.iter_mut() {
            if !is_valid_id(id) {
                return Err(format!(
                    "invalid role id '{id}': must be strict kebab-case (e.g. 'code-reviewer', lowercase letters, digits, and hyphens)"
                ));
            }
            role.validate(id)
                .map_err(|error| format!("role '{id}': {error}"))?;
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&CustomRole> {
        self.roles.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.roles.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }
}

/// Validate whether an identifier strictly conforms to kebab-case (ADR-0246).
/// Must consist of lowercase ASCII alphanumeric segments separated by single hyphens.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Resolve an immutable SessionRoleManifest for a role ID within a workspace context (ADR-0245, ADR-0246).
pub fn resolve_role_manifest(
    workspace_root: Option<&std::path::Path>,
    role_id: Option<&str>,
) -> nuo_wire::SessionRoleManifest {
    let role_id = role_id.unwrap_or("developer");
    let roles_cfg = RolesConfig::load_for_workspace(workspace_root);
    if let Some(user_role) = roles_cfg.get(role_id) {
        let identity = user_role.identity();
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        nuo_wire::SessionRoleManifest {
            role_id: role_id.to_string(),
            name: user_role.name.clone(),
            description: user_role.description.clone(),
            instructions: user_role.instructions.clone(),
            identity,
            tools: user_role.tools.clone(),
            admit_mcp: user_role.admit_mcp.clone(),
            created_at_s: now_s,
        }
    } else if let Some(builtin) = MainAgentRole::parse(role_id) {
        match builtin {
            MainAgentRole::Developer => nuo_wire::SessionRoleManifest::developer(),
            MainAgentRole::Philosophist => nuo_wire::SessionRoleManifest::philosophist(),
            MainAgentRole::Ops => nuo_wire::SessionRoleManifest::ops(),
        }
    } else {
        let mut manifest = nuo_wire::SessionRoleManifest::developer();
        manifest.role_id = role_id.to_string();
        manifest.name = role_id.to_string();
        manifest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[roles.english-practice]
name = "English Practice"
description = "a patient English conversation partner who corrects gently in context"
instructions = "You are a patient English conversation partner."
workspace = "none"
tools = ["ask_user", "read_url"]
admit_mcp = ["obsidian", "phil*"]

[roles.coder]
name = "Coder"

[roles.code-reviewer]
name = "Code Reviewer"
workspace = "inherit"
tools = ["read_*", "code_query"]
"#;

    #[test]
    fn parses_and_round_trips_a_role() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("english-practice").unwrap();
        assert_eq!(role.resolved_workspace(), RoleWorkspace::None);
        assert_eq!(
            role.identity().preamble(),
            "You are a patient English conversation partner."
        );
        assert!(role.admits_mcp_server("obsidian"));
        assert!(role.admits_mcp_server("philpapers"));
        assert!(!role.admits_mcp_server("postgres"));
        assert!(role.admits_tool("ask_user"));
        assert!(role.admits_tool("read_url"));
        assert!(!role.admits_tool("execute_command"));
    }

    #[test]
    fn coder_inherits_workspace_by_default() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("coder").unwrap();
        assert_eq!(role.resolved_workspace(), RoleWorkspace::Inherit);
        assert!(role.admits_tool("execute_command"));
        assert!(role.admits_mcp_server("anything"));
    }

    #[test]
    fn wildcard_tool_allowlist_matches_prefixes() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("code-reviewer").unwrap();
        assert!(role.admits_tool("read_text"));
        assert!(role.admits_tool("read_image"));
        assert!(role.admits_tool("code_query"));
        assert!(!role.admits_tool("write_file"));
        assert!(!role.admits_tool("execute_command"));
    }

    #[test]
    fn invalid_role_id_is_rejected() {
        for bad_id in &["Bad Id", "bad_id", "bad--id", "-bad", "bad-", "UPPERCASE"] {
            let raw = format!(
                r#"
[roles."{bad_id}"]
name = "Bad"
"#
            );
            let error = RolesConfig::from_toml_str(&raw).unwrap_err();
            assert!(
                error.contains("invalid role id"),
                "expected error for '{bad_id}', got: {error}"
            );
        }
    }

    #[test]
    fn workspace_role_override_replaces_global_definition() {
        let temp = tempfile::tempdir().unwrap();
        let nuo_dir = temp.path().join(".nuo");
        std::fs::create_dir_all(&nuo_dir).unwrap();
        let project_cfg = r#"
[roles.coder]
name = "Workspace Coder"
admit_mcp = ["internal_pg"]
"#;
        std::fs::write(nuo_dir.join("config.toml"), project_cfg).unwrap();

        let cfg = RolesConfig::load_for_workspace(Some(temp.path()));
        let coder = cfg.get("coder").unwrap();
        assert_eq!(coder.name, "Workspace Coder");
        assert_eq!(coder.admit_mcp, vec!["internal_pg".to_string()]);
    }

    #[test]
    fn builtin_ops_manifest_resolves() {
        let manifest = resolve_role_manifest(None, Some("ops"));
        assert_eq!(manifest.role_id, "ops");
        assert!(manifest.tools.contains(&"run_command".to_string()));
    }
}
