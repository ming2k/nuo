//! Session snapshots, state deltas, and agent events.

use super::*;

/// Complete state snapshot of a live or persisted thread for client hydration (ADR-0049).
///
/// Since ADR-0133 this is the *only* hydration payload: `Wire::Welcome` carries
/// identity and phase, and this snapshot follows it on the same connection
/// before `ThreadPhase::Ready`. The fields below therefore duplicate nothing
/// from the handshake — a late attacher can rebuild the entire visible thread
/// from this frame alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub thread_id: String,
    pub round_counter: u64,
    pub messages: Vec<Message>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub todos: Vec<crate::todos::TodoItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<ContextTokenSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_state: Option<HarnessSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_picker: Option<ProviderPickerSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_keys: Option<Vec<(String, bool)>>,
    /// The durable command ledger, projected into the thread by the client.
    ///
    /// Every record here is an **act** (ADR-0140): a replication of the ledger's
    /// own residency contract, since a reading never reaches the ledger. The
    /// client joins these rows into the rendered thread at their timestamp seam;
    /// an act therefore appears in the conversation at the moment it happened,
    /// without being dialogue.
    #[serde(default)]
    pub commands: Vec<crate::command::CommandRecord>,
    /// Round-interrupt records at their timestamp seams.
    #[serde(default)]
    pub round_interrupts: Vec<RoundInterrupt>,
    /// Server-owned slash-command vocabulary for completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_catalog: Option<crate::CommandCatalog>,
    #[serde(default)]
    pub watermark_seq: u64,
}

/// Incremental state delta update (ADR-0049).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDelta {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<ContextTokenSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_seq: Option<u64>,
}

