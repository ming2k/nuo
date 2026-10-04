//! The [`Agent`] orchestration type and its supporting machinery.
//!
//! The type definition, builder, subagent handle, queue plumbing, the free
//! helper functions shared across the split, and the embedded tests. The
//! `impl Agent` blocks are split by concern into sibling modules:
//! `state` (configuration/identity), `steering` (rounds/queues/interrupts),
//! `tools_admin` (permissions/catalog), `rounds` (streaming loop), and
//! `execution` (tool tail + hooks).

use super::*;
use nuo_wire::human_request::{
    AutonomousFallbackPolicy, HumanChannelPosture, HumanReply, HumanRequestKind,
};

use futures::future::BoxFuture;

/// Role-reanchoring note appended to a successful subagent's tool-result text in
/// the master's transcript. Counters "role bleed": after a run of read-only
/// delegations the model may over-generalize the subagent's read-only framing onto
/// the master itself. The note pins the boundary explicitly and
/// unconditionally — it does not rely on a `[hooks]` entry, so the guarantee is
/// structural.
const SUBAGENT_REANCHOR_OK: &str = "\
[system] The read-only / toolset-scoped framing above applies to the subagent only. \
You (the parent agent) retain your full toolset — including write and edit tools \
and the shell — across this delegation. Perform any edits or writes yourself; the \
subagent cannot.";

/// Same role-reanchoring for a *failed* subagent. Reaffirms the boundary and nudges
/// the parent toward acting directly rather than re-delegating a failing
/// sub-task.
const SUBAGENT_REANCHOR_FAILED: &str = "\
[system] That subagent could not complete its sub-task. Its read-only / toolset-scoped \
framing does not transfer to you: you (the parent agent) retain your full toolset \
— including write and edit tools and the shell. Act directly on the findings above, \
or re-delegate with a narrower scope.";

/// Same role-reanchoring for an *interrupted* subagent: stopped by the user, not
/// failed. The partial findings above are real work; the parent may continue
/// them directly or re-delegate, and stays accountable for the outcome.
const SUBAGENT_REANCHOR_INTERRUPTED: &str = "\
[system] That subagent was interrupted mid-task (stopped by the user before it finished). \
Its partial findings above are real work, and its read-only / toolset-scoped framing \
does not transfer to you: you (the parent agent) retain your full toolset — \
including write and edit tools and the shell. Continue the work directly from where \
it stopped, or re-delegate with a narrower scope.";

/// Build the model-visible text for a subagent tool result: the subagent's summary
/// wrapped in the standard `[<tool> result]:` header, followed by a
/// deterministic role-reanchoring note (`SUBAGENT_REANCHOR_OK` on success,
/// `SUBAGENT_REANCHOR_FAILED` on `failed`, `SUBAGENT_REANCHOR_INTERRUPTED` on
/// `interrupted`). This is the single choke point where
/// a subagent's read-only framing enters the parent's transcript, so the
/// re-anchor is applied here unconditionally — it cannot be forgotten by a
/// missing `[hooks]` config. Extracted from [`Agent::record_tool_result`] so the
/// contract (the anchor is present, and its tone tracks the failure flag) is
/// unit-testable without a full `Agent` fixture.
pub(crate) fn subagent_result_text(
    name: &str,
    summary: &str,
    failed: bool,
    interrupted: bool,
) -> String {
    let reanchor = if interrupted {
        SUBAGENT_REANCHOR_INTERRUPTED
    } else if failed {
        SUBAGENT_REANCHOR_FAILED
    } else {
        SUBAGENT_REANCHOR_OK
    };
    format!("[{name} result]:\n{summary}\n\n{reanchor}")
}

/// In-memory only mask of tools a hook has temporarily disabled via a
/// [`nuo_wire::HookOutcome::ScopeTools`] outcome, partitioned by the
/// [`nuo_wire::RestorePoint`] at which each should come back.
///
/// Deliberately **separate** from the session-level, persisted
/// [`Agent::disabled_tools`]: scoped disables never reach the session store
/// (the snapshot path only clones the persisted mask), so they never survive a
/// restart and never collide with a user's manual `/tools` toggles. Each bucket
/// is a reference count (`HashMap<String, u32>`) rather than a flat set so two
/// hooks disabling the same tool at different restore points don't fight: the
/// earlier restore only decrements, the tool stays hidden until its last
/// refcount reaches zero.
#[derive(Default, Clone)]
pub(crate) struct ScopedToolDisable {
    round_end: HashMap<String, u32>,
    turn_end: HashMap<String, u32>,
}

impl ScopedToolDisable {
    /// Record a hook-fired disable for `tool` at `restore`. Increments the
    /// refcount so nested disables compose.
    fn disable(&mut self, tool: &str, restore: nuo_wire::RestorePoint) {
        let bucket = match restore {
            nuo_wire::RestorePoint::TurnEnd => &mut self.turn_end,
            nuo_wire::RestorePoint::RoundEnd => &mut self.round_end,
        };
        *bucket.entry(tool.to_string()).or_insert(0) += 1;
    }

    /// Whether `tool` is currently scoped-disabled (hidden from the model and
    /// rejected at dispatch) under any restore point.
    pub(crate) fn contains(&self, tool: &str) -> bool {
        self.round_end.contains_key(tool) || self.turn_end.contains_key(tool)
    }

    /// Drop every `TurnEnd` disable at the ReAct-turn boundary. `RoundEnd`
    /// disables survive until the user round ends.
    fn restore_turn_end(&mut self) {
        self.turn_end.clear();
    }

    /// Drop every disable (both buckets). Called at user-round end so the
    /// toolset is fresh for the next user request.
    fn restore_round_end(&mut self) {
        self.round_end.clear();
        self.turn_end.clear();
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.round_end.is_empty() && self.turn_end.is_empty()
    }
}

/// Mid-turn save-point closure installed by orchestration (ADR-0048).
///
/// Invoked at each ReAct-turn boundary with the current full round history.
/// The implementation diffs against its own durable baseline and appends only
/// the new tail to the session event log (see `SessionStore::append_turn`).
/// Errors are surfaced back to the ReAct loop, which treats a persist failure
/// as a round-ending error (better to stop than to keep mutating state that may
/// not be recoverable).
pub(crate) type TurnPersistFn =
    Arc<dyn Fn(&[Message]) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Invoked once per freshly assembled model request (ADR-0218) with its
/// request-projection record. The runtime installs a closure that enqueues the
/// record to the session's forensic archive. It is synchronous and infallible
/// by design: forensic persistence must never block or fail request dispatch.
/// It is `None` on subagents, the review diagnostic, and tests.
pub(crate) type RequestProjectionFn = Arc<dyn Fn(nuo_wire::RequestProjection) + Send + Sync>;

/// Title-established observer fired by the background session titler
/// (ADR-0022).
///
/// The titler lives in `nuo-harness` and cannot reach the runtime's response
/// channel, so the driver installs a closure that pushes a fresh
/// `SessionsOverview` snapshot (and thereby republishes the monitor row, so
/// every attached client's picker title updates live). Fired at most once per
/// session — a non-`NULL` title is terminal (ADR-0186). Errors and absent
/// observers are non-fatal: the title is already durably persisted when this
/// fires, so the notification is pure presentation.
pub type TitleEstablishedFn = Arc<dyn Fn(&str) -> BoxFuture<'static, ()> + Send + Sync>;

pub use nuo_wire::RequestTokenEstimate;

// `AgentIdentity` now lives in `nuo-wire` (`identity.rs`) as pure domain
// vocabulary, alongside the role profiles. It is re-exported by name at the
// crate root below and via `pub use nuo_wire::*`, so all existing
// `nuo_harness::AgentIdentity` / `crate::AgentIdentity` references keep
// resolving unchanged.

/// Parked oneshots for in-flight interactive-input requests (L3.5 β): a
/// `bash` command classified interactive blocks here until the operator's
/// [`InputReply`] arrives (or `None` on cancel/turn-end).
pub struct Agent {
    pub host: crate::host::KernelHost,
    pub provider: Arc<dyn Provider>,
    /// Execution policy governing this agent's runtime posture, delegation limits, and depth (ADR-0183).
    execution_policy: std::sync::RwLock<nuo_wire::ExecutionPolicy>,
    /// Global/Session tool pool for declarative tool resolution.
    pool: Arc<std::sync::RwLock<nuo_wire::ToolPool>>,
    /// The full capability set: every tool keyed by capability, with all its
    /// variants. The single source of truth from which the model-visible
    /// [`resolved_tools`](Self::resolved_tools) view is derived for the active
    /// [`variant_selection`](Self::variant_selection).
    pub(crate) toolset: nuo_wire::ToolSet,

