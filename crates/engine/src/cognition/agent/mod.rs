//! The [`Agent`] orchestration type and its supporting machinery.
//!
//! The type definition, builder, subagent handle, queue plumbing, the free
//! helper functions shared across the split, and the embedded tests. The
//! `impl Agent` blocks are split by concern into sibling modules:
//! `state` (configuration/identity), `steering` (rounds/queues/interrupts),
//! `tools_admin` (permissions/catalog), `rounds` (streaming loop), and
//! `execution` (tool tail + hooks).
use crate::cognition::skills;
use crate::*;
use protocol::human_request::{
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
    ::tool::tool_result_header::render_with_note(name, summary, reanchor)
}

// `ScopedToolDisable` is a governance-layer type (`[INV-HARNESS-03]`, ADR-0100).
// Re-exported here so the agent's submodules (`use super::*`) and the dispatch
// tool manager keep their existing paths without the agent owning it.
pub(crate) use guardrails::ScopedToolDisable;

/// Mid-turn save-point closure installed by orchestration (ADR-0048).
///
/// Invoked at each ReAct-turn boundary with the current full round history.
/// The implementation diffs against its own durable baseline and appends only
/// the new tail to the session event log (see `ThreadSession::append_turn`).
/// Errors are surfaced back to the ReAct loop, which treats a persist failure
/// as a round-ending error (better to stop than to keep mutating state that may
/// not be recoverable).
pub(crate) type TurnPersistFn =
    Arc<dyn Fn(&[Message]) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Invoked once per freshly assembled model request with its
/// request-projection record. The runtime installs a closure that enqueues the
/// record to the session's forensic archive. It is synchronous and infallible
/// by design: forensic persistence must never block or fail request dispatch.
/// It is `None` on subagents, the review diagnostic, and tests.
pub(crate) type RequestProjectionFn = Arc<dyn Fn(protocol::RequestProjection) + Send + Sync>;

/// Title-established observer fired by the background session titler
/// (ADR-0022).
///
/// The titler lives in `nuo-harness` and cannot reach the runtime's response
/// channel, so the driver installs a closure that pushes a fresh
/// `SessionsOverview` snapshot (and thereby republishes the monitor row, so
/// every attached client's picker title updates live). Fired at most once per
/// session — a non-`NULL` title is terminal. Errors and absent
/// observers are non-fatal: the title is already durably persisted when this
/// fires, so the notification is pure presentation.
pub type TitleEstablishedFn = Arc<dyn Fn(&str) -> BoxFuture<'static, ()> + Send + Sync>;

pub use protocol::RequestTokenEstimate;

// `AgentIdentity` now lives in `protocol` (`identity.rs`) as pure domain
// vocabulary, alongside the role profiles. It is re-exported by name at the
// crate root below and via `pub use protocol::*`, so all existing
// `engine::AgentIdentity` / `crate::AgentIdentity` references keep
// resolving unchanged.

/// Parked oneshots for in-flight interactive-input requests (L3.5 β): a
/// `bash` command classified interactive blocks here until the operator's
/// [`InputReply`](protocol::InputReply) arrives (or `None` on cancel/turn-end).
pub struct Agent {
    pub host: crate::runtime::host::KernelHost,
    pub provider: Arc<dyn Provider>,
    pub(crate) identity: std::sync::RwLock<crate::AgentIdentity>,
    pub(crate) tools: substate::ToolState,
    pub(crate) context: substate::ContextState,
    pub(crate) control: substate::ControlState,
    pub(crate) queues: substate::QueueState,
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

