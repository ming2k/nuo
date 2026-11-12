//! The subagent tool entity and its `Tool` contract.

use super::*;

/// Spawn a read-only exploration subagent to handle a research sub-task.
///
/// The subagent runs the same provider with the tools admitted by the bound
/// [`SubAgentProfile`] (today always [`SubAgentProfile::EXPLORE`]): read-only, non-interactive,
/// non-recursive. Its final answer is returned to the calling agent, which
/// stays in control of any write operations and any questions for the user.

pub struct SubagentTool {
    provider: Arc<dyn nuo_protocol::Provider>,
    toolset: nuo_protocol::ToolSet,
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
        std::sync::Mutex<Option<Arc<std::sync::Mutex<nuo_protocol::VariantSelection>>>>,
    /// User-authored scoped instruction set inherited from the parent
    /// (ADR-0165). Shared by handle so the child resolves the same user source.
    parent_user_instructions:
        std::sync::Mutex<Option<Arc<std::sync::RwLock<Vec<nuo_protocol::ScopedInstruction>>>>>,
    /// Repository-authored scoped instruction set inherited from the parent
    /// (ADR-0164). Shared by handle so the child resolves against the same
    /// declarations without re-reading the workspace.
    parent_project_instructions:
        std::sync::Mutex<Option<Arc<std::sync::RwLock<Vec<nuo_protocol::ScopedInstruction>>>>>,
    /// Provider type behind the parent's active connection (ADR-0164).
    parent_instruction_provider_kind:
        std::sync::Mutex<Option<Arc<std::sync::RwLock<Option<String>>>>>,
    /// Live workspace authority inherited from the parent. Delegation may
    /// narrow this through the subagent's operation scope, never widen it.
    parent_workspace_security:
        std::sync::Mutex<Option<Arc<std::sync::Mutex<nuo_protocol::WorkspaceSecuritySnapshot>>>>,
    /// The parent's human-channel accountant, inherited by every
    /// spawned subagent so a child's posture tracks the session's live OR over
    /// attached clients (an interactive session's subagent can ask the user;
    /// an autonomous one's child never parks on a missing human).
    parent_human_channel:
        std::sync::Mutex<Option<Arc<nuo_protocol::human_request::HumanChannelAccountant>>>,
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
    parent_delegation: std::sync::Mutex<Option<nuo_protocol::AgentRoleDelegation>>,
    /// Parent execution policy enforcing recursion limits and depth bounds.
    parent_execution_policy: std::sync::Mutex<Option<nuo_protocol::ExecutionPolicy>>,
    /// The session's workspace root, captured at bootstrap so the child's
    /// tools resolve relative paths against the session's project — not the
    /// server process's cwd. `None` falls back to the process cwd
    /// (tests, single-project processes).
    workspace_root: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// The parent session store, bound at composition time (see
    /// [`SubagentTool::bind_session`]). Each spawned subagent opens its **own**
    /// session row beneath it, so a child's transcript is durable while it runs
    /// instead of being reconstructed afterwards from the parent's embedded
    /// `children`. `None` (tests, bare `call` paths) runs the child session-free,
    /// which is the pre-existing behaviour.
    parent_session: std::sync::Mutex<Option<Arc<nuo_persistence::session::SessionStore>>>,
    /// Factory producing a child's mid-turn relief (threshold + projection gate).
    /// Injected by the composition root because the implementation lives above
    /// this layer (`[INV-HARNESS-04]`).
    subagent_relief: std::sync::Mutex<Option<SubagentRelief>>,
    retry_config: std::sync::Mutex<SubagentRetryConfig>,
}

/// Installs mid-turn context relief on a freshly spawned child.
///
/// Defined here as a plain function type so this layer never names the concrete
/// gate or its settings (both live in `orchestration`, which wraps `cognition` —
/// `[INV-HARNESS-04]`). The composition root supplies the implementation; this is
/// the seam, not a dependency.
pub type SubagentRelief =
    Arc<dyn Fn(&Agent, Arc<nuo_persistence::session::SessionStore>) + Send + Sync>;