    /// The active resolved view: exactly one variant per capability, for the
    /// current model's [`variant_selection`](Self::variant_selection). Both request
    /// assembly (`visible_tools` → `ModelRequest`) and dispatch (`find` by name)
    /// read this, so re-resolving it on a model/selection switch makes *both* the
    /// schema and the executed implementation track the chosen variant. Held
    /// behind a `RwLock` because it is swapped wholesale on selection change.
    resolved_tools: Arc<std::sync::RwLock<Vec<Arc<dyn Tool>>>>,
    /// Tools published by dynamically changing external sources. MCP and
    /// future connectors replace their own named snapshots through the core
    /// [`DynamicToolSink`] port; the agent owns synchronization, provenance,
    /// collision policy, advertisement, and dispatch.
    dynamic_tools: Arc<crate::dynamic_tools::DynamicToolRegistry>,
    /// Session-level disabled-tool mask. Names here are hidden from the model
    /// (their schemas are omitted from `ModelRequest`) and rejected at
    /// dispatch, but the tool stays installed so it can be re-enabled without
    /// rebuilding the agent. Toggled from the session modal via
    /// `set_tool_enabled` / `ToggleTool`.
    disabled_tools: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Hook-installed *temporary* disable mask ([`HookOutcome::ScopeTools`]).
    /// Not persisted: excluded from `disabled_tools_snapshot()` so it never
    /// reaches the session store. Auto-restored at the configured
    /// [`nuo_wire::RestorePoint`]. See [`ScopedToolDisable`].
    scoped_disabled_tools: Arc<std::sync::Mutex<ScopedToolDisable>>,
    /// The unified two-bucket tool manager. The single authority for
    /// classification, per-turn schema (`loop_tools`), and dispatch lookup.
    /// Shares storage Arcs with the agent's own fields so both see the same
    /// live state. See [`crate::tool_manager`].
    tool_manager: crate::tool_manager::ToolManager,
    /// Unified task list, the single source of truth for "what is left to
    /// do." Drives the sticky panel and persists across restarts. Shared
    /// with the concrete `todo` / `todo_update` tools installed by
    /// [`crate::tool_integration`].
    todos: Arc<std::sync::Mutex<nuo_wire::TodoList>>,
    /// Harness round counter, bumped at the start of every `execute_round`.
    /// Shared with the todo tools so they can stamp
    /// `updated_at_round` for the TUI stale detector.
    round_counter: Arc<std::sync::Mutex<u64>>,
    permissions: Arc<crate::permission_store::PermissionStore>,
    /// Canonicalized additional workspace roots (ADR-0142), set once by the
    /// assembling bootstrap. Kept as an owned copy so system-prompt assembly
    /// never re-reads the project config mid-session.
    additional_workspace_roots: Vec<std::path::PathBuf>,
    /// Workspace authority is orthogonal to interaction posture. Shared with
    /// spawned subagents so delegation cannot silently widen the parent's grant.
    workspace_security: Arc<std::sync::Mutex<nuo_wire::WorkspaceSecuritySnapshot>>,
    /// Session-scoped workspace confinement handle.
    confinement: nuo_wire::SharedConfinement,
    /// Content-attested project instructions from the Rules asset domain.
    /// Replaced live when `/trust` or `/untrust` changes admission.
    project_rules: Arc<std::sync::RwLock<String>>,
    /// Parked interactive-input requests (L3.5 β). Mirrors `ask_user`.
    pub(crate) skills_registry: skills::SkillRegistry,
    thread_id: Arc<std::sync::Mutex<Option<String>>>,
    accounting_actor_id: std::sync::Mutex<String>,
    /// Context-pressure threshold (in tokens) above which the harness asks the
    /// [`ContextProjectionGate`] to project the model-visible window between
    /// ReAct turns. `0` disables mid-round projection. Derived from the active
    /// model's context window.
    context_prune_threshold_tokens: Arc<std::sync::Mutex<usize>>,
    /// Optional mid-turn model-context projection gate.
    context_projection_gate: Arc<std::sync::Mutex<Option<Arc<dyn ContextProjectionGate>>>>,
    /// Learned per-route image-input suppression (ADR-0230): the
    /// [`RouteFingerprint`] of the route on
    /// which a provider rejected image input, once the harness has seen that
    /// rejection.
    ///
    /// History keeps its images forever (ADR-0186 — the transcript is
    /// append-only), so a session that pasted an image while a multimodal model
    /// was active would otherwise fail *every* subsequent request on a
    /// text-only route, with `/retry` re-sending the identical doomed request.
    /// The cure is a projection fact, not a history rewrite: remember the route,
    /// and stop *sending* images on it. Stored as a fingerprint rather than a
    /// plain flag so switching models clears it by construction.
    images_suppressed: Arc<std::sync::RwLock<Option<String>>>,
    /// Opt-in hard-stop budget (ADR-0018): abort a round after this many ReAct
    /// turns. Seeded from `Config::master.hard_stop_turns` (default `0`
    /// = uncapped, matching ADR-0009) and mutated at runtime via
    /// `set_hard_stop_turns`. This is the sole execution cap; session review
    /// is on-demand (`/review`) and never aborts a round.
    hard_stop_turns: Arc<std::sync::Mutex<usize>>,
    /// Advanced pre-dispatch trajectory loop guard configuration (ADR-0247).
    /// Seeded from `[agent.trajectory_guard]` in `config.toml`.
    trajectory_guard_config: Arc<std::sync::RwLock<nuo_wire::TrajectoryGuardConfig>>,
    /// Unified interaction controller governing human posture, stdin policy,
    /// and autonomous fallback behaviors.
    pub(crate) interaction: Arc<crate::interaction::InteractionController>,
    /// ADR-0141: the single owner of parked human-decision oneshots
    /// (permission / ask_user / interactive input).
    human_broker: crate::human_broker::HumanRequestBroker,

