//! Round lifecycle types: phases, scopes, interrupts, summaries, and events.

use super::*;

/// The lifecycle of a claimed thread: existence is not readiness.
///
/// `[INV-ATTACH-04]` (ADR-0133): a client MUST NOT submit a turn before
/// [`Self::Ready`], and the authoritative snapshot (plus the workspace-trust
/// snapshot it carries) MUST arrive before `Ready`. Failure after Welcome is a
/// phase — not a disconnect — and MUST NOT terminate the client (extends
/// `[INV-ATTACH-02]` to post-handshake assembly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ThreadPhase {
    /// Identity claimed; no store or harness exists yet. The server sends this
    /// in the Welcome frame.
    Binding,
    /// The durable thread store is being opened or resumed.
    Loading,
    /// Local harness assembly is in progress.
    Preparing { stage: PrepareStage },
    /// The thread is usable: snapshot delivered, local harness assembled.
    /// Network work (OAuth, remote skills, MCP, catalog) may still be running
    /// and MUST NOT be a prerequisite of this state.
    Ready,
    /// Assembly failed after the handshake. `code` carries the machine-readable
    /// taxonomy (`[INV-ATTACH-01]`), e.g. `thread_unresumable`.
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        message: String,
    },
}

/// A readiness transition for one claimed thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadPhaseUpdate {
    pub thread_id: String,
    pub phase: ThreadPhase,
}

/// The position and send time of one ReAct turn.
///
/// `round` is the 1-indexed enclosing user round. `turn` is the **0-indexed**
/// model-request position within that round, matching
/// [`RoundEvent::TurnStarted`]. `sent_at_ms` is the server's wall-clock stamp
/// for the turn; every component of the turn reads this one value, so a
/// thinking block, answer text and tool step cannot disagree about the band
/// header even when they arrive at different times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnContext {
    pub round: u64,
    pub turn: u64,
    pub sent_at_ms: u64,
}

impl TurnContext {
    pub fn new(round: u64, turn: u64, sent_at_ms: u64) -> Self {
        Self {
            round,
            turn,
            sent_at_ms,
        }
    }
}

/// Whether a round event belongs to one ReAct turn.
///
/// A turn-scoped event carries its own [`TurnContext`] on the wire. A consumer
/// never reconstructs the position from a `TurnStarted` it may or may not have
/// observed; re-attachment, replay and late joiners all read the same value.
/// Events that belong to the session or the round as a whole use
/// [`RoundScope::Session`] and cannot accidentally claim a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoundScope {
    /// The event mutated a specific turn's transcript state.
    Turn {
        context: TurnContext,
        event: RoundEvent,
    },
    /// The event belongs to the session/round, not to one turn. It creates no
    /// positioned transcript row.
    Session { event: RoundEvent },
}

impl RoundScope {
    /// The event, regardless of scope. For consumers that do not need turn
    /// identity (monitors, headless text extraction).
    pub fn event(&self) -> &RoundEvent {
        match self {
            Self::Turn { event, .. } | Self::Session { event } => event,
        }
    }

    /// Consume the scope and return the event.
    pub fn into_event(self) -> RoundEvent {
        match self {
            Self::Turn { event, .. } | Self::Session { event } => event,
        }
    }
}

/// A durable record of one round being stopped before its natural terminal
/// path (`RoundEvent::RoundCompleted`). Written by the harness whenever a
/// round unwinds through an interrupt — user-requested, superseded by newer
/// input, or killed with its host process — so a resumed session can show
/// *that and why* the round stopped, at the moment it stopped.
///
/// This is a **projection record, not a conversation message**: it never
/// enters `model_window` / `archived_transcript`, never reaches the model,
/// and never costs context tokens (the deliberate decision documented in
/// `docs/explanation/interrupt-semantics.md` — omission stays the
/// model-facing signal). It rides in the session store beside the command
/// ledger and, like the ledger, is re-projected into the transcript on
/// resume by timestamp seam.
///
/// `at_ms` lives on the payload (not the event-log envelope) because log
/// compaction rewrites the `.jsonl` and drops every envelope timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundInterrupt {
    /// The interrupt's cause, as observed at the stop site. Maps 1:1 to the
    /// user-facing vocabulary; see [`RoundInterruptReason`].
    pub reason: RoundInterruptReason,
    /// Unix-epoch milliseconds at which the stop was recorded.
    pub at_ms: u64,
    /// The 1-based round that was stopped, when known. `None` when no
    /// agent-side counter was available at the stop site — e.g. the
    /// phase-1 unsend (the round was rewound and its counter restored
    /// before the record was written) and records synthesized for a round
    /// the process abandoned at exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// Optional error payload or stop detail (e.g. fatal provider error message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl RoundInterrupt {
    /// The user-facing label for this record, e.g. `"Esc Esc"`,
    /// `"new message"`, `"process exited"`, `"error"`. One vocabulary shared by the
    /// TUI, the Web app, and headless output.
    pub fn label(&self) -> &'static str {
        self.reason.label()
    }
}

