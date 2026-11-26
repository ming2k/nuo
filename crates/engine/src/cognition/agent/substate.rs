//! Partitioned `Agent` state (ADR-0186).
//!
//! The runtime keeps identity/provider/host on `Agent`; all mutable tool,
//! context, control, and queue state lives in these groups.
use super::*;

type WorkspaceTopologyCache = std::sync::Mutex<
    Option<(
        std::path::PathBuf,
        Option<std::time::SystemTime>,
        Option<String>,
    )>,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MeasuredPromptAnchor {
    pub reported_prompt_tokens: usize,
    pub estimated_at_report: usize,
}

pub(crate) struct ToolState {
    /// Global/Session tool pool for declarative tool resolution.
    pub(crate) pool: Arc<std::sync::RwLock<protocol::ToolPool>>,
    /// The full capability set: every tool keyed by capability, with all its
    /// variants. The single source of truth from which the model-visible
    /// [`resolved_tools`](Self::resolved_tools) view is derived for the active
    /// [`variant_selection`](Self::variant_selection).
    pub(crate) toolset: protocol::ToolSet,

    /// The active resolved view: exactly one variant per capability, for the
    /// current model's [`variant_selection`](Self::variant_selection). Both request
    /// assembly (`visible_tools` → `ModelRequest`) and dispatch (`find` by name)
    /// read this, so re-resolving it on a model/selection switch makes *both* the
    /// schema and the executed implementation track the chosen variant. Held
    /// behind a `RwLock` because it is swapped wholesale on selection change.
    pub(crate) resolved_tools: Arc<std::sync::RwLock<Vec<Arc<dyn Tool>>>>,
    /// Tools published by dynamically changing external sources. MCP and
    /// future connectors replace their own named snapshots through the core
    /// [`DynamicToolSink`] port; the agent owns synchronization, provenance,
    /// collision policy, advertisement, and dispatch.
    pub(crate) dynamic_tools: Arc<crate::dynamic_tools::DynamicToolRegistry>,
    /// Session-level disabled-tool mask. Names here are hidden from the model
    /// (their schemas are omitted from `ModelRequest`) and rejected at
    /// dispatch, but the tool stays installed so it can be re-enabled without
    /// rebuilding the agent. Toggled from the session modal via
    /// `set_tool_enabled` / `ToggleTool`.
    pub(crate) disabled_tools: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Hook-installed *temporary* disable mask ([`HookOutcome::ScopeTools`]).
    /// Not persisted: excluded from `disabled_tools_snapshot()` so it never
    /// reaches the session store. Auto-restored at the configured
    /// [`protocol::RestorePoint`]. See [`ScopedToolDisable`].
    pub(crate) scoped_disabled_tools: Arc<std::sync::Mutex<ScopedToolDisable>>,
    /// The unified two-bucket tool manager. The single authority for
    /// classification, per-turn schema (`loop_tools`), and dispatch lookup.
    /// Shares storage Arcs with the agent's own fields so both see the same
    /// live state. See [`crate::tool_manager`].
    pub(crate) tool_manager: crate::tool_manager::ToolManager,
    /// Session-scoped names activated by `tool_search`.
    pub(crate) tool_activation: Arc<crate::tool_activation::ToolActivation>,
    /// Harness-owned meta-tools installed after construction (today
    /// `tool_search`). They are not part of the role-scoped capability pool, so
    /// they must survive every `reresolve_tools` pass.
    pub(crate) harness_tools: Arc<std::sync::Mutex<Vec<Arc<dyn Tool>>>>,
    /// The immutable declaration/dispatch snapshot for the current request.
    pub(crate) current_snapshot:
        Arc<std::sync::RwLock<Option<Arc<crate::tool_snapshot::ToolSnapshot>>>>,
    /// This agent's **identity-side selection** of the pool (the agent half of
    /// the two-selector model): the capability scope it admits plus any variant
    /// pins it forces. The master agent is
    /// [`ToolSelection::unrestricted`](protocol::ToolSelection::unrestricted)
    /// — every capability, model-chosen variants. A scoped agent (or a future
    /// role-bound master) narrows this. Composed with the live model's
    /// selection by [`protocol::ToolSet::resolve_for`] every time the toolset
    /// is re-resolved: scope by intersection, variants by agent-over-model
    /// precedence, model capability limits applied hard.
    pub(crate) tools: std::sync::Mutex<protocol::ToolSelection>,
    /// Content-addressed per-message token weights (see
    /// [`protocol::MessageTokenWeights`]). Every estimate path consults
    /// this, so BPE tokenization cost collapses from O(total session bytes)
    /// per pass to O(new bytes since the last pass). Messages are immutable
    /// once written, so the cache never needs invalidation: identical bytes
    /// always yield identical weights. Held behind an `Arc` so off-executor
    /// estimate tasks (spawn_blocking) and the context-projection gates can
    /// share the same cache without borrowing the agent.
    pub(crate) token_weights: std::sync::Arc<protocol::MessageTokenWeights>,
    /// Content-addressed per-tool-spec BPE weights: a toolset is stable across
    /// turns, so its schema cost is tokenized once, not per estimate pass.
    pub(crate) tool_schema_weights: std::sync::Arc<protocol::ToolSchemaWeights>,
}

