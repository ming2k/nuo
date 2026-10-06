//! Agent role definitions: declarative principal and subagent profiles.
//!
//! Every agent is an instance of [`Agent<R: AgentRole>`]:
//! - [`MainAgent`]: interactive top-level agent staffed with a [`MainAgentRole`].
//! - [`SubAgent`]: autonomous delegated agent staffed with a [`SubAgentRole`].

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::subagent::SubAgentProfile;
use crate::{AgentIdentity, BuiltinTool, ToolSelection};

/// User-tunable agent runtime behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentRuntimeConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    pub hard_stop_turns: usize,
    /// Doom-loop guard config. Default disabled.
    pub trajectory_guard: crate::TrajectoryGuardConfig,
    /// Whether the model may supply stdin bytes for an `execute_command` call. Default false.
    pub allow_model_stdin: bool,
    /// Whether an interactive `execute_command` call skips the inline input panel. Default false.
    pub skip_interactive_input: bool,
}

/// A declarative agent role profile: an identity, its admitted tools, execution
/// knobs, and equipped atomic extensions.
#[derive(Debug, Clone)]
pub struct AgentRoleProfile {
    /// The role's name, e.g. `"developer"`.
    pub name: Cow<'static, str>,
    /// Who this principal is and what it is for.
    pub identity: AgentIdentity,
    /// The tools this role admits from the pool.
    pub tools: ToolSelection,
    /// Runtime execution knobs (hard stop, doom guard, model stdin).
    pub config: AgentRuntimeConfig,
    /// Whether this principal runs in unattended execution mode.
    pub unattended: bool,
    /// Atomic extensions equipped by this role.
    pub extensions: Vec<std::sync::Arc<dyn crate::Extension>>,
    /// MCP server subscription patterns (e.g. `["*"]` or `["obsidian", "phil*"]`).
    pub admit_mcp: Vec<String>,
}

impl AgentRoleProfile {
    /// Build a role from an identity with full default scope and attended behaviour.
    pub fn with_identity(name: impl Into<Cow<'static, str>>, identity: AgentIdentity) -> Self {
        Self {
            name: name.into(),
            identity,
            tools: ToolSelection::unrestricted(),
            config: AgentRuntimeConfig::default(),
            unattended: false,
            extensions: Vec::new(),
            admit_mcp: vec!["*".to_string()],
        }
    }

    /// Set the admitted MCP server patterns for this role.
    pub fn with_mcp_admission(mut self, patterns: Vec<String>) -> Self {
        self.admit_mcp = patterns;
        self
    }

    /// Attach an atomic extension to this role.
    pub fn with_extension(mut self, extension: std::sync::Arc<dyn crate::Extension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Attach atomic extensions to this role.
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = std::sync::Arc<dyn crate::Extension>>,
    ) -> Self {
        for extension in extensions {
            self.extensions.push(extension);
        }
        self
    }

    /// Preset for standard developer master (native tools, full delegation).
    pub fn developer() -> Self {
        Self::with_identity(
            "developer",
            role_directive(
                "Role: developer. You are an expert AI software engineer with native tool access and deep system architecture capability. Execute commands and edit files with surgical precision, maintain strict testing discipline, and prioritize root-cause solutions over superficial patches.",
            ),
        )
        .with_tools(MainAgentRole::Developer.tool_selection())
    }

    /// Role for standard developer master.
    pub fn role_developer() -> Self {
        Self::developer()
    }

    /// Preset for philosophist role (workspace-free philosophical inquiry).
    pub fn philosophist() -> Self {
        let identity = role_directive(
            "Role: philosophist. Engage in deep philosophical inquiry, examine principles, \
             question assumptions, and explore ideas with clarity and nuance. Do not modify files or run commands. \
             You can perceive our historical dialogue using `recall_memory` to recall past discussions, philosophical \
             positions, or shared reflections when relevant.",
        );
        Self::with_identity("philosophist", identity)
            .with_tools(MainAgentRole::Philosophist.tool_selection())
    }

