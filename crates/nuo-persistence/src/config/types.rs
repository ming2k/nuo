//! Agent, permission, workspace, server, and hook configuration schema.
use nuo_protocol::VariantSelection;
use nuo_tool::{HookEventKind, TrajectoryGuardConfig};
use serde::{Deserialize, Serialize};

pub use nuo_protocol::policy_config::{
    BashPolicyActionConfig, BashPolicyConfig, BashPolicyMatcherConfig, BashPolicyRuleConfig,
    PermissionRuleConfig,
};
use std::collections::HashMap;

pub const THINKING_KEY: &str = "thinking";

/// User-tunable top-level agent behaviour, deserialized from the optional `[agent]`
/// table of `config.toml`.
/// All fields default sensibly, so a `config.toml` with no `[agent]` table
/// (or a partially specified one) is valid.
///
/// ```toml
/// [agent]
/// # Hard-stop a round after this many total ReAct turns. 0 (the default)
/// # means no hard stop — an opt-in execution budget only.
/// # hard_stop_turns = 0
///
/// # Never supervise a command needing stdin (sudo/gpg/passwd/…).
/// # Instead run it sealed (immediate-EOF stdin) so it fails fast
/// # with a non-interactive remedy hint.
/// # skip_interactive_input = false
///
/// # Doom-loop guard (variant-loop defense). On by default; one
/// same-signature re-run is tolerated before a block. Opt out
/// here, or restore the strict first-repeat block with `threshold = 2`.
/// See [`DoomGuardConfig`]. The historical `nudge` key spelling still
/// # [agent.trajectory_guard]
/// # enabled = false
/// # threshold = 2
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    /// `0` (the default) means uncapped. Mutated at runtime via
    /// `Agent::set_hard_stop_turns`.
    pub hard_stop_turns: usize,
    /// Whether the model may supply stdin bytes for a `bash` command it emits
    /// (the opt-in "automatic flow" path, α). Default `false`: the command
    /// tool schema exposes no `stdin` parameter and a command that needs input
    /// either gets it from the operator (interactive-classifier →
    /// runtime supervision) or fails fast with a non-interactive remedy hint.
    /// When `true`, the schema **dynamically** adds a `stdin` field the model
    /// can fill, and the dispatch layer threads it through as
    /// `InputContract::Prefilled`. This is the explicit authorization that
    /// "input may come from the model" — without it, stdin is structurally
    /// unreachable from the model's arguments. Wired through
    /// `Agent::set_allow_model_stdin`.
    pub allow_model_stdin: bool,
    /// Whether an interactive command (one the interactive classifier
    /// matches: `sudo`/`gpg`/`passwd`/TUI editors/`read`/…) should **never** be
    /// supervised for runtime input.
    ///
    /// Default `false`: a command needing input is run supervised (held-open
    /// stdin pipe + controlling terminal), the runtime examiner detects the
    /// wait, and the operator is prompted via the input panel. When `true`, the
    /// command is run sealed — immediate-EOF stdin — and fails fast with a
    /// non-interactive remedy hint, exactly as in unattended mode. This is the
    /// right setting for users who find the prompt disruptive and prefer to
    /// retry the command themselves (or let the model retry with a
    /// non-interactive form). Wired through `Agent::set_skip_interactive_input`.
    ///
    /// Note: this only governs the *input-supervision* path; it does not turn
    /// the agent unattended, so ordinary tool confirmations still apply.
    pub skip_interactive_input: bool,
    /// How an autonomous session (no human channel attached —
    /// piped headless, CI, cron) settles an `ask_user` question. Wire
    /// format: `"fail_closed"` (default) or `"recommended_labeled"`.
    /// Fail-closed refuses the question and tells the model to resolve the
    /// ambiguity itself; recommended-labeled answers each question with
    /// its first (recommended) option, with an explicit
    /// `[answered by policy, not by user]` label so the model cannot
    /// mistake the recommendation for a human decision.
    #[serde(default)]
    pub ask_user_fallback: nuo_protocol::human_request::AutonomousFallbackPolicy,
    /// Trajectory-guard configuration (`nuo_harness::trajectory_guard`).
    /// Default **enabled** (`window: 16`, `threshold: 4`, `cognitive_review: true`).
    #[serde(default)]
    pub trajectory_guard: TrajectoryGuardConfig,
}