pub(crate) struct ContextState {
    /// Unified task list, the single source of truth for "what is left to
    /// do." Drives the sticky panel and persists across restarts. Shared
    /// with the concrete `todo` / `todo_update` tools installed by
    /// [`crate::tool_integration`].
    pub(crate) todos: Arc<std::sync::Mutex<tool::TodoList>>,
    /// Harness round counter, bumped at the start of every `execute_round`.
    /// Shared with the todo tools so they can stamp
    /// `updated_at_round` for the TUI stale detector.
    pub(crate) round_counter: Arc<std::sync::Mutex<u64>>,
    pub(crate) permissions: Arc<guardrails::permission_store::PermissionStore>,
    /// Live-updatable additional workspace roots shared with the execution
    /// environment so runtime grants (cross-workspace prompts, `/trust`,
    /// `/settings reload`) immediately propagate to both the agent gate and
    /// filesystem tools.
    pub(crate) additional_workspace_roots: protocol::SharedAdditionalRoots,
    /// Serializes concurrent cross-workspace admission prompts within a batch
    /// so parallel tool calls targeting the same external directory prompt the
    /// operator once rather than fanning out duplicate sheets.
    pub(crate) cross_workspace_gate: tokio::sync::Mutex<()>,
    /// Cached workspace member topology, keyed by the root manifest's mtime.
    /// The topology is prompt-stable; re-reading every member manifest on
    /// every model request is pure hot-path I/O. One `stat` per request is
    /// enough to notice a manifest edit.
    pub(crate) workspace_topology_cache: WorkspaceTopologyCache,
    /// The context plan measured for the most recent request, plus that
    /// request's token shape once it was sent. `/context` renders this exact
    /// value, so the operator table cannot disagree with the prompt the model
    /// received (`[INV-CTXPLAN-01]`, ADR-0217).
    pub(crate) last_measurement: std::sync::Mutex<Option<protocol::ContextMeasurement>>,
    /// Workspace authority is orthogonal to interaction posture. Shared with
    /// spawned subagents so delegation cannot silently widen the parent's grant.
    pub(crate) workspace_security: Arc<std::sync::Mutex<protocol::WorkspaceSecuritySnapshot>>,
    /// Session-scoped workspace confinement handle.
    pub(crate) confinement: protocol::SharedConfinement,
    /// Content-attested project instructions from the Rules asset domain.
    /// Replaced live when `/trust` or `/untrust` changes admission.
    pub(crate) project_rules: Arc<std::sync::RwLock<String>>,
    /// Nested `AGENTS.md` files already injected into this session. A read
    /// below a directory loads its rule file once; later reads in the same
    /// subtree do not repeat the prompt cost.
    pub(crate) loaded_project_rules:
        Arc<std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>>,
    /// Parked interactive-input requests (L3.5 β). Mirrors `ask_user`.
    pub(crate) skills_registry: skills::SkillRegistry,
    pub(crate) thread_id: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) accounting_actor_id: std::sync::Mutex<String>,
    /// Context-pressure threshold (in tokens) above which the harness asks the
    /// [`ContextProjectionGate`] to project the model-visible window between
    /// ReAct turns. `0` disables mid-round projection. Derived from the active
    /// model's context window.
    pub(crate) context_prune_threshold_tokens: Arc<std::sync::Mutex<usize>>,
    /// Request-body ceiling (in bytes) above which the harness removes
    /// attachments from the request projection before sending. `0` disables
    /// the payload gate.
    ///
    /// The token threshold above answers "will the model choke?"; this one
    /// answers "will the transport refuse to carry it?". They are independent
    /// limits, and a session inside a million-token window can still be a
    /// 55 MB body.
    pub(crate) request_body_limit_bytes: Arc<std::sync::Mutex<usize>>,
    /// Learned per-route request-body ceiling: the `(route fingerprint, size)`
    /// a provider has actually refused.
    ///
    /// The configured ceiling is a conservative default because no vendor
    /// publishes a portable number. A refusal is better evidence than any
    /// default: once a route answers `413` for a body of N bytes, every later
    /// request on that route is projected below N instead of re-uploading the
    /// same failing payload. Keyed by route so a model/endpoint switch clears it
    /// by construction, exactly like [`Self::images_suppressed`].
    pub(crate) payload_ceiling: Arc<std::sync::RwLock<Option<(String, usize)>>>,
    /// Provider-reported prompt token anchor from the most recent completed turn
    /// paired with the local estimate at that turn, so compaction triggers can
    /// account for unmeasured system/tool/image tokens plus incremental delta.
    pub(crate) last_measured_prompt: std::sync::Mutex<Option<MeasuredPromptAnchor>>,
    /// Optional mid-turn model-context projection gate.
    pub(crate) context_projection_gate:
        Arc<std::sync::Mutex<Option<Arc<dyn ContextProjectionGate>>>>,
    /// Learned per-route image-input suppression: the
    /// [`RouteFingerprint`] of the route on
    /// which a provider rejected image input, once the harness has seen that
    /// rejection.
    ///
    /// History keeps its images forever (the transcript is
    /// append-only), so a session that pasted an image while a multimodal model
    /// was active would otherwise fail *every* subsequent request on a
    /// text-only route, with `/retry` re-sending the identical doomed request.
    /// The cure is a projection fact, not a history rewrite: remember the route,
    /// and stop *sending* images on it. Stored as a fingerprint rather than a
    /// plain flag so switching models clears it by construction.
    pub(crate) images_suppressed: Arc<std::sync::RwLock<Option<String>>>,
    /// Opt-in hard-stop budget (ADR-0018): abort a round after this many ReAct
    /// turns. Seeded from `Config::master.hard_stop_turns` (default `0`
    /// = uncapped, matching ADR-0009) and mutated at runtime via
    /// `set_hard_stop_turns`. This is the sole execution cap; session review
    /// is on-demand (`/review`) and never aborts a round.
    pub(crate) hard_stop_turns: Arc<std::sync::Mutex<usize>>,
    /// Advanced pre-dispatch trajectory loop guard configuration.
    /// Seeded from `[agent.trajectory_guard]` in `config.toml`.
    pub(crate) trajectory_guard_config: Arc<std::sync::RwLock<tool::TrajectoryGuardConfig>>,
    /// The ReAct turn currently executing within the round, for journal
    /// coordinates. Advanced by the round loop.
    pub(crate) current_turn: std::sync::atomic::AtomicU64,
    /// Request-scoped projector. The agent owns its lifecycle and supplies live
    /// state snapshots; the assembler owns the pure window-to-request transform.
    pub(crate) model_request_assembler: crate::cognition::model_request::ModelRequestAssembler,
    /// Per-model tool-variant selection (the **override** axis) for the
    /// *current* model: a `capability → variant_id` map. Seeded from
    /// `[tool_variants."<model-id>"]` config via
    /// [`Agent::set_variant_selection`] and re-seeded on model switch so the
    /// resolved toolset always tracks the live model. Held behind an `Arc` so a
    /// spawned subagent — which is an agent on the *same* model — can inherit
    /// the same overrides by sharing this handle (see
    /// [`Agent::variant_selection_handle`]); the agent decides scope, the
    /// model decides variant.
    pub(crate) variant_selection: Arc<std::sync::Mutex<protocol::VariantSelection>>,
    /// Token-source accounting: running tally of how many tokens each
    /// provider+model reported authoritatively (upstream `usage`) vs. how many
    /// were filled in by the local estimator. Shared with the TUI so the
    /// token-source report modal renders live. `None` for subagents/tests that
    /// don't surface the report.
    pub(crate) token_ledger: std::sync::Mutex<Option<Arc<protocol::TokenSourceLedger>>>,
    /// Repository-authored scoped instructions (ADR-0164). Shared by handle
    /// with subagents so a child resolves against the same declaration set.
    pub(crate) project_instructions: Arc<std::sync::RwLock<Vec<protocol::ScopedInstruction>>>,
    /// User-authored scoped instructions (ADR-0165). Shared by handle with
    /// subagents; the user source is cross-project route tuning.
    pub(crate) user_instructions: Arc<std::sync::RwLock<Vec<protocol::ScopedInstruction>>>,
    /// The declared answer language for this session (`agent.language`,
    /// ADR-0222). Shared by handle with subagents: a child answers in the same
    /// language as the parent, because the human reading the child's report is
    /// the same human.
    pub(crate) response_language: Arc<std::sync::RwLock<Option<protocol::ResponseLanguage>>>,
    /// Provider type behind the active connection, resolved by the server at
    /// setup and on every provider switch. Kept off the per-request path:
    /// prompt composition must not read the connections store.
    pub(crate) instruction_provider_kind: Arc<std::sync::RwLock<Option<String>>>,
    /// Active staffing role for this agent (e.g. "developer", "philosophist").
    pub(crate) active_role: std::sync::RwLock<Option<String>>,
}