    /// Command-aware safety policy for `bash`. This sits above the ordinary
    /// permission broker so broad approvals such as `bash *` cannot silently
    /// authorize destructive commands like `git reset --hard`.
    bash_policy: std::sync::RwLock<crate::bash_policy::BashPolicy>,
    /// Lifecycle event hooks (ADR-0025). Installed once at startup from the
    /// `[hooks]` config by the CLI; empty by default (subagents, tests). Read
    /// at the PreToolUse / PostToolUse / Stop insertion points. Held as a
    /// swappable `Arc` behind a `Mutex` so [`Agent::set_hooks`] can replace the
    /// whole registry without the insertion points holding the lock across the
    /// async `fire` — they clone the `Arc` and drop the guard first.
    hooks: crate::hook_runner::HookRunner,
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
    inbox_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<AgentOp>>>,
    inbox_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<AgentOp>>>,
    /// Inbound steering and follow-up queues for the currently running master/side round.
    /// This is deliberately separate from the subagent `AgentOp` inbox: submit,
    /// cancel, and boundary admission all take this one mutex, which gives the
    /// UI an exact answer in the cancellation-vs-admission race. `None` means
    /// the round is not accepting queued messages.
    session_queues: std::sync::Mutex<Option<SessionQueues>>,
    steering_mode: std::sync::RwLock<nuo_wire::QueueMode>,
    follow_up_mode: std::sync::RwLock<nuo_wire::QueueMode>,
    /// Cumulative milliseconds the current round has spent parked on a human
    /// decision (permission prompt or `ask_user`). Reset to 0 at the start of
    /// each user round and added to at every permission/ask_user `await`. Read
    /// at the round exit gate to derive "active" generation time
    /// (`duration_ms - paused_ms`) for an honest tokens/sec that excludes the
    /// human-thinking pause. `AtomicU64` because the parking sites
    /// (`execute_tool`, the bash policy path, `execute_ask_user`) take `&self`,
    /// not `&mut state`.
    round_paused_ms: std::sync::atomic::AtomicU64,
    /// Who this agent is and what it is for. The single string the system
    /// prompt opens with — supplied by the *embedding* (e.g. the CLI), so this
    /// crate stays identity-agnostic and can be reused by frontends that are
    /// not "muta". See [`AgentIdentity`].
    ///
    /// Behind a `RwLock` so a master-role switch ([`Self::set_identity`],
    /// driven by `/master` / `@master:`) can replace it live and the next
    /// request's system prompt reflects the new preamble without rebuilding the
    /// agent. Readers ([`Self::identity`], system-prompt assembly) take a read
    /// lock and clone; writers take a write lock. Contention is negligible —
    /// identity changes at most once per user command, reads once per request.
    pub(crate) identity: std::sync::RwLock<AgentIdentity>,
    /// Optional mid-round save point invoked at every ReAct-turn boundary
    /// (ADR-0048). The embedding (orchestration) installs a closure that
    /// durably appends the round's new messages to the session log so a crash
    /// after a side-effecting tool call leaves the transcript in sync with the
    /// filesystem instead of rewinding to the previous turn. `None` for
    /// subagents, the review diagnostic, and tests — they have no session of
    /// their own to persist, so the turn boundary is a plain no-op there.
    turn_persist: std::sync::Mutex<Option<TurnPersistFn>>,
    /// Request-projection archive sink installed by the session driver
    /// (ADR-0218): fired once per freshly assembled request with its forensic
    /// record. `None` for subagents, the review diagnostic, and tests.
    request_projection_persist: std::sync::Mutex<Option<RequestProjectionFn>>,
    /// Title-established observer installed by the session driver: fired
    /// once when the background titler durably persists a session's first
    /// title, so the runtime can push a fresh sessions overview (and the
    /// monitor tap republishes the row) to every attached client. `None`
    /// keeps titling silent (subagents, tests).
    pub(crate) title_established: std::sync::Mutex<Option<TitleEstablishedFn>>,
    /// Request-scoped projector. The agent owns its lifecycle and supplies live
    /// state snapshots; the assembler owns the pure window-to-request transform.
    model_request_assembler: crate::model_request::ModelRequestAssembler,
    /// Per-model tool-variant selection (the **override** axis) for the
    /// *current* model: a `capability → variant_id` map. Seeded from
    /// `[tool_variants."<model-id>"]` config via
    /// [`Agent::set_variant_selection`] and re-seeded on model switch so the
    /// resolved toolset always tracks the live model. Held behind an `Arc` so a
    /// spawned subagent — which is an agent on the *same* model — can inherit
    /// the same overrides by sharing this handle (see
    /// [`Agent::variant_selection_handle`]); the agent decides scope, the
    /// model decides variant.
    variant_selection: Arc<std::sync::Mutex<nuo_wire::VariantSelection>>,
    /// This agent's **identity-side selection** of the pool (the agent half of
    /// the two-selector model): the capability scope it admits plus any variant
    /// pins it forces. The master agent is
    /// [`ToolSelection::unrestricted`](nuo_wire::ToolSelection::unrestricted)
    /// — every capability, model-chosen variants. A scoped agent (or a future
    /// role-bound master) narrows this. Composed with the live model's
    /// selection by [`nuo_wire::ToolSet::resolve_for`] every time the toolset
    /// is re-resolved: scope by intersection, variants by agent-over-model
    /// precedence, model capability limits applied hard.
    tools: std::sync::Mutex<nuo_wire::ToolSelection>,
    /// Token-source accounting: running tally of how many tokens each
    /// provider+model reported authoritatively (upstream `usage`) vs. how many
    /// were filled in by the local estimator. Shared with the TUI so the
    /// token-source report modal renders live. `None` for subagents/tests that
    /// don't surface the report.
    token_ledger: std::sync::Mutex<Option<Arc<nuo_wire::TokenSourceLedger>>>,
    /// Content-addressed per-message token weights (see
    /// [`nuo_wire::MessageTokenWeights`]). Every estimate path consults
    /// this, so BPE tokenization cost collapses from O(total session bytes)
    /// per pass to O(new bytes since the last pass). Messages are immutable
    /// once written, so the cache never needs invalidation: identical bytes
    /// always yield identical weights. Held behind an `Arc` so off-executor
    /// estimate tasks (spawn_blocking) and the context-projection gates can
    /// share the same cache without borrowing the agent.
    token_weights: std::sync::Arc<nuo_wire::MessageTokenWeights>,
    /// Content-addressed per-tool-spec BPE weights: a toolset is stable across
    /// turns, so its schema cost is tokenized once, not per estimate pass.
    tool_schema_weights: std::sync::Arc<nuo_wire::ToolSchemaWeights>,
    /// Atomic extensions bound to this agent instance (ADR-0224).
    pub(crate) extensions: Arc<std::sync::RwLock<Vec<Arc<dyn nuo_wire::Extension>>>>,
    /// Active staffing role for this agent (e.g. "developer", "philosophist") (ADR-0244).
    pub(crate) active_role: std::sync::RwLock<Option<String>>,
}

/// Capability handle for steering a running agent from the outside — the
/// parent's down-direction of full-duplex (ADR-0029). Cheap to clone (one
/// `Weak` + one `mpsc::Sender`); obtained from [`Agent::install_inbox`] on an
/// `Arc<Agent>` (a spawned subagent) and typically lodged in a
/// [`crate::subagent_tool::SubagentRegistry`] keyed by the parent tool-call id so
/// the harness can look it up when a request surfaces.
///
/// Two classes of operation, deliberately split:
///
/// - **Steering** ([`AgentOp`], via [`SubagentHandle::submit`]): inject a new
///   user message, a hidden inter-agent note, or interrupt/shutdown. Routed
///   through the agent's inbox and applied at the next ReAct-turn boundary —
///   safe to defer because nothing is blocked on it.
/// - **Request/reply** ([`SubagentHandle::reply_permission`] /
///   [`SubagentHandle::reply_user_question`]): resolve a permission broker or
///   `ask_user` oneshot the subagent is parked on **right now**, mid-tool.
///   These bypass the inbox and call the agent's shared-state resolvers
///   directly — a queued reply would deadlock the parked tool.
///
/// The `Weak<Agent>` means the handle observes the agent's lifetime: once the
/// subagent's round ends and the dispatcher drops its `Arc`, every method
/// returns `false` / `None` instead of erroring, so a late reply from the UI
/// after the subagent finished degrades gracefully.
#[derive(Clone)]
pub struct SubagentHandle {
    weak: std::sync::Weak<Agent>,
    ops: mpsc::UnboundedSender<AgentOp>,
}

impl SubagentHandle {
    /// Submit a steering [`AgentOp`] into the agent's inbox. Returns `false`
    /// if the agent has been dropped (receiver gone) — the op is discarded.
    pub fn submit(&self, op: AgentOp) -> bool {
        self.ops.send(op).is_ok()
    }

    /// Resolve a permission broker request the subagent is parked on. Returns
    /// `false` if the agent was dropped or no matching pending request exists.
    /// This is the down-direction counterpart to an up-going
    /// [`AgentEvent::PermissionRequest`] / [`SubagentEvent::PermissionRequest`].
    pub fn reply_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_permission(request_id, decision)
        } else {
            false
        }
    }

    /// Resolve an `ask_user` request the subagent is parked on. Returns
    /// `false` if the agent was dropped or no matching pending request exists.
    /// Down-direction counterpart to an up-going
    /// [`AgentEvent::UserQuestionRequest`] / [`SubagentEvent::UserQuestionRequest`].
    /// An empty outer answer vector means the operator cancelled.
    pub fn reply_user_question(&self, request_id: &str, answers: Vec<Vec<String>>) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_user_question(request_id, answers)
        } else {
            false
        }
    }

    /// Resolve an interactive-input request the subagent's `bash` is parked on
    /// (L3.5 β). Down-direction counterpart to an up-going
    /// [`AgentEvent::StdinRequest`] / [`SubagentEvent::StdinRequest`].
    pub fn reply_input(&self, request_id: &str, text: String) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_input(request_id, text)
        } else {
            false
        }
    }

    /// Whether the underlying agent is still alive (its dispatcher still holds
    /// the `Arc`). Lets a caller drop a stale handle instead of no-op-ing.
    pub fn is_alive(&self) -> bool {
        self.weak.upgrade().is_some()
    }
}

/// Mutable bookkeeping threaded through one user round's ReAct turns.
///
#[derive(Default)]
pub(crate) struct RoundState {
    token_usage: TokenUsage,
    /// Accumulated provider *generation* time across every completed request
    /// in this round (sum of each `RequestAccountingGuard`'s sealed span).
    /// Excludes tool execution, hooks, and human-decision pauses — it is the
    /// honest denominator for tokens/sec. Folded into `RoundOutcome` at the
    /// round-exit gate.
    generation_ms: u64,
    /// Consecutive ReAct turns whose tool calls were all `Read`-tier. Surfaced
    /// to user-configured `Turn` hooks so a hook can act on "exploration
    /// without progress". Reset to 0 by any turn containing an
    /// `Execute`/`Write` call.
    pub(crate) consecutive_readonly_turns: u32,
    /// The round-scoped guard registry: holds one or more `RoundGuard`s (e.g.
    /// `ReadLoopGuard`) and tool-call data for the ReAct turn just dispatched.
    /// It lives and dies with this `RoundState`, so loop
    /// state never crosses user rounds.
    pub(crate) guards: crate::guard::RoundGuardState,
    /// Exact tool calls that reached a terminal result in this round. The set
    /// becomes an idempotency fence only after a transient provider retry;
    /// normal ReAct turns retain their existing repeat-call behavior.
    completed_tool_calls: HashSet<String>,
    /// Snapshot of calls completed before a transient provider failure. Only
    /// this frozen subset is protected: calls first executed after a retry keep
    /// normal same-round semantics unless a later provider failure checkpoints
    /// them too.
    retry_protected_tool_calls: HashSet<String>,
}