    /// Preset for ops role (workspace-free system administration, infrastructure maintenance, and remote operations).
    pub fn ops() -> Self {
        let identity = role_directive(
            "Role: ops. You are an expert systems administrator, site reliability engineer (SRE), and infrastructure operator. \
             Your primary mission is maintaining host systems, diagnosing and troubleshooting environment/service issues, \
             managing processes and daemon lifecycles, configuring networking and host environments, and orchestrating remote \
             nodes or clusters. You operate without a workspace boundary. Execute commands, manage background processes, \
             and inspect/edit host and remote configurations with surgical care.\n\
             Strictly observe non-interactive CLI discipline: never launch blocking interactive prompts or pagers (e.g. use non-interactive \
             flags like `-o BatchMode=yes` for ssh, non-interactive flags for package managers, and `--no-pager` for systemctl/journalctl). \
             Inspect system and service state before mutating configurations, maintain backups before modifying critical configs, \
             verify results post-action, and proactively seek confirmation via `ask_user` before executing high-risk, destructive, \
             or potentially connectivity-breaking operations.",
        );
        Self::with_identity("ops", identity).with_tools(MainAgentRole::Ops.tool_selection())
    }

    /// Narrow the capability scope. Builder-style.
    pub fn with_tools(mut self, selection: ToolSelection) -> Self {
        self.tools = selection;
        self
    }

    /// Attach the runtime knobs. Builder-style.
    pub fn with_runtime_config(mut self, config: AgentRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Run attended (`false`, the default) or in unattended execution mode (`true`).
    pub fn with_unattended(mut self, unattended: bool) -> Self {
        self.unattended = unattended;
        self
    }

    /// Materialize a [`MainAgentRole`] onto a product's base [`AgentIdentity`].
    pub fn from_role(role: MainAgentRole, base: &AgentIdentity) -> Self {
        match role {
            MainAgentRole::Developer => {
                let id = if base.preamble().is_empty() {
                    Self::developer().identity
                } else {
                    base.clone()
                };
                Self::with_identity("developer", id).with_tools(role.tool_selection())
            }
            MainAgentRole::Philosophist => Self::philosophist(),
            MainAgentRole::Ops => Self::ops(),
        }
    }

    /// Preset developer policy (associated constant).
    pub const DEVELOPER: AgentRoleDelegation = AgentRoleDelegation::DEVELOPER;
    /// Preset philosophist policy (associated constant).
    pub const PHILOSOPHIST: AgentRoleDelegation = AgentRoleDelegation::PHILOSOPHIST;
    /// Preset ops policy (associated constant).
    pub const OPS: AgentRoleDelegation = AgentRoleDelegation::OPS;
}

/// The common contract for any agent role (Main or Sub).
pub trait AgentRole: Send + Sync + 'static {
    /// Canonical name of this role.
    fn role_name(&self) -> &str;
    /// Whether this role admits direct interactive user input (e.g. `ask_user`).
    fn admits_user_interaction(&self) -> bool;
    /// The workspace root bound to this role, if any.
    fn workspace_root(&self) -> Option<&std::path::Path>;
    /// Tool selection admitted for this role.
    fn tool_selection(&self) -> ToolSelection;
}

/// Unified generic Agent structure: the single parent struct for both MainAgent and SubAgent.
#[derive(Debug, Clone)]
pub struct Agent<R: AgentRole> {
    /// Declarative profile and tools.
    pub profile: AgentRoleProfile,
    /// The specialized role staffing this agent.
    pub role: R,
}

impl<R: AgentRole> Agent<R> {
    /// Create a new agent from its profile and specialized role.
    pub fn new(profile: AgentRoleProfile, role: R) -> Self {
        Self { profile, role }
    }

    /// Access the agent's role.
    pub fn role(&self) -> &R {
        &self.role
    }

    /// Access the agent's role name.
    pub fn role_name(&self) -> &str {
        self.role.role_name()
    }
}

