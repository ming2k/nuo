//! `SubagentTool` — spawns a read-only exploration subagent for research subtasks.
//!
//! Lives in `nuo-harness` proper (not the [`crate::tools`] module) because it
//! constructs an
//! [`crate::Agent`] internally: spawning a subagent is an orchestration
//! concern, not a domain-tool concern. The other tools (Bash/Read/Web/…)
//! stay in [`crate::tools`] and remain pure trait implementations.
//!
//! Admission of tools to the subagent is driven by [`nuo_wire::SubAgentProfile::EXPLORE`]
//! — the single source of truth for the read-only / non-interactive /
//! non-recursive policy. See ADR-0011.

use std::sync::Arc;

use async_trait::async_trait;

use nuo_wire::{SubAgentProfile, Tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, SubagentHandle};

/// Canonical tool name for spawning a delegated child agent (ADR-0183).
pub const SPAWN_AGENT_TOOL_NAME: &str = "spawn_agent";

/// The roles a **model-facing** dispatch tool may request, in the order the
/// schema advertises them.
///
/// This is the single source of truth for both the `role` enum in
/// [`Tool::parameters`] and the runtime check in `run_subagent_outcome`, so the
/// advertised contract and the enforced one cannot drift: a role the schema
/// offers is always resolvable, and a role it does not offer is rejected
/// instead of being silently treated as the bound default (ADR-0179's
/// "actionable diagnostics for mode errors").
///
/// Deliberately narrower than [`nuo_wire::SubAgentProfile::ALL`]:
/// `title` is a harness-internal role (session titling drives it directly
/// through the cognitive pipeline) and must not become spawnable just because
/// it lives in the same pool.
pub const DISPATCH_ROLES: &[&str] = &["explore", "debug", "skill"];

/// Canonical description of the default `spawn_agent` dispatch tool (ADR-0183).
pub const SPAWN_AGENT_TOOL_DESCRIPTION: &str = "\
Spawn an isolated child agent to perform a focused subtask in a separate \
context window and return a consolidated summary. Set 'role' to 'explore' for read-only \
research (default), 'debug' for non-interactive diagnosis and root-cause analysis, \
or 'skill' for skill discovery, inspection, and domain guideline synthesis.";

/// Canonical description of the diagnostic `delegate_debug` dispatch tool.
pub const DELEGATE_DEBUG_TOOL_DESCRIPTION: &str = "\
Delegate a defect, crash, or test failure investigation to a child agent that \
runs builds, tests, and non-interactive diagnostics (e.g. gdb -batch, sanitizers) \
in an isolated context window, then returns a structured root-cause analysis and \
proposed fix. Unlike the main developer, it has no file-writing tools and will not \
mutate your workspace.";

/// Retry settings for a subagent, inherited from the session's provider retry configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentRetryConfig {
    pub max_attempts: usize,
    pub base_ms: u64,
    pub max_ms: u64,
}

impl Default for SubagentRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 30,
            base_ms: 1_000,
            max_ms: 10_000,
        }
    }
}

/// Live subagent handles keyed by the parent tool-call id — the lookup table
/// that lets the harness route a down-direction reply (a permission decision
/// or `ask_user` answer the user gave in the TUI) back into the specific
/// running subagent that surfaced the request. Full-duplex (ADR-0029).
///
/// The `task` tool populates this when it spawns a child (and clears the entry
/// when the child finishes); the harness reads it when it needs to reply to a
/// `SubagentEvent::PermissionRequest` / `UserQuestionRequest` that arrived
/// nested under a given `parent_call_id`. Entries are best-effort: a late reply
/// after the child already finished finds no entry (or a dead handle) and
/// degrades to a no-op rather than erroring.
#[derive(Default)]
pub struct SubagentRegistry {
    map: std::sync::Mutex<std::collections::HashMap<String, SubagentHandle>>,
}

impl SubagentRegistry {
    /// Register a steering handle for the subagent spawned by the
    /// `parent_call_id` tool call. Replaces any prior entry for that id.
    pub fn register(&self, parent_call_id: &str, handle: SubagentHandle) {
        // Poison-recovery idiom (codebase convention): a panic in another
        // holder poisoned the lock; recover the inner data rather than
        // panicking on a second, downstream error.
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(parent_call_id.to_string(), handle);
    }

    /// Look up the handle for a live subagent by its parent tool-call id.
    /// Returns a cloned handle (cheap) so the caller can reply without holding
    /// the lock.
    pub fn get(&self, parent_call_id: &str) -> Option<SubagentHandle> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(parent_call_id)
            .cloned()
    }

    /// Remove the entry for a finished subagent. Called when the `task` tool
    /// returns, so the registry never accumulates dead handles for completed
    /// calls (a handle whose `Weak` already expired is harmless but useless).
    pub fn remove(&self, parent_call_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(parent_call_id);
    }
}

/// Spawn a read-only exploration subagent to handle a research sub-task.
///
/// The subagent runs the same provider with the tools admitted by the bound
/// [`SubAgentProfile`] (today always [`SubAgentProfile::EXPLORE`]): read-only, non-interactive,
/// non-recursive. Its final answer is returned to the calling agent, which
/// stays in control of any write operations and any questions for the user.
pub struct SubagentTool {
    provider: Arc<dyn nuo_wire::Provider>,
    toolset: nuo_wire::ToolSet,
    profile: &'static SubAgentProfile,
    /// The tool name the model calls this dispatch tool by. The default (set by
    /// [`SubagentTool::new`]) is `"subagent"` for the read-only research role; a
    /// second instance bound to a diagnostic profile (e.g. [`SubAgentProfile::DEBUG`]) takes a
    /// distinct name like `"delegate_debug"` so it registers as its own capability
    /// alongside the read-only `subagent`, instead of colliding on the name.
    tool_name: &'static str,
    /// Human-facing description surfaced to the model as the tool's purpose.
    /// Defaults to the read-only research framing; a diagnostic instance
    /// passes its own so the model knows it is the delegation path for
    /// debugging and root-cause analysis, not exploration.
    tool_description: &'static str,
    /// Shared handle to the parent agent's variant selection (the **override**
    /// axis). Bound after the parent agent is built (see
    /// [`SubagentTool::bind_variant_selection`]). At spawn the child resolves
    /// its scoped capabilities to the model's chosen variants by snapshotting
    /// this, so a subagent — an agent on the same model — inherits the parent's
    /// overrides. `None` (the default, e.g. in tests) means default variants.
    parent_variants:
        std::sync::Mutex<Option<Arc<std::sync::Mutex<nuo_wire::VariantSelection>>>>,
    /// Live workspace authority inherited from the parent. Delegation may
    /// narrow this through the subagent's operation scope, never widen it.
    parent_workspace_security:
        std::sync::Mutex<Option<Arc<std::sync::Mutex<nuo_wire::WorkspaceSecuritySnapshot>>>>,
    /// ADR-0141: the parent's human-channel accountant, inherited by every
    /// spawned subagent so a child's posture tracks the session's live OR over
    /// attached clients (an interactive session's subagent can ask the user;
    /// an autonomous one's child never parks on a missing human).
    parent_human_channel:
        std::sync::Mutex<Option<Arc<nuo_wire::human_request::HumanChannelAccountant>>>,
    /// Full-duplex handle registry (ADR-0029): each spawned subagent's
    /// [`SubagentHandle`] is lodged here keyed by the parent tool-call id, so
    /// the harness can route a user's permission / `ask_user` reply back down
    /// into the exact child that surfaced the request. Owned by the tool and
    /// exposed via [`SubagentTool::registry`] so the binary that constructs the
    /// tool (and drives the harness) can hand the same `Arc` to the harness.
    registry: Arc<SubagentRegistry>,
    accounting: std::sync::Mutex<Option<SubagentAccountingContext>>,
    /// Live child cancellation tokens keyed by the parent tool-call id — the
    /// cooperative-cancel arm of interruption (the counterpoint to dropping
    /// the child future). `call_structured_with_events` stores the token each
    /// spawned subagent runs under; the harness's executor calls
    /// [`Tool::request_cancel`] when the user interrupts the turn, which
    /// cancels the stored token. The child's round loop observes it at its
    /// next safe boundary, returns its partial transcript through
    /// `run_subagent_outcome`, and the parent records it instead of losing it.
    /// Entries are removed when the child's run ends, so a late
    /// `request_cancel` for a finished call degrades to a no-op.
    active_cancels: std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
    /// Agent preset delegation policy gating which subagent presets may be dispatched.
    parent_delegation: std::sync::Mutex<Option<nuo_wire::AgentRoleDelegation>>,
    /// Parent execution policy enforcing recursion limits and depth bounds (ADR-0183).
    parent_execution_policy: std::sync::Mutex<Option<nuo_wire::ExecutionPolicy>>,
    /// The session's workspace root, captured at bootstrap so the child's
    /// tools resolve relative paths against the session's project — not the
    /// daemon process's cwd (ADR-0096). `None` falls back to the process cwd
    /// (tests, single-project processes).
    workspace_root: std::sync::Mutex<Option<std::path::PathBuf>>,
    retry_config: std::sync::Mutex<SubagentRetryConfig>,
}

