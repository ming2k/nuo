//! The translator seam (M1): typed mutations from the response
//! translator to the event loop.
//!
//! The server-response listener (and the monitor client) own **no**
//! application state. They consume `AgentResponse` / `MonitorEvent` frames
//! and produce [`AppMutation`] values onto a bounded channel; the event loop
//! — the sole writer of `App` — drains the channel between input batches and
//! applies each mutation. There are no shared state cells and no per-frame
//! mirroring: a mutation is the state change, delivered as data.
//!
//! Design rules:
//! - Mutations are **semantic**, not raw cell pokes: `InsertAdmittedSeam` (a
//!   steer or follow-up crossed the turn boundary) carries the fully built
//!   message, so the translator never needs to read the transcript document to
//!   decide.
//! - Transcript payloads are **pre-built by the translator** (attribution,
//!   effort, round/turn stamping, reasoning disclosure defaults) so the
//!   applier stays a mechanical executor of document edits.
//! - The channel is bounded. Overflow cannot happen silently: a full channel
//!   applies backpressure to the translator (which is driven by the server
//!   stream), and the loop drains eagerly each iteration.
use std::collections::HashMap;

use nuo_protocol::{InputRequest, ProviderPickerSnapshot, ThreadOverview};

use nuo_tool::{PermissionRequest, SubagentEvent, ToolStream, UserQuestionRequest};

use crate::shell::app::{OauthAddSignal, ProviderRetryState};
use crate::shell::pre_attach::PreAttachSignal;
use nuo_view::model::document::TranscriptMessage;
use nuo_view::phase::Phase;

/// A backend completion round-trip awaiting consumption by the loop
/// (formerly a shared cell drained per frame; now a mutation payload).
#[derive(Debug)]
pub(crate) struct CompletionSignal {
    pub request_id: u64,
    pub input: String,
    pub cursor: usize,
    pub items: Vec<nuo_tool::InputCompletion>,
}