/// Steering operations a parent can submit into a running agent's inbox — the
/// down-direction of full-duplex (ADR-0029). Distinct from the request/reply
/// class ([`PermissionRequest`] / [`UserQuestionRequest`]), which resolve
/// instantly via the agent's shared-state oneshots (`reply_permission` /
/// `reply_user_question`) and therefore do **not** flow through this queue: a
/// reply must unblock a tool that is parked mid-turn, so it cannot wait for
/// the driver loop to drain. This enum covers only the "new input / control"
/// class that is safe to apply at the next ReAct-turn boundary.
///
/// Modeled on codex's `Op` (`codex-rs/protocol/src/protocol.rs`), trimmed to
/// nuo's driver shape: the agent owns an `mpsc` inbox whose receiver is
/// drained at the top of every ReAct turn (and, for `Interrupt`, raced against
/// the live stream). The top-level agent and spawned subagents share the same
/// `Op` vocabulary — a subagent is just an agent whose inbox sender the
/// parent holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentOp {
    /// Append a visible user message to the live transcript before the next
    /// model request, as if the user typed it. Lets a parent (or, for a
    /// subagent, the orchestrating agent) steer a running round with new
    /// information without restarting it. codex `inject_if_running` analogue.
    Steer(String),
    /// Append a hidden (system-level) steering note — like
    /// [`AgentOp::Steer`] but recorded as a hidden user message so
    /// it informs the model without polluting the visible transcript. codex
    /// `InterAgentCommunication` analogue.
    InterAgentMessage { msg: String },
    /// Abort the current round at the next boundary. Coarser than the parent's
    /// `CancellationToken` (which cancels instantly): this is the
    /// handle-addressable path for a caller that owns the inbox but not the
    /// cancel token. codex `Op::Interrupt` analogue.
    Interrupt,
    /// Tear the agent down (interrupt + signal that the shutdown was
    /// requested rather than cancelled). codex `Op::Shutdown` analogue.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Notice(AgentNotice),
    ModelRequestStarted {
        /// 1-indexed enclosing user round.
        round: u64,
        /// 0-indexed model-request position within `round`.
        turn: usize,
        /// Semantic estimate of the exact request projection after turn-start
        /// hooks and immediately before it is sent to the provider.
        context_tokens: usize,
    },
    /// A provider request completed and its high-resolution performance
    /// sample was sealed before any tool dispatch.
    TurnPerformance(crate::TurnPerformanceSnapshot),
    /// Provider-reported context after a completed request. This supersedes the
    /// pre-request projection for that session until its context mutates again.
    ContextTokens(ContextTokenSnapshot),
    /// A steering message was atomically admitted at a safe turn boundary.
    /// The context is the turn that will consume it (the next model request),
    /// not the turn that just finished; the frontend must not infer `turn + 1`.
    SteerAdmitted {
        input: QueuedMessage,
        context: TurnContext,
    },
    AssistantDelta {
        delta: String,
        start: bool,
    },
    AssistantEnd(String),
    /// The assistant text streamed for the current turn is retracted: a
    /// text-fallback tool call consumed it, or the attempt was abandoned. The
    /// turn identity comes from the relay's [`TurnContext`]; the internal event
    /// carries no duplicate position.
    AssistantDiscard,
    ReasoningDelta {
        delta: String,
        start: bool,
    },
    ReasoningEnd(String),
    ToolCall {
        /// The call's single identity (ADR-0166) — the same id its
        /// `ToolCallStarted` announcement carried and every later event about
        /// this call will carry.
        id: String,
        name: String,
        arguments: String,
    },
    /// ADR-0166: an attempt ended before its announced calls could be
    /// dispatched (provider error, truncated stream, loop cut, replaced
    /// attempt). Each id names a provisional step that will never become a
    /// call, so the frontend must **retract** the rows — not cancel them: a
    /// cancellation asserts that a call existed and was stopped, which would
    /// be a lie. A retraction removes a row only while it is still
    /// `Ingesting`; a call that dispatched is real and survives.
    ToolCallsRetracted {
        ids: Vec<String>,
    },
    /// A **pure** preview of what a call will do, emitted strictly before any
    /// execution (ADR-0125).
    ///
    /// Display-only and deliberately non-durable, exactly like
    /// `ToolCall`: the authoritative record of a change is the workspace
    /// journal's completed row, not an event that described an intention. It
    /// exists so a frontend can show the turn's whole footprint before anything
    /// runs - a batch of writes produces one visible set of diffs rather than N
    /// separable approvals.
    ///
    /// A frontend that ignores it loses only the ahead-of-time view; the preview
    /// is also attached to any permission request it gives rise to, so a gated
    /// call still shows its change at the moment of decision. A frontend must
    /// **never** treat a preview as evidence that a change happened: no bytes are
    /// written when this arrives.
    ToolPreview {
        /// The dispatch id, matching the `ToolCall` it describes.
        id: String,
        name: String,
        preview: crate::preview::PreviewResult,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: ToolOutput,
        duration_ms: u64,
    },
    /// Incremental output streamed by a running tool (see [`ToolStream`]).
    ToolStream {
        id: String,
        stream: ToolStream,
    },
    ToolCancelled {
        id: String,
        name: String,
    },
    /// A tool call whose **name is known but whose arguments are still
    /// streaming** (ADR-0026). Announced as soon as the provider names the
    /// call, strictly before the arguments finish, so a frontend can create a
    /// running step and move the activity phase to the tool verb instead of
    /// showing the misleading `answering` phase while the model plans a call.
    ///
    /// ADR-0166: `id` is the call's **single identity** — the same value the
    /// later `ToolCall`, `ToolInputProgress`, `ToolStream`, `ToolPreview`,
    /// `ToolResult`, and `ToolCallsRetracted` events carry. It is minted by the
    /// harness at this announcement when the provider supplied none, and it is
    /// never re-keyed afterwards.
    ToolCallStarted {
        /// The call's identity for every audience: the model-visible
        /// `tool_call_id` and every UI event about this call.
        id: String,
        name: String,
    },
    /// ADR-0026 / ADR-0046: Tool call arguments input progress and streaming partial projection.
    ToolInputProgress {
        /// The call this tick belongs to (ADR-0166: the call id).
        id: String,
        bytes: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<String>,
    },
    /// The task list changed (`todo` / `todo_update`). The TUI uses this to refresh the
    /// unified sticky panel above the input box.
    TodosUpdated(crate::todos::TodoList),
    /// The unattended execution toggle changed (via `/unattended`).
    UnattendedChanged(bool),
    /// The workspace confinement toggle changed (via `/confinement`).
    ConfinementChanged(bool),
    PermissionRequest(PermissionRequest),
    UserQuestionRequest(UserQuestionRequest),
    /// An interactive `bash` command needs a line of stdin from the operator.
    /// The TUI shows an inline input panel; the reply travels back
    /// as [`AgentRequest::StdinReply`].
    #[serde(alias = "InputRequest")]
    StdinRequest(StdinRequest),
    /// An subagent spawned by a tool (e.g. `task`) emitted an event.
    Subagent {
        parent_call_id: String,
        event: SubagentEvent,
    },
    /// A background process or sub-subagent job started.
    BackgroundJobStarted(crate::job::BackgroundJobInfo),
    /// Incremental progress or output line from a background job.
    BackgroundJobProgress {
        job_id: crate::job::JobId,
        line: String,
    },
    /// A service task reported readiness. Wake-eligible.
    BackgroundJobReady {
        job_id: crate::job::JobId,
    },
    /// A background job completed.
    BackgroundJobCompleted(crate::job::BackgroundJobOutcome),
    /// Remote catalog was updated; attached frontends should refresh provider picker.
    CatalogInvalidated,
}
