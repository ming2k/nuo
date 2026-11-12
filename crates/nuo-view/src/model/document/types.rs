//! Semantic document model enums, value objects, and status tags.
use nuo_ui_primitives::design::{COMMAND_CARD_LEAD_COLS, JOIN_ENUMERATE_COLS};
use unicode_width::UnicodeWidthStr;

use super::message::TranscriptMessage;

/// Lifecycle of a tool step, stored explicitly (not inferred from `output`)
/// so an aborted call has its own terminal state instead of being stuck in
/// "no output yet". This is the single source of truth for tool-step state —
/// the renderer classifies it into a [`crate::tools::ToolStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStepStatus {
    /// Ingesting streaming arguments from model provider (pre-dispatch).
    Ingesting,
    /// Arguments complete, actively executing in local host runtime / sandbox.
    Executing,
    /// Finished with a non-error output.
    Ok,
    /// Finished with an explicit error output.
    Failed,
    /// Aborted because the user denied permission for the call.
    Denied,
    /// Aborted mid-flight (e.g. the user interrupted the turn). Terminal, just
    /// like `Ok`/`Failed`: a later result or cancel event is ignored.
    Cancelled,
    /// Stopped by the user (the turn was interrupted) *after* producing real
    /// work: the subagent's partial transcript was preserved. Distinct from
    /// [`ToolStepStatus::Cancelled`] (nothing recovered) and
    /// [`ToolStepStatus::Failed`] (the sub-task errored on its own): this is
    /// resumable work the user deliberately cut short.
    Interrupted,
}

impl ToolStepStatus {
    /// Whether this state can still transition (i.e. the step is in flight).
    pub fn is_running(self) -> bool {
        matches!(self, ToolStepStatus::Ingesting | ToolStepStatus::Executing)
    }

    pub fn is_ingesting(self) -> bool {
        matches!(self, ToolStepStatus::Ingesting)
    }

    pub fn is_executing(self) -> bool {
        matches!(self, ToolStepStatus::Executing)
    }

    /// ADR-0166: lifecycle order, used by the monotone reducer to decide whether
    /// an incoming fact advances the step. Terminal states share one rank: a
    /// step that reached any terminal state is never moved to another one by a
    /// merge (a real result is the only thing that may terminalize a running
    /// step, and it goes through `finish_tool_step`).
    pub fn rank(self) -> u8 {
        match self {
            ToolStepStatus::Ingesting => 0,
            ToolStepStatus::Executing => 1,
            ToolStepStatus::Ok
            | ToolStepStatus::Failed
            | ToolStepStatus::Denied
            | ToolStepStatus::Cancelled
            | ToolStepStatus::Interrupted => 2,
        }
    }
}

/// Detailed execution phase of an autonomous tool slot (ADR-0046).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionPhase {
    /// Ingesting streaming arguments from model provider.
    Ingesting { bytes: Option<usize> },
    /// Dispatched and actively executing in local host/sandbox runtime.
    Executing { elapsed_ms: u64 },
    /// Terminal outcome (succeeded, failed, cancelled, denied, interrupted).
    Terminal(ToolStepStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    pub name: String,
    pub args: String,
}