/// One semantic edit of the primary transcript document. The applier executes
/// these against `App::messages` with the same helpers the translator used to
/// call inline — the document-editing logic is unchanged, only its ownership
/// moved.
#[derive(Debug)]
pub(crate) enum TranscriptEdit {
    /// A stream is starting: retire any provider-retry disclosure row
    /// (`begin_stream`).
    BeginStream,
    /// A visible-text delta. Resolves the streaming text entry of this
    /// `(round, turn)` by backward scan; `created` is the translator-built
    /// first-message fallback (attribution/effort stamped) for the lazy
    /// create when no entry matches.
    StreamTextDelta {
        round: Option<u64>,
        turn: Option<u64>,
        delta: String,
        created: Option<TranscriptMessage>,
        clear_retry: bool,
    },
    /// The stream finished; the final content replaces the streaming entry
    /// in place (`created` is the no-delta fallback message).
    StreamTextFinalize {
        round: Option<u64>,
        turn: Option<u64>,
        content: String,
        created: Option<TranscriptMessage>,
    },
    /// An aborted stream: drop every provisional row of this attempt.
    ///
    /// The position is carried by the event, not looked up: a discard always
    /// names the attempt it destroys (ADR-0167). Rows above the attempt's own
    /// output — a dispatched tool step, a notice, a command result — are
    /// committed dialogue and bound the sweep.
    StreamDiscard { round: u64, turn: u64 },
    /// A disclosed reasoning delta; `created` is the lazy first-trace
    /// message (disclosure default already applied).
    ReasoningDelta {
        round: Option<u64>,
        turn: Option<u64>,
        delta: String,
        created: Option<TranscriptMessage>,
    },
    /// The reasoning trace finished; resolve the streaming Thinking entry of
    /// this position by scan and finalize in place.
    ReasoningFinalize {
        round: Option<u64>,
        turn: Option<u64>,
        content: String,
        duration_ms: Option<u64>,
    },
    /// ADR-0166: **the one way a tool step enters or advances.**
    ///
    /// `message` carries the call id and the status that fact belongs to
    /// (`Ingesting` for an announcement, `Executing` for a dispatch). The
    /// applier resolves by call id and merges monotonically: an id that is
    /// already present advances in place, an unknown id is inserted. Applying
    /// the same fact twice is therefore a no-op, and a duplicate or reordered
    /// frame cannot create a second row.
    ToolStepUpsert { message: TranscriptMessage },
    /// ADR-0166: retract calls whose attempt ended before dispatch. A retracted
    /// step is removed, never terminalized: no call ever existed, so a
    /// `cancelled` row would assert something false. Only a step that is still
    /// `Ingesting` is removable — a call that dispatched is real.
    ToolCallsRetracted { ids: Vec<String> },
    /// ADR-0166: a round boundary with the harness idle leaves no `Ingesting`
    /// step behind (`[INV-TOOL-ID-05]`). This is the boundary sweep: it needs
    /// no ids, so it stays correct after a relink, where the translator has no
    /// local announcement state.
    RetractPendingToolSteps,
    /// ADR-0125: attach a pure preview to the in-flight step it describes, so the
    /// turn's whole footprint is visible before anything runs. The preview lands in
    /// the step's `structured` field (the same one a completed diff renders from),
    /// so a preview card and a result card cannot disagree.
    ///
    /// A no-op when no step matches: the step may not exist yet (a dispatch that
    /// outran its announcement), and the call's eventual permission request still
    /// carries the preview where a decision is at stake.
    ToolPreview {
        id: String,
        preview: nuo_tool::PreviewResult,
    },
    /// ADR-0026 / ADR-0046: argument progress tick and partial argument payload for a still-streaming call, addressed by call id (ADR-0166).
    ToolInputProgress {
        id: String,
        bytes: usize,
        arguments: Option<String>,
    },
    /// A tool step finished. Resolves by call id and applies the
    /// lifecycle-aware default disclosure; `fallback` is the synthesized
    /// finished step when no in-flight call matched (history-restored turn).
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: nuo_tool::ToolOutput,
        duration_ms: u64,
        fallback: Option<TranscriptMessage>,
    },
    /// An in-flight call was aborted; flip it (and nested subagent children) to
    /// Cancelled. `fallback` is the synthesized minimal cancelled step.
    ToolCancel {
        id: String,
        fallback: Option<TranscriptMessage>,
    },
    /// Live partial tool output (bash stdout) accumulating into the running
    /// step.
    ToolStream { id: String, stream: ToolStream },
    /// A subagent (sub-agent) event nested under a parent tool step.
    SubagentEvent {
        parent_call_id: String,
        event: SubagentEvent,
    },
    /// A steering message crossed the safe turn boundary and is now dialogue.
    ///
    /// It is **appended** — not inserted at the head. At the instant
    /// `SteerAdmitted` arrives, the buffer tail IS the turn seam: the harness
    /// persisted the steer after the finished turn's last component and before
    /// the next turn's provider request, and the next `TurnStarted` has not yet
    /// delivered that turn's components. Appending therefore reproduces the
    /// durable order exactly (ADR-0137 `[INV-STEER-02]`).
    ///
    /// Head insertion was the original bug: `App::messages` holds the whole
    /// thread, so index 0 put a round-N steer above round 1.
    InsertAdmittedSeam { message: TranscriptMessage },
    /// A typed slash-command reply: settle the newest *pending* row with
    /// this invocation, or append the fallback component.
    SettleCommandResult {
        invocation: String,
        result: nuo_protocol::CommandResult,
        fallback: Option<TranscriptMessage>,
    },
    /// The newest user prompt matching the interruption semantics is marked
    /// cancelled, any in-flight provider retry is settled into a static failure
    /// notice preserving diagnostics, and a round-interrupt projection row is
    /// appended.
    Interrupted {
        record: nuo_protocol::RoundInterrupt,
    },
    /// The newest user prompt is marked cancelled (transport-level retraction).
    CancelLastUserPrompt,
    /// Append a fully-built message (assistant text, notices, projections).
    Append {
        /// The event class this row was produced by. Admission is gated on it
        /// (`[INV-TRANSCRIPT-01]`): only `Dialogue` becomes a fact and only
        /// `TurnOutcome` becomes an anchor; every other class is refused, so an
        /// operation, a telemetry sample or a receipt can never enter the thread
        /// transcript (`nuo_view::model::thread`).
        class: nuo_protocol::EventClass,
        message: TranscriptMessage,
    },
    /// Freeze any orphaned still-streaming reasoning trace (its round ended
    /// mid-trace; the spinner would breathe forever).
    FinalizeOrphanedReasoning { duration_ms: Option<u64> },
    /// Settle a staged optimistic row when the server's authoritative event says
    /// the reply is **not** a thread row after all.
    ///
    /// The class staged before dispatch is a prediction: the client reads it from
    /// the command catalog, which arrives asynchronously and defaults to
    /// `Mutation` for a name it does not yet know. When the reply's declared
    /// class is `Reading`, the row must be **withdrawn**, not merely left
    /// unsettled — an unsettled row is later marked `Cancelled` by
    /// `CancelPendingCommands` and stays in the thread as a ghost that says "this
    /// command never answered", which is the opposite of what happened
    /// (ADR-0140 `[INV-CMD-08]`: the event is authority, the catalog is only an
    /// optimisation, and a wrong optimisation must be reversible).
    ///
    /// When `restore_draft` is true (ADR-0149), the invocation draft is returned
    /// to the composer if the composer is currently empty.
    WithdrawStagedCommand {
        invocation: String,
        restore_draft: bool,
    },
    /// Cancel every still-Pending command row (round boundary with no reply
    /// on this pass).
    CancelPendingCommands,
    /// Rebase message rounds after a transcript rebuild.
    RebaseRounds { round_counter: u64 },
    /// Stamp the composer's latest unpositioned driving prompt with the
    /// authoritative round at `TurnStarted` admission (M1: the
    /// translator cannot read the document to find it).
    StampTurnPrompt { round: u64 },
    /// Wholesale replace (session switch: rebuilt + merged).
    ReplaceAll { messages: Vec<TranscriptMessage> },
    /// Clear (ConversationCleared).
    Clear,
}