    /// Abort this child's in-flight round at its next safe boundary
    /// ([`AgentOp::Interrupt`]). Returns `false` if the agent has been dropped
    /// or no inbox was ever installed. This is the **scene-scoped** interrupt
    /// verb: it stops this child only, leaving the parent round and
    /// every sibling subagent untouched — the handle counterpart to the
    /// [`AgentRequest::InterruptSubagent`] wire request, whose round loop then
    /// returns its partial transcript so the parent records it as an
    /// interrupted (resumable) result rather than a failure.
    ///
    /// Mirrors the primary interrupt's eager human-channel drain: a child parked
    /// on a permission / `ask_user` / stdin request is unblocked first (its
    /// parked oneshot resolves as cancelled), so the inbox op is observed
    /// promptly instead of waiting behind a gate that would never clear.
    ///
    /// [`AgentRequest::InterruptSubagent`]: protocol::AgentRequest::InterruptSubagent
    pub fn interrupt(&self) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reject_pending_permissions();
            agent.reject_pending_user_questions();
            agent.reject_pending_inputs();
        }
        self.submit(AgentOp::Interrupt)
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
    /// Cost already settled for the requests that completed in this round.
    ///
    /// The Session budget gate reads this instead of re-pricing
    /// `token_usage`: once rates vary by time of day, applying one window to
    /// an aggregate of attempts billed under another silently mis-states the
    /// spend (ADR-0080).
    spend: protocol::pricing::NanoUsd,
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
    pub(crate) guards: guardrails::guard::RoundGuardState,
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
    fn guards_default(config: tool::TrajectoryGuardConfig) -> guardrails::guard::RoundGuardState {
        guardrails::guard::RoundGuardState::new().with_trajectory(
            guardrails::trajectory_guard::TrajectoryLoopGuard::new(config),
        )
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
    turn_context: Arc<protocol::ProviderTurnContext>,
    state: RoundState,
    turn_index: usize,
    /// Number of in-flight stream-loop recoveries already attempted in this
    /// user round. The first detected loop gets one guided retry; a recurrence
    /// is a hard stop. This state survives transient provider retries with the
    /// rest of the round checkpoint.
    stream_loop_recoveries: u8,
    inbox_rx: Option<mpsc::UnboundedReceiver<AgentOp>>,
    started_at: std::time::Instant,
    pending_request: Option<protocol::ModelRequest>,
    /// Whether this round's requests must withhold inline attachments.
    ///
    /// Set when the vision-cause probe arms, and **kept** for the rest of the
    /// round: the probe's evidence is the outcome of a request that already
    /// dropped the images, so the next turn must not silently re-add them.
    /// Without this, a successful probe turn was followed by a turn carrying
    /// the same attachments, which failed again and made the harness report
    /// "retrying without images did not help" about a hypothesis it had just
    /// confirmed.
    withhold_images: bool,
    session_queue_generation: Option<u64>,
}

impl StreamingRoundState {
    pub(crate) fn token_usage(&self) -> tool::TokenUsage {
        self.state.token_usage
    }

    /// Apply `project` to the checkpointed request of the current turn, if one
    /// is armed. Returns `false` when no request is checkpointed (the turn had
    /// not reached assembly yet, so the next attempt assembles afresh).
    ///
    /// This is the narrow way to change *what is sent* on a retry without
    /// invalidating the checkpoint: the request stays the same turn's
    /// projection — same history, same tool schemas, same accounting — and only
    /// the projected field changes. It exists for the image-input recovery
    /// a provider that rejects an attachment must be retried without
    /// it, and clearing `pending_request` instead would re-run the turn's
    /// preparation and TurnStart hooks, which the checkpoint exists to prevent.
    ///
    /// The durable request-projection archive is written once per
    /// logical invocation from the freshly assembled request, so a projection
    /// applied here intentionally post-dates — and therefore differs from — that
    /// record; the withholding is reported to the user and the log instead.
    pub(crate) fn project_pending_request(
        &mut self,
        project: impl FnOnce(&mut protocol::ModelRequest) -> usize,
    ) -> Option<usize> {
        self.pending_request.as_mut().map(|request| {
            let changed = project(request);
            if changed > 0 {
                // The cacheable-prefix identity must describe the bytes this
                // attempt will actually send.
                request.recompute_revisions();
            }
            changed
        })
    }

    /// The request this turn has checkpointed, if assembly has run.
    pub(crate) fn pending_request(&self) -> Option<&protocol::ModelRequest> {
        self.pending_request.as_ref()
    }

    /// Whether this round is running with attachments withheld.
    pub(crate) fn withholds_images(&self) -> bool {
        self.withhold_images
    }

    /// Arm or clear attachment withholding for the rest of this round.
    ///
    /// Arming is the probe's hypothesis made operational: every later turn of
    /// the round is projected without attachments, so one confirmed success
    /// cannot be undone by the next turn re-sending them.
    pub(crate) fn set_withhold_images(&mut self, withhold: bool) {
        self.withhold_images = withhold;
    }

