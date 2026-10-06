//! Declarative policy schemas the kernel's guards read.
//!
//! These are *configuration shapes*, not configuration storage. A kernel guard
//! (the bash policy evaluator, the permission broker) must be able to express
//! "this command is refused" or "this tool call is pre-approved" without linking
//! the crate that reads `config.toml` — the schema is vocabulary, the file and
//! its resolution are the host's.
//!
//! Same placement rule as [`crate::SkillsConfig`] and
//! [`crate::model_providers::UserDeclaredProvider`]: several layers exchange the
//! type, and the store is only one of them.
//!
//! ## Wire stability
//!
//! These types deserialize from the product's `config.toml`, so their serde
//! spelling is a contract with every existing user file. The `match` field's
//! rename and alias, the `snake_case` matcher/action enums, and the `*` default
//! scope are all load-bearing and must not be "tidied".

use serde::{Deserialize, Serialize};

/// Safety policy for model-issued `bash` commands.
///
/// Built-in dangerous-command rules are compiled into the kernel so this
/// carries only user choices: toggles and project-local overrides.
///
/// ```toml
/// [bash_policy]
/// enabled = true
///
/// [[bash_policy.rules]]
/// name = "deny git reset hard"
/// match = "regex"
/// pattern = '(?i)\bgit\s+reset\s+--hard\b'
/// action = "deny"
/// reason = "This project never allows git reset --hard from the agent."
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyConfig {
    /// Master switch for the bash policy guard. Defaults to `true` so dangerous
    /// built-in commands are protected even when the user has broadly allowed
    /// the `bash` tool.
    pub enabled: bool,
    /// Whether an explicit user `allow` rule may override a compiled-in `deny`
    /// rule. Defaults to `false`; user `allow` rules can still override
    /// compiled-in `confirm` rules.
    pub allow_user_override_builtin_deny: bool,
    /// Project/user-defined rules. Evaluated before built-in `confirm` rules,
    /// but built-in `deny` rules remain a hard floor unless
    /// `allow_user_override_builtin_deny` is set.
    pub rules: Vec<BashPolicyRuleConfig>,
}

impl Default for BashPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_user_override_builtin_deny: false,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyRuleConfig {
    /// Human-readable rule name shown in policy decisions.
    pub name: String,
    /// Matcher type. TOML uses `match = "regex"`; `matcher` is accepted as an
    /// alias for callers that avoid the keyword-like field name.
    #[serde(rename = "match", alias = "matcher")]
    pub matcher: BashPolicyMatcherConfig,
    /// Pattern consumed by the selected matcher.
    pub pattern: String,
    /// Decision to apply when this rule matches.
    pub action: BashPolicyActionConfig,
    /// Optional user-facing explanation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyMatcherConfig {
    /// Rust `regex` matched against the full shell command string.
    #[default]
    Regex,
    /// Case-sensitive substring match against the full command string.
    Contains,
    /// Case-sensitive prefix match after trimming leading whitespace.
    StartsWith,
    /// Match the leading program name (after leading env assignments), e.g.
    /// `git`, `cargo`, `kubectl`.
    Program,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyActionConfig {
    /// Let the command proceed to the normal permission broker.
    Allow,
    /// Require a one-off human confirmation, ignoring any broad `bash *` allow.
    Confirm,
    /// Refuse the command before spawning a shell.
    #[default]
    Deny,
}

/// One declarative permission rule from `[permissions]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleConfig {
    /// Tool name (e.g. `"execute_command"`, `"read_text"`, `"mcp__fs__read"`).
    pub tool: String,
    /// Permission scope. `"*"` matches every call to the tool. Any other value
    /// must match the call's scope *exactly* (e.g. a full path, or the exact
    /// command string for `bash`) — there is no prefix/substring matching.
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "*".to_string()
}

/// Where a declared role expects to run (ADR-0253).
///
/// A role declaration's workspace policy: run anywhere, inherit the caller's
/// workspace, or require exactly one. Deserialized from a plain string
/// (`"none"`, `"inherit"`, or a path), which is why the `Deserialize` impl is
/// hand-written rather than derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleWorkspace {
    None,
    Inherit,
    Fixed(std::path::PathBuf),
}

impl RoleWorkspace {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "none" => RoleWorkspace::None,
            "inherit" => RoleWorkspace::Inherit,
            other => RoleWorkspace::Fixed(std::path::PathBuf::from(other)),
        }
    }

    /// Whether this policy demands a workspace binding.
    pub fn requires_binding(&self) -> bool {
        !matches!(self, RoleWorkspace::None)
    }
}

impl<'de> Deserialize<'de> for RoleWorkspace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(RoleWorkspace::parse(&raw))
    }
}

impl Serialize for RoleWorkspace {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Inherit => serializer.serialize_str("inherit"),
            Self::Fixed(path) => serializer.serialize_str(&path.to_string_lossy()),
        }
    }
}

/// A user-declared role from `roles.toml` (ADR-0246).
///
/// The declaration only: name, identity material, capability lists, and the
/// workspace policy. Discovery, file loading, and the built-in fallbacks are the
/// host's (`nuo-persistence::roles`), which resolves a declaration into a
/// [`crate::SessionRoleManifest`] or an [`crate::AgentRoleProfile`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomRole {
    pub name: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub workspace: Option<RoleWorkspace>,
    #[serde(default = "default_tools")]
    pub tools: Vec<String>,
    #[serde(default = "default_admit_mcp")]
    pub admit_mcp: Vec<String>,
}

fn default_tools() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_admit_mcp() -> Vec<String> {
    vec!["*".to_string()]
}