pub(crate) struct ControlState {
    /// Execution policy governing this agent's runtime posture, delegation limits, and depth.
    pub(crate) execution_policy: std::sync::RwLock<protocol::ExecutionPolicy>,
    /// Unified interaction controller governing human posture, stdin policy,
    /// and autonomous fallback behaviors.
    pub(crate) interaction: Arc<guardrails::interaction::InteractionController>,
    /// The single owner of parked human-decision oneshots
    /// (permission / ask_user / interactive input).
    pub(crate) human_broker: crate::human_broker::HumanRequestBroker,

    /// Command-aware safety policy for `bash`. This sits above the ordinary
    /// permission broker so broad approvals such as `bash *` cannot silently
    /// authorize destructive commands like `git reset --hard`.
    pub(crate) bash_policy: std::sync::RwLock<guardrails::bash_policy::BashPolicy>,
    /// Lifecycle event hooks (ADR-0025). Installed once at startup from the
    /// `[hooks]` config by the CLI; empty by default (subagents, tests). Read
    /// at the PreToolUse / PostToolUse / Stop insertion points. Held as a
    /// swappable `Arc` behind a `Mutex` so [`Agent::set_hooks`] can replace the
    /// whole registry without the insertion points holding the lock across the
    /// async `fire` — they clone the `Arc` and drop the guard first.
    pub(crate) hooks: crate::runtime::hook_runner::HookRunner,
    /// Optional mid-round save point invoked at every ReAct-turn boundary
    /// (ADR-0048). The embedding (orchestration) installs a closure that
    /// durably appends the round's new messages to the session log so a crash
    /// after a side-effecting tool call leaves the transcript in sync with the
    /// filesystem instead of rewinding to the previous turn. `None` for
    /// subagents, the review diagnostic, and tests — they have no session of
    /// their own to persist, so the turn boundary is a plain no-op there.
    pub(crate) turn_persist: std::sync::Mutex<Option<TurnPersistFn>>,
    /// Workspace effect journal port installed by the server (ADR-0125). `None`
    /// disables journaling entirely — the honest posture for subagents, tests, and
    /// any embedding with no durable journal, because a journal that cannot be
    /// written must not silently pretend a mutation was reversible.
    ///
    /// Installed rather than required so the harness stays usable without
    /// persistence, but when present the bracket is unconditional: dispatch calls
    /// it around every tool invocation, and a capture failure vetoes the call.
    pub(crate) workspace_journal:
        std::sync::RwLock<Option<std::sync::Arc<dyn crate::runtime::WorkspaceJournalPort>>>,
    /// Request-projection archive sink installed by the session driver
    /// fired once per freshly assembled request with its forensic
    /// record. `None` for subagents, the review diagnostic, and tests.
    pub(crate) request_projection_persist: std::sync::Mutex<Option<RequestProjectionFn>>,
    /// Optional agent-scoped reasoning-effort cap or override (e.g.
    /// capping read-only discovery subagents at `Low`). `None` inherits the
    /// provider's configured effort.
    pub(crate) reasoning_effort: std::sync::Mutex<Option<protocol::ReasoningEffortControl>>,
}