// `TrajectoryGuardConfig` is defined in `nuo_protocol::trajectory_guard_config`
// and re-exported above. It is the `[agent.trajectory_guard]` TOML table.

/// Declarative permission configuration — the `[permissions]` table. Lets users
/// pre-declare "always allow" rules in `config.toml` so default policies are
/// data-driven, not purely interactive:
///
/// ```toml
/// [[permissions.allow]]
/// tool = "execute_command"
/// scope = "*"
///
/// [[permissions.allow]]
/// tool = "read"
/// scope = "*"
/// ```
///
/// These seed the allowlist at startup; runtime "Always" decisions still write
/// to the persisted `permissions.json`. A config rule with scope `"*"` allows
/// every call to that tool without prompting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionConfig {
    /// Rules to pre-seed the "always allow" allowlist at startup.
    pub allow: Vec<PermissionRuleConfig>,
}

/// User-owned filesystem admission policy for the active workspace.
///
/// This table lives in the global `config.toml`, not in project-authored
/// `.nuo/config.toml`: repository content must never be able to widen its own
/// filesystem boundary. Relative entries are resolved from the active
/// workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// Additional directory roots that native file tools may access.
    pub additional_roots: Vec<String>,
}

/// Diagnostic report of resolved additional roots and skipped entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAdditionalRoots {
    /// Canonical, admitted directory paths.
    pub admitted: Vec<std::path::PathBuf>,
    /// Configured entries that could not be admitted, with the failure reason.
    pub skipped: Vec<(String, String)>,
}

/// Server lifecycle configuration, deserialized from the `[server]` table of
/// `server.toml` (ADR-0031).
///
/// ```toml
/// [server]
/// shutdown_grace_secs = 10      # graceful-teardown budget before forced exit
/// idle_exit_minutes = 5         # auto-exit after N minutes of zero sessions
///                                # and zero attached clients; 0 = never
/// local_auth = true             # bearer-token the loopback listener too
///                                #; false = trust local processes
/// ```
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct ServerConfig {
    /// Total budget for graceful shutdown (listeners close → connections
    /// drain → sessions tear down with `SessionEnd` hooks). When the budget
    /// expires — or a second signal arrives — remaining tasks are aborted and
    /// the process exits anyway, so a hung external hook can never pin the
    /// server open. A always-on/service deployment should set this at or
    /// above the supervisor's stop timeout (e.g. systemd's `TimeoutStopSec`).
    pub shutdown_grace_secs: u64,
    /// Auto-exit after this many continuous minutes hosting **zero sessions
    /// with zero attached clients** (rule 3): the server becomes
    /// born-on-demand, gone-when-useless. `0` disables idle exit for
    /// always-on deployments.
    pub idle_exit_minutes: u64,
    /// Require a bearer token even on the loopback TCP listener.
    /// The token is generated per server start and published in the
    /// owner-only (0600) discovery record, so co-located CLI/TUI clients
    /// authenticate transparently while other local processes, other users
    /// on a shared machine, and drive-by browser pages cannot drive the
    /// control plane. `false` restores the pre-0105 trust-the-loopback
    /// posture; the UDS listener is always exempt (filesystem permissions
    /// are its boundary). Default: true.
    pub local_auth: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            shutdown_grace_secs: 10,
            idle_exit_minutes: 5,
            local_auth: true,
        }
    }
}