/// Why a round stopped before completing. The closed classifier for
/// [`RoundInterrupt::reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundInterruptReason {
    /// The user explicitly stopped the round: double-Esc in the thread
    /// view, or the equivalent control-plane `Interrupt` request. Interrupt semantics documentation calls this
    /// the plain "interrupt" path — the round unwinds and emits its own
    /// cleanup because the generation is not bumped.
    User,
    /// A newer round replaced this one: the user sent a new message (or a
    /// `!shell` command / scheduled follow-up) while this round was still
    /// live, or switched sessions (`/resume`, `/session open|fork|new`).
    /// The stale round's own cleanup is generation-suppressed, which before
    /// this record existed left no trace at all.
    Superseded,
    /// The host process terminated with the round still in flight — a
    /// server stop (signal, control verb, or kill), a TUI signal exit, or a
    /// crash. Inferred on load when a recorded interrupt's round never
    /// completed and no terminal interrupt was recorded for it.
    Terminated,
    /// The round ended in a fatal / terminal error (e.g. rate limit / network
    /// failure / provider quota exhaustion).
    Error,
}

impl RoundInterruptReason {
    /// The single user-facing word/phrase for this reason. Kept short so it
    /// fits the transcript's meta strip (`Interrupted | <label> | HH:MM`).
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "Esc Esc",
            Self::Superseded => "new message",
            Self::Terminated => "process exited",
            Self::Error => "error",
        }
    }
}

/// The durable `/retry` resume point: everything a later round needs to
/// *continue* a stopped round as itself — same round number, contiguous turn
/// ordinals — rather than minting a fresh round.
///
/// `/retry`'s whole contract is "finish the round that did not finish": the
/// round counter must not advance, the turn sequence must stay unbroken, and
/// the model-visible history must be exactly the committed checkpoint the
/// stopped round left behind. A `RetryPoint` is the harness's capture of that
/// checkpoint at the moment the round stopped (terminal error after retries
/// were exhausted, or an interrupt that left committed content).
///
/// This is **projection state, not a conversation message**: like
/// [`RoundInterrupt`] it never enters `model_window`, never reaches the
/// model, and costs zero context tokens. It rides in the session store until
/// the parked round completes (then it is cleared) or the session moves on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPoint {
    /// 1-based round the point refers to. `/retry` may only fire while this
    /// still equals the session's current round counter — any newer round
    /// (or a `/new`) retires the point.
    pub round: u64,
    /// How many complete ReAct turns the stopped round committed. The resume
    /// continues numbering turns from here (`round.turn_index` starts at this
    /// value) so the transcript's `round N | turn M` sequence stays unbroken.
    pub turns_committed: usize,
    /// `model_window` length at the stopped round's last durable checkpoint.
    /// The resume seeds its working history from the window and re-checkpoints
    /// from this watermark so no partially streamed content leaks back in.
    pub history_watermark: usize,
    /// Human-decision pause time (permission prompts / `ask_user`) the
    /// stopped round had already accumulated, in milliseconds. Seeded back
    /// into the resume so a later tokens/sec stays honest across the stop.
    pub paused_ms: u64,
    /// Unix-epoch milliseconds at which the point was recorded. Lets a
    /// resumed session show *when* the round stalled.
    pub at_ms: u64,
}

