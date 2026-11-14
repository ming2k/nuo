//! Pure policy configuration shapes shared by the config parser and the
//! guardrail layer.
//!
//! These types are data, not storage rows. They live in the protocol layer so
//! `nuo-guardrails` can compile without linking `nuo-persistence`
//! ([ADR-0188](../../docs/adr/0188-guardrails-depend-on-contracts-not-persistence.md)).
//! `nuo-persistence::config` re-exports them as part of its configuration
//! facade.

use serde::{Deserialize, Serialize};

/// One declarative permission rule from `[permissions]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleConfig {
    /// Tool name (e.g. `"execute_command"`, `"read"`, `"mcp__fs__read"`).
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

/// Safety policy for model-issued `bash` commands.
///
/// Built-in dangerous-command rules are compiled into the guardrail; this
/// configuration supplies only user choices: toggles and project-local
/// overrides/additions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyConfig {
    /// Master switch for the bash policy guard.
    pub enabled: bool,
    /// Whether an explicit user `allow` rule may override a compiled-in `deny`
    /// rule.
    pub allow_user_override_builtin_deny: bool,
    /// Project/user-defined rules.
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

/// One user-defined bash-policy rule.
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

/// The matcher a user rule uses.
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
    /// Match the leading program name (after leading env assignments).
    Program,
}

/// The decision a user rule applies.
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
