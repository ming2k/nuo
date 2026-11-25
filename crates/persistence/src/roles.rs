use protocol::MainAgentRole;
pub use protocol::policy_schema::CustomRole;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RolesConfig {
    #[serde(default)]
    pub roles: BTreeMap<String, CustomRole>,
}

impl RolesConfig {
    /// Load `roles.toml` from the resolved global config directory (ADR-0254).
    /// Absent file = no custom roles. A parse or validation error is logged and
    /// degrades to an empty registry rather than failing startup.
    pub fn load() -> Self {
        let path = host::paths::get().roles_file();
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

/// Validate whether an identifier strictly conforms to kebab-case.
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

/// Canonicalize a role id against the global role registry (ADR-0254).
/// Custom role ids are preserved verbatim; built-in aliases (`dev`, `sre`,
/// `philosopher`, …) collapse to their canonical id. Unknown ids are refused.
pub fn canonical_role_id(role_id: &str) -> Result<String, String> {
    let trimmed = role_id.trim();
    if trimmed.is_empty() {
        return Ok("developer".to_string());
    }
    let roles_cfg = RolesConfig::load();
    if roles_cfg.get(trimmed).is_some() {
        return Ok(trimmed.to_string());
    }
    MainAgentRole::parse(trimmed)
        .map(|role| role.as_str().to_string())
        .ok_or_else(|| {
            format!(
                "unknown role '{trimmed}'; define it in roles.toml or use a built-in role \
                 (developer, philosophist, ops)"
            )
        })
}

/// Resolve a self-contained [`protocol::ThreadRoleManifest`] for a role ID.
pub fn resolve_role_manifest(role_id: Option<&str>) -> protocol::ThreadRoleManifest {
    let role_id = role_id.unwrap_or("developer");
    let roles_cfg = RolesConfig::load();
    if let Some(user_role) = roles_cfg.get(role_id) {
        let identity = user_role.identity();
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        protocol::ThreadRoleManifest {
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
            MainAgentRole::Developer => protocol::ThreadRoleManifest::developer(),
            MainAgentRole::Philosophist => protocol::ThreadRoleManifest::philosophist(),
            MainAgentRole::Ops => protocol::ThreadRoleManifest::ops(),
        }
    } else {
        let mut manifest = protocol::ThreadRoleManifest::developer();
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
tools = ["ask_user", "read_url"]
admit_mcp = ["obsidian", "phil*"]

[roles.coder]
name = "Coder"

[roles.code-reviewer]
name = "Code Reviewer"
tools = ["read", "code_query"]
"#;

    #[test]
    fn parses_and_round_trips_a_role() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("english-practice").unwrap();
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
    fn coder_defaults_to_all_tools_and_mcp() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("coder").unwrap();
        assert!(role.admits_tool("execute_command"));
        assert!(role.admits_mcp_server("anything"));
    }

    #[test]
    fn wildcard_tool_allowlist_matches_prefixes() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("code-reviewer").unwrap();
        assert!(role.admits_tool("read"));
        assert!(!role.admits_tool("read_text"));
        assert!(!role.admits_tool("read_image"));
        assert!(role.admits_tool("code_query"));
        assert!(!role.admits_tool("write_file"));
        assert!(!role.admits_tool("execute_command"));
    }

    #[test]
    fn canonical_role_id_resolves_aliases_and_rejects_unknown() {
        assert_eq!(canonical_role_id("").unwrap(), "developer");
        assert_eq!(canonical_role_id("dev").unwrap(), "developer");
        assert_eq!(canonical_role_id("sre").unwrap(), "ops");
        assert_eq!(canonical_role_id("philosopher").unwrap(), "philosophist");
        assert!(canonical_role_id("no-such-role").is_err());
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
    fn builtin_ops_manifest_resolves() {
        let manifest = resolve_role_manifest(Some("ops"));
        assert_eq!(manifest.role_id, "ops");
        assert!(
            manifest
                .builtin_tools()
                .contains(&tool::BuiltinTool::ExecuteCommand)
        );
        assert!(manifest.tools.contains(&"execute_command".to_string()));
    }
}