/// A top-level interactive agent staffed with a [`MainAgentRole`].
pub type MainAgent = Agent<MainAgentRole>;

/// An autonomous delegated subagent staffed with a [`SubAgentRole`].
pub type SubAgent = Agent<SubAgentRole>;

/// An immutable, self-contained snapshot of a session's governing role manifest (ADR-0245, ADR-0246).
/// Captured at session birth to guarantee hermetic restoration and deterministic prompt caching,
/// completely decoupled from external mutable `roles.toml` files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRoleManifest {
    pub role_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub identity: AgentIdentity,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub admit_mcp: Vec<String>,
    #[serde(default)]
    pub created_at_s: u64,
}

impl SessionRoleManifest {
    pub fn new(
        role_id: impl Into<String>,
        name: impl Into<String>,
        identity: AgentIdentity,
    ) -> Self {
        Self {
            role_id: role_id.into(),
            name: name.into(),
            description: None,
            instructions: None,
            identity,
            tools: vec!["*".to_string()],
            admit_mcp: vec!["*".to_string()],
            created_at_s: 0,
        }
    }

    pub fn with_description(mut self, desc: Option<String>) -> Self {
        self.description = desc;
        self
    }

    pub fn with_instructions(mut self, inst: Option<String>) -> Self {
        self.instructions = inst;
        self
    }

    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_admit_mcp(mut self, admit: Vec<String>) -> Self {
        self.admit_mcp = admit;
        self
    }

    pub fn with_created_at(mut self, created_at_s: u64) -> Self {
        self.created_at_s = created_at_s;
        self
    }

    /// Build a manifest for standard developer role.
    pub fn developer() -> Self {
        let profile = AgentRoleProfile::developer();
        Self {
            role_id: "developer".to_string(),
            name: "developer".to_string(),
            description: Some(
                "the default developer role (full native capabilities with workspace)".to_string(),
            ),
            instructions: profile.identity.directive.clone(),
            identity: profile.identity,
            tools: MainAgentRole::DEVELOPER_TOOLS
                .iter()
                .map(|t| t.as_str().to_string())
                .collect(),
            admit_mcp: vec!["*".to_string()],
            created_at_s: 0,
        }
    }

    /// Build a manifest for standard philosophist role.
    pub fn philosophist() -> Self {
        let profile = AgentRoleProfile::philosophist();
        Self {
            role_id: "philosophist".to_string(),
            name: "philosophist".to_string(),
            description: Some("philosophical inquiry & reflection (workspace-free)".to_string()),
            instructions: profile.identity.directive.clone(),
            identity: profile.identity,
            tools: MainAgentRole::PHILOSOPHIST_TOOLS
                .iter()
                .map(|t| t.as_str().to_string())
                .collect(),
            admit_mcp: Vec::new(),
            created_at_s: 0,
        }
    }

    /// Build a manifest for standard ops role.
    pub fn ops() -> Self {
        let profile = AgentRoleProfile::ops();
        Self {
            role_id: "ops".to_string(),
            name: "ops".to_string(),
            description: Some(
                "system administration, infrastructure maintenance & remote operations (workspace-free)".to_string(),
            ),
            instructions: profile.identity.directive.clone(),
            identity: profile.identity,
            tools: MainAgentRole::OPS_TOOLS
                .iter()
                .map(|t| t.as_str().to_string())
                .collect(),
            admit_mcp: vec!["*".to_string()],
            created_at_s: 0,
        }
    }

    /// Return typed built-in tools admitted by this manifest.
    pub fn builtin_tools(&self) -> Vec<BuiltinTool> {
        self.tools
            .iter()
            .filter_map(|s| BuiltinTool::from_name(s))
            .collect()
    }
}

impl MainAgent {
    /// Switch this main agent's role.
    pub fn switch_role(&mut self, new_role: MainAgentRole) {
        self.role = new_role;
        self.profile.tools = self.role.tool_selection();
    }

