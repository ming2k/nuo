//! The translator seam (ADR-0197 M1): typed mutations from the response
//! translator to the event loop.
//!
//! The daemon-response listener (and the monitor client) own **no**
//! application state. They consume `AgentResponse` / `MonitorEvent` frames
//! and produce [`AppMutation`] values onto a bounded channel; the event loop
//! — the sole writer of `App` — drains the channel between input batches and
//! applies each mutation. There are no shared state cells and no per-frame
//! mirroring: a mutation is the state change, delivered as data.
//!
//! Design rules:
//! - Mutations are **semantic**, not raw cell pokes: `SettleInserted` (a
//!   steer was admitted) carries the settle *and* its fallback append, so
//!   the translator never needs to read the transcript document to decide.
//! - Transcript payloads are **pre-built by the translator** (attribution,
//!   effort, round/turn stamping, reasoning disclosure defaults) so the
//!   applier stays a mechanical executor of document edits.
//! - The channel is bounded. Overflow cannot happen silently: a full channel
//!   applies backpressure to the translator (which is driven by the daemon
//!   stream), and the loop drains eagerly each iteration.

use std::collections::HashMap;

use nuo_wire::{
    InputRequest, PermissionRequest, ProviderPickerSnapshot, SessionOverview, SubagentEvent,
    ToolStream, UserQuestionRequest,
};

use crate::app::{OauthAddSignal, ProviderRetryState};
use crate::event_loop::runtime::SideViewSignal;
use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::phase::Phase;
use crate::pre_attach::PreAttachSignal;

/// A backend completion round-trip awaiting consumption by the loop
/// (formerly a shared cell drained per frame; now a mutation payload).
#[derive(Debug)]
pub(crate) struct CompletionSignal {
    pub request_id: u64,
    pub input: String,
    pub cursor: usize,
    pub items: Vec<nuo_wire::InputCompletion>,
}

/// Which transcript document a mutation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Buffer {
    /// The primary conversation.
    Primary,
    /// The focused `/btw` aside's buffer (ADR-0103).
    Side,
}

/// One semantic edit of a transcript document. The applier executes these
/// against `App::messages` / `App::side_messages` with the same helpers the
/// translator used to call inline — the document-editing logic is unchanged,
/// only its ownership moved.
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
    /// An aborted stream: drop the trailing assistant entry of this position
    /// if it is the live tail (never pops an older round's entry).
    StreamDiscard {
        round: Option<u64>,
        turn: Option<u64>,
    },
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
    /// A tool step started (`message` is fully built and stamped).
    ToolStart { message: TranscriptMessage },
    /// A tool step finished. Resolves by call id and applies the
    /// lifecycle-aware default disclosure; `fallback` is the synthesized
    /// finished step when no in-flight call matched (history-restored turn).
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: nuo_wire::ToolOutput,
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
    /// A staged (optimistic) user message was admitted by the daemon:
    /// settle the newest entry with this correlation id, or append the
    /// fallback message.
    SettleInserted {
        insert_id: String,
        origin: crate::model::document::UserMessageOrigin,
        sent_at_ms: Option<u64>,
        fallback: Option<TranscriptMessage>,
    },
    /// A steer could not be admitted before the round closed: the staged
    /// entry waits for the next round.
    #[allow(dead_code)]
    HoldInserted { insert_id: String },
    /// A typed slash-command reply: settle the newest *pending* row with
    /// this invocation, or append the fallback component.
    SettleCommandResult {
        invocation: String,
        result: nuo_wire::CommandResult,
        fallback: Option<TranscriptMessage>,
    },
    /// The newest user prompt matching the interruption semantics is marked
    /// cancelled, any in-flight provider retry is settled into a static failure
    /// notice preserving diagnostics, and a round-interrupt projection row is
    /// appended.
    Interrupted {
        record: nuo_wire::RoundInterrupt,
    },
    /// The newest user prompt is marked cancelled (transport-level retraction).
    CancelLastUserPrompt,
    /// Evict every provider-retry disclosure row (`RetryResolved`,
    /// round-completed housekeeping).
    RetainNotRetry,
    /// Update-in-place the trailing retry disclosure row, or append the
    /// fallback disclosure message (RetryScheduled).
    UpsertRetry {
        attempt: usize,
        max_attempts: usize,
        retry_at: std::time::Instant,
        failure: String,
        fallback: TranscriptMessage,
    },
    /// Append a fully-built message (assistant text, notices, projections).
    Append { message: TranscriptMessage },
    /// Freeze any orphaned still-streaming reasoning trace (its round ended
    /// mid-trace; the spinner would breathe forever).
    FinalizeOrphanedReasoning { duration_ms: Option<u64> },
    /// Cancel every still-Pending command row (round boundary with no reply
    /// on this pass; ADR-0108).
    CancelPendingCommands,
    /// Rebase message rounds after a transcript rebuild.
    RebaseRounds { round_counter: u64 },
    /// Stamp the composer's latest unpositioned driving prompt with the
    /// authoritative round at `TurnStarted` admission (ADR-0197 M1: the
    /// translator cannot read the document to find it).
    StampTurnPrompt { round: u64 },
    /// Wholesale replace (session switch / aside open: rebuilt + merged).
    ReplaceAll { messages: Vec<TranscriptMessage> },
    /// Clear (ConversationCleared).
    Clear,
}