impl RoundState {
    /// Build a fresh per-round guard state with the standard guard set, tuned
    /// by `config`. Whether the guard is *enabled* (allowed to inject) is
    /// controlled by `config.enabled`, checked at the turn boundary in
    /// `Agent::apply_guard_actions` — so the guard state is always present
    /// even when disabled (it just never fires). It lives and dies with this
    /// `RoundState`, so loop state never crosses user rounds.
    fn guards_default(
        config: nuo_wire::TrajectoryGuardConfig,
    ) -> crate::guard::RoundGuardState {
        crate::guard::RoundGuardState::new()
            .with_trajectory(crate::trajectory_guard::TrajectoryLoopGuard::new(config))
    }

    pub(crate) fn remember_completed_tool(&mut self, call: &ToolCall) {
        self.completed_tool_calls
            .insert(checkpoint_tool_signature(call));
    }

    fn protect_completed_tools_for_retry(&mut self) {
        self.retry_protected_tool_calls
            .extend(self.completed_tool_calls.iter().cloned());
    }

    pub(crate) fn is_checkpoint_replay(&self, call: &ToolCall) -> bool {
        self.retry_protected_tool_calls
            .contains(&checkpoint_tool_signature(call))
    }
}

/// Exact, stable identity for retry idempotency. JSON arguments are parsed and
/// serialized once so insignificant object-key ordering does not turn the same
/// call into a different identity; malformed argument blobs fall back to their
/// trimmed wire form.
fn checkpoint_tool_signature(call: &ToolCall) -> String {
    let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| call.arguments.trim().to_string());
    format!("{}\u{0}{arguments}", call.name)
}

/// Live state for one streaming user round.
///
/// Orchestration keeps this value across transient provider retries. A retry
/// therefore resumes the exact provider request that failed while preserving
/// completed tool results, loop-guard state, hook scope, accounting, and the
/// steering inbox. `pending_request` stays set until a complete, valid
/// assistant response has been accepted; re-entry while it is set skips
/// request preparation and turn-start hooks so retrying cannot replay work
/// that already happened at the request boundary.
pub(crate) struct StreamingRoundState {
    turn_context: Arc<nuo_wire::ProviderTurnContext>,
    state: RoundState,
    turn_index: usize,
    /// Number of in-flight stream-loop recoveries already attempted in this
    /// user round. The first detected loop gets one guided retry; a recurrence
    /// is a hard stop. This state survives transient provider retries with the
    /// rest of the round checkpoint.
    stream_loop_recoveries: u8,
    inbox_rx: Option<mpsc::UnboundedReceiver<AgentOp>>,
    started_at: std::time::Instant,
    pending_request: Option<nuo_wire::ModelRequest>,
    session_queue_generation: Option<u64>,
}

impl StreamingRoundState {
    /// Apply `project` to the checkpointed request of the current turn, if one
    /// is armed. Returns `false` when no request is checkpointed (the turn had
    /// not reached assembly yet, so the next attempt assembles afresh).
    ///
    /// This is the narrow way to change *what is sent* on a retry without
    /// invalidating the checkpoint: the request stays the same turn's
    /// projection — same history, same tool schemas, same accounting — and only
    /// the projected field changes. It exists for the image-input recovery
    /// (ADR-0230): a provider that rejects an attachment must be retried without
    /// it, and clearing `pending_request` instead would re-run the turn's
    /// preparation and TurnStart hooks, which the checkpoint exists to prevent.
    ///
    /// The durable request-projection archive (ADR-0218) is written once per
    /// logical invocation from the freshly assembled request, so a projection
    /// applied here intentionally post-dates — and therefore differs from — that
    /// record; the withholding is reported to the user and the log instead.
    pub(crate) fn project_pending_request(
        &mut self,
        project: impl FnOnce(&mut nuo_wire::ModelRequest) -> usize,
    ) -> Option<usize> {
        self.pending_request.as_mut().map(project)
    }
}

impl StreamingRoundState {
    /// How many complete ReAct turns this round has committed — the ordinal
    /// the *next* turn would take (0-based `turn_index`). `/retry` captures
    /// this into a [`nuo_wire::RetryPoint`] so the resumed round
    /// keeps numbering turns contiguously instead of restarting at 0.
    pub(crate) fn committed_turns(&self) -> usize {
        self.turn_index
    }
}