#[derive(Clone)]
struct SubagentAccountingContext {
    ledger: Arc<nuo_wire::TokenSourceLedger>,
    session_id: Arc<std::sync::Mutex<Option<String>>>,
    round_counter: Arc<std::sync::Mutex<u64>>,
}

impl SubagentTool {
    /// `toolset` should be the parent agent's full capability set; `profile`
    /// declares what the spawned subagent may actually use (admission + variant
    /// pins + framing). The caller binds the role explicitly — `&SubAgentProfile::EXPLORE` for
    /// the `spawn_agent` tool.
    pub fn new(
        provider: Arc<dyn nuo_wire::Provider>,
        toolset: nuo_wire::ToolSet,
        profile: &'static SubAgentProfile,
    ) -> Self {
        Self::named(
            provider,
            toolset,
            profile,
            SPAWN_AGENT_TOOL_NAME,
            SPAWN_AGENT_TOOL_DESCRIPTION,
        )
    }

    /// Like [`new`](Self::new) but shares an existing [`SubagentRegistry`] instead
    /// of creating a fresh one.
    pub fn with_registry(
        provider: Arc<dyn nuo_wire::Provider>,
        toolset: nuo_wire::ToolSet,
        profile: &'static SubAgentProfile,
        registry: Arc<SubagentRegistry>,
    ) -> Self {
        Self::named_with_registry(
            provider,
            toolset,
            profile,
            SPAWN_AGENT_TOOL_NAME,
            SPAWN_AGENT_TOOL_DESCRIPTION,
            registry,
        )
    }

    /// Build a dispatch tool under an explicit name and description. This is
    /// how a second subagent dispatch tool is constructed: a
    /// profile like [`SubAgentProfile::DEBUG`] is paired with a distinct tool name
    /// (e.g. `"delegate_debug"`) and a description that tells the model this is the
    /// delegation path for debugging and diagnostics. The read-only `subagent` tool and
    /// a named variant coexist as separate capabilities in the parent toolset.
    pub fn named(
        provider: Arc<dyn nuo_wire::Provider>,
        toolset: nuo_wire::ToolSet,
        profile: &'static SubAgentProfile,
        tool_name: &'static str,
        tool_description: &'static str,
    ) -> Self {
        Self {
            provider,
            toolset,
            profile,
            tool_name,
            tool_description,
            parent_variants: std::sync::Mutex::new(None),
            parent_workspace_security: std::sync::Mutex::new(None),
            parent_human_channel: std::sync::Mutex::new(None),
            registry: Arc::new(SubagentRegistry::default()),
            accounting: std::sync::Mutex::new(None),
            active_cancels: std::sync::Mutex::new(std::collections::HashMap::new()),
            parent_delegation: std::sync::Mutex::new(None),
            parent_execution_policy: std::sync::Mutex::new(None),
            workspace_root: std::sync::Mutex::new(None),
            retry_config: std::sync::Mutex::new(SubagentRetryConfig::default()),
        }
    }

    /// [`Self::named`] sharing an existing registry. The companion of
    /// [`Self::with_registry`]: a named dispatch tool whose children are
    /// reachable through the same harness reply path as its sibling.
    pub fn named_with_registry(
        provider: Arc<dyn nuo_wire::Provider>,
        toolset: nuo_wire::ToolSet,
        profile: &'static SubAgentProfile,
        tool_name: &'static str,
        tool_description: &'static str,
        registry: Arc<SubagentRegistry>,
    ) -> Self {
        Self {
            provider,
            toolset,
            profile,
            tool_name,
            tool_description,
            parent_variants: std::sync::Mutex::new(None),
            parent_workspace_security: std::sync::Mutex::new(None),
            parent_human_channel: std::sync::Mutex::new(None),
            registry,
            accounting: std::sync::Mutex::new(None),
            active_cancels: std::sync::Mutex::new(std::collections::HashMap::new()),
            parent_delegation: std::sync::Mutex::new(None),
            parent_execution_policy: std::sync::Mutex::new(None),
            workspace_root: std::sync::Mutex::new(None),
            retry_config: std::sync::Mutex::new(SubagentRetryConfig::default()),
        }
    }

    /// Bind the agent preset delegation policy to enforce allowed subagent profiles.
    pub fn bind_delegation(&self, delegation: nuo_wire::AgentRoleDelegation) {
        *self
            .parent_delegation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(delegation);
    }

    /// Bind the parent agent's execution policy to govern child depth bounds (ADR-0183).
    pub fn bind_execution_policy(&self, policy: nuo_wire::ExecutionPolicy) {
        *self
            .parent_execution_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(policy);
    }

    /// Pin the session's workspace root so spawned subagents resolve relative
    /// paths against the session's project rather than the daemon process's
    /// cwd (ADR-0096). Called by the bootstrap right after construction;
    /// `None` (the default) keeps the process-cwd fallback.
    pub fn set_workspace_root(&self, root: Option<std::path::PathBuf>) {
        self.workspace_root
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone_from(&root);
    }

    /// Bind the parent's session-scoped accounting handles. Each spawned
    /// subagent gets its own actor id while sharing the session ledger, so nested
    /// provider requests are visible without colliding with the principal's
    /// round/turn numbers.
    pub fn bind_accounting(
        &self,
        ledger: Arc<nuo_wire::TokenSourceLedger>,
        session_id: Arc<std::sync::Mutex<Option<String>>>,
        round_counter: Arc<std::sync::Mutex<u64>>,
    ) {
        *self.accounting.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(SubagentAccountingContext {
                ledger,
                session_id,
                round_counter,
            });
    }