/// A compact per-round accounting handed to frontends when a user round
/// completes naturally. The "active" generation time is
/// `duration_ms.saturating_sub(paused_ms)`; dividing `output_tokens` by it
/// yields an honest tokens/sec that reflects the server's real throughput,
/// unaffected by how long the user deliberated on a permission prompt or
/// `ask_user` question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundSummary {
    /// 1-based user-round index, mirroring the transcript's round counter.
    pub round: u64,
    /// Total output (completion) tokens the model generated this round.
    pub output_tokens: u64,
    /// Full wall-clock duration of the round, including any human-decision
    /// pause time (`paused_ms`) and all tool execution.
    pub duration_ms: u64,
    /// Time within `duration_ms` the round spent parked on a human decision
    /// (a permission request or an `ask_user`). `0` when nothing blocked.
    pub paused_ms: u64,
    /// Time the model actually spent *generating* — summed across every
    /// completed provider request in the round, measured from request start
    /// to a validated response and therefore **excluding** tool execution,
    /// hooks, and human-decision pauses. This is the honest denominator for
    /// tokens/sec: `tps = output_tokens / generation_ms`. Falls back to the
    /// round `active_ms()` only when no request completed measurably.
    pub generation_ms: u64,
    /// The durable session revision this completion follows (D5): the
    /// authoritative commit was acknowledged at this revision before the
    /// completion was published. A client can order the completion against
    /// later session state and discard a stale replay instead of regressing a
    /// newer view. `0` for a completion emitted before this field existed.
    #[serde(default)]
    pub session_revision: u64,
}

impl RoundSummary {
    /// Net-active generation time: the wall-clock minus the human-decision
    /// pause. Saturates at 0 so a round that somehow paused longer than it ran
    /// still yields a finite (large) TPS rather than dividing by a negative.
    pub fn active_ms(&self) -> u64 {
        self.duration_ms.saturating_sub(self.paused_ms)
    }

    /// The time `tps()` divides by: `generation_ms` when at least one request
    /// completed measurably, otherwise the round `active_ms()` fallback.
    /// Exposed so a UI can render *exactly* the denominator the throughput
    /// figure was computed from, instead of a coincidentally-larger span.
    pub fn denominator_ms(&self) -> u64 {
        if self.generation_ms > 0 {
            self.generation_ms
        } else {
            self.active_ms()
        }
    }

    /// Output tokens per second of *generation* time — the time the model
    /// actually spent streaming a response, excluding tool execution and
    /// human-decision pauses. Falls back to round `active_ms()` (wall-clock
    /// minus human pause) when no provider request completed measurably, and
    /// returns `0.0` when there is no usable denominator so the UI renders `–`
    /// rather than `inf`.
    pub fn tps(&self) -> f64 {
        let denominator_ms = self.denominator_ms();
        if denominator_ms == 0 {
            0.0
        } else {
            (self.output_tokens as f64) * 1000.0 / (denominator_ms as f64)
        }
    }
}

/// The class of a [`RoundEvent`] — the wire-level answer to "what kind of fact
/// is this?" (ADR-0117, `[INV-TRANSCRIPT-03]`).
///
/// The event stream is deliberately **one ordered enum**: dialogue, operations
/// and telemetry interleave on one channel, and that interleaving is what lets a
/// frontend position an operation row among dialogue by timestamp. The class is
/// therefore carried *alongside* the variant rather than replacing it, and it is
/// declared once, next to the definition, by [`RoundEvent::class`].
///
/// Only [`EventClass::Dialogue`] may become a transcript fact, and only
/// [`EventClass::TurnOutcome`] may become a turn anchor; everything else lives in
/// another layer entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventClass {
    /// Someone said something (user input admitted, model output, tool steps).
    /// The only class admitted to the thread transcript as dialogue.
    Dialogue,
    /// How a round ended or a structural event that must hold a position in
    /// time (completion, interrupt, retry resolution, compaction checkpoint,
    /// background-job lifecycle). Admitted only as a turn anchor.
    TurnOutcome,
    /// An operation on the session (slash-command result, configuration
    /// mutation). Never admitted: an *act* lives in the durable command ledger
    /// and is joined into the rendering by timestamp, while a *reading* — a
    /// report of the world at the moment it was asked — lives only as an
    /// ephemeral receipt on the client (ADR-0117, ADR-0140).
    Operation,
    /// Counters, timings, phase transitions. Never admitted.
    Telemetry,
    /// Interaction bookkeeping and one-shot notices. Never admitted; the layer
    /// is chosen by `nuo_view::model::layer`.
    Ephemera,
}