/// A queue of pending messages controlled by a [`nuo_wire::QueueMode`].
#[derive(Debug, Clone)]
pub struct PendingMessageQueue {
    messages: std::collections::VecDeque<nuo_wire::QueuedMessage>,
    pub mode: nuo_wire::QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: nuo_wire::QueueMode) -> Self {
        Self {
            messages: std::collections::VecDeque::new(),
            mode,
        }
    }

    pub fn enqueue(&mut self, message: nuo_wire::QueuedMessage) {
        self.messages.push_back(message);
    }

    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn drain(&mut self) -> Vec<nuo_wire::QueuedMessage> {
        match self.mode {
            nuo_wire::QueueMode::All => self.messages.drain(..).collect(),
            nuo_wire::QueueMode::OneAtATime => {
                self.messages.pop_front().into_iter().collect()
            }
        }
    }

    pub fn drain_all(&mut self) -> Vec<nuo_wire::QueuedMessage> {
        self.messages.drain(..).collect()
    }

    pub fn cancel(&mut self, input_id: &str) -> Option<nuo_wire::QueuedMessage> {
        let position = self
            .messages
            .iter()
            .position(|input| input.id == input_id)?;
        self.messages.remove(position)
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

struct SessionQueues {
    session_id: String,
    generation: u64,
    steering: PendingMessageQueue,
    follow_up: PendingMessageQueue,
}

/// Result of one tool-execution phase, returned by the cancellation-aware
/// executors ([`Agent::schedule_tool_calls`] and
/// [`Agent::execute_tool_evented`]). The executors never return
/// `Err(HarnessError::Interrupted)` themselves anymore: when the user
/// interrupts a turn they signal cooperatively-cancellable in-flight calls
/// (subagents), drain them within a bounded grace period, and report
/// `interrupted: true` with whatever results were recovered. The caller
/// ([`Agent::dispatch_finalize`]) records the recovered results, then
/// propagates the interruption itself — so an interrupted subagent's partial
/// transcript survives into the persisted transcript even though the round
/// ends as interrupted.
pub(crate) struct ConcurrentOutcome {
    /// Per-input results in input order. A `None` slot means the call was
    /// dropped by the cancel grace deadline (no result recovered); the
    /// executor already paired it with a terminal [`AgentEvent::ToolCancelled`].
    pub(crate) results: Vec<Option<(ToolOutput, u64)>>,
    /// Whether the cancellation token fired during execution.
    pub(crate) interrupted: bool,
}

/// Single-call counterpart of [`ConcurrentOutcome`] for
/// [`Agent::execute_tool_evented`]. `result` is `Some` when the call reached a
/// terminal result — normally, or after a graceful drain on interrupt.
pub(crate) struct SingleToolOutcome {
    pub(crate) result: Option<ToolOutput>,
    pub(crate) interrupted: bool,
}

/// RAII settlement for one concrete provider request. Any early-return path
/// (interrupt, timeout, provider error, invalid response) still terminally
/// records the attempt; normal completion explicitly settles it with the
/// provider usage or the local fallback estimate.
struct RequestAccountingGuard {
    ledger: Option<Arc<nuo_wire::TokenSourceLedger>>,
    key: Option<nuo_wire::RequestUsageKey>,
    round: u64,
    turn: u32,
    attempt: u32,
    cancel: CancellationToken,
    projected_prompt_tokens: i64,
    observed_completion_tokens: i64,
    /// Incremental BPE counter for streamed deltas (exact across delta
    /// boundaries; a per-delta sum would over-count merges that span them).
    output_counter: nuo_wire::tokenizer::StreamingCounter,
    observed_usage: Option<TokenUsage>,
    error: Option<String>,
    settled: bool,
    /// Monotonic performance anchors. The request clock starts at the actual
    /// provider-call boundary, after local projection/events. Stream events
    /// are sampled as the provider stream yields them; missing stages remain
    /// `None` rather than becoming fabricated zero-duration measurements.
    started_at: Option<std::time::Instant>,
    stream_ready_at: Option<std::time::Instant>,
    first_output_at: Option<std::time::Instant>,
    last_output_at: Option<std::time::Instant>,
    stream_end_at: Option<std::time::Instant>,
    validated_at: Option<std::time::Instant>,
    first_output_fragment: String,
    output_events: u32,
    generation_ms: u64,
    /// First provider stream event of *any* kind (including preamble frames).
    /// Distinct from `first_output_at`, which waits for content.
    first_frame_at: Option<std::time::Instant>,
    /// This attempt's telemetry handle (ADR-0232). Created here, stamped onto
    /// the request this attempt dispatches, and read back when it settles — so
    /// the transport's timings reach the attempt that caused them, including
    /// when a shared provider is running several attempts at once.
    transport_telemetry: nuo_wire::TransportTelemetry,
}

impl RequestAccountingGuard {
    fn begin(
        agent: &Agent,
        cancel: &CancellationToken,
        provider: &str,
        model: &str,
        turn_index: usize,
        projected_prompt_tokens: usize,
    ) -> Self {
        let ledger = agent.token_ledger();
        let key = ledger.as_ref().map(|ledger| {
            let thread_id = agent.thread_id().unwrap_or_default();
            let actor_id = agent.accounting_actor_id();
            ledger.begin_request_for_actor(nuo_wire::BeginRequestParams {
                session_id: &thread_id,
                actor_id: &actor_id,
                provider,
                model,
                round: agent.round_count(),
                turn: turn_index.saturating_add(1) as u32,
                projected_prompt_tokens: projected_prompt_tokens as i64,
            })
        });
        let round = agent.round_count();
        let turn = turn_index.saturating_add(1) as u32;
        let attempt = key.as_ref().map_or(1, |key| key.attempt);
        Self {
            ledger,
            key,
            round,
            turn,
            attempt,
            cancel: cancel.clone(),
            projected_prompt_tokens: projected_prompt_tokens as i64,
            observed_completion_tokens: 0,
            output_counter: nuo_wire::tokenizer::StreamingCounter::new(),
            observed_usage: None,
            error: None,
            settled: false,
            started_at: None,
            stream_ready_at: None,
            first_output_at: None,
            last_output_at: None,
            stream_end_at: None,
            validated_at: None,
            first_output_fragment: String::new(),
            output_events: 0,
            generation_ms: 0,
            first_frame_at: None,
            transport_telemetry: nuo_wire::TransportTelemetry::new(),
        }
    }

    /// This attempt's telemetry handle, to be stamped onto the request the
    /// attempt dispatches. A retry must take a fresh one.
    fn transport_telemetry(&self) -> nuo_wire::TransportTelemetry {
        self.transport_telemetry.clone()
    }

    /// Start the monotonic request clock at the provider-call boundary.
    fn start_request(&mut self) {
        self.started_at.get_or_insert_with(std::time::Instant::now);
    }

    /// The provider returned a live response stream (normally after response
    /// headers were received).
    fn mark_stream_ready(&mut self) {
        self.stream_ready_at
            .get_or_insert_with(std::time::Instant::now);
    }

    fn mark_stream_end(&mut self) {
        self.stream_end_at
            .get_or_insert_with(std::time::Instant::now);
    }

    fn record_error(&mut self, err: impl Into<String>) {
        self.error = Some(err.into());
    }

    fn observe_output(&mut self, text: &str) {
        // Streamed deltas feed an exact incremental BPE counter: BPE is not
        // additive across delta boundaries (merges span them), so summing
        // per-delta counts overestimates by 2–100% depending on chunk size.
        // `push` returns the counter's *running* total — not a per-delta
        // increment — so the count is read off the counter afterwards rather
        // than summed per call (summing would re-count every early token once
        // per later delta; a real interrupted 4 000-delta stream booked 14.7M
        // "completion tokens" and a 130 050 tok/s rate from exactly that).
        self.output_counter.push(text);
        self.observed_completion_tokens = self.output_counter.tokens() as i64;
    }

    /// Observe one provider stream event at a single monotonic instant. One
    /// tool-call event may carry both a name and arguments; they share the
    /// same event timestamp and first-event token bucket.
    fn observe_stream_event(
        &mut self,
        event: &nuo_wire::ProviderStreamEvent,
        received_at: std::time::Instant,
    ) {
        let mut fragments: Vec<&str> = Vec::new();
        // Any event from the origin — including a preamble or usage frame —
        // proves the server started responding.
        self.first_frame_at.get_or_insert(received_at);
        match event {
            nuo_wire::ProviderStreamEvent::ModelCatalogEtag(_) => return,
            nuo_wire::ProviderStreamEvent::TextDelta(delta)
            | nuo_wire::ProviderStreamEvent::ReasoningDelta(delta) => {
                if !delta.is_empty() {
                    fragments.push(delta);
                }
            }
            nuo_wire::ProviderStreamEvent::ToolCallDelta {
                name, arguments, ..
            } => {
                if let Some(name) = name.as_deref().filter(|name| !name.is_empty()) {
                    fragments.push(name);
                }
                if !arguments.is_empty() {
                    fragments.push(arguments);
                }
            }
            nuo_wire::ProviderStreamEvent::Usage(usage) => {
                self.observe_usage(*usage);
                return;
            }
            nuo_wire::ProviderStreamEvent::Completed(meta) => {
                if let Some(usage) = meta.usage {
                    self.observe_usage(usage);
                }
                return;
            }
        }

        if fragments.is_empty() {
            return;
        }
        let first_event = self.first_output_at.is_none();
        if first_event {
            self.first_output_at = Some(received_at);
        }
        self.last_output_at = Some(received_at);
        self.output_events = self.output_events.saturating_add(1);
        for fragment in fragments {
            if first_event {
                self.first_output_fragment.push_str(fragment);
            }
            self.observe_output(fragment);
        }
    }

    /// Close the stream counter (finalizing the unfinished trailing pretoken)
    /// so the observed count equals a whole-text tokenization of everything
    /// the attempt streamed. Idempotent.
    fn finish_output(&mut self) {
        let finished = self.output_counter.finish() as i64;
        if finished > self.observed_completion_tokens {
            self.observed_completion_tokens = finished;
        }
    }

    fn observe_usage(&mut self, usage: TokenUsage) {
        self.observed_usage = Some(usage);
    }

    /// Freeze the generation clock at the point a validated assistant response
    /// is available — *before* tool calls are dispatched, so their execution
    /// time never inflates the measured generation span. Safe to call more
    /// than once within one guard; only the first call records a span.
    fn seal_generation(&mut self) {
        if self.validated_at.is_some() {
            return;
        }
        let end = std::time::Instant::now();
        self.validated_at = Some(end);
        self.stream_end_at.get_or_insert(end);
        if let Some(start) = self.started_at {
            self.generation_ms = end.saturating_duration_since(start).as_millis() as u64;
        }
    }

    fn performance(&self) -> nuo_wire::RequestPerformance {
        let offset = |end: Option<std::time::Instant>| {
            Some(end?.saturating_duration_since(self.started_at?).as_micros() as u64)
        };
        let span = |start: Option<std::time::Instant>, end: Option<std::time::Instant>| {
            Some(end?.saturating_duration_since(start?).as_micros() as u64)
        };
        // The transport's own phases, read from this attempt's own handle. Read
        // (not taken) because the settled record and the live snapshot are built
        // from the same attempt; the handle belongs to this attempt alone, so a
        // second read can only ever return this attempt's numbers.
        let transport = self.transport_telemetry.read().unwrap_or_default();
        // The transport reports its offsets from its own dispatch. Shift them
        // onto this attempt's anchor, and refuse them outright when either
        // anchor is missing: a number measured from one epoch and rendered
        // against another is worse than no number. Durations need no anchor, so
        // the phases and the `TCP_INFO` sample transfer either way.
        let anchor_shift_us = match (self.started_at, transport.dispatch_at) {
            (Some(started), Some(dispatch)) => dispatch
                .checked_duration_since(started)
                .map(|delta| delta.as_micros() as u64),
            _ => None,
        };
        let anchored =
            |transport_offset_us: Option<u64>| match (anchor_shift_us, transport_offset_us) {
                (Some(shift), Some(offset)) => Some(shift.saturating_add(offset)),
                _ => None,
            };
        nuo_wire::RequestPerformance {
            dns_us: transport.dns_us,
            tcp_us: transport.tcp_us,
            tls_us: transport.tls_us,
            request_sent_us: anchored(transport.request_sent_us),
            connected_us: anchored(transport.connected_us),
            first_frame_us: offset(self.first_frame_at),
            rtt_us: transport.rtt_us,
            retransmits: transport.retransmits,
            stream_ready_us: anchored(transport.stream_ready_us)
                .or_else(|| offset(self.stream_ready_at)),
            ttft_us: offset(self.first_output_at),
            stream_us: span(self.first_output_at, self.last_output_at),
            tail_us: span(self.last_output_at, self.stream_end_at),
            e2e_us: offset(self.validated_at),
            streamed_output_tokens: self.observed_completion_tokens.max(0) as u64,
            first_output_tokens: nuo_wire::count_tokens(&self.first_output_fragment) as u64,
            output_events: self.output_events,
            timing_source: nuo_wire::PerformanceTimingSource::ClientObserved,
            stream_token_source: nuo_wire::StreamTokenSource::Cl100k,
            observation: transport.observation,
            ..Default::default()
        }
    }

    fn performance_snapshot(
        &self,
        completion_tokens: i64,
        usage_source: nuo_wire::RequestUsageSource,
    ) -> nuo_wire::TurnPerformanceSnapshot {
        nuo_wire::TurnPerformanceSnapshot {
            round: self.round,
            turn: self.turn,
            attempt: self.attempt,
            completion_tokens: completion_tokens.max(0) as u64,
            usage_source,
            performance: self.performance(),
        }
    }

    fn settle(
        &mut self,
        status: nuo_wire::RequestUsageStatus,
        usage: Option<TokenUsage>,
        estimated_completion_tokens: i64,
    ) {
        self.settle_with_error(
            status,
            usage,
            estimated_completion_tokens,
            self.error.clone(),
        );
    }

    fn settle_with_error(
        &mut self,
        status: nuo_wire::RequestUsageStatus,
        usage: Option<TokenUsage>,
        estimated_completion_tokens: i64,
        error: Option<String>,
    ) {
        if self.settled {
            return;
        }
        self.seal_generation();
        if let (Some(ledger), Some(key)) = (&self.ledger, &self.key) {
            ledger.settle_request_with_performance_and_error(
                key,
                status,
                usage,
                estimated_completion_tokens,
                self.generation_ms,
                Some(self.performance()),
                error,
            );
        }
        self.settled = true;
    }
}

impl Drop for RequestAccountingGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.seal_generation();
        // Finalize the streamed-output counter so the estimate equals a
        // whole-text count of everything the attempt streamed before it was
        // interrupted or failed (the interrupted path cannot go through
        // `book_turn_usage`, which normally closes the counter).
        self.finish_output();
        let status = if self.cancel.is_cancelled() {
            nuo_wire::RequestUsageStatus::Interrupted
        } else {
            nuo_wire::RequestUsageStatus::Failed
        };
        self.settle_with_error(
            status,
            self.observed_usage,
            self.observed_completion_tokens,
            self.error.clone(),
        );
    }
}