impl CommandInvocation {
    pub fn new(name: impl Into<String>, args: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: args.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserPromptKind {
    #[default]
    Normal,
    Steer,
    FollowUp,
}

/// Admission provenance of a user message, used by the renderer to choose the
/// message's visual weight.
///
/// `UserMessageOrigin` answers *who/what* produced the message (`steer`,
/// `follow-up`, `cmd`); this enum answers *where it landed causally*. The
/// distinction matters because the two steer outcomes render differently:
///
/// ```text
/// › steer   不要改协议层，只改 nuo-view 布局      14:21      ← InRound (one row)
/// › round 4  follow-up                            14:22      ← RoundOpening (panel)
/// ```
///
/// These tags are **render metadata, not dialogue**: they are never
/// serialized and are reconstructed by the restore engine from the durable
/// `InjectionKind` (ADR-0137). A message with no tags renders at round weight
/// (the legacy default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOriginTag {
    /// Admitted at the turn seam of a *running* round (`SteerAdmitted`): a
    /// tactical course-correction inside the ReAct chain. Renders as a
    /// one-body-row interjection with no header/gap/transition rows.
    InRound,
    /// "Follow-up" content admitted as a fresh round's opening prompt
    /// (`FollowUpStarted`): opens a new round, so it keeps the full
    /// round-level user panel.
    RoundOpening,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemNoticeTopic {
    Interrupted,
    Trust,
    Review,
    TurnGuard,
    CommandAck,
    /// Image attachments were withheld from a request because the route cannot
    /// take them.
    Images,
    /// A user-authored configuration surface was rejected.
    Configuration,
    Custom(String),
}

/// Who produced a notice.
///
/// There used to be a `Provider { provider_name, attempt }` variant carrying the
/// retry attempt pair. Both fields became dead ends once the live retry stopped
/// being a notice: the attempt count lives in `nuo_view::retry::RetryProgress`,
/// and a provider fault that reaches the transcript is a *round outcome*
/// (`round_interrupted`), which is a system topic. Keeping the variant would
/// have left a field that is always `None` — the exact shape that invites a
/// future producer to fill it with a second, disagreeing count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeOrigin {
    System { topic: SystemNoticeTopic },
}

#[derive(Debug, Clone)]
pub enum MessageKind {
    Text,
    ToolStep {
        id: String,
        name: String,
        /// The bound subagent profile name (`explore` / `plan` / `verify` / …)
        /// for a subagent-spawning tool step, populated from the first
        /// `SubagentEvent::Started` and used to label the step by its role.
        /// `None` for non-subagent steps, or until the `Started` event lands.
        profile: Option<String>,
        arguments: String,
        output: Option<String>,
        /// Typed result (ADR-0001). `None` until the result lands, then a
        /// [`nuo_protocol::ToolOutput`] carrying structured data (e.g. a shell
        /// exit code) alongside the legacy `output` text. Consumed by the
        /// renderer for data-level classification — `finish_tool_step` derives
        /// [`ToolStepStatus`] from `ToolOutput::is_error()` instead of
        /// string-sniffing the output, and `bash_command_for` reads the typed
        /// `Shell` command. The legacy `output`/`arguments` strings remain the
        /// fallback for restored sessions that predate the typed payload.
        ///
        /// Boxed to keep this enum variant small: `ToolOutput` (and especially
        /// its `Subagent`/`Patch` variants) is large enough that an unboxed
        /// `Option<ToolOutput>` would dominate the `MessageKind` enum size
        /// (clippy::large_enum_variant). The indirection is transparent to
        /// callers — the surrounding accessors deref it as needed.
        structured: Option<Box<nuo_protocol::ToolOutput>>,
        /// Explicit lifecycle. Kept in sync with `output` by the
        /// `finish_tool_step` / `cancel_tool_step` transitions below.
        status: ToolStepStatus,
        expanded: bool,
        /// Whether the user has manually pinned `expanded`. While true, the
        /// auto/system setter (`set_tool_step_expanded`) is a no-op so
        /// lifecycle transitions can't override a deliberate user choice.
        user_pinned: bool,
        /// ADR-0115: whether the user has revealed the folded middle of this
        /// step's output. Ephemeral view state (like `started_at`): never
        /// persisted, and only meaningful while `expanded` — collapsing the
        /// step forgets the reveal so the next expansion starts folded.
        output_revealed: bool,
        duration_ms: Option<u64>,
        /// Wall-clock instant the step started, so the UI can show a live
        /// elapsed time while the call (or subagent) is still running.
        /// `Instant` is cheap to capture at construction time and is not
        /// serialized — session restore reconstructs finished steps without it.
        started_at: Option<std::time::Instant>,
        /// ADR-0046: Wall-clock instant when arguments completed and local execution
        /// began. `None` while arguments are still streaming (pre-dispatch).
        execution_started_at: Option<std::time::Instant>,
        /// Set when this subagent surfaced a permission / user-input request that
        /// is still parked awaiting a human decision. The peek row reads it to
        /// show `awaiting approval` instead of the last tool activity, which
        /// would misleadingly suggest the subagent is still making progress.
        /// Cleared by the next progress event from this subagent (tool call,
        /// tool result, or streamed text) and on any terminal transition.
        awaiting: bool,
        /// Latest free-text activity line the subagent reported via
        /// `SubagentEvent::Activity` (`waiting for model`, `waiting to retry
        /// (3s)`, …). The peek row prefers it over the derived
        /// `starting`/`thinking` fallbacks while no child event has landed
        /// yet, so a long model call reads as alive instead of stuck on
        /// `starting`. Not serialized — restored sessions render terminal
        /// steps, which never show a peek.
        activity: Option<String>,
        /// The subagent's provider-retry progress, from
        /// [`nuo_protocol::SubagentEvent::RetryScheduled`].
        ///
        /// A subagent's setback is real state with a real countdown, so it gets
        /// the same typed treatment as the principal round's
        /// (`nuo_view::retry`) instead of being flattened into an `activity`
        /// string the renderer would have to parse. Boxed because almost every
        /// step in a transcript has never retried, and `ToolStep` is already the
        /// largest `MessageKind` variant.
        retry: Option<Box<crate::retry::RetryProgress>>,
        /// ADR-0026: argument bytes received so far while this call's
        /// arguments are still streaming (count-only, `[INV-STREAM-TOOL-03]`).
        /// `None` once the call dispatches (the count stops being meaningful)
        /// or when the arguments arrived whole. Rendered as a static summary
        /// clause — never a `+`/`-` disclosure marker (that glyph means
        /// "foldable", not "streaming") and never an animated glyph
        /// (`[INV-STREAM-TOOL-05]`).
        input_bytes: Option<usize>,
        /// Child events emitted by a subagent spawned from this tool step.
        children: Vec<TranscriptMessage>,
    },
    Reasoning {
        content: String,
        duration_ms: Option<u64>,
        expanded: bool,
        /// User-pinned flag — see [`MessageKind::ToolStep::user_pinned`].
        user_pinned: bool,
        /// Milestone (heading) count, frozen at construction / stream end.
        /// Only displayed for finished traces, so it never needs per-frame
        /// recomputation.
        milestones: usize,
    },
    /// A harness-level notice — errors, turn-pause signals, compaction
    /// summaries, provider switches, and other status lines that previously
    /// were smuggled through `Role::System` with hand-rolled `"Error: "`
    /// / `"System: "` text prefixes. Carrying an explicit [`NoticeSeverity`]
    /// lets one renderer (the `paint::notice` module) own the
    /// severity→color/icon mapping and lets callers stop string-sniffing.
    ///
    /// [`NoticeParts`] carries the architecture-agreed split — *topic*
    /// (which subsystem is speaking, from the contract's
    /// `NoticeKind`/`NoticeSource` vocabulary) + *detail* (`title`/`body`) —
    /// so core notices render from structure instead of re-parsing `raw`.
    /// `None` for local/legacy notices; the renderer then falls back to its
    /// heuristic text parse.
    Notice {
        severity: NoticeSeverity,
        parts: Option<Box<NoticeParts>>,
        expanded: bool,
        user_pinned: bool,
    },
    /// A command invocation as **one component that owns both its input and its output**
    CommandResult {
        invocation: CommandInvocation,
        /// The typed result. `None` while the command is still
        /// running, and when the invocation was recorded but the reply was
        /// never persisted (legacy echo folds). Boxed to keep this enum
        /// variant small (`CommandResult` carries `Vec<SearchHit>` / `Vec<ReviewVerdict>`).
        result: Option<Box<nuo_protocol::CommandResult>>,
        /// Lifecycle of the invocation — see [`CommandPhase`].
        phase: CommandPhase,
        expanded: bool,
        /// User-pinned flag — see [`MessageKind::ToolStep::user_pinned`].
        user_pinned: bool,
    },
    /// A structured, inspectable compaction checkpoint card.
    CompactedCard {
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
        summary: Option<String>,
        tracked_files: Vec<String>,
        expanded: bool,
        user_pinned: bool,
    },
}

/// Lifecycle of a command component. Commands are synchronous
/// control-plane operations, so the lifecycle has exactly two live states plus
/// the cancel mark — unlike a tool step there is no permission-denied or
/// interrupted state to represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPhase {
    /// Dispatched, no result yet. The row shows the invocation alone in the
    /// muted running tone (`⌘ /delegate`) — the input half of the component
    /// is already durable in the transcript, so a slow command never leaves
    /// the user wondering whether it ran.
    Pending,
    /// The typed result arrived (or is known not to exist — legacy folds,
    /// shell passthroughs): the row shows `invocation  reply` per its
    /// [`CommandRowLayout`].
    Completed,
    /// No result will ever arrive (the session view moved on before the reply
    /// landed, or the runtime errored out of band). Reads as a settled row
    /// with no reply, never as a promise.
    Cancelled,
}