pub(crate) struct QueueState {
    /// Inbound steering inbox — the down-direction of full-duplex (ADR-0029).
    /// `None` for agents that were never given a handle (the top-level agent
    /// driven directly by the harness, legacy tests); lazily created by
    /// [`Agent::install_inbox`], which a spawned subagent's dispatcher
    /// (`SubagentTool`) calls so the parent can steer it mid-round. The driver loop
    /// `take`s the receiver at round entry and drains it at every ReAct-turn
    /// boundary (see [`Agent::drain_inbox`]). Carries only the
    /// "new-input / control" class ([`AgentOp`]); the request/reply class
    /// (permission / ask_user) bypasses this queue and resolves the parked
    /// oneshot directly via `reply_permission` / `reply_user_question`, since a
    /// reply must unblock a tool parked mid-round and cannot wait for the loop.
    pub(crate) inbox_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<AgentOp>>>,
    pub(crate) inbox_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<AgentOp>>>,
    /// Inbound steering and follow-up queues for the currently running master/side round.
    /// This is deliberately separate from the subagent `AgentOp` inbox: submit,
    /// cancel, and boundary admission all take this one mutex, which gives the
    /// UI an exact answer in the cancellation-vs-admission race. `None` means
    /// the round is not accepting queued messages.
    pub(crate) session_queues: std::sync::Mutex<Option<SessionQueues>>,
    pub(crate) steering_mode: std::sync::RwLock<protocol::QueueMode>,
    pub(crate) follow_up_mode: std::sync::RwLock<protocol::QueueMode>,
    /// Cumulative milliseconds the current round has spent parked on a human
    /// decision (permission prompt or `ask_user`). Reset to 0 at the start of
    /// each user round and added to at every permission/ask_user `await`. Read
    /// at the round exit gate to derive "active" generation time
    /// (`duration_ms - paused_ms`) for an honest tokens/sec that excludes the
    /// human-thinking pause. `AtomicU64` because the parking sites
    /// (`execute_tool`, the bash policy path, `execute_ask_user`) take `&self`,
    /// not `&mut state`.
    pub(crate) round_paused_ms: std::sync::atomic::AtomicU64,
}
