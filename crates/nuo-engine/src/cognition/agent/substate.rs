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

pub(crate) struct ToolState {
    /// Global/Session tool pool for declarative tool resolution.
    pub(crate) pool: Arc<std::sync::RwLock<nuo_protocol::ToolPool>>,
    /// The full capability set: every tool keyed by capability, with all its
    /// variants. The single source of truth from which the model-visible
    /// [`resolved_tools`](Self::resolved_tools) view is derived for the active
    /// [`variant_selection`](Self::variant_selection).
    pub(crate) toolset: nuo_protocol::ToolSet,

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
    /// [`nuo_protocol::RestorePoint`]. See [`ScopedToolDisable`].
    pub(crate) scoped_disabled_tools: Arc<std::sync::Mutex<ScopedToolDisable>>,
    /// The unified two-bucket tool manager. The single authority for
    /// classification, per-turn schema (`loop_tools`), and dispatch lookup.
    /// Shares storage Arcs with the agent's own fields so both see the same
    /// live state. See [`crate::tool_manager`].
    pub(crate) tool_manager: crate::tool_manager::ToolManager,
    /// This agent's **identity-side selection** of the pool (the agent half of
    /// the two-selector model): the capability scope it admits plus any variant
    /// pins it forces. The master agent is
    /// [`ToolSelection::unrestricted`](nuo_protocol::ToolSelection::unrestricted)
    /// — every capability, model-chosen variants. A scoped agent (or a future
    /// role-bound master) narrows this. Composed with the live model's
    /// selection by [`nuo_protocol::ToolSet::resolve_for`] every time the toolset
    /// is re-resolved: scope by intersection, variants by agent-over-model
    /// precedence, model capability limits applied hard.
    pub(crate) tools: std::sync::Mutex<nuo_protocol::ToolSelection>,
    /// Content-addressed per-message token weights (see
    /// [`nuo_protocol::MessageTokenWeights`]). Every estimate path consults
    /// this, so BPE tokenization cost collapses from O(total session bytes)
    /// per pass to O(new bytes since the last pass). Messages are immutable
    /// once written, so the cache never needs invalidation: identical bytes
    /// always yield identical weights. Held behind an `Arc` so off-executor
    /// estimate tasks (spawn_blocking) and the context-projection gates can
    /// share the same cache without borrowing the agent.
    pub(crate) token_weights: std::sync::Arc<nuo_protocol::MessageTokenWeights>,
    /// Content-addressed per-tool-spec BPE weights: a toolset is stable across
    /// turns, so its schema cost is tokenized once, not per estimate pass.
    pub(crate) tool_schema_weights: std::sync::Arc<nuo_protocol::ToolSchemaWeights>,
}