#[derive(Clone)]
struct SubagentAccountingContext {
    ledger: Arc<nuo_protocol::TokenSourceLedger>,
    session_id: Arc<std::sync::Mutex<Option<String>>>,
    round_counter: Arc<std::sync::Mutex<u64>>,
}

impl SubagentTool {
    /// `toolset` should be the parent agent's full capability set; `profile`
    /// declares what the spawned subagent may actually use (admission + variant
    /// pins + framing). The caller binds the role explicitly — `&SubAgentProfile::EXPLORE` for
    /// the `spawn_agent` tool.
    pub fn new(
        provider: Arc<dyn nuo_protocol::Provider>,
        toolset: nuo_protocol::ToolSet,
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
        provider: Arc<dyn nuo_protocol::Provider>,
        toolset: nuo_protocol::ToolSet,
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
        provider: Arc<dyn nuo_protocol::Provider>,
        toolset: nuo_protocol::ToolSet,
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
            parent_user_instructions: std::sync::Mutex::new(None),
            parent_project_instructions: std::sync::Mutex::new(None),
            parent_instruction_provider_kind: std::sync::Mutex::new(None),
            parent_workspace_security: std::sync::Mutex::new(None),
            parent_human_channel: std::sync::Mutex::new(None),
            registry: Arc::new(SubagentRegistry::default()),
            accounting: std::sync::Mutex::new(None),
            active_cancels: std::sync::Mutex::new(std::collections::HashMap::new()),
            parent_delegation: std::sync::Mutex::new(None),
            parent_execution_policy: std::sync::Mutex::new(None),
            workspace_root: std::sync::Mutex::new(None),
            parent_session: std::sync::Mutex::new(None),
            subagent_relief: std::sync::Mutex::new(None),
            retry_config: std::sync::Mutex::new(SubagentRetryConfig::default()),
        }
    }

    /// [`Self::named`] sharing an existing registry. The companion of
    /// [`Self::with_registry`]: a named dispatch tool whose children are
    /// reachable through the same harness reply path as its sibling.
    pub fn named_with_registry(
        provider: Arc<dyn nuo_protocol::Provider>,
        toolset: nuo_protocol::ToolSet,
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
            parent_user_instructions: std::sync::Mutex::new(None),
            parent_project_instructions: std::sync::Mutex::new(None),
            parent_instruction_provider_kind: std::sync::Mutex::new(None),
            parent_workspace_security: std::sync::Mutex::new(None),
            parent_human_channel: std::sync::Mutex::new(None),
            registry,
            accounting: std::sync::Mutex::new(None),
            active_cancels: std::sync::Mutex::new(std::collections::HashMap::new()),
            parent_delegation: std::sync::Mutex::new(None),
            parent_execution_policy: std::sync::Mutex::new(None),
            workspace_root: std::sync::Mutex::new(None),
            parent_session: std::sync::Mutex::new(None),
            subagent_relief: std::sync::Mutex::new(None),
            retry_config: std::sync::Mutex::new(SubagentRetryConfig::default()),
        }
    }

    /// Bind the agent preset delegation policy to enforce allowed subagent profiles.
    pub fn bind_delegation(&self, delegation: nuo_protocol::AgentRoleDelegation) {
        *self
            .parent_delegation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(delegation);
    }

    /// Bind the parent agent's execution policy to govern child depth bounds.
    pub fn bind_execution_policy(&self, policy: nuo_protocol::ExecutionPolicy) {
        *self
            .parent_execution_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(policy);
    }

    /// Pin the session's workspace root so spawned subagents resolve relative
    /// paths against the session's project rather than the server process's
    /// cwd. Called by the bootstrap right after construction;
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
        ledger: Arc<nuo_protocol::TokenSourceLedger>,
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
        handle: Arc<std::sync::Mutex<nuo_protocol::VariantSelection>>,
    ) {
        *self
            .parent_variants
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// Bind the parent session so each spawned subagent opens its own session row
    /// and carries a mid-turn projection gate.
    ///
    /// Without this a subagent had no session at all while running: its transcript
    /// only became durable *after* the round, when `admit_subagent_children`
    /// rebuilt the child from the parent message's embedded `children`. That left
    /// the running child with no projection target, so context relief — pre-round
    /// pruning, mid-turn projection, compaction — could not reach the one shape
    /// that most needs it: a single round running to completion with a large tool
    /// budget and no round boundary to relieve pressure at.
    ///
    /// `relief` is a factory rather than a gate because the gate lives above
    /// `cognition` and `[INV-HARNESS-04]` forbids this layer from naming it. The
    /// composition root decides what relief a child gets; this layer only decides
    /// *when* it lacks one.
    pub fn bind_session(
        &self,
        session: Arc<nuo_persistence::session::SessionStore>,
        relief: SubagentRelief,
    ) {
        *self
            .parent_session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session);
        *self
            .subagent_relief
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(relief);
    }

    /// Bind the parent agent's user-instruction handle so subagents resolve the
    /// same user source as their parent.
    pub fn bind_user_instructions(
        &self,
        handle: Arc<std::sync::RwLock<Vec<nuo_protocol::ScopedInstruction>>>,
    ) {
        *self
            .parent_user_instructions
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Bind the parent agent's project-instruction handle so subagents resolve
    /// the same scoped declarations as their parent.
    pub fn bind_project_instructions(
        &self,
        handle: Arc<std::sync::RwLock<Vec<nuo_protocol::ScopedInstruction>>>,
    ) {
        *self
            .parent_project_instructions
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Bind the parent's provider-type fact so subagents resolve provider
    /// selectors the same way.
    pub fn bind_instruction_provider_kind(&self, handle: Arc<std::sync::RwLock<Option<String>>>) {
        *self
            .parent_instruction_provider_kind
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Bind the session's channel accountant; spawned subagents
    /// inherit it (see the `run_subagent` wiring of
    /// `Agent::set_human_channel_accountant`).
    pub fn bind_human_channel(
        &self,
        handle: Arc<nuo_protocol::human_request::HumanChannelAccountant>,
    ) {
        *self
            .parent_human_channel
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    fn parent_human_channel(
        &self,
    ) -> Option<Arc<nuo_protocol::human_request::HumanChannelAccountant>> {
        self.parent_human_channel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn bind_workspace_security(
        &self,
        handle: Arc<std::sync::Mutex<nuo_protocol::WorkspaceSecuritySnapshot>>,
    ) {
        *self
            .parent_workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Snapshot the parent's current variant selection (empty when unbound).
    fn variant_snapshot(&self) -> nuo_protocol::VariantSelection {
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

    /// Whether invoking this tool spawns a nested sub-agent.
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

    async fn call_structured_with_events<'a>(
        &self,
        invocation: nuo_protocol::ToolInvocation<'a>,
        on_event: Box<dyn FnMut(nuo_protocol::SubagentEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(nuo_protocol::ToolStream) + Send + 'a),
    ) -> Result<nuo_protocol::ToolOutput, String> {
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
        Ok(nuo_protocol::ToolOutput::Subagent {
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
    messages: Vec<nuo_protocol::Message>,
    token_usage: nuo_protocol::TokenUsage,
    /// Final assistant content, mirrored for convenience so the parent doesn't
    /// have to scan `messages` for the last Assistant turn.
    final_content: String,
    /// Whether the subagent terminated abnormally (hit its turn cap,
    /// repeated-call guard, or a provider error). Drives the structured
    /// `failed` flag on the returned [`nuo_protocol::ToolOutput::Subagent`]
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

mod run;

#[cfg(test)]
mod tests;