/// Construction-time configuration for an [`Agent`].
///
/// System-prompt policy is assembled before the agent starts running and is immutable
/// afterwards. This keeps request preparation deterministic while allowing an
/// embedding to add product-specific sections or replace the composition for a
/// specialized agent such as the session reviewer.
pub struct AgentBuilder {
    provider: Arc<dyn Provider>,
    host: crate::host::KernelHost,
    toolset: nuo_wire::ToolSet,
    skills_registry: skills::SkillRegistry,
    identity: AgentIdentity,
    model_request_assembler: crate::model_request::ModelRequestAssembler,
    extensions: Vec<Arc<dyn nuo_wire::Extension>>,
}

impl AgentBuilder {
    fn new(
        provider: Arc<dyn Provider>,
        toolset: nuo_wire::ToolSet,
        identity: AgentIdentity,
    ) -> Self {
        Self {
            provider,
            toolset,
            host: crate::host::KernelHost::none(),
            skills_registry: skills::SkillRegistry::empty(),
            identity,
            model_request_assembler: crate::model_request::ModelRequestAssembler::new(
                crate::model_request::default_system_prompt_registry(),
            ),
            extensions: Vec::new(),
        }
    }

    /// Add an ambient harness facet to this agent (ADR-0211).
    /// Supply the host's ports: declared roles and per-project path policy (ADR-0300 §1).
    pub fn with_host(mut self, host: crate::host::KernelHost) -> Self {
        self.host = host;
        self
    }

    pub fn with_extension(mut self, extension: Arc<dyn nuo_wire::Extension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Add atomic extensions to this agent (ADR-0224).
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = Arc<dyn nuo_wire::Extension>>,
    ) -> Self {
        for extension in extensions {
            self.extensions.push(extension);
        }
        self
    }

    /// Add one caller-supplied tool to this agent's capability set.
    ///
    /// Agent-owned stateful identities are installed during build and take
    /// precedence over a caller tool with the same `(name, variant)`.
    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.toolset.insert(tool);
        self
    }

    /// Add caller-supplied tools to this agent's capability set.
    pub fn with_tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        for tool in tools {
            self.toolset.insert(tool);
        }
        self
    }

    /// Attach a live skill registry. Agents without one use an empty registry
    /// and expose no skill tools or implicit skill context.
    pub fn with_skills(mut self, registry: skills::SkillRegistry) -> Self {
        self.skills_registry = registry;
        self
    }

    /// Add an embedding-owned section to the default system-prompt policy.
    pub fn register_system_prompt_section<S: crate::SystemPromptSection + 'static>(
        mut self,
        section: S,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler
            .registry_mut()
            .try_register(section)?;
        Ok(self)
    }

    /// Disable a registered default or custom section by its stable id.
    pub fn disable_system_prompt_section(
        mut self,
        id: &str,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler.registry_mut().disable(id)?;
        Ok(self)
    }

    /// Override a registered section's semantic ordering in the final composition.
    pub fn order_system_prompt_section(
        mut self,
        id: &str,
        order: crate::InstructionOrder,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler
            .registry_mut()
            .set_order(id, order)?;
        Ok(self)
    }

    /// Override a registered section's rank in the final composition.
    pub fn rank_system_prompt_section(
        mut self,
        id: &str,
        rank: u32,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler
            .registry_mut()
            .set_rank(id, rank)?;
        Ok(self)
    }

    /// Replace the default composition wholesale.
    pub fn with_system_prompt_registry(mut self, registry: crate::SystemPromptRegistry) -> Self {
        self.model_request_assembler.replace_registry(registry);
        self
    }

    /// Freeze the configuration and construct the agent.
    pub fn build(self) -> Agent {
        let mut agent = Agent::from_toolset_with_model_request_assembler(
            self.provider,
            self.toolset,
            self.skills_registry,
            self.identity,
            self.model_request_assembler,
        );
        agent.host = self.host;
        if !self.extensions.is_empty() {
            *agent.extensions.write().unwrap_or_else(|e| e.into_inner()) = self.extensions;
        }
        agent
    }
}

/// Outcome returned by the agent after running one round.
#[derive(Debug, Clone)]
pub struct RoundOutcome {
    pub message: crate::Message,
    pub token_usage: TokenUsage,
    pub duration_ms: u64,
    /// Milliseconds of `duration_ms` spent parked on a human decision (a
    /// permission prompt or an `ask_user`). The "active" generation time is
    /// `duration_ms - paused_ms`; the harness derives an honest tokens/sec
    /// from the active time so the human-thinking pause never drags the
    /// measured server throughput down.
    pub paused_ms: u64,
    /// Time the model actually spent *generating* across this round's
    /// completed provider requests — excluding tool execution, hooks, and
    /// human-decision pauses. The most accurate denominator for tokens/sec.
    pub generation_ms: u64,
}

mod execution;
mod rounds;
mod state;
mod steering;
pub use steering::SwitchedRole;
mod tools_admin;
pub mod cognitive_bridge;
pub use cognitive_bridge::session_event_to_agent_events;

pub(crate) use rounds::ToolResultRecord;
pub(crate) use state::strip_images;

/// Render a missing runtime grant without conflating it with project asset
/// trust. This is returned when no interactive approver is available.
fn permission_required_output(request: &nuo_wire::PermissionRequest) -> ToolOutput {
    use nuo_wire::ToolPermissionPayload;

    let operation = match request.submission.as_ref().map(|s| &s.payload) {
        Some(ToolPermissionPayload::Command { command, .. }) => {
            format!("Command '{command}' requires runtime execution grant.")
        }
        Some(ToolPermissionPayload::FileEdit { paths, operation }) => format!(
            "File operation '{operation}' on '{}' requires runtime file-modification grant.",
            paths.join(", ")
        ),
        Some(ToolPermissionPayload::Process { target, action }) => {
            format!("Process operation '{action}' on '{target}' requires runtime lifecycle grant.")
        }
        Some(ToolPermissionPayload::Generic { summary, .. }) => {
            format!("External operation '{summary}' requires runtime grant.")
        }
        None => format!(
            "Tool '{}' for scope '{}' requires runtime grant.",
            request.tool, request.scope
        ),
    };
    ToolOutput::Error {
        message: format!(
            "[permission required] {operation}\nAdd a permission rule in settings or approve interactively."
        ),
        detail: None,
    }
}