/// A role declaration with no capability narrowing.
///
/// Hand-written rather than derived: `#[serde(default = "...")]` only applies
/// while deserializing, so a derived `Default` would hand a caller a role with
/// *no* tools where the file format gives it every tool. The two must agree.
impl Default for CustomRole {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            instructions: None,
            workspace: None,
            tools: default_tools(),
            admit_mcp: default_admit_mcp(),
        }
    }
}

impl CustomRole {
    /// The role's workspace policy, with the documented default.
    pub fn resolved_workspace(&self) -> RoleWorkspace {
        self.workspace.clone().unwrap_or(RoleWorkspace::Inherit)
    }

    /// The identity bound at agent construction.
    ///
    /// An explicit `instructions` body is the whole identity (a role directive);
    /// otherwise the name plus description compose one.
    pub fn identity(&self) -> crate::AgentIdentity {
        if let Some(instructions) = self.instructions.as_ref().filter(|p| !p.trim().is_empty()) {
            crate::AgentIdentity::from_directive(instructions.clone())
        } else if let Some(description) = self.description.as_ref().filter(|m| !m.trim().is_empty())
        {
            crate::AgentIdentity::new(self.name.clone(), description.clone())
        } else {
            crate::AgentIdentity::new(self.name.clone(), "")
        }
    }

    /// Validate the role against its declaration.
    pub fn validate(&mut self, id: &str) -> Result<(), String> {
        if self.name.trim().is_empty() {
            self.name = id.to_string();
        }
        Ok(())
    }

    /// Whether this role admits tools from the given MCP server name (ADR-0242, ADR-0246).
    pub fn admits_mcp_server(&self, server: &str) -> bool {
        if self.admit_mcp.is_empty() {
            return false;
        }
        self.admit_mcp.iter().any(|pat| {
            if pat == "*" {
                true
            } else if let Some(prefix) = pat.strip_suffix('*') {
                server.starts_with(prefix)
            } else {
                server == pat
            }
        })
    }

    /// Whether this role admits the tool by name (ADR-0242, ADR-0246).
    pub fn admits_tool(&self, tool: &str) -> bool {
        if self.tools.is_empty() {
            return false;
        }
        self.tools.iter().any(|pat| {
            if pat == "*" {
                true
            } else if let Some(prefix) = pat.strip_suffix('*') {
                tool.starts_with(prefix)
            } else {
                tool == pat
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The serde spelling is a contract with existing user files.
    #[test]
    fn bash_policy_accepts_both_match_spellings() {
        let by_match: BashPolicyRuleConfig =
            toml::from_str("name = \"r\"\nmatch = \"regex\"\npattern = \"x\"\naction = \"deny\"")
                .expect("`match` is the documented spelling");
        let by_matcher: BashPolicyRuleConfig =
            toml::from_str("name = \"r\"\nmatcher = \"regex\"\npattern = \"x\"\naction = \"deny\"")
                .expect("`matcher` is the accepted alias");
        assert_eq!(by_match.matcher, by_matcher.matcher);
        assert_eq!(by_match.action, BashPolicyActionConfig::Deny);
    }

    #[test]
    fn bash_policy_defaults_to_enabled_and_deny() {
        let policy = BashPolicyConfig::default();
        assert!(policy.enabled, "the guard protects by default");
        assert!(!policy.allow_user_override_builtin_deny);
        assert_eq!(
            BashPolicyActionConfig::default(),
            BashPolicyActionConfig::Deny,
            "an unspecified action refuses rather than permits"
        );
    }

    #[test]
    fn permission_rule_scope_defaults_to_wildcard() {
        let rule: PermissionRuleConfig = toml::from_str("tool = \"bash\"").unwrap();
        assert_eq!(rule.scope, "*");
        let exact: PermissionRuleConfig =
            toml::from_str("tool = \"read_text\"\nscope = \"/etc/passwd\"").unwrap();
        assert_eq!(exact.scope, "/etc/passwd");
    }

    #[test]
    fn role_workspace_round_trips_through_its_string_form() {
        for raw in ["none", "inherit"] {
            let mut table = std::collections::BTreeMap::new();
            table.insert(
                "w".to_string(),
                RoleWorkspace::parse(raw),
            );
            let encoded = toml::to_string(&table).unwrap();
            assert!(
                encoded.contains(raw),
                "round trip must preserve {raw}: {encoded}"
            );
        }
        let fixed = RoleWorkspace::parse("/srv/project");
        assert_eq!(fixed, RoleWorkspace::Fixed("/srv/project".into()));
        assert!(fixed.requires_binding());
        assert!(!RoleWorkspace::parse("none").requires_binding());
    }

    #[test]
    fn role_identity_prefers_instructions_then_description() {
        let mut role = CustomRole {
            name: "reviewer".into(),
            description: Some("a strict reviewer".into()),
            ..Default::default()
        };
        assert_eq!(
            role.identity().preamble(),
            "You are reviewer, a strict reviewer."
        );
        role.instructions = Some("Role: reviewer. Report; never apply.".into());
        assert_eq!(
            role.identity().preamble(),
            "Role: reviewer. Report; never apply.",
            "an explicit directive is the whole identity"
        );
    }

    #[test]
    fn role_capability_lists_default_to_everything() {
        let role = CustomRole::default();
        assert_eq!(role.tools, vec!["*".to_string()]);
        assert_eq!(role.admit_mcp, vec!["*".to_string()]);
        assert_eq!(role.resolved_workspace(), RoleWorkspace::Inherit);
    }
}