    /// Bind the parent agent's variant-selection handle (the **override** axis)
    /// so spawned subagents inherit the model's tool overrides. Called once,
    /// after the parent agent is constructed (the agent owns the handle). When
    /// unbound, subagents use each capability's default variant.
    pub fn bind_variant_selection(
        &self,
        handle: Arc<std::sync::Mutex<nuo_wire::VariantSelection>>,
    ) {
        *self
            .parent_variants
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// ADR-0141: bind the session's channel accountant; spawned subagents
    /// inherit it (see the `run_subagent` wiring of
    /// `Agent::set_human_channel_accountant`).
    pub fn bind_human_channel(
        &self,
        handle: Arc<nuo_wire::human_request::HumanChannelAccountant>,
    ) {
        *self
            .parent_human_channel
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    fn parent_human_channel(
        &self,
    ) -> Option<Arc<nuo_wire::human_request::HumanChannelAccountant>> {
        self.parent_human_channel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn bind_workspace_security(
        &self,
        handle: Arc<std::sync::Mutex<nuo_wire::WorkspaceSecuritySnapshot>>,
    ) {
        *self
            .parent_workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Snapshot the parent's current variant selection (empty when unbound).
    fn variant_snapshot(&self) -> nuo_wire::VariantSelection {
        self.parent_variants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or_default()
    }

    /// Bind the parent's provider retry settings so spawned subagents inherit
    /// the session's retry budget and backoff parameters.
    pub fn bind_retry_policy(&self, max_attempts: usize, base_ms: u64, max_ms: u64) {
        *self.retry_config.lock().unwrap_or_else(|e| e.into_inner()) = SubagentRetryConfig {
            max_attempts: max_attempts.clamp(1, 60),
            base_ms,
            max_ms,
        };
    }

    /// The shared handle registry for subagents spawned by this tool. The
    /// binary passes this `Arc` to the harness so a user reply in the TUI can
    /// be routed back into the live child (ADR-0029). Each `SubagentTool` instance
    /// owns its own registry (children of different dispatch tools are
    /// disjoint), which is fine because the harness that needs to reply is the
    /// same one that constructed the tool.
    pub fn registry(&self) -> Arc<SubagentRegistry> {
        self.registry.clone()
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn description(&self) -> &str {
        self.tool_description
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "Short label for the sub-task (<=60 chars)" },
                "prompt": { "type": "string", "description": "The full, self-contained instructions for the sub-agent" },
                "role": {
                    "type": "string",
                    "enum": DISPATCH_ROLES,
                    "description": "Optional sub-agent role: 'explore' (default, read-only research), 'debug' (non-interactive diagnosis and root-cause analysis), or 'skill' (skill discovery, inspection, and domain guideline synthesis). Defaults to 'explore'."
                }
            },
            "required": ["description", "prompt"]
        })
    }

    /// Whether invoking this tool spawns a nested sub-agent (ADR-0183).
    /// `spawn_agent` does; subagent presets exclude it to prevent unbounded
    /// recursion.
    fn spawns_subagent(&self) -> bool {
        true
    }

    /// The subagent's in-flight call owns a partial transcript worth preserving,
    /// so the harness routes turn cancellation through
    /// [`Tool::request_cancel`] instead of dropping the future: the child
    /// stops at its next safe boundary, returns its partial work, and the
    /// parent records it as an interrupted result.
    fn supports_cooperative_cancel(&self) -> bool {
        true
    }

    /// Cancel the live child spawned by the `call_id` call. The child's round
    /// loop observes its token at the next safe boundary and returns its
    /// partial transcript through `run_subagent_outcome`, so the parent executor
    /// can drain it instead of dropping it. Returns `false` for an unknown or
    /// already-finished call — the harness then falls back to the drop path.
    fn request_cancel(&self, call_id: &str) -> bool {
        let Some(token) = self
            .active_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(call_id)
            .cloned()
        else {
            return false;
        };
        token.cancel();
        true
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.run_subagent(None, arguments, Box::new(|_| {})).await
    }

    async fn call_with_events<'a>(
        &self,
        call_id: &str,
        arguments: &str,
        on_event: Box<dyn FnMut(nuo_wire::SubagentEvent) + Send + 'a>,
    ) -> Result<String, String> {
        self.run_subagent(Some(call_id), arguments, on_event).await
    }

    async fn call_structured_with_events<'a>(
        &self,
        invocation: nuo_wire::ToolInvocation<'a>,
        on_event: Box<dyn FnMut(nuo_wire::SubagentEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(nuo_wire::ToolStream) + Send + 'a),
    ) -> Result<nuo_wire::ToolOutput, String> {
        let call_id = invocation.call_id;
        let arguments = invocation.arguments;
        // Run the subagent, streaming its lifecycle as SubagentEvents to the
        // parent harness (so the live TUI builds the nested view in real
        // time), then return a structured payload carrying the full transcript
        // + real token usage so the parent can persist children and account
        // cost truthfully.
        //
        // `call_id` is now used (not discarded): it keys the child's duplex
        // handle in the registry (ADR-0029) so a user reply can flow back down
        // into this exact child while it runs.
        //
        // Failure path: a subagent that hit the 32-turn limit, repeated-call
        // guard, or a provider error returns a Subagent payload too — the
        // structured `failed` flag is set so the UI classifies it as Failed
        // without text-sniffing, and the partial transcript is preserved so
        // the user can resume into the half-finished work and the real token
        // cost is accounted. The summary still carries an `Error:` prefix so
        // the parent *model* understands the sub-task did not succeed. Only
        // input-validation errors (bad JSON, missing fields) propagate as
        // `Err`, because they have no partial transcript worth keeping.
        let outcome = self
            .run_subagent_outcome(Some(call_id), arguments, on_event)
            .await?;
        let summary = if outcome.final_content.trim().is_empty() {
            if outcome.failed {
                "(subagent failed before producing an answer)".to_string()
            } else {
                "(subagent returned no answer)".to_string()
            }
        } else {
            outcome.final_content.trim().to_string()
        };
        Ok(nuo_wire::ToolOutput::Subagent {
            summary,
            messages: outcome.messages,
            usage: outcome.token_usage,
            generation_ms: outcome.generation_ms,
            failed: outcome.failed,
            interrupted: outcome.interrupted,
        })
    }
}

/// Internal result of running a sub-agent. Bundles everything the parent
/// harness needs to persist the nested transcript and account for real cost.
pub struct SubagentOutcome {
    messages: Vec<nuo_wire::Message>,
    token_usage: nuo_wire::TokenUsage,
    /// Final assistant content, mirrored for convenience so the parent doesn't
    /// have to scan `messages` for the last Assistant turn.
    final_content: String,
    /// Whether the subagent terminated abnormally (hit its turn cap,
    /// repeated-call guard, or a provider error). Drives the structured
    /// `failed` flag on the returned [`nuo_wire::ToolOutput::Subagent`]
    /// instead of the old `summary.starts_with("Error")` text sniff.
    failed: bool,
    /// Whether the subagent was stopped by the parent before finishing (the turn
    /// was cancelled). Distinct from `failed`: the partial transcript is
    /// preserved either way, but interruption is a user-initiated stop that
    /// the model should treat as resumable work, not a sub-task error.
    interrupted: bool,
    /// The subagent's own generation time (summed across its completed provider
    /// requests). Folded into the parent round's `generation_ms` so the
    /// throughput denominator matches its numerator scope (subagent output
    /// tokens already reach the parent via `token_usage`).
    generation_ms: u64,
}