/// View-scoped chrome edit (per-session bookkeeping; /0103).
#[derive(Debug, Clone)]
pub(crate) enum ChromeEdit {
    /// The authoritative running/idle transition (`HarnessState`).
    RoundLifecycle {
        round_count: u64,
        running: bool,
        can_retry: bool,
    },
    /// A wire activity label folded into a phase (implies responding).
    ActivityFolded(Phase),
    /// A bare phase fact, no responding implication (stream deltas).
    PhaseOnly(Option<Phase>),
    /// First stream byte: responding + timer origin + phase upgrade.
    StreamStarted,
    /// A new ReAct turn: structural counters + `AwaitingModel`.
    TurnStarted { round: u64, turn: u64 },
    /// The round ended (interrupt / error): retire the live surface.
    RoundEnded,
    /// The turn finished; performance snapshot for the Activity modal.
    TurnPerformance(Box<nuo_protocol::TurnPerformanceSnapshot>),
}

/// Everything the response translator (and monitor client) can ask the event
/// loop to do to `App`. One enum, one applier, one writer.
// Payloads stay inline because this bounded channel is the ownership-transfer seam.
#[allow(clippy::large_enum_variant)]
pub(crate) enum AppMutation {
    Transcript {
        edit: TranscriptEdit,
    },
    ChromeEdit {
        session_id: String,
        edit: ChromeEdit,
    },

