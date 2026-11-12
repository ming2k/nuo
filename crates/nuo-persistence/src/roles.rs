use nuo_protocol::MainAgentRole;
pub use nuo_protocol::policy_schema::{CustomRole, RoleWorkspace};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

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

    /// Load with optional workspace override.
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

/// Canonicalize a role id against the role registry visible from
/// `project_root`. Custom role ids are preserved verbatim (they are already
/// strict kebab-case); built-in aliases (`dev`, `sre`, `philosopher`, …)
/// collapse to their canonical id. Unknown ids are refused: a destructive or
/// scoped operation must never silently fall back to another role.
pub fn canonical_role_id(role_id: &str, project_root: Option<&Path>) -> Result<String, String> {
    let trimmed = role_id.trim();
    if trimmed.is_empty() {
        return Ok("developer".to_string());
    }
    let roles_cfg = RolesConfig::load_for_workspace(project_root);
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

/// Resolve a role id to its canonical spelling and workspace policy.
///
/// This is the role-first primitive every scope, create, and bulk operation
/// shares. The policy is a *resolution* rule: `Unbound` never binds,
/// `Inherit` takes the invocation project, and `Fixed` is a constant.
pub fn resolve_role_workspace(
    role_id: Option<&str>,
    project_root: Option<&Path>,
) -> Result<(String, RoleWorkspace), String> {
    let requested = role_id.unwrap_or("developer");
    let role = canonical_role_id(requested, project_root)?;
    let roles_cfg = RolesConfig::load_for_workspace(project_root);
    let policy = if let Some(user_role) = roles_cfg.get(&role) {
        user_role.resolved_workspace()
    } else {
        MainAgentRole::parse(&role)
            .ok_or_else(|| format!("unknown role '{role}'"))?
            .workspace_policy()
    };
    Ok((role, policy))
}

/// Resolve the exact `(role, workspace)` scope an operation must use.
///
/// The role policy is authoritative: `developer` and `workspace = "inherit"`
/// require a project workspace, `workspace = "none"` (and the built-in
/// workspace-free roles) resolve to the unbound set, and `workspace =
/// "/fixed/path"` binds that exact path. A workspace-requiring role with no
/// project context is an error, never a silent unbound fallback.
pub fn resolve_thread_scope(
    role_id: Option<&str>,
    project_root: Option<&Path>,
) -> Result<nuo_protocol::ThreadScope, String> {
    let (role, policy) = resolve_role_workspace(role_id, project_root)?;
    let workspace = match policy {
        RoleWorkspace::Unbound => None,
        RoleWorkspace::Inherit => Some(
            project_root
                .ok_or_else(|| {
                    format!("role '{role}' requires a workspace; pass --project <path>")
                })?
                .to_path_buf(),
        ),
        RoleWorkspace::Fixed(root) => Some(root),
    };
    Ok(nuo_protocol::ThreadScope::new(role, workspace))
}

/// Resolve an immutable SessionRoleManifest for a role ID within a workspace context.
pub fn resolve_role_manifest(
    workspace_root: Option<&std::path::Path>,
    role_id: Option<&str>,
) -> nuo_protocol::SessionRoleManifest {
    let role_id = role_id.unwrap_or("developer");
    let roles_cfg = RolesConfig::load_for_workspace(workspace_root);
    if let Some(user_role) = roles_cfg.get(role_id) {
        let identity = user_role.identity();
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        nuo_protocol::SessionRoleManifest {
            role_id: role_id.to_string(),
            name: user_role.name.clone(),
            description: user_role.description.clone(),
            instructions: user_role.instructions.clone(),
            identity,
            tools: user_role.tools.clone(),
            admit_mcp: user_role.admit_mcp.clone(),
            workspace: Some(user_role.resolved_workspace()),
            created_at_s: now_s,
        }
    } else if let Some(builtin) = MainAgentRole::parse(role_id) {
        match builtin {
            MainAgentRole::Developer => nuo_protocol::SessionRoleManifest::developer(),
            MainAgentRole::Philosophist => nuo_protocol::SessionRoleManifest::philosophist(),
            MainAgentRole::Ops => nuo_protocol::SessionRoleManifest::ops(),
        }
    } else {
        let mut manifest = nuo_protocol::SessionRoleManifest::developer();
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
        assert_eq!(role.resolved_workspace(), RoleWorkspace::Unbound);
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
    fn scope_resolution_is_role_first() {
        let project = Path::new("/repo/a");

        let developer = resolve_thread_scope(None, Some(project)).unwrap();
        assert_eq!(developer.role, "developer");
        assert_eq!(developer.workspace.as_deref(), Some(project));

        let philosophist = resolve_thread_scope(Some("philosophist"), Some(project)).unwrap();
        assert_eq!(philosophist.role, "philosophist");
        assert_eq!(philosophist.workspace, None);

        // A built-in alias canonicalizes before it becomes a scope key.
        let alias = resolve_thread_scope(Some("dev"), Some(project)).unwrap();
        assert_eq!(alias.role, "developer");
    }

    #[test]
    fn scope_resolution_refuses_unknown_and_missing_workspace() {
        assert!(resolve_thread_scope(Some("no-such-role"), Some(Path::new("/repo"))).is_err());
        assert!(resolve_thread_scope(Some("developer"), None).is_err());
    }

    #[test]
    fn scope_resolution_honors_custom_workspace_policies() {
        let temp = tempfile::tempdir().unwrap();
        let nuo_dir = temp.path().join(".nuo");
        std::fs::create_dir_all(&nuo_dir).unwrap();
        std::fs::write(
            nuo_dir.join("roles.toml"),
            r#"
[roles.coder]
name = "Coder"
workspace = "inherit"

[roles.translator]
name = "Translator"
workspace = "none"

[roles.auditor]
name = "Auditor"
workspace = "/srv/audit"
"#,
        )
        .unwrap();

        let coder = resolve_thread_scope(Some("coder"), Some(temp.path())).unwrap();
        assert_eq!(coder.workspace.as_deref(), Some(temp.path()));

        let translator = resolve_thread_scope(Some("translator"), Some(temp.path())).unwrap();
        assert_eq!(translator.workspace, None);

        let auditor = resolve_thread_scope(Some("auditor"), Some(temp.path())).unwrap();
        assert_eq!(auditor.workspace.as_deref(), Some(Path::new("/srv/audit")));
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
        assert!(
            manifest
                .builtin_tools()
                .contains(&nuo_protocol::BuiltinTool::ExecuteCommand)
        );
        assert!(manifest.tools.contains(&"execute_command".to_string()));
    }
}