/// How a command row presents its result — derived at render time from the
/// result's shape, not stored. Commands are operations, not thread dialogue:
/// most replies are one short line that should simply *be* the row, with no
/// disclosure marker at all. Only a genuinely long reply earns the `+`/`-`
/// affordance..
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRowLayout {
    /// No result at all (shell passthroughs, legacy folds) — the row is just
    /// the invocation, dimmed. Nothing to expand.
    Plain,
    /// A single-line, short reply (acks, `/new`'s confirmation, `/schedule`):
    /// rendered inline on the same row as `invocation  reply`. No marker —
    /// there is no second view to disclose.
    Inline,
    /// A multi-line or long reply (`/search`, `/session status`, `/review`,
    /// …): the disclosure pattern is correct — a `+`/`-` header row that
    /// expands to the body.
    Disclose,
}

/// Width reserved for a trailing `HH:MM` timestamp on a command
/// card when `sent_at_ms` is present — reserved by
/// [`TranscriptMessage::command_row_layout`] before the inline/Disclose
/// classification so a timestamped row never flips to Disclose at render
/// time.
pub const SENT_TIME_LABEL_COLS: usize = 8;

/// The single classifier for [`CommandRowLayout`]: a reply joins inline when
/// it is exactly one line and fits beside the invocation; otherwise it
/// discloses. `available_width` is the row's usable columns (the terminal
/// band minus gutters), not the full terminal width.
///
/// The classifier subtracts the fixed command-card chrome (identity bar +
/// marker slot + family glyph) from the budget: the inline join
/// has to fit *inside the card*, not merely inside the terminal.
pub const COMMAND_ROW_CHROME_COLS: usize = COMMAND_CARD_LEAD_COLS + 2 /* marker slot */ + 2 /* glyph */;
pub fn command_row_layout(
    result: Option<&nuo_protocol::CommandResult>,
    invocation: &str,
    available_width: usize,
) -> CommandRowLayout {
    let Some(result) = result else {
        return CommandRowLayout::Plain;
    };
    let text = result.to_text();
    if text.contains('\n') {
        return CommandRowLayout::Disclose;
    }
    // The inline join is `invocation` + a two-column peer gap + reply; the row
    // must hold both without truncation for the reply to read as an
    // attribute, not a fragment. The card chrome (identity bar + marker slot
    // + glyph) and the trailing timestamp eat into the same row, so
    // the budget subtracts them — but the time label is render-time state the
    // classifier cannot see, so the classifier subtracts only the fixed
    // chrome and the renderer's clamp guards the timestamp.
    let used = COMMAND_ROW_CHROME_COLS + invocation.width() + JOIN_ENUMERATE_COLS + text.width();
    if used <= available_width {
        CommandRowLayout::Inline
    } else {
        CommandRowLayout::Disclose
    }
}