    /// Replace the checkpointed request with a fresh projection of the window.
    ///
    /// Used by the payload recovery: the window changed (attachments left the
    /// projection), so the checkpoint must describe the bytes the retry will
    /// send — without re-running the turn's preparation or its `TurnStart`
    /// hooks, which is what the checkpoint exists to prevent.
    pub(crate) fn reproject_pending_request(&mut self, mut request: protocol::ModelRequest) {
        request.turn_context = Arc::clone(&self.turn_context);
        request.recompute_revisions();
        self.pending_request = Some(request);
    }
}

impl StreamingRoundState {
    /// How many complete ReAct turns this round has committed — the ordinal
    /// the *next* turn would take (0-based `turn_index`). `/retry` captures
    /// this into a [`protocol::RetryPoint`] so the resumed round
    /// keeps numbering turns contiguously instead of restarting at 0.
    pub(crate) fn committed_turns(&self) -> usize {
        self.turn_index
    }

    #[cfg(test)]
    pub(crate) fn set_token_usage_for_test(&mut self, usage: tool::TokenUsage) {
        self.state.token_usage = usage;
    }
}

/// A queue of pending messages controlled by a [`protocol::QueueMode`].
#[derive(Debug, Clone)]
pub struct PendingMessageQueue {
    messages: std::collections::VecDeque<protocol::QueuedMessage>,
    pub mode: protocol::QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: protocol::QueueMode) -> Self {
        Self {
            messages: std::collections::VecDeque::new(),
            mode,
        }
    }

    pub fn enqueue(&mut self, message: protocol::QueuedMessage) {
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

    pub fn drain(&mut self) -> Vec<protocol::QueuedMessage> {
        match self.mode {
            protocol::QueueMode::All => self.messages.drain(..).collect(),
            protocol::QueueMode::OneAtATime => self.messages.pop_front().into_iter().collect(),
        }
    }

    pub fn drain_all(&mut self) -> Vec<protocol::QueuedMessage> {
        self.messages.drain(..).collect()
    }

    pub fn cancel(&mut self, input_id: &str) -> Option<protocol::QueuedMessage> {
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

pub(crate) struct SessionQueues {
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
    ledger: Option<Arc<protocol::TokenSourceLedger>>,
    key: Option<protocol::RequestUsageKey>,
    provider: String,
    model: String,
    round: u64,
    turn: u32,
    attempt: u32,
    cancel: CancellationToken,
    projected_prompt_tokens: i64,
    observed_completion_tokens: i64,
    /// Incremental BPE counter for streamed deltas (exact across delta
    /// boundaries; a per-delta sum would over-count merges that span them).
    output_counter: tool::tokenizer::StreamingCounter,
    observed_usage: Option<TokenUsage>,
    error: Option<String>,
    settled: bool,
    /// Monotonic performance anchors. The request clock starts at the actual
    /// provider-call boundary, after local projection/events. Stream events
    /// are sampled as the provider stream yields them; missing stages remain
    /// `None` rather than becoming fabricated zero-duration measurements.
    started_at: Option<std::time::Instant>,
    /// Epoch-millisecond twin of the dispatch boundary. Stamped once here,
    /// handed to the ledger record, and reused for cost rating so the durable
    /// attempt and the rate window it was billed under can never disagree
    /// (ADR-0080).
    started_at_epoch_ms: u64,
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
    /// This attempt's telemetry handle. Created here, stamped onto
    /// the request this attempt dispatches, and read back when it settles — so
    /// the transport's timings reach the attempt that caused them, including
    /// when a shared provider is running several attempts at once.
    transport_telemetry: protocol::TransportTelemetry,
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
        let started_at_epoch_ms = crate::runtime::time::unix_epoch_ms();
        let key = ledger.as_ref().map(|ledger| {
            let thread_id = agent.thread_id().unwrap_or_default();
            let actor_id = agent.accounting_actor_id();
            ledger.begin_request_for_actor(protocol::BeginRequestParams {
                session_id: &thread_id,
                actor_id: &actor_id,
                provider,
                model,
                round: agent.round_count(),
                turn: turn_index.saturating_add(1) as u32,
                projected_prompt_tokens: projected_prompt_tokens as i64,
                started_at_ms: started_at_epoch_ms,
            })
        });
        let round = agent.round_count();
        let turn = turn_index.saturating_add(1) as u32;
        let attempt = key.as_ref().map_or(1, |key| key.attempt);
        Self {
            ledger,
            key,
            provider: provider.to_string(),
            model: model.to_string(),
            round,
            turn,
            attempt,
            cancel: cancel.clone(),
            projected_prompt_tokens: projected_prompt_tokens as i64,
            observed_completion_tokens: 0,
            output_counter: tool::tokenizer::StreamingCounter::new(),
            observed_usage: None,
            error: None,
            settled: false,
            started_at: None,
            started_at_epoch_ms,
            stream_ready_at: None,
            first_output_at: None,
            last_output_at: None,
            stream_end_at: None,
            validated_at: None,
            first_output_fragment: String::new(),
            output_events: 0,
            generation_ms: 0,
            first_frame_at: None,
            transport_telemetry: protocol::TransportTelemetry::new(),
        }
    }

    /// This attempt's telemetry handle, to be stamped onto the request the
    /// attempt dispatches. A retry must take a fresh one.
    fn transport_telemetry(&self) -> protocol::TransportTelemetry {
        self.transport_telemetry.clone()
    }

    /// Start the monotonic request clock at the provider-call boundary.
    fn start_request(&mut self) {
        self.started_at.get_or_insert_with(std::time::Instant::now);
    }

    /// The epoch-millisecond dispatch boundary this attempt is rated at.
    fn started_at_epoch_ms(&self) -> u64 {
        self.started_at_epoch_ms
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
        event: &protocol::ProviderStreamEvent,
        received_at: std::time::Instant,
    ) {
        let mut fragments: Vec<&str> = Vec::new();
        // Any event from the origin — including a preamble or usage frame —
        // proves the server started responding.
        self.first_frame_at.get_or_insert(received_at);
        match event {
            protocol::ProviderStreamEvent::ModelCatalogEtag(_) => return,
            protocol::ProviderStreamEvent::TextDelta(delta)
            | protocol::ProviderStreamEvent::ReasoningDelta(delta) => {
                if !delta.is_empty() {
                    fragments.push(delta);
                }
            }
            protocol::ProviderStreamEvent::ToolCallDelta {
                name, arguments, ..
            } => {
                if let Some(name) = name.as_deref().filter(|name| !name.is_empty()) {
                    fragments.push(name);
                }
                if !arguments.is_empty() {
                    fragments.push(arguments);
                }
            }
            protocol::ProviderStreamEvent::Usage(usage) => {
                self.observe_usage(*usage);
                return;
            }
            protocol::ProviderStreamEvent::Completed(meta) => {
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

    fn performance(&self) -> protocol::RequestPerformance {
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
        protocol::RequestPerformance {
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
            first_output_tokens: tool::count_tokens(&self.first_output_fragment) as u64,
            output_events: self.output_events,
            timing_source: protocol::PerformanceTimingSource::ClientObserved,
            stream_token_source: protocol::StreamTokenSource::Cl100k,
            observation: transport.observation,
            ..Default::default()
        }
    }

    fn performance_snapshot(
        &self,
        completion_tokens: i64,
        reported_usage: Option<&TokenUsage>,
        usage_source: protocol::RequestUsageSource,
    ) -> protocol::TurnPerformanceSnapshot {
        let prompt_tokens = reported_usage
            .map(|usage| usage.prompt_tokens)
            .unwrap_or(self.projected_prompt_tokens);
        let cache_read_tokens = reported_usage
            .map(|usage| usage.cache_read_input_tokens)
            .unwrap_or(0);
        let cache_write_tokens = reported_usage
            .map(|usage| usage.cache_creation_input_tokens)
            .unwrap_or(0);
        let total_tokens = reported_usage
            .map(|usage| usage.total_tokens)
            .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
        let rated_usage = tool::usage::TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_creation_input_tokens: cache_write_tokens,
            cache_read_input_tokens: cache_read_tokens,
            cache_miss_input_tokens: reported_usage
                .map(|usage| usage.cache_miss_input_tokens)
                .unwrap_or(0),
            reasoning_tokens: reported_usage
                .map(|usage| usage.reasoning_tokens)
                .unwrap_or(0),
        };
        let cost_nano_usd = protocol::pricing::resolve_model_pricing(&self.provider, &self.model)
            .map(|pricing| {
                pricing
                    .calculate_cost(
                        &rated_usage,
                        protocol::pricing::RatingInstant::from_epoch_ms(self.started_at_epoch_ms),
                    )
                    .amount
                    .0
            });
        protocol::TurnPerformanceSnapshot {
            round: self.round,
            turn: self.turn,
            attempt: self.attempt,
            completion_tokens: completion_tokens.max(0) as u64,
            prompt_tokens: prompt_tokens.max(0) as u64,
            cache_read_tokens: cache_read_tokens.max(0) as u64,
            cache_write_tokens: cache_write_tokens.max(0) as u64,
            cost_nano_usd,
            usage_source,
            performance: self.performance(),
        }
    }

    fn settle(
        &mut self,
        status: protocol::RequestUsageStatus,
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
        status: protocol::RequestUsageStatus,
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
            protocol::RequestUsageStatus::Interrupted
        } else {
            protocol::RequestUsageStatus::Failed
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
    host: crate::runtime::host::KernelHost,
    toolset: protocol::ToolSet,
    skills_registry: skills::SkillRegistry,
    identity: AgentIdentity,
    /// Declared context sources. Compiled at [`Self::build`]; every
    /// builder-configured composition is validated before it gets there.
    context_sources: Vec<Box<dyn crate::ContextSource + Send + Sync>>,
    user_instructions: Vec<protocol::ScopedInstruction>,
    response_language: Option<protocol::ResponseLanguage>,
    install_agent_owned_tools: bool,
}

impl AgentBuilder {
    fn new(
        provider: Arc<dyn Provider>,
        toolset: protocol::ToolSet,
        identity: AgentIdentity,
    ) -> Self {
        Self {
            provider,
            toolset,
            host: crate::runtime::host::KernelHost::none(),
            skills_registry: skills::SkillRegistry::empty(),
            identity,
            context_sources: crate::cognition::context::default_sources(),
            user_instructions: Vec::new(),
            response_language: None,
            install_agent_owned_tools: true,
        }
    }

    /// Skip installing agent-owned `todo` tools (used for subagent profiles
    /// whose policy does not admit todo state).
    pub fn without_agent_owned_tools(mut self) -> Self {
        self.install_agent_owned_tools = false;
        self
    }

    /// Supply the host's ports: declared roles and per-project path policy.
    pub fn with_host(mut self, host: crate::runtime::host::KernelHost) -> Self {
        self.host = host;
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

    /// Add an embedding-owned context source to the default policy.
    ///
    /// Validates the whole declaration set immediately, so a duplicate topic, a
    /// missing anchor, or a cross-band anchor is reported here rather than at
    /// agent construction (ADR-0215).
    pub fn add_context_source<S: crate::ContextSource + 'static>(
        mut self,
        source: S,
    ) -> Result<Self, crate::ContextPolicyError> {
        self.context_sources.push(Box::new(source));
        match crate::cognition::context::Policy::compile(self.context_sources) {
            Ok(policy) => {
                self.context_sources = policy.into_sources();
                Ok(self)
            }
            Err(error) => Err(error),
        }
    }

    /// Replace the default context composition wholesale.
    ///
    /// Validated on the spot: a composition that cannot be compiled never reaches
    /// an agent.
    pub fn with_context_sources(
        mut self,
        sources: Vec<Box<dyn crate::ContextSource + Send + Sync>>,
    ) -> Result<Self, crate::ContextPolicyError> {
        let policy = crate::cognition::context::Policy::compile(sources)?;
        self.context_sources = policy.into_sources();
        Ok(self)
    }

    /// Remove a context source by its stable id.
    ///
    /// This is the break-clean replacement for the retired
    /// `disable_instruction_section`: disabling is a policy mutation, so it must
    /// happen before the policy is compiled.
    pub fn disable_context_source(mut self, id: &str) -> Result<Self, crate::ContextPolicyError> {
        let position = self
            .context_sources
            .iter()
            .position(|source| source.id() == id)
            .ok_or(crate::ContextPolicyError::UnknownSource(id.to_owned()))?;
        self.context_sources.remove(position);
        Ok(self)
    }

    /// Set initial user-authored scoped instructions (ADR-0165).
    pub fn with_user_instructions(
        mut self,
        instructions: Vec<protocol::ScopedInstruction>,
    ) -> Self {
        self.user_instructions = instructions;
        self
    }

    /// Declare the language the agent answers in (ADR-0222).
    ///
    /// Builder-shaped like [`Self::with_user_instructions`] so an embedding that
    /// constructs its policy wholesale declares the language the same way it
    /// declares everything else. The value is already validated
    /// ([`protocol::ResponseLanguage`]), so `build()` has nothing to reject.
    pub fn with_response_language(mut self, language: Option<protocol::ResponseLanguage>) -> Self {
        self.response_language = language;
        self
    }

    /// Freeze the configuration and construct the agent.
    pub fn build(self) -> Agent {
        // Infallible by construction. `add_context_source` / `with_context_sources`
        // validate every edit through `Policy::compile` and hand back an
        // already-ordered set, and the compiled-in default is pinned by a test,
        // so nothing is left to prove here. No request path — and no constructor —
        // may fail on a layout condition (`[INV-CTXSRC-05]`).
        let policy = crate::cognition::context::Policy::from_declared(self.context_sources);
        let mut agent = Agent::from_toolset_with_model_request_assembler(
            self.provider,
            self.toolset,
            self.skills_registry,
            self.identity,
            crate::cognition::model_request::ModelRequestAssembler::new(policy),
            self.install_agent_owned_tools,
        );
        agent.host = self.host;
        if !self.user_instructions.is_empty() {
            agent.set_user_instructions(self.user_instructions);
        }
        if self.response_language.is_some() {
            agent.set_response_language(self.response_language);
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
mod substate;
pub use steering::SwitchedRole;
mod tools_admin;

pub(crate) use rounds::ToolResultRecord;
pub(crate) use state::{payload_relief_notice, strip_images};

/// Render a missing runtime grant without conflating it with project asset
/// trust. This is returned when no interactive approver is available.
fn permission_required_output(request: &tool::PermissionRequest) -> ToolOutput {
    use tool::ToolPermissionPayload;

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
/// by the provider at `protocol::provider=debug`.
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
        target: "protocol::agent",
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
        protocol::ProviderError::new(
            response.provider.as_deref().unwrap_or("harness"),
            protocol::ProviderErrorKind::Upstream,
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
impl guardrails::permission_policy::PermissionContext for Agent {
    async fn check_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> crate::runtime::hooks::PreToolUseVerdict {
        self.hooks()
            .check_pre_tool_use(
                tool_name,
                tool_input,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await
    }

    fn apply_scoped_disables(&self, disables: &[(String, protocol::RestorePoint)]) {
        // Delegate to the existing agent method (same signature).
        Agent::apply_scoped_disables(self, disables);
    }

    async fn check_bash_policy(
        &self,
        command: &str,
        _arguments: &str,
    ) -> guardrails::permission_policy::BashVerdict {
        // The single source of truth for the chain's BashPolicy gate. Returns
        // a disjoint Allow / Confirm / Deny verdict so the gate can decide
        // everything (including the interactive confirm) without the caller
        // re-evaluating outside the chain.
        let policy = self
            .control
            .bash_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(decision) = policy.evaluate(command) else {
            return guardrails::permission_policy::BashVerdict::Allow;
        };
        match decision.action {
            guardrails::bash_policy::BashPolicyAction::Deny => {
                tracing::warn!(command = %command, rule = %decision.name, "bash command blocked by policy");
                guardrails::permission_policy::BashVerdict::Deny {
                    output: decision.blocked_output(command),
                }
            }
            guardrails::bash_policy::BashPolicyAction::Confirm => {
                guardrails::permission_policy::BashVerdict::Confirm { match_: decision }
            }
            guardrails::bash_policy::BashPolicyAction::Allow => {
                guardrails::permission_policy::BashVerdict::Allow
            }
        }
    }

    fn permissions(&self) -> &guardrails::permission_store::PermissionStore {
        &self.context.permissions
    }
}
#[cfg(test)]
mod tests;