    /// Spawn a delegated SubAgent under this MainAgent.
    pub fn spawn_subagent(&self, sub_role: SubAgentRole) -> SubAgent {
        let sub_profile = AgentRoleProfile::with_identity(
            sub_role.as_str(),
            AgentIdentity::from_mission(format!("Autonomous {} subagent", sub_role.as_str())),
        )
        .with_tools(sub_role.tool_selection());

        SubAgent::new(sub_profile, sub_role)
    }
}

/// Roles an interactive MainAgent may switch into.
/// Built-in roles: `developer` (workspace-bound), `philosophist` (workspace-free), and `ops` (workspace-free).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MainAgentRole {
    /// The default developer role (full native capabilities with workspace).
    #[default]
    Developer,
    /// Philosophist: philosophical inquiry & reflection (workspace-free).
    Philosophist,
    /// Ops: system administration, infrastructure maintenance & remote operations (workspace-free).
    Ops,
}

impl MainAgentRole {
    /// Every main role in its canonical display order.
    pub const ALL: &[MainAgentRole] = &[
        MainAgentRole::Developer,
        MainAgentRole::Philosophist,
        MainAgentRole::Ops,
    ];

    /// The stable string name used in `/role <name>`.
    pub fn as_str(self) -> &'static str {
        match self {
            MainAgentRole::Developer => "developer",
            MainAgentRole::Philosophist => "philosophist",
            MainAgentRole::Ops => "ops",
        }
    }

    /// Parse a role name (case-insensitive). Returns `None` for an unknown name.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "developer" | "dev" | "code" | "coder" | "default" => Some(MainAgentRole::Developer),
            "philosophist" | "philosopher" | "philosophy" | "conversational" | "chat"
            | "companion" | "tutor" => Some(MainAgentRole::Philosophist),
            "ops" | "operator" | "sre" | "sysadmin" | "devops" | "admin" => {
                Some(MainAgentRole::Ops)
            }
            _ => None,
        }
    }

    /// A short human description of what this role does, for confirmations.
    pub fn description(self) -> &'static str {
        match self {
            MainAgentRole::Developer => {
                "the default developer role (full native capabilities with workspace)"
            }
            MainAgentRole::Philosophist => "philosophical inquiry & reflection (workspace-free)",
            MainAgentRole::Ops => {
                "system administration, infrastructure maintenance & remote operations (workspace-free)"
            }
        }
    }

    /// Whether this role requires a filesystem workspace binding.
    pub fn requires_workspace(self) -> bool {
        match self {
            MainAgentRole::Developer => true,
            MainAgentRole::Philosophist | MainAgentRole::Ops => false,
        }
    }

    /// Default confinement posture for this role when starting without an explicit override.
    pub fn default_confined(self) -> bool {
        match self {
            MainAgentRole::Developer | MainAgentRole::Philosophist => true,
            MainAgentRole::Ops => false,
        }
    }

    /// Built-in tools admitted for standard developer sessions.
    ///
    /// AST-level structural queries (`code_query`) are strictly excluded by design:
    /// developer delegates exploration and AST investigation to dedicated
    /// subagents (`explore`/`debug`) to maintain a lean, clean context window.
    pub const DEVELOPER_TOOLS: &'static [BuiltinTool] = &[
        BuiltinTool::ExecuteCommand,
        BuiltinTool::ReadText,
        BuiltinTool::EditText,
        BuiltinTool::WriteFile,
        BuiltinTool::ListDir,
        BuiltinTool::FindFiles,
        BuiltinTool::SearchText,
        BuiltinTool::ReadUrl,
        BuiltinTool::SearchWeb,
        BuiltinTool::ReadImage,
        BuiltinTool::AskUser,
        BuiltinTool::Todo,
        BuiltinTool::SpawnAgent,
        BuiltinTool::RecallMemory,
    ];

    /// Built-in tools admitted for philosophist sessions.
    pub const PHILOSOPHIST_TOOLS: &'static [BuiltinTool] = &[
        BuiltinTool::ReadUrl,
        BuiltinTool::SearchWeb,
        BuiltinTool::AskUser,
        BuiltinTool::RecallMemory,
    ];

    /// Built-in tools admitted for ops sessions.
    pub const OPS_TOOLS: &'static [BuiltinTool] = &[
        BuiltinTool::ExecuteCommand,
        BuiltinTool::ReadText,
        BuiltinTool::EditText,
        BuiltinTool::WriteFile,
        BuiltinTool::ListDir,
        BuiltinTool::FindFiles,
        BuiltinTool::SearchText,
        BuiltinTool::ReadUrl,
        BuiltinTool::SearchWeb,
        BuiltinTool::ReadImage,
        BuiltinTool::AskUser,
        BuiltinTool::Todo,
        BuiltinTool::SpawnAgent,
    ];
}

