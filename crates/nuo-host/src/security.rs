//! Workspace trust vocabulary for project-supplied assets and configurations.
//!
//! Controls whether project-authored contributions (skills, MCP servers, hooks,
//! AGENTS.md instructions, and workspace config) are loaded into the runtime.
//!
//! Distinct and decoupled from AI runtime tool permissions, which are governed
//! purely by the Tool Hazard model (`HazardLevel` and `PermissionStore`).

use serde::{Deserialize, Serialize};

/// Trust state for one project-authored asset domain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrustState {
    /// The workspace declares no project-level contributions (skills, MCP, hooks, AGENTS.md).
    #[default]
    Absent,
    /// Contributions exist in the workspace, but have not been explicitly trusted by the user.
    Quarantined,
    /// The exact current content digest of contributions was explicitly trusted by the user.
    Trusted,
    /// Contributions were explicitly rejected/denied by human review (ADR-0253).
    Denied,
    /// Contributions were previously trusted or denied, but their content/digest has changed.
    Changed,
    /// Asset 30-day lease has expired and requires routine human re-attestation (ADR-0252).
    Expired,
}

impl WorkspaceTrustState {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted)
    }

    pub fn is_denied(self) -> bool {
        matches!(self, Self::Denied)
    }

    pub fn is_expired(self) -> bool {
        matches!(self, Self::Expired)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Quarantined => "quarantined",
            Self::Trusted => "trusted",
            Self::Denied => "denied",
            Self::Changed => "changed",
            Self::Expired => "expired",
        }
    }
}

/// Concrete domains for project asset trust.
///
/// `all` is deliberately not a domain. It is a command-layer selection that
/// expands to [`TrustDomain::ALL`]. Persisting an aggregate grant would create
/// a second source of truth and make a concrete domain impossible to revoke.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TrustDomain {
    /// Trust project-level Model Context Protocol (MCP) server definitions.
    Mcp,
    /// Trust project-level custom skills.
    Skills,
    /// Trust project-level lifecycle hook definitions and hook assets.
    Hooks,
    /// Trust project-authored instructions and rules (`AGENTS.md`, rules).
    Instructions,
    /// Trust project-level external workspace roots (`[workspace].additional_roots`).
    #[serde(rename = "ex_workspace", alias = "ex-workspace")]
    ExWorkspace,
    /// Trust user-level MCP and global executable assets (ADR-0252).
    #[serde(rename = "user_assets", alias = "user-assets")]
    UserAssets,
}

impl TrustDomain {
    pub const ALL: [Self; 6] = [
        Self::Mcp,
        Self::Skills,
        Self::Hooks,
        Self::Instructions,
        Self::ExWorkspace,
        Self::UserAssets,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::Hooks => "hooks",
            Self::Instructions => "instructions",
            Self::ExWorkspace => "ex-workspace",
            Self::UserAssets => "user-assets",
        }
    }
}

/// First-class security state attached to every harness snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSecuritySnapshot {
    /// Canonical exact workspace root used for persisted decisions.
    pub root: String,
    /// Trust status for MCP domain.
    #[serde(default)]
    pub mcp: WorkspaceTrustState,
    /// Trust status for Skills domain.
    #[serde(default)]
    pub skills: WorkspaceTrustState,
    /// Trust status for lifecycle hooks.
    #[serde(default)]
    pub hooks: WorkspaceTrustState,
    /// Trust status for project instructions (AGENTS.md).
    #[serde(default)]
    pub instructions: WorkspaceTrustState,
    /// Trust status for project-declared external workspace roots.
    #[serde(default)]
    pub ex_workspace: WorkspaceTrustState,
    /// Trust status for user-level MCP and global executable assets (ADR-0252).
    #[serde(default)]
    pub user_assets: WorkspaceTrustState,
}