pub(crate) struct ContextState {
    /// Unified task list, the single source of truth for "what is left to
    /// do." Drives the sticky panel and persists across restarts. Shared
    /// with the concrete `todo` / `todo_update` tools installed by
    /// [`crate::tool_integration`].
    pub(crate) todos: Arc<std::sync::Mutex<nuo_tool::TodoList>>,
    /// Harness round counter, bumped at the start of every `execute_round`.
    /// Shared with the todo tools so they can stamp
    /// `updated_at_round` for the TUI stale detector.
    pub(crate) round_counter: Arc<std::sync::Mutex<u64>>,
    pub(crate) permissions: Arc<nuo_guardrails::permission_store::PermissionStore>,
    /// Canonicalized additional workspace roots, set once by the
    /// assembling bootstrap. Kept as an owned copy so system-prompt assembly
    /// never re-reads the project config mid-session.
    pub(crate) additional_workspace_roots: Vec<std::path::PathBuf>,
    /// Cached workspace member topology, keyed by the root manifest's mtime.
    /// The topology is prompt-stable; re-reading every member manifest on
    /// every model request is pure hot-path I/O. One `stat` per request is
    /// enough to notice a manifest edit.
    pub(crate) workspace_topology_cache: WorkspaceTopologyCache,
    /// Workspace authority is orthogonal to interaction posture. Shared with
    /// spawned subagents so delegation cannot silently widen the parent's grant.
    pub(crate) workspace_security: Arc<std::sync::Mutex<nuo_protocol::WorkspaceSecuritySnapshot>>,
    /// Session-scoped workspace confinement handle.
    pub(crate) confinement: nuo_protocol::SharedConfinement,
    /// Content-attested project instructions from the Rules asset domain.
    /// Replaced live when `/trust` or `/untrust` changes admission.
    pub(crate) project_rules: Arc<std::sync::RwLock<String>>,
    /// Parked interactive-input requests (L3.5 β). Mirrors `ask_user`.
    pub(crate) skills_registry: skills::SkillRegistry,
    pub(crate) thread_id: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) accounting_actor_id: std::sync::Mutex<String>,
    /// Context-pressure threshold (in tokens) above which the harness asks the
    /// [`ContextProjectionGate`] to project the model-visible window between
    /// ReAct turns. `0` disables mid-round projection. Derived from the active
    /// model's context window.
    pub(crate) context_prune_threshold_tokens: Arc<std::sync::Mutex<usize>>,
    /// Tool-output retention budget for the per-request history projection.
    /// Seeded from the context policy and re-seeded on every model switch so a
    /// long ReAct round cannot accumulate every tool byte it ever produced.
    pub(crate) tool_output_projection: Arc<std::sync::Mutex<ToolOutputProjectionBudget>>,
    /// Optional mid-turn model-context projection gate.
    pub(crate) context_projection_gate:
        Arc<std::sync::Mutex<Option<Arc<dyn ContextProjectionGate>>>>,
    /// Set when a context projection replaced the live window; consumed by the
    /// next durable commit to pick [`CommitBasis::Reprojected`] instead of the
    /// O(delta) append path. Conservative by construction: a spurious `true`
    /// costs one comparison, a missing `true` could persist a stale prefix.
    pub(crate) history_reprojected: std::sync::atomic::AtomicBool,
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
    pub(crate) trajectory_guard_config: Arc<std::sync::RwLock<nuo_tool::TrajectoryGuardConfig>>,
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
    pub(crate) variant_selection: Arc<std::sync::Mutex<nuo_protocol::VariantSelection>>,
    /// Token-source accounting: running tally of how many tokens each
    /// provider+model reported authoritatively (upstream `usage`) vs. how many
    /// were filled in by the local estimator. Shared with the TUI so the
    /// token-source report modal renders live. `None` for subagents/tests that
    /// don't surface the report.
    pub(crate) token_ledger: std::sync::Mutex<Option<Arc<nuo_protocol::TokenSourceLedger>>>,
    /// Repository-authored scoped instructions (ADR-0164). Shared by handle
    /// with subagents so a child resolves against the same declaration set.
    pub(crate) project_instructions: Arc<std::sync::RwLock<Vec<nuo_protocol::ScopedInstruction>>>,
    /// User-authored scoped instructions (ADR-0165). Shared by handle with
    /// subagents; the user source is cross-project route tuning.
    pub(crate) user_instructions: Arc<std::sync::RwLock<Vec<nuo_protocol::ScopedInstruction>>>,
    /// Provider type behind the active connection, resolved by the server at
    /// setup and on every provider switch. Kept off the per-request path:
    /// prompt composition must not read the connections store.
    pub(crate) instruction_provider_kind: Arc<std::sync::RwLock<Option<String>>>,
    /// Active staffing role for this agent (e.g. "developer", "philosophist").
    pub(crate) active_role: std::sync::RwLock<Option<String>>,
}

pub(crate) struct ControlState {
    /// Execution policy governing this agent's runtime posture, delegation limits, and depth.
    pub(crate) execution_policy: std::sync::RwLock<nuo_protocol::ExecutionPolicy>,
    /// Unified interaction controller governing human posture, stdin policy,
    /// and autonomous fallback behaviors.
    pub(crate) interaction: Arc<nuo_guardrails::interaction::InteractionController>,
    /// The single owner of parked human-decision oneshots
    /// (permission / ask_user / interactive input).
    pub(crate) human_broker: crate::human_broker::HumanRequestBroker,

    /// Command-aware safety policy for `bash`. This sits above the ordinary
    /// permission broker so broad approvals such as `bash *` cannot silently
    /// authorize destructive commands like `git reset --hard`.
    pub(crate) bash_policy: std::sync::RwLock<nuo_guardrails::bash_policy::BashPolicy>,
    /// Lifecycle event hooks (ADR-0025). Installed once at startup from the
    /// `[hooks]` config by the CLI; empty by default (subagents, tests). Read
    /// at the PreToolUse / PostToolUse / Stop insertion points. Held as a
    /// swappable `Arc` behind a `Mutex` so [`Agent::set_hooks`] can replace the
    /// whole registry without the insertion points holding the lock across the
    /// async `fire` — they clone the `Arc` and drop the guard first.
    pub(crate) hooks: crate::runtime::hook_runner::HookRunner,
    /// Title-established observer installed by the session driver: fired
    /// once when the background titler durably persists a session's first
    /// title, so the runtime can push a fresh sessions overview (and the
    /// monitor tap republishes the row) to every attached client. `None`
    /// keeps titling silent (subagents, tests).
    pub(crate) title_established: std::sync::Mutex<Option<TitleEstablishedFn>>,
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
    /// Atomic extensions bound to this agent instance.
    pub(crate) extensions: Arc<std::sync::RwLock<Vec<Arc<dyn nuo_protocol::Extension>>>>,
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
    pub(crate) steering_mode: std::sync::RwLock<nuo_protocol::QueueMode>,
    pub(crate) follow_up_mode: std::sync::RwLock<nuo_protocol::QueueMode>,
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