    // Session lifecycle / harness.
    Harness(nuo_protocol::HarnessSnapshot),
    /// Server-attested readiness transition (ADR-0133 `[INV-ATTACH-04]`).
    SessionPhase(nuo_protocol::ThreadPhase),
    /// Replace the server-owned slash-command vocabulary from an authoritative
    /// hydration snapshot.
    CommandCatalog(nuo_tool::CommandCatalog),
    HarnessUnattended(bool),
    HarnessConfined(bool),
    ClearSwitchingSession,
    /// The live session moved (ADR-0123 `[INV-TAB-13]`).
    ///
    /// Identity-addressed: `previous_id` names the session being left, and only
    /// a tab carrying that id is rebased. `next_id` is the freshly minted one.
    ///
    /// Addressing this by "the active tab" — the previous design — rewrote
    /// whichever tab happened to be focused. When the discard of an empty thread
    /// raced a synchronous tab replacement, the receipt landed on the *new*
    /// tab and turned it into a ghost: a tab naming a never-persisted session
    /// that the idle reaper then reclaimed, leaving a click that had no target.
    /// With no tab holding `previous_id` the rebase is correctly a no-op.
    LiveSession {
        previous_id: String,
        next_id: String,
    },
    // Activity / phase / counters.
    SetPhase(Option<Phase>),
    SetResponding(bool),
    SetRoundCount(u64),
    SetCurrentTurn(u64),
    SetRoundStartedAt(Option<std::time::Instant>),
    /// Publish the primary session's transport setback (provider retry
    /// countdown) beside its phase.
    ///
    /// Not an `Option`: the clause cannot be "set to nothing" because clearing
    /// it here is precisely how the bar went stale. It is retired by
    /// the next [`AppMutation::SetPhase`] that is not `AwaitingModel` — the one
    /// writer that sees every phase move.
    SetProviderRetry(ProviderRetryState),

    // Human-in-the-loop queues.
    QueuePermission {
        request: PermissionRequest,
        parent_call_id: Option<String>,
    },
    QueueQuestion {
        request: UserQuestionRequest,
        parent_call_id: Option<String>,
    },
    QueueInput(InputRequest),
    ClearPermissions,

    // Background task mutations (TaskBar authority).
    BackgroundTaskStarted {
        id: String,
        label: String,
        started_at_ms: u64,
    },
    BackgroundTaskCompleted {
        id: String,
        success: bool,
        exit_code: Option<i32>,
        duration_secs: u64,
    },
    #[allow(dead_code)]
    BackgroundTaskDismissSettled,