fn valid_assistant_response(message: &Message) -> bool {
    !message.content.is_empty()
        || message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        || message
            .reasoning_content
            .as_ref()
            .is_some_and(|reasoning| !reasoning.is_empty())
}

/// Build the "empty assistant response" error, after logging enough state to
/// diagnose why: whether reasoning came through, whether any tool calls were
/// parsed, and which provider/model was responsible. The matching per-turn
/// stream summary (chars fed vs emitted, reasoning/tool-call traffic) is logged
/// by the provider at `nuo_wire::provider=debug`.
///
/// The error is classified as a *retryable* upstream fault: an entirely empty
/// assistant frame is a transient provider glitch (truncated stream, flaky
/// gateway), not a harness or request problem. The pending request checkpoint
/// is still armed at this point, so the orchestration retry loop resends the
/// exact same request — completed tool results are preserved and no side
/// effect replays. Returning a terminal `HarnessError::Other` here instead
/// would kill the round and force a manual `/retry` (observed live: an
/// omen-alpha empty frame at turn 4 stopped an unattended session for ~11
/// minutes until a human nudged it).
fn empty_response_error(response: &Message) -> HarnessError {
    tracing::warn!(
        target: "nuo_wire::agent",
        provider = ?response.provider,
        model = ?response.model,
        content_chars = response.content.len(),
        reasoning_chars = response
            .reasoning_content
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0),
        tool_calls = response.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0),
        "empty assistant response: provider returned no content and no tool calls",
    );
    HarnessError::Provider(
        nuo_wire::ProviderError::new(
            response.provider.as_deref().unwrap_or("harness"),
            nuo_wire::ProviderErrorKind::Upstream,
            "Provider returned an empty assistant response (no content, no tool calls).",
        )
        .retryable(None),
    )
}

/// Drop assistant messages that carry neither text nor a tool call — the model
/// occasionally emits an empty assistant frame that would otherwise confuse
/// the next provider request. Called by the shared request assembler, which
/// both turn loops route through (ADR-0061).
pub(crate) fn remove_empty_assistant_messages(messages: &mut Vec<Message>) {
    messages.retain(|message| message.role != Role::Assistant || valid_assistant_response(message));
}

// PermissionContext: the agent's implementation of the policy-chain capability
// trait. Policies reach the agent's async machinery (hooks, bash policy,
// permission store) through this, keeping permission_policy.rs decoupled from
// the concrete Agent type.

#[async_trait::async_trait]
impl crate::permission_policy::PermissionContext for Agent {
    async fn check_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> crate::hooks::PreToolUseVerdict {
        self.hooks()
            .check_pre_tool_use(
                tool_name,
                tool_input,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await
    }

    fn apply_scoped_disables(&self, disables: &[(String, nuo_wire::RestorePoint)]) {
        // Delegate to the existing agent method (same signature).
        Agent::apply_scoped_disables(self, disables);
    }

    async fn check_bash_policy(
        &self,
        command: &str,
        _arguments: &str,
    ) -> crate::permission_policy::BashVerdict {
        // The single source of truth for the chain's BashPolicy gate. Returns
        // a disjoint Allow / Confirm / Deny verdict so the gate can decide
        // everything (including the interactive confirm) without the caller
        // re-evaluating outside the chain.
        let policy = self
            .bash_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(decision) = policy.evaluate(command) else {
            return crate::permission_policy::BashVerdict::Allow;
        };
        match decision.action {
            crate::bash_policy::BashPolicyAction::Deny => {
                tracing::warn!(command = %command, rule = %decision.name, "bash command blocked by policy");
                crate::permission_policy::BashVerdict::Deny {
                    output: decision.blocked_output(command),
                }
            }
            crate::bash_policy::BashPolicyAction::Confirm => {
                crate::permission_policy::BashVerdict::Confirm { match_: decision }
            }
            crate::bash_policy::BashPolicyAction::Allow => {
                crate::permission_policy::BashVerdict::Allow
            }
        }
    }

    fn permissions(&self) -> &crate::permission_store::PermissionStore {
        &self.permissions
    }
}
#[cfg(test)]
mod tests {
    use super::{
        RoundState, ScopedToolDisable, checkpoint_tool_signature, permission_required_output,
        subagent_result_text,
    };

    #[test]
    fn missing_command_authority_has_runtime_only_guidance() {
        let command = "pwd; ls -la".to_string();
        let request = nuo_wire::PermissionRequest {
            id: String::new(),
            tool: "execute_command".to_string(),
            label: "run command".to_string(),
            description: String::new(),
            arguments: String::new(),
            scope: command.clone(),
            elevation: false,
            one_off: false,
            origin: None,
            hazard: Some(nuo_wire::HazardLevel::CommandExecution),
            submission: Some(nuo_wire::ToolPermissionSubmission {
                hazard_level: nuo_wire::HazardLevel::CommandExecution,
                label: "run command".to_string(),
                description: String::new(),
                scope: command.clone(),
                payload: nuo_wire::ToolPermissionPayload::Command {
                    command: command.clone(),
                    cwd: None,
                    kill_spec: nuo_wire::ProcessKillSpec {
                        command: "pwd".to_string(),
                        process_group_killable: true,
                        pkill_target: "pkill -f pwd".to_string(),
                        cwd: None,
                    },
                },
            }),
        };

        let output = permission_required_output(&request).to_text();
        assert!(output.contains(
            "[permission required] Command 'pwd; ls -la' requires runtime execution grant."
        ));
        assert!(output.contains("Add a permission rule in settings or approve interactively."));
        assert!(!output.contains("/trust"));
    }