impl EventClass {
    /// Whether this class may become a dialogue fact in the thread document.
    pub fn admits_dialogue(self) -> bool {
        matches!(self, Self::Dialogue)
    }

    /// Whether this class may become a structural turn marker.
    pub fn admits_anchor(self) -> bool {
        matches!(self, Self::TurnOutcome)
    }
}

impl RoundEvent {
    /// What kind of fact this event is.
    ///
    /// Exhaustive by design: there is no wildcard arm, so a new variant fails
    /// the build until its class is chosen. This function is the single audit
    /// surface for "what may enter the transcript".
    pub fn class(&self) -> EventClass {
        match self {
            // ── dialogue ──────────────────────────────────────────────────
            Self::Text(_)
            | Self::StreamStart
            | Self::StreamDelta(_)
            | Self::StreamEnd(_)
            | Self::StreamDiscard
            | Self::StreamReasoningDelta(_)
            | Self::StreamReasoningEnd(_)
            | Self::ToolCall { .. }
            | Self::ToolCallsRetracted { .. }
            | Self::ToolResult { .. }
            | Self::ToolStream { .. }
            | Self::ToolCancelled { .. }
            | Self::ToolCallStarted { .. }
            | Self::ToolInputProgress { .. }
            | Self::SubagentStep { .. }
            | Self::SteerAdmitted(_)
            | Self::FollowUpStarted(_) => EventClass::Dialogue,

            // ── turn structure ────────────────────────────────────────────
            Self::RoundCompleted(_)
            | Self::RoundInterrupted(_)
            | Self::Error(_)
            | Self::Compacted { .. }
            | Self::RetryScheduled { .. }
            | Self::BackgroundJobStarted(_)
            | Self::BackgroundJobReady { .. }
            | Self::BackgroundJobCompleted(_) => EventClass::TurnOutcome,

            // ── operations ────────────────────────────────────────────────
            Self::CommandResult { .. } => EventClass::Operation,

            // ── telemetry ─────────────────────────────────────────────────
            Self::ContextTokens(_)
            | Self::TurnStarted
            | Self::TurnPerformance(_)
            | Self::HarnessState(_)
            | Self::TodosUpdated(_)
            | Self::UnattendedChanged(_)
            | Self::ConfinementChanged(_)
            | Self::BackgroundJobProgress { .. } => EventClass::Telemetry,

            // ── ephemera ──────────────────────────────────────────────────
            Self::Notice(_)
            | Self::SteerUnavailable { .. }
            | Self::SteerCancelled { .. }
            | Self::SteerCancelFailed { .. }
            | Self::FollowUpQueued { .. }
            | Self::QueueUpdated { .. }
            | Self::PermissionRequest(_)
            | Self::UserQuestionRequest(_)
            | Self::StdinRequest(_)
            | Self::Activity(_)
            | Self::UnsentInput { .. }
            // A preview describes an *intention*: "this call would write these
            // bytes". It is therefore ephemera by classification, not by
            // convention — the durable record of a change is the journal's
            // completed row, and a resumed transcript must never replay a diff
            // nothing ever applied.
            | Self::ToolPreview { .. } => EventClass::Ephemera,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoundEvent {
    Notice(AgentNotice),
    /// Current AI-visible context size for this session. Frontends must use
    /// this instead of estimating from the persisted/rendered transcript.
    ContextTokens(ContextTokenSnapshot),
    /// A steering message crossed the agent's safe turn boundary and is now part
    /// of the live conversation (the agent persists that boundary before the
    /// provider observes it). Frontends append it to the transcript at this
    /// event, never when it was merely queued.
    SteerAdmitted(QueuedMessage),
    /// The addressed round stopped accepting steering messages before this id
    /// could be admitted. A frontend may safely retain it as a paused follow-up item.
    SteerUnavailable {
        input_id: String,
    },
    /// The driver accepted a follow-up into its queue while the target round
    /// was running (M4). The item will ship automatically at the
    /// round boundary, unless the queue is paused; the authoritative queue
    /// snapshot rides [`RoundEvent::QueueUpdated`].
    FollowUpQueued {
        input_id: String,
    },
    /// The authoritative follow-up queue snapshot for a session, sent after
    /// every queue change (enqueue, ship, remove, clear, promote, pause).
    /// A full-replace diff: frontends rebuild their queue projection from
    /// `items` verbatim. Ships also emit [`RoundEvent::FollowUpStarted`] —
    /// the snapshot reflects the queue *after* the ship.
    QueueUpdated {
        items: Vec<QueuedMessage>,
        paused: bool,
    },
    /// A pending steering message was cancelled before admission.
    SteerCancelled {
        input_id: String,
    },
    /// Cancellation lost the race with admission (or the id was unknown).
    /// The subsequent admitted/unavailable event remains authoritative.
    SteerCancelFailed {
        input_id: String,
    },
    /// A follow-up outbox item was accepted by its exact live session and a
    /// fresh round was started. Like steering admission, this is the transcript
    /// commit point for frontends.
    FollowUpStarted(QueuedMessage),
    /// The user-driven round reached its natural, successful terminal path.
    /// Interruptions, blocked prompts, and errors deliberately emit no such
    /// event, so next-round outbox items pause instead of auto-running.
    /// Carries a small per-round summary so frontends can show an honest
    /// generation throughput (tokens/sec) that excludes the time the round
    /// spent parked on human decisions (permission prompts / ask_user).
    RoundCompleted(RoundSummary),
    /// A round stopped before its natural terminal path, with the reason and
    /// timestamp. Emitted exactly once per stopped round, after the round's
    /// own cleanup — including the generation-suppressed case (a superseded
    /// round), which previously left no visible trace. The durable twin of
    /// this event (`RoundInterrupt`) is persisted in the session store and
    /// re-projected into the transcript on resume; this live event never
    /// reaches the model context.
    ///
    /// Distinct from [`RoundEvent::UnsentInput`] (a Phase-1 interrupt is an
    /// *unsend*: the user message returned to the composer and nothing
    /// committed) and from [`RoundEvent::ToolCancelled`] (per-tool). This
    /// event covers the round as a whole on every interrupt phase.
    RoundInterrupted(RoundInterrupt),
    Text(String),
    /// A typed slash-command result.
    ///
    /// Replaces the `Text` replies commands used to emit: a frontend renders it
    /// as a command row (dimmed invocation + expandable reply), never as
    /// assistant prose.
    ///
    /// `effect` is the server's authoritative class for this invocation
    /// (ADR-0140) and it decides where the reply is filed — nothing else does:
    ///
    /// - [`crate::CommandEffect::Mutation`] — an act. A durable row already exists in
    ///   the session's command ledger; the client settles the staged thread row
    ///   in place, so the act holds its position in the conversation.
    /// - [`crate::CommandEffect::Reading`] — a report of the world at the moment of
    ///   asking. **No** ledger row exists and none may be fabricated: the reply
    ///   is an ephemeral receipt on the client. A reading is answered in full,
    ///   it is simply never asserted later as still true.
    ///
    /// Carrying the class on the event (rather than making the client consult the
    /// catalog) is what keeps a frontend that never fetched a catalog honest, and
    /// it is the analogue of ADR-0119 §2: admission belongs to the dispatcher.
    CommandResult {
        /// Command word without the leading slash (e.g. `"search"`).
        name: String,
        /// Raw argument remainder after the command word.
        args: String,
        result: crate::command::CommandResult,
        /// Whether this invocation was an act (recorded) or a reading (ephemeral).
        effect: crate::CommandEffect,
    },
    /// Turn-level error (e.g. a provider failure mid-turn). Distinct from the
    /// global [`AgentResponse::Error`] only in that it belongs to a specific
    /// session's transcript and is therefore carried under the [`Round`]
    /// envelope.
    ///
    /// [`Round`]: AgentResponse::Round
    Error(String),
    ToolCall {
        /// The call's single identity (ADR-0166) — the same id its
        /// `ToolCallStarted` announcement carried and every later event about
        /// this call will carry.
        id: String,
        name: String,
        arguments: String,
    },
    /// ADR-0166: an attempt ended before its announced calls could be
    /// dispatched. The frontend retracts the named provisional steps; it must
    /// not render them as cancellations (no call ever existed), and it must
    /// retract only rows that are still `Ingesting`.
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
    /// A tool call announced **before its arguments finished streaming**
    /// (ADR-0026). Mirrors [`AgentEvent::ToolCallStarted`]: `name` is known,
    /// `arguments` are still arriving, and the later whole-argument
    /// [`RoundEvent::ToolCall`] carries the same call id.
    ///
    /// ADR-0166: `id` is the call's **single identity**, not a separate
    /// announcement token. Every later event about this call — progress,
    /// dispatch, stream, preview, result, retraction — carries the same value,
    /// so a frontend never reconstructs identity from a provider slot, a
    /// round, or arrival order.
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
    PermissionRequest(PermissionRequest),
    UserQuestionRequest(UserQuestionRequest),
    /// Mirrors [`AgentEvent::StdinRequest`]: an interactive `bash` command
    /// needs operator stdin.
    #[serde(alias = "InputRequest")]
    StdinRequest(StdinRequest),
    /// A context projection (compaction or prune) was committed. Token
    /// samples of the active window around the projection.
    Compacted {
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tracked_files: Vec<String>,
    },
    HarnessState(HarnessSnapshot),
    /// The task list changed (full-replace via `todo`, surgical update via
    /// `todo_update`). Mirrors [`AgentEvent::TodosUpdated`]. An empty list
    /// means "no active task list" and hides the sticky panel.
    TodosUpdated(crate::todos::TodoList),
    /// The unattended execution toggle changed. `true` = the agent runs in unattended
    /// mode (suppresses interactive human confirmations). Emitted by `/unattended` so
    /// the TUI can refresh its badge without waiting for the next harness snapshot.
    UnattendedChanged(bool),
    /// The workspace confinement toggle changed. `true` = confined to workspace,
    /// `false` = confinement disabled. Emitted by `/confinement` so the TUI can refresh its
    /// badge without waiting for the next harness snapshot.
    ConfinementChanged(bool),
    RetryScheduled {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        message: String,
    },
    Activity(String),
    /// A new ReAct turn started within the current user round.
    ///
    /// The position and send time ride the enclosing [`RoundScope::Turn`]
    /// context; the event itself is just the lifecycle marker. Emitted just
    /// before the matching `Activity("waiting for model")`.
    TurnStarted,
    /// One concrete provider attempt completed with client-observed
    /// performance telemetry. This is pushed at the ReAct-turn boundary so
    /// hint bars update immediately; detailed history remains queryable from
    /// the request ledger.
    TurnPerformance(crate::TurnPerformanceSnapshot),
    StreamStart,
    StreamDelta(String),
    StreamReasoningDelta(String),
    StreamReasoningEnd(String),
    StreamEnd(String),
    /// A streamed attempt was abandoned. The enclosing [`RoundScope::Turn`]
    /// context names the attempt it discards, so the client can sweep that
    /// attempt's provisional rows without consulting a side channel (ADR-0167,
    /// ADR-0168). `turn` is the **0-indexed** model-request position, matching
    /// [`Self::TurnStarted`].
    StreamDiscard,
    /// The user interrupted the round before any model output reached the
    /// client (Phase 1: request in-flight, no content delta yet). The round's
    /// user message has been removed from the conversation context and
    /// session store, and the client should offer `prompt` (and any
    /// `images`) back for re-editing — the conversation is back to its
    /// pre-send state. Restoring into the composer is advisory: a client
    /// whose composer holds in-progress input should leave it alone and
    /// surface the prompt another way (history recall / a notice), since the
    /// unsend arrives asynchronously. The cancelled network request may
    /// still bill its input tokens on the provider side, but no assistant
    /// message, tool calls, or output tokens are produced or recorded.
    UnsentInput {
        prompt: String,
        images: Vec<crate::ImagePart>,
    },
    /// A subagent event to render nested inside the parent tool step. The
    /// variant carries the child's own [`SubagentEvent`] plus the parent
    /// tool-call id it was nested under.
    SubagentStep {
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
    /// A service task reported readiness: the process is alive and
    /// its readiness condition (first output, grace, or port probe) is met.
    /// Wake-eligible.
    BackgroundJobReady {
        job_id: crate::job::JobId,
    },
    /// A background job completed.
    BackgroundJobCompleted(crate::job::BackgroundJobOutcome),
}