    // Dispatch-queue facts (former `OutboxSignal`s + the M4 queue snapshot).
    DispatchRemoved {
        session_id: String,
        input_id: String,
    },
    /// Steer missed the active round. Restored to composer draft,
    /// never queued as a follow-up item.
    SteerMissed {
        session_id: String,
        input_id: String,
    },
    /// A pending steering message was cancelled before admission
    /// (SteerCancelled): remove the outbox `Steering` row and restore the
    /// text to the composer draft. Identical lifecycle to `SteerMissed` —
    /// both are "this steer will never enter the thread".
    SteerCancelledRestored {
        session_id: String,
        input_id: String,
    },
    /// The cancel lost its race with admission (`SteerCancelFailed`): the steer
    /// will land after all. Clear the deletion-intent flag it may have set so
    /// the flag cannot outlive the item, and disclose the outcome rather than
    /// silently dropping the user's deletion (ADR-0137).
    SteerCancelFailedCleared {
        session_id: String,
        input_id: String,
    },
    /// The server admitted an optimistic follow-up into its queue: the local
    /// item settles from `Dispatching` back to `Waiting`.
    DispatchQueued {
        session_id: String,
        input_id: String,
    },
    /// The authoritative follow-up queue snapshot (M4): replace the
    /// session's projection, keeping in-flight optimistic entries the
    /// snapshot cannot know about yet, and mirror the server's paused flag.
    QueueSnapshot {
        session_id: String,
        items: Vec<nuo_protocol::QueuedMessage>,
        paused: bool,
    },
    // Views / panels / modal data.
    /// The server's persisted prompt input history (the server is the SSOT;
    /// the TUI never opens the database itself).
    InputHistory(Vec<nuo_protocol::HistoryEntry>),
    /// Stored capability overrides for one provider/model route — the model
    /// editor's prefill, delivered after the editor opened.
    RouteSettings {
        provider_id: String,
        model: String,
        overrides: Option<nuo_protocol::model::CapabilityOverrides>,
    },
    KeyStatus(HashMap<String, bool>),
    ProviderPicker(ProviderPickerSnapshot),
    SessionsOverview(Vec<ThreadOverview>),
    OpenSessionsPanel,
    OpenTreePanel,
    SessionDetail(nuo_protocol::ThreadDetail),
    ConnectionDetail(nuo_protocol::ConnectionDetail),
    TokenReport(Option<nuo_protocol::TokenSourceReport>),
    UsageStats(nuo_protocol::usage_stats::UsageStatsReport),
    /// A usage fetch failed. The report already on screen is kept (a refresh
    /// must never blank a populated view); only the loading flag clears.
    UsageFailed(String),
    ProviderQuotas(nuo_protocol::ProviderQuotaSnapshot),
    SessionTree(nuo_protocol::ThreadTree),
    SessionContext(nuo_protocol::ThreadContextSnapshot),
    CompletionSignal(CompletionSignal),
    /// One disclosure the client surfaced: a routed notice, or a local fault
    /// (a clipboard write that failed, a link that could not be opened).
    ///
    /// The applier owns the residency decision: **every** disclosure is
    /// appended to the notice log, and only a fault additionally raises a
    /// bubble (`app::receipts`). Producers therefore cannot forget the log —
    /// there is no mutation that shows a bubble without recording it.
    ///
    /// The payload is a [`RenderedNotice`](crate::shell::app::notice_catalog::RenderedNotice)
    /// (ADR-0138): its only constructors are the closed
    /// [`ClientNotice`](crate::shell::app::notice_catalog::ClientNotice) enum
    /// and the closed wire projection, so this cross-thread funnel cannot carry
    /// an ad-hoc topic string or a faked timestamp either.
    NoticeDisclosure(crate::shell::app::notice_catalog::RenderedNotice),
    Oauth(OauthAddSignal),
    WebSearchConfig(Option<nuo_protocol::WebSearchConfigView>),
    ServerEndpoints(nuo_protocol::ServerEndpointsReport),
    PreAttach(PreAttachSignal),
    /// A failed trust-grant round-trip for the mounted PreAttach surface
    /// the server could not persist the trust decision. The
    /// applier gates on the surface being mounted — with no gate up, the
    /// notice keeps its normal transcript/toast flow.
    PreAttachAttestationError(String),
    ProviderSwitched {
        provider: String,
        model: String,
    },
    ContextTokens {
        session_id: String,
        snapshot: nuo_protocol::ContextTokenSnapshot,
    },
    ClearContextTokens,
    /// The server asked this client to stop: `/exit`, a killed thread, or the
    /// server's own drain. Only the *server* can end a session, so this is the
    /// one mutation that quits without a user gesture.
    Quit,
    /// The attach link died with the session still live (ADR-0136).
    ///
    /// Distinct from `Self::Quit` by construction. The rung this replaces
    /// synthesized `AgentResponse::Exit` whenever the reader task stopped —
    /// including when it stopped *because it could not decode one frame* — so a
    /// protocol skew and a user typing `/exit` were the same message, and the
    /// TUI closed the window mid-round with no explanation.
    ///
    /// A dead link is recoverable: the attach loop re-dials the server, the
    /// same way it does for a thread switch. So this surfaces the fault and
    /// keeps the process alive.
    LinkLost {
        /// The transport error, verbatim, for the notice log.
        reason: String,
    },
    HostSessions(Vec<nuo_protocol::MonitoredThread>),
    PersistenceHealth(Option<nuo_protocol::monitor::PersistenceHealth>),
    /// A dashboard console receipt: appended to
    /// `App::host_console_log` by the applier.
    HostConsole(crate::shell::overlays::ConsoleLine),
}

/// The translator's end of the mutation channel.
#[derive(Clone)]
pub(crate) struct MutationSink {
    tx: tokio::sync::mpsc::Sender<AppMutation>,
}

impl MutationSink {
    pub(crate) fn new(tx: tokio::sync::mpsc::Sender<AppMutation>) -> Self {
        Self { tx }
    }

    /// Send one mutation with bounded backpressure. A closed channel means
    /// the event loop is gone (the TUI is exiting) — the translator has
    /// nowhere to report to and simply stops mattering; that send error is
    /// deliberately not escalated.
    pub(crate) async fn send(&self, mutation: AppMutation) {
        let _ = self.tx.send(mutation).await;
    }

    /// Blocking-task counterpart to `Self::send`. This is only for
    /// producers already running on Tokio's blocking pool.
    pub(crate) fn blocking_send(&self, mutation: AppMutation) {
        let _ = self.tx.blocking_send(mutation);
    }
}