/// The headline/detail split of an ack reply, when the record carries detail
/// lines. The title is the part worth the reader's first glance; the detail
/// is the muted explanation beneath it (two-tone ack scheme).
pub fn command_ack_split(
    result: Option<&nuo_protocol::CommandResult>,
) -> Option<(&str, &[String])> {
    let nuo_protocol::CommandResult::Ack { title, detail } = result? else {
        return None;
    };
    let detail = detail.as_deref().filter(|d| !d.is_empty())?;
    Some((title, detail))
}

/// Severity of a [`MessageKind::Notice`]. Drives the color and the leading
/// icon through the central severity→presentation map in
/// `render/notice.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    /// Neutral status (compaction summary, provider switch, …). Replaces the
    /// old `Role::System` + `system_text()` rendering.
    Info,
    /// A non-terminal condition that needs attention.
    Warning,
    /// A terminal failure surfaced from the harness or a tool.
    Error,
}

impl NoticeSeverity {
    /// Whether this severity is a fault: something the user must know about
    /// even if they were not looking. An `Info` disclosure confirms an action
    /// the user just took and can repeat, so it is retained but not escalated.
    ///
    /// One predicate, read by the bubble shape, its duration and its hint, so
    /// "what counts as a fault" cannot drift between them.
    pub fn is_fault(self) -> bool {
        matches!(self, Self::Warning | Self::Error)
    }
}