/// Per-model tool-variant selection, deserialized from the `[tool_variants]`
/// section of `config.toml`. Maps a model id to a `capability → variant_id`
/// map. A capability is realized by the named variant (a genuinely different
/// implementation/schema/description), not a re-worded copy of one impl.
///
/// ```toml
/// [tool_variants."glm-5.2"]       # model id (quoted: has dots)
/// read        = "terse"    # capability = variant id
/// execute_command  = "workspace"
/// ```
///
/// Capabilities and models not listed use their default variant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolVariantsConfig(pub HashMap<String, ModelToolVariants>);

/// One model's variant selection: a transparent wrapper around the
/// `capability → variant_id` map so it serializes directly as a TOML table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelToolVariants(pub VariantSelection);

impl ToolVariantsConfig {
    /// Look up the variant selection for `model_id`, if any. Returns an empty
    /// map (not `None`) for unknown models so callers can always borrow
    /// `&VariantSelection`.
    pub fn for_model(&self, model_id: &str) -> &VariantSelection {
        self.0
            .get(model_id)
            .map(|m| &m.0)
            .unwrap_or_else(|| nuo_protocol::empty_variant_selection())
    }
}

/// One user-authored scoped instruction (`[[instructions.user]]`, ADR-0165).
///
/// Selectors mirror project-instruction frontmatter; `model` is always a glob.
/// The text is Markdown prose and is validated together with the selector.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInstructionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// Glob matched against the wire model id; exact ids are patterns too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub text: String,
}

impl UserInstructionConfig {
    pub fn selector(&self) -> nuo_protocol::InstructionSelector {
        nuo_protocol::InstructionSelector {
            role: self.role.clone(),
            provider: self.provider.clone(),
            connection: self.connection.clone(),
            model: self.model.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.selector().validate()?;
        if self.text.trim().is_empty() {
            return Err("instruction text is empty".to_string());
        }
        Ok(())
    }

    fn into_scoped(self, ordinal: usize) -> nuo_protocol::ScopedInstruction {
        let selector = self.selector();
        nuo_protocol::ScopedInstruction {
            id: format!("user:{ordinal}"),
            source: nuo_protocol::InstructionSource::User,
            ordinal,
            selector,
            text: self.text.trim().to_string(),
        }
    }
}

/// The user source: `[[instructions.user]]` entries in `agent.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserInstructionsConfig {
    pub user: Vec<UserInstructionConfig>,
}

impl UserInstructionsConfig {
    pub fn is_empty(&self) -> bool {
        self.user.is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        for (index, entry) in self.user.iter().enumerate() {
            entry
                .validate()
                .map_err(|error| format!("[[instructions.user]] #{index}: {error}"))?;
        }
        Ok(())
    }

    pub fn scoped(&self) -> Vec<nuo_protocol::ScopedInstruction> {
        self.user
            .iter()
            .cloned()
            .enumerate()
            .map(|(ordinal, entry)| entry.into_scoped(ordinal))
            .collect()
    }
}

/// One lifecycle event hook entry (ADR-0025). Deserialized from a `[[hooks]]`
/// table in `config.toml`:
///
/// ```toml
/// [[hooks]]
/// event   = "PostToolUse"          # a [`HookEventKind`] variant
/// matcher = "Write|Edit"           # optional; tool-name `|`-list or regex
/// command = ".nuo/hooks/lint.sh"
/// ```
///
/// The command receives the [`nuo_protocol::HookContext`] as JSON on stdin and
/// communicates a decision via exit code / stdout JSON (see the CLI subagent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpec {
    /// When this hook fires.
    pub event: HookEventKind,
    /// Tool-name filter. `None` (or unset) matches every event; only tool
    /// events (`PreToolUse` / `PostToolUse` / `PostToolUseFailure`) honour it.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Shell command run when the event matches. Executed with the project
    /// root as cwd and the hook context as JSON on stdin.
    pub command: String,
    /// Runtime-only origin marker. Project-defined hooks carry their exact
    /// workspace root and execute read-only/offline inside the workspace
    /// sandbox. Global user hooks leave this unset.
    #[serde(skip)]
    pub sandbox_root: Option<std::path::PathBuf>,
}