impl AgentRole for MainAgentRole {
    fn role_name(&self) -> &str {
        self.as_str()
    }

    fn admits_user_interaction(&self) -> bool {
        true
    }

    fn workspace_root(&self) -> Option<&std::path::Path> {
        None
    }

    fn tool_selection(&self) -> ToolSelection {
        match self {
            MainAgentRole::Developer => {
                ToolSelection::only_builtin(MainAgentRole::DEVELOPER_TOOLS.iter().copied())
            }
            MainAgentRole::Philosophist => {
                ToolSelection::only_builtin(MainAgentRole::PHILOSOPHIST_TOOLS.iter().copied())
            }
            MainAgentRole::Ops => {
                ToolSelection::only_builtin(MainAgentRole::OPS_TOOLS.iter().copied())
            }
        }
    }
}

/// Delegated functional roles for autonomous SubAgents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentRole {
    Explore,
    Debug,
    Skill,
}

impl SubAgentRole {
    pub const ALL: &[SubAgentRole] = &[
        SubAgentRole::Explore,
        SubAgentRole::Debug,
        SubAgentRole::Skill,
    ];

    pub const EXPLORE: SubAgentProfile = SubAgentProfile::EXPLORE;
    pub const DEBUG: SubAgentProfile = SubAgentProfile::DEBUG;
    pub const SKILL: SubAgentProfile = SubAgentProfile::SKILL;

    pub fn as_str(self) -> &'static str {
        match self {
            SubAgentRole::Explore => "explore",
            SubAgentRole::Debug => "debug",
            SubAgentRole::Skill => "skill",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "explore" => Some(SubAgentRole::Explore),
            "debug" => Some(SubAgentRole::Debug),
            "skill" => Some(SubAgentRole::Skill),
            _ => None,
        }
    }
}

impl AgentRole for SubAgentRole {
    fn role_name(&self) -> &str {
        self.as_str()
    }

    fn admits_user_interaction(&self) -> bool {
        false
    }

    fn workspace_root(&self) -> Option<&std::path::Path> {
        None
    }

    fn tool_selection(&self) -> ToolSelection {
        match self {
            SubAgentRole::Explore => {
                ToolSelection::only_builtin(SubAgentProfile::EXPLORE_TOOLS.iter().copied())
            }
            SubAgentRole::Debug => {
                ToolSelection::only_builtin(SubAgentProfile::DEBUG_TOOLS.iter().copied())
            }
            SubAgentRole::Skill => {
                ToolSelection::only_builtin(SubAgentProfile::SKILL_TOOLS.iter().copied())
            }
        }
    }
}

/// The delegation face of an agent role: which subagents it may load, and the tools it declares.
#[derive(Debug, Clone)]
pub struct AgentRoleDelegation {
    /// Stable id.
    pub role_id: &'static str,
    /// Subagent role names this agent may load, in preference order.
    pub subagent_roles: &'static [&'static str],
}

pub type DelegationPolicy = AgentRoleDelegation;