impl WorkspaceSecuritySnapshot {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            mcp: WorkspaceTrustState::Absent,
            skills: WorkspaceTrustState::Absent,
            hooks: WorkspaceTrustState::Absent,
            instructions: WorkspaceTrustState::Absent,
            ex_workspace: WorkspaceTrustState::Absent,
            user_assets: WorkspaceTrustState::Absent,
        }
    }

    pub fn state(&self, domain: TrustDomain) -> WorkspaceTrustState {
        match domain {
            TrustDomain::Mcp => self.mcp,
            TrustDomain::Skills => self.skills,
            TrustDomain::Hooks => self.hooks,
            TrustDomain::Instructions => self.instructions,
            TrustDomain::ExWorkspace => self.ex_workspace,
            TrustDomain::UserAssets => self.user_assets,
        }
    }

    pub fn is_trusted(&self, domain: TrustDomain) -> bool {
        self.state(domain).is_trusted()
    }

    /// Aggregate state for display only. It never participates in admission.
    pub fn aggregate(&self) -> WorkspaceTrustState {
        let states = [
            self.mcp,
            self.skills,
            self.hooks,
            self.instructions,
            self.ex_workspace,
            self.user_assets,
        ];
        let present = states
            .into_iter()
            .filter(|state| *state != WorkspaceTrustState::Absent)
            .collect::<Vec<_>>();
        if present.is_empty() {
            WorkspaceTrustState::Absent
        } else if present.contains(&WorkspaceTrustState::Changed) {
            WorkspaceTrustState::Changed
        } else if present.contains(&WorkspaceTrustState::Expired) {
            WorkspaceTrustState::Expired
        } else if present.iter().all(|state| {
            matches!(
                *state,
                WorkspaceTrustState::Trusted | WorkspaceTrustState::Denied
            )
        }) {
            if present
                .iter()
                .all(|state| *state == WorkspaceTrustState::Trusted)
            {
                WorkspaceTrustState::Trusted
            } else {
                WorkspaceTrustState::Denied
            }
        } else {
            WorkspaceTrustState::Quarantined
        }
    }
}

/// Specification of an external capability unit subject to attestation (ADR-0243).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetSpec {
    /// Physical OS child process (e.g. Stdio MCP server, lifecycle hooks).
    Process {
        command: Vec<String>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        env: std::collections::BTreeMap<String, String>,
    },
    /// Remote network service endpoint (e.g. Streamable HTTP/SSE MCP server).
    RemoteEndpoint {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        headers: std::collections::BTreeMap<String, String>,
    },
}

impl AssetSpec {
    /// Compute the canonical SHA-256 cryptographic fingerprint of this specification.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        match self {
            Self::Process { command, env } => {
                hasher.update(b"process\0");
                for arg in command {
                    hasher.update(arg.as_bytes());
                    hasher.update(b"\0");
                }
                for (k, v) in env {
                    hasher.update(k.as_bytes());
                    hasher.update(b"=");
                    hasher.update(v.as_bytes());
                    hasher.update(b"\0");
                }
            }
            Self::RemoteEndpoint { url, headers } => {
                hasher.update(b"endpoint\0");
                hasher.update(url.as_bytes());
                hasher.update(b"\0");
                for (k, v) in headers {
                    hasher.update(k.as_bytes());
                    hasher.update(b":");
                    hasher.update(v.as_bytes());
                    hasher.update(b"\0");
                }
            }
        }
        format!("{:x}", hasher.finalize())
    }

    /// User-friendly one-line summary of this asset.
    pub fn summary(&self) -> String {
        match self {
            Self::Process { command, .. } => command.join(" "),
            Self::RemoteEndpoint { url, .. } => url.clone(),
        }
    }
}

/// Canonical locator identifying an external capability unit (ADR-0252).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetLocator {
    /// User-level MCP server declared in global config.toml.
    UserMcp { name: String },
    /// User-level custom skill in ~/.config/nuo/skills or global paths.
    UserSkill { name: String },
    /// User-level lifecycle hook in global configuration.
    UserHook { event: String },
    /// Role-scoped MCP server declared in role bundle (ADR-0253).
    RoleMcp { role: String, name: String },
    /// Role-scoped custom skill (ADR-0253).
    RoleSkill { role: String, name: String },
    /// Workspace-scoped MCP server declared in workspace configuration.
    WorkspaceMcp {
        workspace_root: String,
        name: String,
    },
    /// Project-level custom skill.
    WorkspaceSkill {
        workspace_root: String,
        name: String,
    },
    /// Project-level lifecycle hook.
    WorkspaceHook {
        workspace_root: String,
        event: String,
    },
    /// Project instructions and rules (AGENTS.md).
    WorkspaceInstructions { workspace_root: String },
    /// Project-declared external workspace roots.
    WorkspaceExRoots { workspace_root: String },
    /// Standalone external script or executable referenced by absolute path.
    ExternalPath { path: String },
}