pub fn notice_severity_from_core(severity: nuo_protocol::NoticeSeverity) -> NoticeSeverity {
    match severity {
        nuo_protocol::NoticeSeverity::Info => NoticeSeverity::Info,
        nuo_protocol::NoticeSeverity::Warning => NoticeSeverity::Warning,
        nuo_protocol::NoticeSeverity::Error => NoticeSeverity::Error,
    }
}

/// The two-part, architecture-agreed content of a notice entry.
///
/// A notice is not an opaque string: it is *who is speaking* plus *what
/// happened*. The topic is a predictable label from the contract vocabulary
/// (see [`notice_topic_label`]), and the detail is the structured
/// `title`/`body` pair carried by `AgentNotice` across the wire. Populated by
/// [`TranscriptMessage::notice_from_core`]; local/legacy notices leave it
/// absent and the renderer falls back to its heuristic text parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeParts {
    /// Origin classification (System vs Provider).
    pub origin: Option<NoticeOrigin>,
    /// Predictable subsystem label ("trust", "provider", "turn guard",
    /// "review", "command", "interrupted") identifying the notice's origin. Rendered as the
    /// entry head in place of the generic "notification" constant.
    pub topic: Option<String>,
    /// Summary line (`AgentNotice::title`), rendered as the bold body lead.
    pub title: String,
    /// Optional detail prose (`AgentNotice::body`), rendered muted below the
    /// title with blank-line paragraph separators preserved.
    pub detail: Option<String>,
}

/// Map a contract notice kind to its user-facing topic label — the
/// predictable, architecture-agreed vocabulary for "what is speaking".
/// Each kind maps 1:1 to its topic; frontends may localize these, the
/// *kind* stays stable on the wire.
pub fn notice_topic_label(kind: nuo_protocol::NoticeKind) -> &'static str {
    match kind {
        nuo_protocol::NoticeKind::CheckpointReplay => "replay",
        nuo_protocol::NoticeKind::NudgeInjected => "turn guard",
        nuo_protocol::NoticeKind::ReviewAlert => "review",
        nuo_protocol::NoticeKind::TrustChanged => "trust",
        nuo_protocol::NoticeKind::TrustFailure => "trust error",
        nuo_protocol::NoticeKind::CommandAck => "command",
        nuo_protocol::NoticeKind::ImageInputWithheld => "images",
        nuo_protocol::NoticeKind::Configuration => "config",
    }
}

/// Table column text alignment for GFM tables parsed by the in-house parser,
/// kept as a separate type so the `Block` definition stays dependency-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlignment {
    None,
    Left,
    Center,
    Right,
}

/// Lifecycle of a user-authored message from the user's point of view.
///
/// All other roles are inherently "delivered" (the harness only renders them
/// once they exist), so this only matters on `Role::User` messages.
///
/// There is deliberately **no** "pending in the outbox" and no "held for next
/// round" state (ADR-0137): a message enters the thread only when the harness
/// admitted it, so a row in the document is never waiting for anything. Pending
/// queue items live in the outbox, not here — `SteerUnavailable` hands a missed
/// steer back to the outbox or the composer, and it never remains in the
/// transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryStatus {
    /// The message has been handed off to the agent (or is an assistant /
    /// tool / system message that doesn't go through the queue).
    #[default]
    Delivered,
    /// The prompt was sent and is waiting for a response from the agent.
    Sending,
    /// The prompt was cancelled / interrupted before the model responded.
    Cancelled,
}

/// A structured transcript message.
/// What kind of user message this `Role::User` message originates from. Only
/// meaningful for user messages; the other roles carry the default
/// ([`UserMessageOrigin::Chat`]) and it is never consulted for them.
///
/// The Activity modal uses this to decide whether a `Role::User` message is
/// the genuine prompt that drove the current round: slash commands
/// (`/review …`) are surfaced as user messages in the transcript but are
/// *not* the LLM prompt, so they must not be shown as the round's "Prompt".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserMessageOrigin {
    /// A normal chat prompt the user composed and sent to the model. This is
    /// the only origin the Activity modal treats as the round's prompt.
    #[default]
    Chat,
    /// Steering input admitted at an inner turn boundary of a running round.
    Steer,
    /// Follow-up input executed after the current round completes.
    FollowUp,
    /// A slash command (`/review`, `/pursue …`, …). The harness handles these
    /// directly; the model never sees them as a prompt.
    Slash,
}