    fn tool_call(id: &str, arguments: &str) -> nuo_wire::ToolCall {
        nuo_wire::ToolCall {
            id: id.to_string(),
            name: "write_file".to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn checkpoint_tool_identity_ignores_json_object_key_order() {
        let first = tool_call("first", r#"{"path":"x","content":"y"}"#);
        let retried = tool_call("retry", r#"{"content":"y","path":"x"}"#);
        assert_eq!(
            checkpoint_tool_signature(&first),
            checkpoint_tool_signature(&retried)
        );
    }

    #[test]
    fn provider_retry_protects_only_calls_completed_before_its_checkpoint() {
        let before_retry = tool_call("first", r#"{"path":"before"}"#);
        let after_retry = tool_call("second", r#"{"path":"after"}"#);
        let mut state = RoundState::default();
        state.remember_completed_tool(&before_retry);
        state.protect_completed_tools_for_retry();
        state.remember_completed_tool(&after_retry);

        assert!(state.is_checkpoint_replay(&before_retry));
        assert!(!state.is_checkpoint_replay(&after_retry));
    }

    /// The successful subagent result carries the `[<tool> result]:` header, the
    /// original summary verbatim, and the success re-anchor note.
    #[test]
    fn subagent_result_text_reanchors_on_success() {
        let text = subagent_result_text("subagent", "Found the symbol in lib.rs", false, false);
        assert!(
            text.starts_with("[subagent result]:\n"),
            "header present: {text}"
        );
        assert!(
            text.contains("Found the symbol in lib.rs"),
            "summary preserved verbatim: {text}"
        );
        // The anchor must pin the master's write capability back to the
        // master and call out the read-only scope as subagent-only.
        assert!(
            text.contains("applies to the subagent only"),
            "anchor scope pin missing: {text}"
        );
        assert!(
            text.contains("retain your full toolset"),
            "parent re-anchor missing: {text}"
        );
    }

    /// A failed subagent carries a different (re-delegate-or-act-directly) anchor,
    /// and still preserves the partial summary for the master to act on.
    #[test]
    fn subagent_result_text_reanchors_on_failure() {
        let text = subagent_result_text("subagent", "partial findings before crash", true, false);
        assert!(
            text.contains("partial findings before crash"),
            "partial summary preserved: {text}"
        );
        assert!(
            text.contains("could not complete its sub-task"),
            "failure anchor missing: {text}"
        );
        // Both anchors must re-affirm the master retains write capability.
        assert!(
            text.contains("retain your full toolset"),
            "parent re-anchor missing on failure: {text}"
        );
        // And must NOT carry the success-only phrasing (regression guard against
        // the success anchor leaking onto a failed subagent).
        assert!(
            !text.contains("applies to the subagent only"),
            "success anchor leaked onto failure: {text}"
        );
    }

    /// The re-anchor is unconditional for any subagent result — a regression guard
    /// that a future refactor cannot silently drop it.
    #[test]
    fn subagent_result_text_anchor_is_unconditional() {
        for (failed, interrupted) in [(false, false), (true, false), (false, true)] {
            let text = subagent_result_text("subagent", "x", failed, interrupted);
            assert!(
                text.contains("[system]"),
                "system anchor tag present (failed={failed}, interrupted={interrupted}): {text}"
            );
        }
    }

    /// An interrupted subagent gets its own re-anchor: the partial findings are
    /// real work to continue, not an error to work around — and the read-only
    /// framing still does not transfer to the parent.
    #[test]
    fn subagent_result_text_reanchors_interruption() {
        let text = subagent_result_text("subagent", "found 2 of 5 handlers", false, true);
        assert!(
            text.contains("found 2 of 5 handlers"),
            "partial summary preserved: {text}"
        );
        assert!(
            text.contains("interrupted mid-task"),
            "interruption anchor missing: {text}"
        );
        assert!(
            !text.contains("could not complete its sub-task"),
            "failure anchor leaked onto interruption: {text}"
        );
        assert!(
            text.contains("retain your full toolset"),
            "parent re-anchor missing: {text}"
        );
    }

    use nuo_wire::RestorePoint;

    /// A scoped disable hides the tool until its restore point fires.
    #[test]
    fn scoped_disable_hides_until_restore() {
        let mut scoped = ScopedToolDisable::default();
        assert!(!scoped.contains("execute_command"));
        scoped.disable("execute_command", RestorePoint::TurnEnd);
        assert!(scoped.contains("execute_command"));
        scoped.restore_turn_end();
        assert!(
            !scoped.contains("execute_command"),
            "TurnEnd restore must re-enable the tool"
        );
        assert!(scoped.is_empty(), "both buckets drained");
    }

    /// `TurnEnd` restore clears the turn-scoped bucket only; `RoundEnd`
    /// disables survive until the user-round boundary.
    #[test]
    fn turn_end_restore_keeps_round_end_disables() {
        let mut scoped = ScopedToolDisable::default();
        scoped.disable("execute_command", RestorePoint::TurnEnd);
        scoped.disable("edit_text", RestorePoint::RoundEnd);
        scoped.restore_turn_end();
        assert!(
            !scoped.contains("execute_command"),
            "TurnEnd disable must be restored at the ReAct-turn boundary"
        );
        assert!(
            scoped.contains("edit_text"),
            "RoundEnd disable must survive the ReAct-turn boundary"
        );
    }

    /// Nested disables compose via refcount: two hooks disable `execute_command` at
    /// different restore points; the earlier (TurnEnd) restore must NOT bring
    /// it back while the later (RoundEnd) is still in effect.
    #[test]
    fn nested_disables_refcount_correctly() {
        let mut scoped = ScopedToolDisable::default();
        scoped.disable("execute_command", RestorePoint::RoundEnd);
        scoped.disable("execute_command", RestorePoint::TurnEnd);
        assert!(scoped.contains("execute_command"));
        scoped.restore_turn_end();
        assert!(
            scoped.contains("execute_command"),
            "execute_command still hidden: the RoundEnd disable outlives the TurnEnd restore"
        );
        scoped.restore_round_end();
        assert!(
            !scoped.contains("execute_command"),
            "execute_command back after round end"
        );
    }

    // skip_interactive_input wiring (ADR-0043 interactive-input opt-out)

    /// Minimal provider mock so an `Agent` can be constructed in unit tests
    /// without a live model. `decide_command_stdin` never reaches the provider, so
    /// the chat/stream impls are unreachable panics.
    struct NoopProvider;

    #[async_trait::async_trait]
    impl nuo_wire::Provider for NoopProvider {
        async fn chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            unreachable!("decide_command_stdin must not call the provider")
        }
        async fn stream_chat(
            &self,
            _: nuo_wire::ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            unreachable!("decide_command_stdin must not call the provider")
        }
    }

    fn stdin_test_agent() -> super::Agent {
        use std::sync::Arc;
        super::Agent::new(
            Arc::new(NoopProvider) as Arc<dyn nuo_wire::Provider>,
            vec![],
            nuo_wire::AgentIdentity::default(),
        )
    }

    /// A `sudo` command (matched by the interactive classifier) must, with
    /// `skip_interactive_input` on, be **sealed** and emit **no** `InputRequest`
    /// — the inline panel never pops. This is the opt-out's core contract and
    /// mirrors the delegated-autonomous path.
    #[tokio::test]
    async fn skip_interactive_input_seals_without_input_request() {
        use nuo_wire::AgentEvent;
        use tokio::sync::mpsc;
        let agent = stdin_test_agent();
        agent.set_unattended(false);
        agent.set_skip_interactive_input(true);

        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let contract = agent.decide_command_input(r#"{"command":"sudo ls /root"}"#);
        assert_eq!(
            contract,
            nuo_wire::InputContract::Sealed,
            "input must be sealed under skip_interactive_input"
        );
        assert!(
            rx.try_recv().is_err(),
            "no InputRequest must be emitted under skip_interactive_input"
        );
        let _ = tx;
    }

    /// Without the opt-out (and attended, on a platform that can fully
    /// supervise), the same `sudo` command must be **supervised** — carrying the
    /// classifier's advisory expectation. Regression guard: a refactor must not
    /// silently route the interactive path to `Sealed` when the opt-out is off.
    #[tokio::test]
    async fn interactive_input_path_supervises_when_opt_out_is_off() {
        let agent = stdin_test_agent();
        agent.set_unattended(false);
        agent.set_skip_interactive_input(false);

        let contract = agent.decide_command_input(r#"{"command":"sudo ls /root"}"#);
        if nuo_host::supervised::input_supervision()
            == nuo_host::supervised::InputSupervision::Supervised
        {
            match contract {
                nuo_wire::InputContract::Supervised { expectation: Some(exp) } => {
                    assert!(exp.secret, "sudo expectation must be masked");
                }
                other => panic!("expected Supervised with a secret expectation, got {other:?}"),
            }
        } else {
            // A platform without full supervision must fall back to Sealed —
            // never induce interactivity it cannot service.
            assert_eq!(contract, nuo_wire::InputContract::Sealed);
        }
    }

    /// `apply_preset` must seed `skip_interactive_input` from the
    /// profile's runtime config — the wiring the bootstrap path relies on.
    #[test]
    fn apply_preset_seeds_skip_interactive_input() {        let agent = stdin_test_agent();
        assert!(!agent.skip_interactive_input(), "default off");
        let profile = nuo_wire::AgentRoleProfile::with_identity(
            "developer",
            nuo_wire::AgentIdentity::default(),
        )
        .with_runtime_config(nuo_wire::AgentRuntimeConfig {
            skip_interactive_input: true,
            ..Default::default()
        });
        agent.apply_preset(&profile);
        assert!(
            agent.skip_interactive_input(),
            "profile overlay took effect"
        );
    }

    /// The capability gate is atomic and structural (ADR-0293): on a platform
    /// that is not fully `Supervised`, an interactive command can NEVER be
    /// dispatched as `Supervised` — it must fall back to `Sealed`. This is the
    /// invariant that prevents "arm a terminal without detection" (the macOS
    /// stall the seam removes).
    #[tokio::test]
    async fn non_supervised_platform_never_dispatches_supervised() {
        let agent = stdin_test_agent();
        agent.set_unattended(false);
        agent.set_skip_interactive_input(false);

        // Every interactive classification the classifier knows about.
        for command in ["sudo ls", "gpg --decrypt f", "vim x", "passwd", "less a"] {
            let args = serde_json::json!({ "command": command }).to_string();
            let contract = agent.decide_command_input(&args);
            if nuo_host::supervised::input_supervision()
                != nuo_host::supervised::InputSupervision::Supervised
            {
                assert_eq!(
                    contract,
                    nuo_wire::InputContract::Sealed,
                    "non-Supervised platform must seal '{command}'"
                );
            } else {
                assert!(
                    matches!(contract, nuo_wire::InputContract::Supervised { .. }),
                    "Supervised platform must supervise '{command}', got {contract:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn to_cognitive_agent_constructs_valid_nuo_agent() {
        let harness_agent = stdin_test_agent();
        let cognitive_agent = harness_agent
            .to_cognitive_agent()
            .await
            .expect("builds cognitive agent");
        assert_eq!(cognitive_agent.manifest().name, "agent");
    }
}