impl AgentRoleDelegation {
    /// The developer agent role delegation policy: native toolchain authority.
    pub const DEVELOPER: AgentRoleDelegation = AgentRoleDelegation {
        role_id: "developer",
        subagent_roles: &[SubAgentProfile::EXPLORE.name, SubAgentProfile::DEBUG.name],
    };

    /// The philosophist agent role delegation policy: workspace-free philosophical exploration.
    pub const PHILOSOPHIST: AgentRoleDelegation = AgentRoleDelegation {
        role_id: "philosophist",
        subagent_roles: &[SubAgentProfile::EXPLORE.name],
    };

    /// The ops agent role delegation policy: workspace-free system administration and remote operations.
    pub const OPS: AgentRoleDelegation = AgentRoleDelegation {
        role_id: "ops",
        subagent_roles: &[
            SubAgentProfile::EXPLORE.name,
            SubAgentProfile::TITLE.name,
            SubAgentProfile::SKILL.name,
        ],
    };

    /// Whether an agent bound to this role may load the subagent role
    pub fn admits_subagent(&self, name: &str) -> bool {
        self.subagent_roles.contains(&name)
    }

    /// All shipping agent role delegations, developer first.
    pub const ALL: &'static [AgentRoleDelegation] =
        &[Self::DEVELOPER, Self::PHILOSOPHIST, Self::OPS];

    pub fn declared_tools(&self) -> Option<&'static [BuiltinTool]> {
        match self.role_id {
            "developer" => Some(MainAgentRole::DEVELOPER_TOOLS),
            "philosophist" => Some(MainAgentRole::PHILOSOPHIST_TOOLS),
            "ops" => Some(MainAgentRole::OPS_TOOLS),
            _ => None,
        }
    }

    pub fn selection(&self) -> ToolSelection {
        match self.declared_tools() {
            None => ToolSelection::unrestricted(),
            Some(tools) => ToolSelection::only_builtin(tools.iter().copied()),
        }
    }
}