impl AssetLocator {
    /// Deterministic string key for persistence lookup.
    pub fn to_key_string(&self) -> String {
        match self {
            Self::UserMcp { name } => format!("user:mcp:{name}"),
            Self::UserSkill { name } => format!("user:skill:{name}"),
            Self::UserHook { event } => format!("user:hook:{event}"),
            Self::RoleMcp { role, name } => format!("role:{role}:mcp:{name}"),
            Self::RoleSkill { role, name } => format!("role:{role}:skill:{name}"),
            Self::WorkspaceMcp {
                workspace_root,
                name,
            } => {
                format!("ws:{}:mcp:{name}", canonical_root_prefix(workspace_root))
            }
            Self::WorkspaceSkill {
                workspace_root,
                name,
            } => {
                format!("ws:{}:skill:{name}", canonical_root_prefix(workspace_root))
            }
            Self::WorkspaceHook {
                workspace_root,
                event,
            } => {
                format!("ws:{}:hook:{event}", canonical_root_prefix(workspace_root))
            }
            Self::WorkspaceInstructions { workspace_root } => {
                format!("ws:{}:instructions", canonical_root_prefix(workspace_root))
            }
            Self::WorkspaceExRoots { workspace_root } => {
                format!("ws:{}:ex_roots", canonical_root_prefix(workspace_root))
            }
            Self::ExternalPath { path } => format!("ext:{path}"),
        }
    }

    /// User-friendly label for UI presentation.
    pub fn label(&self) -> String {
        match self {
            Self::UserMcp { name } => format!("User MCP: {name}"),
            Self::UserSkill { name } => format!("User Skill: {name}"),
            Self::UserHook { event } => format!("User Hook: {event}"),
            Self::RoleMcp { role, name } => format!("Role ({role}) MCP: {name}"),
            Self::RoleSkill { role, name } => format!("Role ({role}) Skill: {name}"),
            Self::WorkspaceMcp { name, .. } => format!("Project MCP: {name}"),
            Self::WorkspaceSkill { name, .. } => format!("Project Skill: {name}"),
            Self::WorkspaceHook { event, .. } => format!("Project Hook: {event}"),
            Self::WorkspaceInstructions { .. } => "Project Instructions (AGENTS.md)".to_string(),
            Self::WorkspaceExRoots { .. } => "External Workspace Roots".to_string(),
            Self::ExternalPath { path } => format!("External Executable: {path}"),
        }
    }
}

fn canonical_root_prefix(root: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(root.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_string()
}

/// Attestation status for an asset in the universal ledger (ADR-0243, ADR-0252).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationStatus {
    /// Asset is quarantined and blocked from physical execution until user attestation.
    #[default]
    Quarantined,
    /// Exact asset fingerprint has been attested and trusted by the user.
    Trusted,
    /// Asset is temporarily allowed for the active session lifetime only.
    SessionEphemeral,
    /// Asset was explicitly rejected / denied by human review (ADR-0253).
    /// Silently quarantined; does not re-trigger trust gate unless content hash changes.
    Denied,
    /// Asset content has changed compared to previously trusted or denied fingerprint (ADR-0252, ADR-0253).
    Changed,
    /// Asset lease has expired (> 30 days) and requires re-attestation (ADR-0252).
    Expired,
}

impl AttestationStatus {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted | Self::SessionEphemeral)
    }

    pub fn is_denied(self) -> bool {
        matches!(self, Self::Denied)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quarantined => "quarantined",
            Self::Trusted => "trusted",
            Self::SessionEphemeral => "session_ephemeral",
            Self::Denied => "denied",
            Self::Changed => "changed",
            Self::Expired => "expired",
        }
    }
}