/// View-scoped chrome edit (per-session bookkeeping; ADR-0089/0103).
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
    TurnPerformance(Box<nuo_wire::TurnPerformanceSnapshot>),
    /// The session's in-flight model request was retried (`RetryScheduled`):
    /// publish the setback clause beside its phase.
    ///
    /// There is no clear counterpart, and that is the point (ADR-0235): the
    /// clause is retired by the *next phase write* for this session
    /// ([`crate::app::SessionChrome::set_phase`]), so the producer states the fact and
    /// forgets it. A producer that could clear it would reintroduce the
    /// hand-maintained "events that mean the retry is over" list that let the
    /// clause outlive its setback.
    TransportSetback(Box<ProviderRetryState>),
}

/// Everything the response translator (and monitor client) can ask the event
/// loop to do to `App`. One enum, one applier, one writer.
// Payloads stay inline because this bounded channel is the ownership-transfer seam.
#[allow(clippy::large_enum_variant)]
pub(crate) enum AppMutation {
    Transcript {
        buffer: Buffer,
        edit: TranscriptEdit,
    },
    ChromeEdit {
        session_id: String,
        edit: ChromeEdit,
    },

    // Session lifecycle / harness.
    Harness(nuo_wire::HarnessSnapshot),
    HarnessUnattended(bool),
    HarnessConfined(bool),
    ClearSwitchingSession,
    LiveSession(String),

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
    /// it here is precisely how the bar went stale (ADR-0235). It is retired by
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

    // Background task mutations (ADR-0212 TaskBar authority).
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
    /// ADR-0212: Steer missed the active round. Restored to composer draft,
    /// never queued as a follow-up item.
    SteerMissed {
        session_id: String,
        input_id: String,
    },
    /// The daemon admitted an optimistic follow-up into its queue: the local
    /// item settles from `Dispatching` back to `Waiting`.
    DispatchQueued {
        session_id: String,
        input_id: String,
    },
    /// The authoritative follow-up queue snapshot (ADR-0197 M4): replace the
    /// session's projection, keeping in-flight optimistic entries the
    /// snapshot cannot know about yet, and mirror the daemon's paused flag.
    QueueSnapshot {
        session_id: String,
        items: Vec<nuo_wire::QueuedMessage>,
        paused: bool,
    },
    // Views / panels / modal data.
    ParentStatus(nuo_wire::ParentStatus),
    SideView(SideViewSignal),
    BtwList(Vec<nuo_wire::BtwAsideSummary>),
    /// The daemon's persisted prompt input history (the daemon is the SSOT;
    /// the TUI never opens the database itself).
    InputHistory(Vec<nuo_wire::HistoryEntry>),
    /// Stored capability overrides for one provider/model route — the model
    /// editor's prefill, delivered after the editor opened.
    RouteSettings {
        provider_id: String,
        model: String,
        overrides: Option<nuo_wire::model::CapabilityOverrides>,
    },
    KeyStatus(HashMap<String, bool>),
    ProviderPicker(ProviderPickerSnapshot),
    SessionsOverview(Vec<SessionOverview>),
    OpenSessionsPanel,
    OpenTreePanel,
    OpenHostPanel,
    SessionDetail(nuo_wire::SessionDetail),
    ConnectionDetail(nuo_wire::ConnectionDetail),
    TokenReport(Option<nuo_wire::TokenSourceReport>),
    UsageStats(nuo_wire::usage_stats::UsageStatsReport),
    SessionTree(nuo_wire::SessionTree),
    SessionContext(nuo_wire::SessionContextSnapshot),
    CompletionSignal(CompletionSignal),
    NoticeToast {
        severity: NoticeSeverity,
        text: String,
    },
    Oauth(OauthAddSignal),
    WebSearchConfig(Option<nuo_wire::WebSearchConfigView>),
    PreAttach(PreAttachSignal),
    ProviderSwitched {
        provider: String,
        model: String,
    },
    ContextTokens {
        session_id: String,
        snapshot: nuo_wire::ContextTokenSnapshot,
    },
    ClearContextTokens,
    Quit,
    HostSessions(Vec<nuo_wire::MonitoredSession>),
    PersistenceHealth(Option<nuo_wire::monitor::PersistenceHealth>),
    /// A dashboard console receipt (ADR-0097 §3): appended to
    /// `App::host_console_log` by the applier.
    HostConsole(crate::overlays::ConsoleLine),
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

    /// Blocking-task counterpart to [`Self::send`]. This is only for
    /// producers already running on Tokio's blocking pool.
    pub(crate) fn blocking_send(&self, mutation: AppMutation) {
        let _ = self.tx.blocking_send(mutation);
    }
}