impl SubagentTool {
    async fn run_subagent_outcome<'a>(
        &self,
        call_id: Option<&str>,
        arguments: &str,
        mut on_event: Box<dyn FnMut(nuo_wire::SubagentEvent) + Send + 'a>,
    ) -> Result<SubagentOutcome, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let description = args["description"]
            .as_str()
            .ok_or("Missing 'description'")?
            .trim();
        let prompt = args["prompt"].as_str().ok_or("Missing 'prompt'")?;
        if description.is_empty() {
            return Err("'description' must not be empty.".to_string());
        }
        if prompt.trim().is_empty() {
            return Err("'prompt' must not be empty.".to_string());
        }

        // The role is a capability grant, not a free-form label: an explicit
        // value must be one this tool advertises, and is refused with the
        // dispatchable set named instead of being silently downgraded to the
        // bound default (which would hand the caller a read-only child while it
        // planned for a write-capable one).
        let profile = match args.get("role").and_then(|role| role.as_str()) {
            // Absent role: the bound profile, verbatim. It is deliberately
            // resolved without the pool — a dispatch tool may be bound to a
            // caller-supplied preset that has no pool entry (session-specific
            // and test profiles do exactly that), and rewriting that to a pool
            // default would silently change the child's capability grant.
            None => self.profile,
            Some(role) => {
                if !DISPATCH_ROLES.contains(&role) {
                    return Err(format!(
                        "Unknown 'role' '{role}'. Dispatchable roles: {DISPATCH_ROLES:?}. \
                         Omit 'role' to use the default ('{}').",
                        self.profile.name
                    ));
                }
                nuo_wire::SubAgentProfile::find(role).ok_or_else(|| {
                    format!(
                        "Role '{role}' is advertised by this dispatch tool but has no preset in \
                         the pool. This is a dispatch-tool configuration error, not a caller \
                         error."
                    )
                })?
            }
        };

        let is_background = args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if is_background {
            // ADR-0234: the parameter is gone from the schema, and the
            // execution path below awaits the child round inside this tool
            // call — there is no background job registration, no `job_id`, and
            // no result to poll. Rejecting an explicitly requested background
            // dispatch is honest; silently blocking would let the caller plan
            // around a continuation that never arrives.
            return Err(
                "Background sub-agent dispatch is not available: this sub-agent runs to \
                 completion inside the calling turn. Re-issue the call without `background` \
                 to run it synchronously and receive its result, or run long independent \
                 work through the run_command tool's `background`/`service` modes."
                    .to_string(),
            );
        }

        if let Some(delegation) = self
            .parent_delegation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && !delegation.admits_subagent(profile.name)
        {
            return Err(format!(
                "Agent role '{}' does not admit subagent role '{}'. Admitted roles: {:?}",
                delegation.role_id, profile.name, delegation.subagent_roles
            ));
        }

        if let Some(parent_policy) = self
            .parent_execution_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && !parent_policy.can_spawn_subagent()
        {
            return Err(format!(
                "Sub-agent spawning is rejected by ExecutionPolicy: current depth {} has reached max_depth {}.",
                parent_policy.depth, parent_policy.max_depth
            ));
        }

        // Announce the bound profile name first so the parent harness / TUI
        // can label this subagent by its role (explore / plan / verify / …)
        // rather than a generic "Subagent". Emitted before the child runs.
        on_event(nuo_wire::SubagentEvent::Started {
            profile: profile.name.to_string(),
        });

        // Resolve the pool for this subagent: profile selection ⊓ model selection.
        // The subagent is an agent on the *same* model as the parent, so it carries
        // the parent's model (capability limits + variant overrides). The profile
        // contributes the role scope and any variant pins; the model contributes
        // its variant overrides (snapshotted from the parent) and its hard
        // capability limits. `resolve_tools` composes both and applies the subagent
        // runtime hard rules (no recursion / control-flow / blocking-on-user).
        let model = nuo_wire::resolve_model(&self.provider.model());
        let model_sel =
            nuo_wire::ToolSelection::unrestricted().with_variants(self.variant_snapshot());
        let sub_tools = profile.resolve_tools(&self.toolset, &model, &model_sel);

        // The subagent's identity *is* its profile's task prompt — that is the
        // role framing for this child (e.g. SubAgentProfile::EXPLORE's research mission),
        // while posture (no human interaction, ephemeral scratchpad, depth cap)
        // is enforced by the execution policy below, not by prose.
        let identity = crate::AgentIdentity::from_directive(profile.system_prompt);
        let mut subagent = Agent::new(self.provider.clone(), sub_tools, identity);
        subagent.set_kind(nuo_wire::AgentKind::Subagent);

        // ADR-0224: bind instance-scoped extensions according to the child's mission.
        match profile.name {
            "explore" => {
                subagent.add_extension(std::sync::Arc::new(
                    crate::extension::CodeIntelligenceExtension::read_only(),
                ));
            }
            "code" => {
                subagent.add_extension(std::sync::Arc::new(
                    crate::extension::CodeIntelligenceExtension::new(),
                ));
            }
            _ => {}
        }

        let parent_policy = self
            .parent_execution_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(nuo_wire::ExecutionPolicy::root_default);
        let child_policy = parent_policy
            .derive_child(Some(profile.tool_policy))
            .unwrap_or_else(|_| nuo_wire::ExecutionPolicy {
                depth: parent_policy.depth + 1,
                max_depth: parent_policy.max_depth,
                allow_human_interaction: false,
                lifecycle: nuo_wire::ContextLifecycle::EphemeralScratchpad,
                tool_policy: Some(profile.tool_policy),
                max_children_budget: 0,
            });
        subagent.set_execution_policy(child_policy);

        if let Some(handle) = self
            .parent_workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .cloned()
        {
            subagent.bind_workspace_security_handle(handle);
        }
        let subagent = Arc::new(subagent);
        if let Some(accounting) = self
            .accounting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            let session_id = accounting
                .session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default();
            let round = *accounting
                .round_counter
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let actor = call_id
                .map(|id| format!("subagent:{id}"))
                .unwrap_or_else(|| format!("subagent:{}", uuid::Uuid::new_v4()));
            subagent.set_thread_id(session_id);
            subagent.restore_round_count(round);
            subagent.set_accounting_actor_id(actor);
            subagent.install_token_ledger(accounting.ledger);
        }
        // A `task` subagent runs unobstructed: disable the deterministic
        // read-loop guard's nudge (ADR-0034) so a short-lived, parent-supervised
        // subagent is never steered by it. The parent and `abort` remain its
        // backstops.
        subagent.set_trajectory_guard_config(nuo_wire::TrajectoryGuardConfig::disabled());
        // Full-duplex (ADR-0029): install the child's steering inbox and lodge
        // its handle in the registry keyed by the parent tool-call id. Now any
        // permission / `ask_user` request the child surfaces travels *up* via
        // `forward_event`, and the user's reply can travel *down* via the
        // registry → handle → `reply_permission` / `reply_user_question`,
        // resolving the child's parked oneshot. A `None` call_id (the bare
        // `call` path, no harness involvement) skips registration — there is no
        // one to reply, so the child must stay self-contained.
        let _handle = subagent.install_inbox();
        if let Some(id) = call_id {
            self.registry.register(id, _handle.clone());
        }
        // Full-duplex (ADR-0029): the broker gate is now profile-driven. The
        // built-in profiles keep `delegated: true` to preserve the legacy
        // autonomous contract, but a profile with `delegated: false` lets a
        // subagent's write/execute tool calls surface as
        // `SubagentEvent::PermissionRequest` up to the parent, with the user's
        // reply routed back down via the registry → handle →
        // `reply_permission` (the parked oneshot resolves directly, no inbox
        // drain needed).
        subagent.set_unattended(profile.unattended);
        // ADR-0141: the child inherits the parent's human-channel posture
        // source. An interactive session's subagents can ask the user through
        // the parent's channel (permission requests flow up via
        // SubagentEvent); an autonomous parent's children never park on a
        // human that is not there — they fail closed / settle by labeled
        // policy in the child, instead of deadlocking the parent's tool
        // call on an unanswerable question.
        if let Some(accountant) = self.parent_human_channel() {
            subagent.set_human_channel_accountant(accountant);
        }
        // Subagents are short-lived and read-only by profile, and session
        // review is on-demand (`/review`) with no automatic firing — so a
        // research subagent never pays for a diagnostic and review can never
        // recurse. No setup needed here. ADR-0018.

        // The subagent's durable transcript opens with just the task as the user
        // message. Request assembly composes a fresh head system message every
        // round from the profile persona (carried via `AgentIdentity`, set
        // above) and mission-neutral system-prompt policy — see ADR-0061.
        //
        // An earlier `Task: {description}` system message here was dead code:
        // request assembly projects legacy system messages out before adding
        // the profile composition, so the task wrapper never reached the
        // model. Dropping it makes the single-message path honest. The task
        // itself is the user message; `description` remains a required label
        // arg (validated above) for the parent / TUI.
        let mut messages = vec![crate::conversation_context::visible_user(
            nuo_wire::InjectionKind::SubagentTask,
            prompt,
        )];
        // The subagent runs under its own cancellation token. When the parent
        // turn is interrupted, the harness's executor calls
        // [`Tool::request_cancel`] on this tool with the parent tool-call id,
        // which cancels the stored token below; the child's round loop
        // observes it at its next safe boundary and returns its partial
        // transcript through the error arm of this function — so the parent
        // records the half-finished work instead of dropping it. A `None`
        // call_id (the bare `call` path, no harness involvement) means nothing
        // can cancel the child, so it keeps a fresh never-cancelled token.
        let child_cancel = CancellationToken::new();
        if let Some(id) = call_id {
            self.active_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), child_cancel.clone());
        }
        // Track the subagent's own ReAct position as `ModelRequestStarted`
        // events arrive so the streamed `StreamStart` / `ToolCall` events can
        // carry it (mirroring the main session's `(round, turn)` stamping).
        let mut position: (u64, usize) = (1, 0);
        // Transient-provider-retry loop. The top-level interactive round
        // retries `HarnessError::Retryable` in `orchestration::execute_round`
        // (config: `provider_retry_max_attempts`), but a subagent runs through
        // `run_streaming_with_events` directly and had *no* retry at all —
        // one flaky long SSE generation (the GLM `xhigh`-effort stream that
        // gets cut mid-body) killed the whole sub-task after minutes of
        // work. Mirror the top-level contract here, bounded and simpler:
        // reuse the same round state across attempts so completed turns are
        // not replayed, back off exponentially, and never retry an
        // interruption, a hard terminal error, or a non-retryable one.
        let retry_config = *self.retry_config.lock().unwrap_or_else(|e| e.into_inner());
        let retry_limit = retry_config.max_attempts.clamp(1, 60);
        let short_id = call_id
            .map(|id| {
                let clean = id.trim_start_matches("call_");
                &clean[..clean.len().min(6)]
            })
            .unwrap_or("subagent");
        let origin_label = format!("subagent #{} · {}", short_id, profile.name);

        let mut round = subagent.begin_streaming_round();
        let mut attempt: usize = 0;
        let result = loop {
            attempt += 1;
            let run = subagent
                .resume_streaming_with_events(&mut messages, &child_cancel, &mut round, |event| {
                    if let nuo_wire::AgentEvent::ModelRequestStarted { round, turn, .. } =
                        &event
                    {
                        position = (*round, *turn);
                    }
                    Self::forward_event(event, position, Some(&origin_label), &mut on_event)
                })
                .await;
            match run {
                Ok(outcome) => break Ok(outcome),
                Err(nuo_wire::HarnessError::Provider(provider_err))
                    if {
                        matches!(
                            provider_err.retry_disposition(),
                            nuo_wire::RetryDisposition::Retry { .. }
                        )
                    } && attempt < retry_limit =>
                {
                    let retry_after_ms = match provider_err.retry_disposition() {
                        nuo_wire::RetryDisposition::Retry { retry_after_ms } => {
                            retry_after_ms
                        }
                        _ => unreachable!(),
                    };
                    let message = provider_err.message();
                    let base_ms = crate::orchestration::retry_delay_ms(
                        attempt,
                        retry_after_ms,
                        retry_config.base_ms,
                        retry_config.max_ms,
                    );
                    let delay_ms = crate::orchestration::apply_jitter_ms(base_ms, |_| {
                        fastrand::u64(0..base_ms)
                    });
                    tracing::warn!(
                        attempt,
                        max_attempts = retry_limit,
                        delay_ms,
                        error = %message,
                        "subagent hit a transient provider error; retrying"
                    );
                    on_event(nuo_wire::SubagentEvent::Notice(
                        nuo_wire::AgentNotice::new(
                            nuo_wire::NoticeKind::ProviderRetry,
                            nuo_wire::NoticeSeverity::Warning,
                            format!(
                                "Subagent retrying after transient provider error \
                                 ({attempt}/{retry_limit})"
                            ),
                            nuo_wire::NoticeSource::Harness,
                        )
                        .with_body(format!(
                            "Waiting {}s before retrying: {}",
                            delay_ms.div_ceil(1_000),
                            crate::orchestration::public_retry_reason(message),
                        )),
                    ));
                    on_event(nuo_wire::SubagentEvent::Activity(format!(
                        "waiting to retry ({}s)",
                        delay_ms.div_ceil(1_000)
                    )));
                    tokio::select! {
                        _ = child_cancel.cancelled() => {
                            break Err(nuo_wire::HarnessError::Interrupted)
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }
                }
                Err(error) => break Err(error),
            }
        };
        // Drop the registry entry for this call regardless of outcome so it
        // never holds a dead handle. The child `Arc` is also dropped here
        // (the last strong ref besides the registry's `Weak`), so any late
        // reply via the handle degrades to a no-op. The cancellation entry is
        // dropped alongside, so a late `request_cancel` finds nothing to
        // cancel and the harness falls back to dropping a finished call.
        if let Some(id) = call_id {
            self.registry.remove(id);
            self.active_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
        }
        match result {
            Ok(result) => {
                let final_content = result.message.content.clone();
                Ok(SubagentOutcome {
                    messages,
                    token_usage: result.token_usage,
                    final_content,
                    failed: false,
                    interrupted: false,
                    generation_ms: result.generation_ms,
                })
            }
            Err(error) => {
                // Interruption (the parent cancelled the turn): preserve the
                // partial transcript as an *interrupted* outcome — not an
                // error. The model must understand the sub-task was stopped by
                // the user (resumable work), not that it failed. On a genuine
                // failure we surface the partial transcript too — both so the
                // parent's tool-result message carries the subagent's
                // work-in-progress `children` and so the real token cost
                // reaches the parent round's accounting; the `final_content`
                // is prefixed `Error: …` so the failure classifier and the
                // TUI's Failed badge both trigger.
                if matches!(error, nuo_wire::HarnessError::Interrupted) {
                    let tool_calls = messages
                        .iter()
                        .filter(|m| m.role == nuo_wire::Role::Tool)
                        .count();
                    let partial = messages.iter().rev().find_map(|m| {
                        (m.role == nuo_wire::Role::Assistant && !m.content.trim().is_empty())
                            .then(|| m.content.trim().to_string())
                    });
                    let final_content = match partial {
                        Some(text) => format!(
                            "Interrupted: the subagent was stopped by the user before completing. \
                             It ran {tool_calls} tool call(s) and produced the following partial \
                             findings:\n{text}"
                        ),
                        None => format!(
                            "Interrupted: the subagent was stopped by the user before producing any \
                             findings (it ran {tool_calls} tool call(s))."
                        ),
                    };
                    tracing::info!(
                        tool_calls,
                        "subagent interrupted by parent; preserving partial transcript"
                    );
                    return Ok(SubagentOutcome {
                        messages,
                        token_usage: nuo_wire::TokenUsage::default(),
                        final_content,
                        failed: false,
                        interrupted: true,
                        generation_ms: 0,
                    });
                }
                let error_string = error.to_string();
                tracing::warn!(error = %error_string, "subagent failed; preserving partial transcript");
                Ok(SubagentOutcome {
                    messages,
                    token_usage: nuo_wire::TokenUsage::default(),
                    final_content: format!("Error: {error_string}"),
                    failed: true,
                    interrupted: false,
                    generation_ms: 0,
                })
            }
        }
    }

    async fn run_subagent<'a>(
        &self,
        call_id: Option<&str>,
        arguments: &str,
        on_event: Box<dyn FnMut(nuo_wire::SubagentEvent) + Send + 'a>,
    ) -> Result<String, String> {
        let outcome = self
            .run_subagent_outcome(call_id, arguments, on_event)
            .await?;
        let content = outcome.final_content.trim().to_string();
        if content.is_empty() {
            Ok("(subagent returned no answer)".to_string())
        } else {
            Ok(content)
        }
    }

    fn forward_event(
        event: nuo_wire::AgentEvent,
        position: (u64, usize),
        origin: Option<&str>,
        on_event: &mut dyn FnMut(nuo_wire::SubagentEvent),
    ) {
        match event {
            nuo_wire::AgentEvent::Notice(notice) => {
                on_event(nuo_wire::SubagentEvent::Notice(notice));
            }
            nuo_wire::AgentEvent::ModelRequestStarted { turn, .. } => {
                let status = if turn == 0 {
                    "waiting for model".to_string()
                } else {
                    format!("waiting for model (turn {})", turn + 1)
                };
                on_event(nuo_wire::SubagentEvent::Activity(status));
            }
            nuo_wire::AgentEvent::AssistantDelta { delta, start } => {
                if start {
                    on_event(nuo_wire::SubagentEvent::StreamStart {
                        round: position.0,
                        turn: position.1,
                    });
                }
                on_event(nuo_wire::SubagentEvent::StreamDelta(delta));
            }
            nuo_wire::AgentEvent::AssistantEnd(content) => {
                on_event(nuo_wire::SubagentEvent::StreamEnd(content));
            }
            // The subagent's reasoning chain, streamed live instead of surfacing
            // only after a session reload.
            nuo_wire::AgentEvent::ReasoningDelta { delta, start } => {
                if start {
                    on_event(nuo_wire::SubagentEvent::StreamReasoningStart {
                        round: position.0,
                        turn: position.1,
                    });
                }
                on_event(nuo_wire::SubagentEvent::StreamReasoningDelta(delta));
            }
            nuo_wire::AgentEvent::ReasoningEnd(content) => {
                on_event(nuo_wire::SubagentEvent::StreamReasoningEnd(content));
            }
            nuo_wire::AgentEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                on_event(nuo_wire::SubagentEvent::ToolCall {
                    id,
                    name,
                    arguments,
                    round: position.0,
                    turn: position.1,
                });
            }
            nuo_wire::AgentEvent::ToolResult {
                id,
                name,
                output,
                duration_ms,
                ..
            } => {
                on_event(nuo_wire::SubagentEvent::ToolResult {
                    id,
                    name,
                    output,
                    duration_ms,
                });
            }
            // Full-duplex (ADR-0029 / ADR-0138): a permission broker request from the
            // child travels *up* stamped with the child's short hash and profile origin.
            nuo_wire::AgentEvent::PermissionRequest(mut request) => {
                if request.origin.is_none() {
                    request.origin = origin.map(str::to_string);
                }
                on_event(nuo_wire::SubagentEvent::PermissionRequest(request));
            }
            // Same full-duplex contract as the permission arm above.
            nuo_wire::AgentEvent::UserQuestionRequest(mut request) => {
                if request.origin.is_none() {
                    request.origin = origin.map(str::to_string);
                }
                on_event(nuo_wire::SubagentEvent::UserQuestionRequest(request));
            }
            // An interactive `bash` inside the subagent needs operator
            // stdin; forward the request up so the parent harness can surface
            // it, with the reply routed back down via `reply_input`.
            nuo_wire::AgentEvent::StdinRequest(request) => {
                on_event(nuo_wire::SubagentEvent::StdinRequest(request));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, BoxStream};
    use nuo_wire::{Message, Provider, ProviderStreamEvent, Role, SubAgentProfile};

    struct CannedProvider;

    #[async_trait::async_trait]
    impl Provider for CannedProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "found 3 relevant files",
            )))
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::once(async {
                Ok("found 3 relevant files".to_string())
            })))
        }
    }

    #[derive(Default)]
    struct RecordingProvider {
        request: std::sync::Mutex<Option<nuo_wire::ModelRequest>>,
    }

    /// Fails the first `stream_chat_events` call with a retryable transport
    /// error, succeeds afterwards — the exact shape of a GLM long SSE stream
    /// cut off mid-body (`Kind::Decode` → `[MUTA_RETRYABLE]`), which before
    /// the subagent retry loop killed the sub-task outright.
    struct FlakyThenOkProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FlakyThenOkProvider {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for FlakyThenOkProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "recovered",
            )))
        }

        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::once(async {
                Ok("recovered".to_string())
            })))
        }

        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            let seen = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if seen == 0 {
                return Err(nuo_wire::ProviderError::new(
                    "OpenAI",
                    nuo_wire::ProviderErrorKind::Transport,
                    "OpenAI transport error: error decoding response body (connection closed before message completed)",
                ).retryable(None));
            }
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("recovered".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_wire::ProviderCompletionMeta::default(),
                )),
            ])))
        }
    }
    #[async_trait::async_trait]
    impl Provider for RecordingProvider {
        async fn chat(
            &self,
            request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "found 3 relevant files",
            )))
        }

        async fn stream_chat(
            &self,
            request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            *self.request.lock().unwrap() = Some(request);
            Ok(Box::pin(stream::once(async {
                Ok("found 3 relevant files".to_string())
            })))
        }
    }

    struct EchoReadTool;

    #[async_trait::async_trait]
    impl Tool for EchoReadTool {
        fn name(&self) -> &str {
            "read_text"
        }
        fn description(&self) -> &str {
            "test read tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("echo".to_string())
        }
    }

    /// A terse `read_text` variant and a write tool, to prove a subagent
    /// resolves the *model's* variant (override axis) and then narrows to the
    /// *profile's* scope (scope axis) — the two are orthogonal.
    struct TerseReadTool;
    #[async_trait::async_trait]
    impl Tool for TerseReadTool {
        fn name(&self) -> &str {
            "read_text"
        }
        fn variant(&self) -> &str {
            "terse"
        }
        fn description(&self) -> &str {
            "terse read tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("terse".to_string())
        }
    }
    #[test]
    fn subagent_inherits_model_variant_then_applies_profile_scope() {
        // `StubWriteTool` (name "stub_write") is not in SubAgentProfile::EXPLORE's read-only
        // scope, so it is always excluded; `read_text` has two variants.
        let toolset = nuo_wire::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(TerseReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(StubWriteTool) as std::sync::Arc<dyn Tool>,
        ]);
        let tool = SubagentTool::new(
            std::sync::Arc::new(CannedProvider),
            toolset,
            &SubAgentProfile::EXPLORE,
        );

        let resolve = |tool: &SubagentTool| {
            let model = nuo_wire::resolve_model(&CannedProvider.model());
            let model_sel = nuo_wire::ToolSelection::unrestricted()
                .with_variants(tool.variant_snapshot());
            tool.profile
                .resolve_tools(&tool.toolset, &model, &model_sel)
        };

        // Unbound (no model override) → read_text resolves to its default
        // variant; the out-of-scope write tool is excluded regardless.
        let scoped = resolve(&tool);
        let read = scoped.iter().find(|t| t.name() == "read_text");
        assert_eq!(read.map(|t| t.variant()), Some("default"));
        assert!(scoped.iter().all(|t| t.name() != "stub_write"));

        // Bind a model selection pinning read_text=terse: the subagent inherits
        // the override (terse), while scope is still profile-driven.
        let mut sel = nuo_wire::VariantSelection::new();
        sel.insert("read_text".to_string(), "terse".to_string());
        tool.bind_variant_selection(std::sync::Arc::new(std::sync::Mutex::new(sel)));
        let scoped = resolve(&tool);
        let read = scoped.iter().find(|t| t.name() == "read_text");
        assert_eq!(read.map(|t| t.variant()), Some("terse"));
        assert!(scoped.iter().all(|t| t.name() != "stub_write"));
    }

    #[tokio::test]
    async fn subagent_retries_after_transient_stream_failure() {
        let provider = std::sync::Arc::new(FlakyThenOkProvider::new());
        let tool = SubagentTool::new(
            std::sync::Arc::clone(&provider) as std::sync::Arc<dyn Provider>,
            nuo_wire::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &SubAgentProfile::EXPLORE,
        );

        let output = tool
            .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
            .await
            .expect("the subagent must recover from one transient stream failure");

        assert_eq!(
            output, "recovered",
            "the retry must reach the successful attempt's answer"
        );
        assert_eq!(
            provider.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly one transient failure then one success"
        );
    }

    #[tokio::test]
    async fn subagent_inherits_and_respects_custom_retry_policy() {
        let provider = std::sync::Arc::new(FlakyThenOkProvider::new());
        let tool = SubagentTool::new(
            std::sync::Arc::clone(&provider) as std::sync::Arc<dyn Provider>,
            nuo_wire::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &SubAgentProfile::EXPLORE,
        );
        tool.bind_retry_policy(1, 10, 10); // only 1 attempt

        let output = tool
            .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
            .await
            .expect("tool call returns error string in outcome");

        assert!(
            output.starts_with("Error:"),
            "should return error string when retry limit is 1 and first call fails: {output}"
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn task_tool_runs_read_only_subagent_and_returns_answer() {
        let tool = SubagentTool::new(
            std::sync::Arc::new(CannedProvider),
            nuo_wire::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &SubAgentProfile::EXPLORE,
        );

        let output = tool
            .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
            .await
            .unwrap();

        assert_eq!(output, "found 3 relevant files");
    }

    /// A provider that lets the test control when the *second* model request
    /// is in flight: the first request returns a `read_text` tool call (which
    /// the subagent executes), the second flips `second_request_started` and
    /// then never produces a stream event — so the subagent is parked mid-flight
    /// until its cancellation token fires.
    struct GatedProvider {
        requests: std::sync::atomic::AtomicUsize,
        second_request_started: tokio::sync::watch::Sender<bool>,
    }

    #[async_trait]
    impl Provider for GatedProvider {
        async fn chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "gated",
            )))
        }
        async fn stream_chat(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: nuo_wire::ModelRequest,
        ) -> Result<
            BoxStream<'static, Result<ProviderStreamEvent, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            if self
                .requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                // First request: ask the subagent to run its `read_text` tool.
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderStreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("subagent_inner_1".to_string()),
                        name: Some("read_text".to_string()),
                        arguments: "{}".to_string(),
                    }),
                    Ok(ProviderStreamEvent::Completed(
                        nuo_wire::ProviderCompletionMeta::default(),
                    )),
                ])))
            } else {
                // Second request: tell the test the subagent is mid-flight, then
                // stall forever. The subagent's streaming loop races its
                // cancellation token against `stream.next()`, so cancelling
                // the child token resolves this immediately.
                let _ = self.second_request_started.send(true);
                Ok(Box::pin(stream::pending()))
            }
        }
    }

    /// Regression for cooperative interruption: when the parent cancels a
    /// running subagent, the partial transcript is preserved as an *interrupted*
    /// outcome (not dropped, not a failure). The child's completed tool call,
    /// the task message, and a model-facing "Interrupted:" summary all survive
    /// so the parent can record them and the user can resume.
    #[tokio::test]
    async fn interrupting_subagent_preserves_partial_transcript() {
        let (started_tx, started_rx) = tokio::sync::watch::channel(false);
        let provider = std::sync::Arc::new(GatedProvider {
            requests: std::sync::atomic::AtomicUsize::new(0),
            second_request_started: started_tx,
        });
        let tool = std::sync::Arc::new(SubagentTool::new(
            provider,
            nuo_wire::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &SubAgentProfile::EXPLORE,
        ));

        let tool_for_run = tool.clone();
        let run = tokio::spawn(async move {
            tool_for_run
                .run_subagent_outcome(
                    Some("call_interrupt"),
                    r#"{"description":"interrupt me","prompt":"find the handlers"}"#,
                    Box::new(|_event: nuo_wire::SubagentEvent| {}),
                )
                .await
        });

        // Wait until the subagent is genuinely mid-flight (its second model
        // request is in the air), then interrupt it the way the harness's
        // executor does: via `Tool::request_cancel` keyed by the call id.
        let mut started_rx = started_rx;
        started_rx
            .changed()
            .await
            .expect("subagent reached its second request");
        assert!(
            tool.request_cancel("call_interrupt"),
            "an in-flight subagent must accept the cancel request"
        );

        let outcome = run.await.expect("subagent run task").expect("outcome");
        assert!(outcome.interrupted, "interruption must be flagged");
        assert!(!outcome.failed, "interruption is not a failure");
        assert!(
            outcome.final_content.starts_with("Interrupted:"),
            "model-facing summary should say Interrupted, got: {}",
            outcome.final_content
        );
        // The partial transcript must contain the completed read_text round.
        let tool_result_msgs = outcome
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count();
        assert_eq!(
            tool_result_msgs, 1,
            "the completed child tool call must survive in the partial transcript"
        );
        assert!(outcome.messages[0].role == Role::User);
        // A late cancel for a finished call degrades to a no-op, not an error.
        assert!(
            !tool.request_cancel("call_interrupt"),
            "a finished call must reject a late cancel"
        );
    }

    /// The subagent persona belongs to the immutable provider request, not its
    /// durable child transcript. The delegated task remains a user message.
    #[tokio::test]
    async fn subagent_head_system_message_has_no_dead_task_line() {
        let provider = std::sync::Arc::new(RecordingProvider::default());
        let tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &SubAgentProfile::EXPLORE,
        );
        let outcome = tool
            .run_subagent_outcome(
                None,
                r#"{"description":"find files","prompt":"where are the handlers?"}"#,
                Box::new(|_event: nuo_wire::SubagentEvent| {}),
            )
            .await
            .unwrap();

        let request = provider
            .request
            .lock()
            .unwrap()
            .clone()
            .expect("subagent request captured");
        let system_content = request.instructions.render_combined();
        assert!(
            system_content.starts_with("You are a delegated research subagent"),
            "system instructions should open with the SubAgentProfile::EXPLORE task prompt"
        );
        assert!(
            !system_content.contains("Task: find files"),
            "the dead `Task: {{description}}` line must not appear (ADR-0039)"
        );

        assert!(
            outcome
                .messages
                .iter()
                .all(|message| message.role != nuo_wire::Role::System),
            "request-scoped policy must not be persisted in the child transcript"
        );

        // The task is the first durable user message.
        assert_eq!(outcome.messages[0].role, nuo_wire::Role::User);
        assert_eq!(outcome.messages[0].content, "where are the handlers?");
        assert_eq!(
            outcome.messages[0]
                .origin
                .as_ref()
                .map(|origin| origin.kind),
            Some(nuo_wire::InjectionKind::SubagentTask)
        );
    }

    #[tokio::test]
    async fn task_tool_rejects_missing_fields() {
        let tool = SubagentTool::new(
            std::sync::Arc::new(CannedProvider),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );
        assert!(tool.call(r#"{"description":"x"}"#).await.is_err());
        assert!(tool.call(r#"{"prompt":"x"}"#).await.is_err());
    }

    /// ADR-0234: the tool cannot dispatch a background child (the call awaits
    /// the child round and returns its result), so both the schema and an
    /// explicit request must say so instead of promising a notification that
    /// never arrives.
    #[tokio::test]
    async fn subagent_schema_hides_and_rejects_background_dispatch() {
        let tool = SubagentTool::new(
            std::sync::Arc::new(CannedProvider),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );
        assert!(
            tool.parameters()
                .get("properties")
                .and_then(|p| p.get("background"))
                .is_none(),
            "the unsupported background parameter must not be advertised"
        );
        let err = tool
            .call(r#"{"description":"x","prompt":"y","background":true}"#)
            .await
            .expect_err("background dispatch must be rejected explicitly");
        assert!(
            err.contains("not available") && err.contains("without `background`"),
            "rejection must be actionable: {err}"
        );
    }

    /// A non-whitelisted stub, used to prove the explore profile rejects tools
    /// by name (it is not in READ_ONLY_TOOLS).
    struct StubWriteTool;

    #[async_trait::async_trait]
    impl Tool for StubWriteTool {
        fn name(&self) -> &str {
            "stub_write"
        }
        fn description(&self) -> &str {
            "test write tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("write".to_string())
        }
    }

    /// Regression for the deadlock fixed in ADR-0011: the explore profile
    /// must exclude (a) the real `ask_user` tool — Read but interactive,
    /// (b) any write tool, and (c) `task` itself — Read but a dispatch tool
    /// that would recurse. Built with the real tool instances the harness
    /// registers, not stubs, so a future capability-bit regression on either
    /// side is caught here.
    #[test]
    fn explore_profile_excludes_user_write_and_recursion_using_real_tools() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let subagent_tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let toolset = nuo_wire::ToolSet::from_tools(vec![
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(crate::tools::AskUserTool),
            std::sync::Arc::new(StubWriteTool),
            std::sync::Arc::new(subagent_tool),
        ]);

        let model = nuo_wire::resolve_model(&CannedProvider.model());
        let model_sel = nuo_wire::ToolSelection::unrestricted();
        let admitted = SubAgentProfile::EXPLORE.resolve_tools(&toolset, &model, &model_sel);
        let admitted_names: Vec<&str> = admitted.iter().map(|t| t.name()).collect();

        assert_eq!(admitted_names, vec!["read_text"]);
    }

    /// Cross-cut regression: `SubAgentProfile::EXPLORE` admits only its whitelisted read tools —
    /// `ask_user`, the non-whitelisted write stub, and recursion are all
    /// excluded. The read stub is admitted because it is named `read_text`,
    /// which is in [`READ_ONLY_TOOLS`].
    #[test]
    fn explore_profile_excludes_bash_writes_user_and_recursion() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let subagent_tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let toolset = nuo_wire::ToolSet::from_tools(vec![
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(crate::tools::ExecuteCommandTool::new(None)),
            std::sync::Arc::new(crate::tools::AskUserTool),
            std::sync::Arc::new(StubWriteTool),
            std::sync::Arc::new(subagent_tool),
        ]);

        // SubAgentProfile::EXPLORE: only the whitelisted read tool survives (bash, ask_user,
        // the write stub, and recursion are all excluded).
        let model = nuo_wire::resolve_model(&CannedProvider.model());
        let model_sel = nuo_wire::ToolSelection::unrestricted();
        let explore_selected = SubAgentProfile::EXPLORE.resolve_tools(&toolset, &model, &model_sel);
        let explore_names: Vec<&str> = explore_selected.iter().map(|t| t.name()).collect();
        assert_eq!(explore_names, vec!["read_text"]);
    }

    /// A diagnostic `delegate_debug` tool (bound to [`nuo_wire::SubAgentProfile::DEBUG`])
    /// admits read tools and command execution/process tools, but strictly excludes
    /// workspace writes, human interaction, and recursion. Built with the real tools the
    /// harness registers so a future capability regression is caught here —
    /// mirrors `explore_profile_excludes_bash_writes_user_and_recursion` for
    /// the inverse contract.
    #[test]
    fn debug_profile_admits_command_and_read_tools_excluding_writes_and_recursion() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let delegate_debug_arc = std::sync::Arc::new(SubagentTool::named(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &nuo_wire::SubAgentProfile::DEBUG,
            "delegate_debug",
            "debugging subagent",
        ));

        let toolset = nuo_wire::ToolSet::from_tools(vec![
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(crate::tools::ExecuteCommandTool::new(None)),
            std::sync::Arc::new(crate::tools::WriteFileTool::new(None)),
            std::sync::Arc::new(crate::tools::EditTextTool::new(None)),
            std::sync::Arc::new(crate::tools::AskUserTool),
            delegate_debug_arc.clone() as std::sync::Arc<dyn Tool>,
        ]);

        // SubAgentProfile::DEBUG admits bash (run_command) and the read tools; it
        // strictly excludes write_file, edit_text, ask_user, and the subagent dispatch tool itself (recursion).
        let model = nuo_wire::resolve_model(&CannedProvider.model());
        let model_sel = nuo_wire::ToolSelection::unrestricted();
        let selected = nuo_wire::SubAgentProfile::DEBUG.resolve_tools(&toolset, &model, &model_sel);
        let names: std::collections::HashSet<&str> = selected.iter().map(|t| t.name()).collect();
        assert!(names.contains("read_text"));
        assert!(names.contains("run_command"));
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_text"));
        assert!(!names.contains("ask_user"));
        assert!(
            !names.contains("delegate_debug"),
            "recursion must be excluded"
        );

        // The tool surfaces under its own name.
        assert_eq!(delegate_debug_arc.name(), "delegate_debug");
    }

    /// Two dispatch tools sharing one registry is the load-bearing property
    /// that lets the harness route a reply to the correct child regardless of
    /// which tool spawned it. A child registered under one tool's call id is
    /// reachable via the shared registry, and neither tool's `registry()` is
    /// a distinct allocation.
    #[test]
    fn named_with_registry_shares_the_registry_across_tools() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let explore = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );
        let shared = explore.registry();
        let debug = SubagentTool::named_with_registry(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &nuo_wire::SubAgentProfile::DEBUG,
            "delegate_debug",
            "debugging subagent",
            shared.clone(),
        );
        // Same Arc<SubagentRegistry> — the driver hands one to the harness, and
        // children of either tool land in the same table.
        assert!(
            std::sync::Arc::ptr_eq(&explore.registry(), &debug.registry()),
            "named_with_registry must share the registry, not clone-allocate"
        );
        // The two tools are distinct capabilities (different names) so they
        // coexist in a parent toolset without one shadowing the other.
        assert_ne!(explore.name(), debug.name());
        assert_eq!(explore.name(), "spawn_agent");
        assert!(!explore.matches_name("delegate_debug"));
    }

    #[tokio::test]
    async fn debug_subagent_role_execution() {
        let provider = std::sync::Arc::new(CannedProvider);
        let tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let summary = tool
            .call(r#"{"description":"debug crash","prompt":"investigate segfault in parser","role":"debug"}"#)
            .await
            .expect("debug subagent dispatch succeeds");

        assert_eq!(summary, "found 3 relevant files");
    }

    #[tokio::test]
    async fn skill_subagent_role_execution() {
        let provider = std::sync::Arc::new(CannedProvider);
        let tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let summary = tool
            .call(r#"{"description":"find rust skill","prompt":"locate rust guidelines in .nuo/skills","role":"skill"}"#)
            .await
            .expect("skill subagent dispatch succeeds");

        assert_eq!(summary, "found 3 relevant files");
    }

    /// The `role` enum in the schema is the enforced contract, not a hint: an
    /// unadvertised value is refused with the dispatchable set named, instead
    /// of being silently downgraded to the bound default (a caller that asked
    /// for a write-capable child must not receive a read-only one without
    /// being told).
    #[tokio::test]
    async fn unknown_role_is_rejected_with_the_dispatchable_set() {
        let provider = std::sync::Arc::new(CannedProvider);
        let tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let error = tool
            .call(r#"{"description":"typo","prompt":"do something","role":"explort"}"#)
            .await
            .expect_err("an unadvertised role must not silently run");
        assert!(error.contains("Unknown 'role' 'explort'"), "got: {error}");
        assert!(
            error.contains("explore"),
            "must name dispatchable roles: {error}"
        );
    }

    /// `title` lives in the preset pool but is harness-internal (session
    /// titling drives it directly). It must not be spawnable through the
    /// model-facing dispatch tool just because the pool contains it.
    #[tokio::test]
    async fn internal_title_role_is_not_dispatchable() {
        let provider = std::sync::Arc::new(CannedProvider);
        let tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let error = tool
            .call(r#"{"description":"title","prompt":"name this session","role":"title"}"#)
            .await
            .expect_err("the internal titling role must not be model-dispatchable");
        assert!(error.contains("Unknown 'role' 'title'"), "got: {error}");
    }

    /// The advertised enum and the enforced check come from one constant, so a
    /// role can never be offered by the schema and refused at runtime (or
    /// accepted at runtime and hidden from the schema).
    #[test]
    fn schema_role_enum_matches_the_enforced_dispatch_roles() {
        let tool = SubagentTool::new(
            std::sync::Arc::new(CannedProvider),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        let advertised: Vec<String> = tool.parameters()["properties"]["role"]["enum"]
            .as_array()
            .expect("role enum is an array")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("role entries are strings")
                    .to_string()
            })
            .collect();
        assert_eq!(
            advertised,
            DISPATCH_ROLES
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
        );
        // Every advertised role must resolve to a real preset.
        for role in DISPATCH_ROLES {
            assert!(
                nuo_wire::SubAgentProfile::find(role).is_some(),
                "advertised role '{role}' has no preset"
            );
        }
    }

    #[tokio::test]
    async fn execution_policy_rejects_subagent_spawning_when_depth_capped() {
        let provider = std::sync::Arc::new(CannedProvider);
        let tool = SubagentTool::new(
            provider.clone(),
            nuo_wire::ToolSet::default(),
            &SubAgentProfile::EXPLORE,
        );

        // Max depth is 1, current depth is already 1 (child trying to spawn grandchild)
        let capped_policy = nuo_wire::ExecutionPolicy {
            depth: 1,
            max_depth: 1,
            allow_human_interaction: false,
            lifecycle: nuo_wire::ContextLifecycle::EphemeralScratchpad,
            tool_policy: None,
            max_children_budget: 0,
        };
        tool.bind_execution_policy(capped_policy);

        let result = tool
            .call(r#"{"description":"nested subagent","prompt":"spawn more subagents","role":"explore"}"#)
            .await;

        assert!(
            result.is_err(),
            "must be rejected when depth limit is reached"
        );
        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains("Sub-agent spawning is rejected by ExecutionPolicy"),
            "error should mention ExecutionPolicy: {err_msg}"
        );
    }
}
