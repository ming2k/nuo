use serde::{Deserialize, Serialize};
use crate::risk::RiskProfile;

/// Consequence / threat level of a tool invocation.
///
/// Distinguishes harmless read-only inspection from destructive mutations,
/// command executions, and process lifecycle operations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
pub enum HazardLevel {
    /// Safe / Read-Only (e.g. view_file, grep, find, list_dir).
    /// Does not mutate files, run arbitrary binaries, or alter system state.
    Safe,
    /// File Mutation / Overwrite risk (e.g. write_to_file, replace_file_content, patch).
    /// Can alter source code or overwrite data in workspace.
    FileModification,
    /// Arbitrary Command Execution / System Process risk (e.g. bash, native shell).
    /// Can execute arbitrary OS processes, open sockets, or mutate environment.
    CommandExecution,
    /// Process / Task Lifecycle management risk (e.g. kill, cancel, signal).
    ProcessLifecycle,
    /// Network / External service / MCP connection risk.
    NetworkOrExternal,
}

impl HazardLevel {
    /// Whether this hazard level requires permission evaluation from the permission handler.
    pub fn requires_permission(self) -> bool {
        !matches!(self, Self::Safe)
    }

    /// User-facing summary label for this threat classification.
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "Safe (Read-Only)",
            Self::FileModification => "File Modification Risk",
            Self::CommandExecution => "Command Execution Risk",
            Self::ProcessLifecycle => "Process Lifecycle Risk",
            Self::NetworkOrExternal => "Network / External Risk",
        }
    }
}

impl From<RiskProfile> for HazardLevel {
    fn from(risk: RiskProfile) -> Self {
        match risk {
            RiskProfile::ReadOnly => Self::Safe,
            RiskProfile::IdempotentMutation => Self::FileModification,
            RiskProfile::Destructive => Self::FileModification,
            RiskProfile::NetworkAccess => Self::NetworkOrExternal,
            RiskProfile::ArbitraryExecution => Self::CommandExecution,
        }
    }
}

impl From<HazardLevel> for RiskProfile {
    fn from(hazard: HazardLevel) -> Self {
        match hazard {
            HazardLevel::Safe => Self::ReadOnly,
            HazardLevel::FileModification => Self::IdempotentMutation,
            HazardLevel::NetworkOrExternal => Self::NetworkAccess,
            HazardLevel::CommandExecution => Self::ArbitraryExecution,
            HazardLevel::ProcessLifecycle => Self::Destructive,
        }
    }
}

/// Four-tier runtime hazard taxonomy for tool invocation and prompt-injection defense (ADR-0243).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
pub enum HazardTier {
    /// Tier 0: Pure Local Query / Read-Only (silent execution with audit logging).
    Tier0Query,
    /// Tier 1: Local Workspace Mutation (sandboxed within workspace; commit journal undo).
    Tier1Mutation,
    /// Tier 2: Outbound Egress & Data Exfiltration (interactive consent sheet; blocks confused deputy).
    Tier2Egress,
    /// Tier 3: Shell & Arbitrary OS Execution (highest guard; strict approval gates).
    Tier3Shell,
}

impl From<HazardLevel> for HazardTier {
    fn from(level: HazardLevel) -> Self {
        match level {
            HazardLevel::Safe => Self::Tier0Query,
            HazardLevel::FileModification => Self::Tier1Mutation,
            HazardLevel::NetworkOrExternal => Self::Tier2Egress,
            HazardLevel::CommandExecution | HazardLevel::ProcessLifecycle => Self::Tier3Shell,
        }
    }
}

impl From<RiskProfile> for HazardTier {
    fn from(risk: RiskProfile) -> Self {
        HazardLevel::from(risk).into()
    }
}

/// Linux process termination / intercept specification submitted by command execution tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct ProcessKillSpec {
    /// The base command or binary (e.g. "cargo", "npm", "python", "rm").
    pub command: String,
    /// Whether the spawned process runs in its own process group and can be reaped via killpg / -$PGID.
    pub process_group_killable: bool,
    /// Standard pkill pattern (e.g. "pkill -P $PID" or "pkill -f <command>").
    pub pkill_target: String,
    /// Working directory where the command will execute.
    pub cwd: Option<String>,
}

/// Detailed, tool-specific payload submitted to the permission handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolPermissionPayload {
    /// File edit / write submission.
    FileEdit {
        /// Target file path(s) being created or modified.
        paths: Vec<String>,
        /// Specific operation (e.g. "write_to_file", "replace_file_content").
        operation: String,
    },
    /// Command execution submission.
    Command {
        /// The full command line to execute.
        command: String,
        /// Working directory.
        cwd: Option<String>,
        /// Process group / pkill descriptor for emergency or cancellation termination.
        kill_spec: ProcessKillSpec,
    },
    /// Process lifecycle submission.
    Process { target: String, action: String },
    /// Generic / external tool submission (e.g. MCP tools).
    Generic {
        summary: String,
        #[ts(type = "unknown")]
        details: serde_json::Value,
    },
}

/// Complete submission from a tool to the permission handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct ToolPermissionSubmission {
    /// Threat / Hazard classification of the tool invocation.
    pub hazard_level: HazardLevel,
    /// User-friendly label for the operation.
    pub label: String,
    /// Detailed consequence description.
    pub description: String,
    /// Canonical scope string for allowlist matching (e.g. path or command pattern).
    pub scope: String,
    /// Structured tool-specific payload.
    pub payload: ToolPermissionPayload,
}