fn role_directive(text: &str) -> AgentIdentity {
    AgentIdentity::from_directive(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;

    #[test]
    fn with_identity_is_unrestricted_and_attended() {
        let p = AgentRoleProfile::with_identity("developer", AgentIdentity::new("n", "m"));
        assert_eq!(p.name, "developer");
        assert!(!p.unattended);
        assert_eq!(p.tools.scope, crate::ToolScope::All);
        assert!(p.tools.variants.is_empty());
        assert_eq!(p.config.hard_stop_turns, 0);
        assert!(!p.config.allow_model_stdin);
        assert!(!p.config.skip_interactive_input);
        assert_eq!(
            p.config.trajectory_guard,
            crate::TrajectoryGuardConfig::default()
        );
    }

    #[test]
    fn builders_override_defaults() {
        let p = AgentRoleProfile::with_identity("ops", AgentIdentity::default())
            .with_unattended(true)
            .with_runtime_config(AgentRuntimeConfig {
                hard_stop_turns: 7,
                ..Default::default()
            });
        assert!(p.unattended);
        assert_eq!(p.config.hard_stop_turns, 7);
    }

    #[test]
    fn runtime_config_is_copy() {
        let c = AgentRuntimeConfig::default();
        let _copy = c;
        let _again = c;
    }

    #[test]
    fn role_round_trips_through_parse() {
        for role in MainAgentRole::ALL {
            let parsed = MainAgentRole::parse(role.as_str());
            assert_eq!(parsed, Some(*role), "{} should parse back", role.as_str());
        }
        assert_eq!(
            MainAgentRole::parse("Coder"),
            Some(MainAgentRole::Developer)
        );
        assert_eq!(MainAgentRole::parse("dev"), Some(MainAgentRole::Developer));
        assert_eq!(
            MainAgentRole::parse("philosopher"),
            Some(MainAgentRole::Philosophist)
        );
        assert_eq!(
            MainAgentRole::parse("conversational"),
            Some(MainAgentRole::Philosophist)
        );
        assert_eq!(MainAgentRole::parse("ops"), Some(MainAgentRole::Ops));
        assert_eq!(MainAgentRole::parse("sre"), Some(MainAgentRole::Ops));
        assert_eq!(MainAgentRole::parse("sysadmin"), Some(MainAgentRole::Ops));
        assert_eq!(MainAgentRole::parse("operator"), Some(MainAgentRole::Ops));
        assert!(MainAgentRole::parse("wizard").is_none());
    }

    #[test]
    fn developer_role_preserves_base_identity_and_philosophist_installs_directive() {
        let base = AgentIdentity::from_mission("an expert AI coding assistant");
        let dev = AgentRoleProfile::from_role(MainAgentRole::Developer, &base);
        assert_eq!(dev.identity.preamble(), base.preamble());
        assert_eq!(dev.name, "developer");

        let default_dev = AgentRoleProfile::developer();
        assert!(
            default_dev
                .identity
                .preamble()
                .starts_with("Role: developer.")
        );

        let phil = AgentRoleProfile::from_role(MainAgentRole::Philosophist, &base);
        assert_eq!(phil.name, "philosophist");
        let directive = phil.identity.preamble();
        assert!(directive.starts_with("Role: "), "directive: {directive}");
        assert!(!directive.starts_with("You are"));
        assert!(directive.ends_with('.'));
    }

    #[test]
    fn developer_role_excludes_ast_code_query() {
        let base = AgentIdentity::from_mission("coding assistant");
        let dev = AgentRoleProfile::from_role(MainAgentRole::Developer, &base);
        let crate::ToolScope::Only(names) = &dev.tools.scope else {
            panic!("developer must be scoped with explicit tools");
        };
        assert!(names.contains(BuiltinTool::ExecuteCommand.as_str()));
        assert!(names.contains(BuiltinTool::ReadText.as_str()));
        assert!(names.contains(BuiltinTool::EditText.as_str()));
        assert!(names.contains(BuiltinTool::WriteFile.as_str()));
        assert!(names.contains(BuiltinTool::SpawnAgent.as_str()));
        assert!(names.contains(BuiltinTool::RecallMemory.as_str()));
        assert!(dev.tools.scope.admits_builtin(BuiltinTool::ExecuteCommand));
        assert!(
            !dev.tools.scope.admits_builtin(BuiltinTool::CodeQuery),
            "code_query must be delegated to explore/debug subagents"
        );
    }

    #[test]
    fn philosophist_role_is_workspace_free() {
        let base = AgentIdentity::from_mission("coding assistant");
        let phil = AgentRoleProfile::from_role(MainAgentRole::Philosophist, &base);
        let crate::ToolScope::Only(names) = &phil.tools.scope else {
            panic!("philosophist must be scoped, not unrestricted");
        };
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_text"));
        assert!(!names.contains("execute_command"));
        assert!(!names.contains("read_text"));
        assert!(names.contains("read_url"));
        assert!(names.contains("search_web"));
        assert!(names.contains("ask_user"));
        assert!(names.contains("recall_memory"));
    }

    #[test]
    fn main_and_sub_agent_derived_from_same_parent() {
        let dev = AgentRoleProfile::developer();
        let mut main_agent: MainAgent = Agent::new(dev, MainAgentRole::Developer);
        assert_eq!(main_agent.role_name(), "developer");
        assert!(main_agent.role().admits_user_interaction());

        let sub_agent: SubAgent = main_agent.spawn_subagent(SubAgentRole::Explore);
        assert_eq!(sub_agent.role_name(), "explore");
        assert!(!sub_agent.role().admits_user_interaction());

        main_agent.switch_role(MainAgentRole::Philosophist);
        assert_eq!(main_agent.role_name(), "philosophist");
    }

    #[test]
    fn session_role_manifest_defaults_and_serde() {
        let dev_manifest = SessionRoleManifest::developer();
        assert_eq!(dev_manifest.role_id, "developer");
        assert_eq!(dev_manifest.builtin_tools(), MainAgentRole::DEVELOPER_TOOLS);
        assert!(!dev_manifest.tools.contains(&BuiltinTool::CodeQuery.as_str().to_string()));
        assert!(
            dev_manifest
                .identity
                .preamble()
                .starts_with("Role: developer.")
        );

        let phil_manifest = SessionRoleManifest::philosophist();
        assert_eq!(phil_manifest.role_id, "philosophist");
        assert_eq!(phil_manifest.builtin_tools(), MainAgentRole::PHILOSOPHIST_TOOLS);
        assert!(
            phil_manifest
                .identity
                .preamble()
                .starts_with("Role: philosophist.")
        );

        let ops_manifest = SessionRoleManifest::ops();
        assert_eq!(ops_manifest.role_id, "ops");
        assert!(ops_manifest.builtin_tools().contains(&BuiltinTool::ExecuteCommand));
        assert!(ops_manifest.builtin_tools().contains(&BuiltinTool::AskUser));
        assert!(!ops_manifest.builtin_tools().contains(&BuiltinTool::CodeQuery));
        assert!(ops_manifest.identity.preamble().starts_with("Role: ops."));

        let serialized = serde_json::to_string(&ops_manifest).expect("serialize ops manifest");
        let deserialized: SessionRoleManifest =
            serde_json::from_str(&serialized).expect("deserialize ops manifest");
        assert_eq!(ops_manifest, deserialized);

        let serialized = serde_json::to_string(&dev_manifest).expect("serialize manifest");
        let deserialized: SessionRoleManifest =
            serde_json::from_str(&serialized).expect("deserialize manifest");
        assert_eq!(dev_manifest, deserialized);
    }

    #[test]
    fn ops_role_is_workspace_free_and_has_host_and_remote_tools() {
        let base = AgentIdentity::from_mission("coding assistant");
        let ops = AgentRoleProfile::from_role(MainAgentRole::Ops, &base);
        assert_eq!(ops.name, "ops");
        assert!(!MainAgentRole::Ops.requires_workspace());
        assert!(!MainAgentRole::Ops.default_confined());
        assert!(MainAgentRole::Developer.requires_workspace());
        assert!(MainAgentRole::Developer.default_confined());
        assert!(!MainAgentRole::Philosophist.requires_workspace());
        assert!(MainAgentRole::Philosophist.default_confined());

        let crate::ToolScope::Only(names) = &ops.tools.scope else {
            panic!("ops must be scoped with explicit tools");
        };
        // System and remote operational capabilities admitted:
        assert!(names.contains(BuiltinTool::ExecuteCommand.as_str()));
        assert!(names.contains(BuiltinTool::ReadText.as_str()));
        assert!(names.contains(BuiltinTool::EditText.as_str()));
        assert!(names.contains(BuiltinTool::WriteFile.as_str()));
        assert!(names.contains(BuiltinTool::ListDir.as_str()));
        assert!(names.contains(BuiltinTool::FindFiles.as_str()));
        assert!(names.contains(BuiltinTool::SearchText.as_str()));
        assert!(names.contains(BuiltinTool::ReadUrl.as_str()));
        assert!(names.contains(BuiltinTool::SearchWeb.as_str()));
        assert!(names.contains(BuiltinTool::AskUser.as_str()));
        assert!(names.contains(BuiltinTool::Todo.as_str()));
        assert!(names.contains(BuiltinTool::SpawnAgent.as_str()));

        // AST code parsing and philosophical memory dialogue are excluded:
        assert!(!ops.tools.scope.admits_builtin(BuiltinTool::CodeQuery));
        assert!(!ops.tools.scope.admits_builtin(BuiltinTool::RecallMemory));

        let dev = AgentRoleProfile::from_role(MainAgentRole::Developer, &base);
        assert!(!dev.tools.scope.admits_builtin(BuiltinTool::CodeQuery));
        assert!(dev.tools.scope.admits_builtin(BuiltinTool::RecallMemory));
    }
}
