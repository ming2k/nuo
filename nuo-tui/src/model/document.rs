//! Semantic document model for the TUI.
//!
//! Unlike storing raw strings, this model preserves the structure of messages
//! so that selection and copy operate on semantic units (blocks) rather than
//! terminal grid characters.

use nuo_wire::{Role, SubagentEvent};

use crate::design::{COMMAND_CARD_LEAD_COLS, JOIN_ENUMERATE_COLS};
use unicode_width::UnicodeWidthStr;

/// Lifecycle of a tool step, stored explicitly (not inferred from `output`)
/// so an aborted call has its own terminal state instead of being stuck in
/// "no output yet". This is the single source of truth for tool-step state —
/// the renderer classifies it into a [`crate::tools::ToolStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolStepStatus {
    /// Still in flight (no terminal event observed yet).
    #[default]
    Running,
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
        matches!(self, ToolStepStatus::Running)
    }
}

pub type ToolInvocationStatus = ToolStepStatus;

/// A structured tool invocation with its lifecycle and output.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    pub profile: Option<String>,
    pub arguments: String,
    pub output: Option<String>,
    pub structured: Option<Box<nuo_wire::ToolOutput>>,
    pub status: ToolInvocationStatus,
    pub expanded: bool,
    pub user_pinned: bool,
    pub duration_ms: Option<u64>,
    pub started_at: Option<std::time::Instant>,
    pub awaiting: bool,
    pub activity: Option<String>,
    /// ADR-0026: argument bytes received so far while the call's arguments
    /// stream; `None` once dispatched or when the arguments arrived whole.
    pub input_bytes: Option<usize>,
    /// ADR-0026: the provider's tool-call slot while pre-dispatch; `None` once
    /// dispatched or for a never-announced step.
    pub input_slot: Option<usize>,
    pub children: Vec<TranscriptMessage>,
}

impl ToolInvocation {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            profile: None,
            arguments: arguments.into(),
            output: None,
            structured: None,
            status: ToolInvocationStatus::Running,
            expanded: false,
            user_pinned: false,
            duration_ms: None,
            started_at: Some(std::time::Instant::now()),
            awaiting: false,
            activity: None,
            input_bytes: None,
            input_slot: None,
            children: Vec::new(),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemNoticeTopic {
    Interrupted,
    Trust,
    Review,
    TurnGuard,
    CommandAck,
    /// Image attachments were withheld from a request because the route cannot
    /// take them (ADR-0230).
    Images,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeOrigin {
    System {
        topic: SystemNoticeTopic,
    },
    Provider {
        provider_name: Option<String>,
        attempt: Option<(usize, usize)>,
    },
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
        /// [`nuo_wire::ToolOutput`] carrying structured data (e.g. a shell
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
        structured: Option<Box<nuo_wire::ToolOutput>>,
        /// Explicit lifecycle. Kept in sync with `output` by the
        /// `finish_tool_step` / `cancel_tool_step` transitions below.
        status: ToolStepStatus,
        expanded: bool,
        /// Whether the user has manually pinned `expanded`. While true, the
        /// auto/system setter (`set_tool_step_expanded`) is a no-op so
        /// lifecycle transitions can't override a deliberate user choice.
        user_pinned: bool,
        duration_ms: Option<u64>,
        /// Wall-clock instant the step started, so the UI can show a live
        /// elapsed time while the call (or subagent) is still running.
        /// `Instant` is cheap to capture at construction time and is not
        /// serialized — session restore reconstructs finished steps without it.
        started_at: Option<std::time::Instant>,
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
        /// ADR-0026: argument bytes received so far while this call's
        /// arguments are still streaming (count-only, `[INV-STREAM-TOOL-03]`).
        /// `None` once the call dispatches (the count stops being meaningful)
        /// or when the arguments arrived whole. Rendered as a static summary
        /// clause — never a `+`/`-` disclosure marker (that glyph means
        /// "foldable", not "streaming") and never an animated glyph
        /// (`[INV-STREAM-TOOL-05]`).
        input_bytes: Option<usize>,
        /// ADR-0026: the provider's tool-call slot for a step that was
        /// announced before dispatch. `Some(index)` while the call's arguments
        /// stream (so a repeated announcement for the same slot — a retried
        /// turn — supersedes the prior one); `None` once dispatched or for a
        /// step that was never announced (whole-argument providers, history).
        input_slot: Option<usize>,
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
    /// Transient provider-retry state rendered live in the transcript as a notification entry.
    ///
    /// Unlike a static notice, this message is updated in place for every failed
    /// attempt and removed when the request succeeds or terminates. Keeping
    /// the timing data structured lets the renderer derive a live countdown
    /// on every frame without appending duplicate lines per retry.
    ProviderRetry {
        attempt: usize,
        max_attempts: usize,
        failure: String,
        retry_at: std::time::Instant,
        expanded: bool,
        user_pinned: bool,
    },
    /// A command invocation as **one component that owns both its input and its output**
    /// (ADR-0108, ADR-0111).
    CommandResult {
        invocation: CommandInvocation,
        /// The typed result (ADR-0091). `None` while the command is still
        /// running, and when the invocation was recorded but the reply was
        /// never persisted (legacy echo folds). Boxed to keep this enum
        /// variant small (`CommandResult` carries `Vec<SearchHit>` / `Vec<ReviewVerdict>`).
        result: Option<Box<nuo_wire::CommandResult>>,
        /// Lifecycle of the invocation (ADR-0108) — see [`CommandPhase`].
        phase: CommandPhase,
        expanded: bool,
        /// User-pinned flag — see [`MessageKind::ToolStep::user_pinned`].
        user_pinned: bool,
    },
    /// A structured, inspectable compaction checkpoint card (ADR-0296).
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

/// Lifecycle of a command component (ADR-0108). Commands are synchronous
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
/// result's shape, not stored. Commands are operations, not conversation:
/// most replies are one short line that should simply *be* the row, with no
/// disclosure marker at all. Only a genuinely long reply earns the `+`/`-`
/// affordance. See ADR-0106.
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
/// marker slot + family glyph, ADR-0109) from the budget: the inline join
/// has to fit *inside the card*, not merely inside the terminal.
pub const COMMAND_ROW_CHROME_COLS: usize = COMMAND_CARD_LEAD_COLS + 2 /* marker slot */ + 2 /* glyph */;
pub fn command_row_layout(
    result: Option<&nuo_wire::CommandResult>,
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
    // + glyph, ADR-0109) and the trailing timestamp eat into the same row, so
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
/// is the muted explanation beneath it (ADR-0106's two-tone ack scheme).
pub fn command_ack_split(
    result: Option<&nuo_wire::CommandResult>,
) -> Option<(&str, &[String])> {
    let nuo_wire::CommandResult::Ack { title, detail } = result? else {
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

pub fn notice_severity_from_core(severity: nuo_wire::NoticeSeverity) -> NoticeSeverity {
    match severity {
        nuo_wire::NoticeSeverity::Info => NoticeSeverity::Info,
        nuo_wire::NoticeSeverity::Warning => NoticeSeverity::Warning,
        nuo_wire::NoticeSeverity::Error => NoticeSeverity::Error,
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
pub fn notice_topic_label(kind: nuo_wire::NoticeKind) -> &'static str {
    match kind {
        nuo_wire::NoticeKind::ProviderRetry => "provider",
        nuo_wire::NoticeKind::NudgeInjected => "turn guard",
        nuo_wire::NoticeKind::ReviewAlert => "review",
        nuo_wire::NoticeKind::TrustChanged => "trust",
        nuo_wire::NoticeKind::CommandAck => "command",
        nuo_wire::NoticeKind::ImageInputWithheld => "images",
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

/// A byte range `[start, end)` within a prose block's `content` that should be
/// rendered as inline code. The in-house parser keeps the backtick delimiters
/// in the flattened `content` and records the range here so the renderer can
/// paint it on the code surface without disturbing the byte-addressable
/// copy/selection model (which still sees plain text).
///
/// Ranges always cover the full `` `…` `` span including both backticks, and
/// are clamped to `content.len()`. An empty vector means "no inline code".
pub type CodeRange = (usize, usize);

/// A byte range `[start, end)` within a prose block's `content` that should be
/// rendered as inline math. The source delimiters stay in `content` for exact
/// copy/selection; renderers may elide them visually.
pub type MathRange = (usize, usize);

/// A byte range for a recognized hyperlink inside prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRange {
    /// Full source range, including markdown / TeX link delimiters when present.
    pub range: (usize, usize),
    /// The visible label range inside `range`. For bare URLs this equals `range`.
    pub label_range: (usize, usize),
    /// Normalized URL target. First-pass support intentionally records only
    /// browser-safe `http://` and `https://` targets.
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineScan {
    pub code_ranges: Vec<CodeRange>,
    pub bold_ranges: Vec<CodeRange>,
    pub math_ranges: Vec<MathRange>,
    pub link_ranges: Vec<LinkRange>,
}

/// Inline-prose payload shared by the prose block variants: the flattened
/// text plus the byte ranges of its inline markup.
///
/// The ranges are produced by [`scan_inline`] at parse time, address bytes in
/// `content`, and are clamped to `content.len()`. `content` keeps the original
/// delimiters (backticks, `**`, link syntax) so copy/selection yields exact
/// source while renderers may elide them visually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub content: String,
    /// Byte ranges of inline-code runs within `content` (see [`CodeRange`]).
    pub code_ranges: Vec<CodeRange>,
    /// Byte ranges of strong/bold text runs within `content`.
    pub bold_ranges: Vec<CodeRange>,
    /// Byte ranges of inline math runs within `content`.
    pub math_ranges: Vec<MathRange>,
    /// Hyperlink ranges within `content`.
    pub link_ranges: Vec<LinkRange>,
}

impl Inline {
    /// Verbatim text with no inline markup.
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }

    /// Scan `content` for inline markup, trimming trailing whitespace and
    /// clamping every range to the trimmed length.
    pub(crate) fn scanned(content: &str) -> Self {
        let scan = scan_inline(content);
        let trimmed_len = content.trim_end().len();
        Self {
            content: content[..trimmed_len].to_string(),
            code_ranges: clamp_ranges(&scan.code_ranges, trimmed_len),
            bold_ranges: clamp_ranges(&scan.bold_ranges, trimmed_len),
            math_ranges: clamp_ranges(&scan.math_ranges, trimmed_len),
            link_ranges: clamp_link_ranges(&scan.link_ranges, trimmed_len),
        }
    }
}

/// A single semantic block within a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Plain text paragraph.
    Text(Inline),
    /// Display math block (`$$…$$` or `\[…\]`).
    Math { content: String },
    /// Inline or fenced code.
    Code {
        language: Option<String>,
        content: String,
    },
    /// A heading.
    Heading { level: u8, inline: Inline },
    /// A list item, preserving its marker and nesting level.
    ListItem {
        inline: Inline,
        ordered: Option<u64>,
        depth: usize,
        checked: Option<bool>,
    },
    /// A blockquote.
    Quote(Inline),
    /// A GFM-style table, kept as a semantic unit so columns stay aligned and
    /// copy yields the rendered grid rather than re-wrapped prose.
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        aligns: Vec<TableAlignment>,
        /// Pre-rendered aligned grid (what is drawn and what copy returns).
        rendered: String,
    },
    /// A horizontal rule.
    Rule,
    /// Soft / hard line break marker.
    Break,
}

impl Block {
    /// Returns the raw text content of this block (without formatting).
    pub fn raw_text(&self) -> &str {
        match self {
            Block::Text(inline) | Block::Quote(inline) => &inline.content,
            Block::Math { content } => content,
            Block::Code { content, .. } => content,
            Block::Heading { inline, .. } => &inline.content,
            Block::ListItem { inline, .. } => &inline.content,
            Block::Table { rendered, .. } => rendered,
            Block::Rule => "",
            Block::Break => "\n",
        }
    }

    /// Returns the inline markup metadata of this block, if it has one.
    pub fn inline(&self) -> Option<&Inline> {
        match self {
            Block::Text(inline)
            | Block::Quote(inline)
            | Block::Heading { inline, .. }
            | Block::ListItem { inline, .. } => Some(inline),
            _ => None,
        }
    }

    /// Returns true if this block is empty.
    pub fn is_empty(&self) -> bool {
        self.raw_text().is_empty()
    }
}

/// Lifecycle of a user-authored message from the user's point of view.
///
/// All other roles are inherently "delivered" (the harness only renders them
/// once they exist), so this only matters on `Role::User` messages. The TUI
/// uses it to draw a distinct "⏸ Queued" panel while a message is waiting for
/// the in-flight turn to finish, and the event loop flips it back to
/// [`DeliveryStatus::Delivered`] once the queued message is actually shipped
/// to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryStatus {
    /// The message has been handed off to the agent (or is an assistant /
    /// tool / system message that doesn't go through the queue).
    #[default]
    Delivered,
    /// The user pressed Enter while a turn was still running, so the message
    /// is staged in the TUI's send queue and will be dispatched automatically
    /// when the harness returns to idle.
    Queued,
    /// A busy-Enter steer whose round ended — naturally or by an
    /// interrupt (Esc Esc) — before it could be admitted at a turn boundary.
    /// The entry stays in the transcript (it never leaves the conversation)
    /// but is re-queued as the **next round's** prompt: it renders with the
    /// same pending treatment as [`DeliveryStatus::Queued`] and flips to
    /// delivered when that round starts.
    HeldNextRound,
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

/// Monotonic source of per-message identities. A message keeps its `id` across
/// the per-frame clone into `App::messages`, so the renderer
/// can use it as a stable cache key for the message's laid-out height (see the
/// height cache in `render`). Ids are process-unique; cloning a message copies
/// its id (a clone represents the same logical message), which is exactly what
/// the height cache wants.
static NEXT_MESSAGE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_message_id() -> u64 {
    NEXT_MESSAGE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct TranscriptMessage {
    /// Stable, process-unique identity used as the renderer's height-cache key.
    /// Assigned at construction and preserved across clones.
    pub id: u64,
    pub role: Role,
    pub blocks: Vec<Block>,
    /// The original raw markdown/text, preserved byte-for-byte. Copy resolves
    /// against the parsed `blocks` (rendered plain text), so this is retained
    /// for re-parsing, layout, and any consumer that needs the source form.
    pub raw: String,
    pub kind: MessageKind,
    /// What kind of user message this `Role::User` message is. Defaults to
    /// [`UserMessageOrigin::Chat`]; slash commands and shell passthroughs mark
    /// themselves so they are not mistaken for the round's driving prompt.
    pub origin: UserMessageOrigin,
    /// Lifecycle of this message from the send queue's point of view. Only
    /// `Role::User` messages ever carry [`DeliveryStatus::Queued`]; everything
    /// else stays at the default [`DeliveryStatus::Delivered`]. The renderer
    /// and the queue dispatch/recall paths key off this.
    pub delivery: DeliveryStatus,
    /// Correlation id for a busy-Enter steer (`AgentRequest::Steer`). Set when
    /// the entry is staged into the
    /// transcript as [`DeliveryStatus::Queued`]; the response listener uses it
    /// to find and settle the entry when the harness reports
    /// `UserInputInserted` / `NextRoundStarted` / `UserInputUnavailable`.
    /// `None` for every non-insert message.
    pub insert_id: Option<String>,
    /// Provider/solution id that produced this message, mirrored from the
    /// core [`nuo_wire::Message`] so the transcript stays traceable across
    /// model switches. `None` for messages that don't carry attribution.
    pub provider: Option<String>,
    /// Model id that produced this message, companion to [`TranscriptMessage::provider`].
    pub model: Option<String>,
    /// The reasoning effort (depth) this message's model request ran with
    /// (`"high"`, `"max"`, …), when the active channel exposes one. Stamped at
    /// the same point as [`TranscriptMessage::model`] so the turn header can
    /// show the depth a given turn actually ran at. `None` for non-reasoning
    /// channels and messages that carry no attribution.
    pub effort: Option<String>,
    /// The user-visible round this message belongs to (1-indexed). Driving
    /// user messages open a round; assistant-side messages inherit it.
    pub round: Option<u64>,
    /// The ReAct turn this assistant-side message belongs to within its round
    /// (1-indexed, stamped from `TurnStarted`). The renderer uses the
    /// `(round, turn)` position to identify compact tool batches. `None`
    /// means the position is unknown; legacy tool batches retain a compatible
    /// flush-stack fallback.
    pub turn: Option<u64>,
    /// Wall-clock send time for transcript headers, in Unix epoch milliseconds.
    /// Restored messages use the persisted millisecond value when available and
    /// fall back to the durable core-message timestamp for legacy sessions.
    pub sent_at_ms: Option<u64>,
    /// Durable provenance / injection origin stamped by the harness (ADR-0050).
    pub injection_origin: Option<nuo_wire::InjectionOrigin>,
    /// Component state revision. Incremented on mutations so the cascade
    /// layout cache instantly detects if remeasurement is required.
    pub rev: u64,
    /// Incremental parse state (ADR-0184): byte offset in `raw` where the
    /// still-open final construct begins. Everything before it is a frozen
    /// prefix whose blocks can never change; `push_stream` re-parses only
    /// from here, making streaming parse cost O(delta) amortized.
    resume_byte: usize,
    /// Number of trailing blocks in `blocks` produced by that open construct
    /// (the region a suffix parse replaces). The rest of `blocks` is frozen.
    live_blocks: usize,
}

impl TranscriptMessage {
    pub fn new(role: Role, raw: impl Into<String>) -> Self {
        let raw = sanitize_text(&raw.into()).into_owned();
        // User messages are rendered verbatim as plain text — no markdown
        // interpretation — so pasted text containing markdown-like syntax
        // does not get mangled into headings/code fences/lists and the
        // transcript stays readable. The raw text becomes a single `Text`
        // block; `wrap_text` preserves intra-block line breaks.
        let (blocks, resume) = if role == Role::User {
            let blocks = parse_blocks_plain(&raw);
            let resume = ParseResume {
                resume_offset: raw.len(),
                live_len: 0,
            };
            (blocks, resume)
        } else {
            parse_blocks_tracked(&raw)
        };
        Self {
            id: next_message_id(),
            role,
            blocks,
            raw,
            kind: MessageKind::Text,
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: resume.resume_offset,
            live_blocks: resume.live_len,
        }
    }

    /// Increment state revision after a mutation to invalidate stale height cache entries.
    pub fn bump_rev(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    /// Label this `Role::User` message with its turn origin (slash command /
    /// shell passthrough). No-op for non-user messages, which never surface
    /// an origin. Builder-style, used alongside [`Self::queued`].
    pub fn with_origin(mut self, origin: UserMessageOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// User prompt classification (Normal / Steer / FollowUp).
    pub fn prompt_kind(&self) -> UserPromptKind {
        match self.origin {
            UserMessageOrigin::Chat => UserPromptKind::Normal,
            UserMessageOrigin::Steer => UserPromptKind::Steer,
            UserMessageOrigin::FollowUp => UserPromptKind::FollowUp,
            _ => UserPromptKind::Normal,
        }
    }

    /// Mark this message as queued in the send queue (waiting for the
    /// in-flight turn to finish before it is dispatched). Only meaningful on
    /// `Role::User` messages; the renderer and dispatch logic key off this.
    pub fn queued(mut self) -> Self {
        self.delivery = DeliveryStatus::Queued;
        self
    }

    /// Whether this message is currently sending / waiting for response.
    pub fn is_sending(&self) -> bool {
        self.delivery == DeliveryStatus::Sending
    }

    /// Whether this message was cancelled / interrupted.
    pub fn is_cancelled(&self) -> bool {
        self.delivery == DeliveryStatus::Cancelled
    }

    /// Mark this message as sending (in-flight prompt waiting for response).
    pub fn sending(mut self) -> Self {
        self.delivery = DeliveryStatus::Sending;
        self
    }

    /// Mark this message as cancelled / interrupted.
    pub fn cancelled(mut self) -> Self {
        self.delivery = DeliveryStatus::Cancelled;
        self
    }

    /// Cancel this prompt in place if it is sending, queued, or an unpositioned user prompt.
    pub fn cancel_prompt(&mut self) {
        if self.delivery == DeliveryStatus::Sending
            || self.delivery == DeliveryStatus::Queued
            || (self.role == Role::User && self.round.is_none())
        {
            self.delivery = DeliveryStatus::Cancelled;
            self.rev += 1;
        }
    }

    /// Settle an in-flight sending prompt to delivered when model starts responding.
    pub fn settle_delivered(&mut self) {
        if self.delivery == DeliveryStatus::Sending {
            self.delivery = DeliveryStatus::Delivered;
            self.rev += 1;
        }
    }

    /// Correlate this message with a busy-Enter steer by its
    /// harness-side input id, so the response listener can settle the entry
    /// when the insert is admitted or handed back. Builder-style companion of
    /// [`Self::queued`].
    pub fn with_insert_id(mut self, insert_id: impl Into<String>) -> Self {
        self.insert_id = Some(insert_id.into());
        self
    }

    /// Mark this insert entry as waiting for the **next** round: the round it
    /// was steered into ended (naturally or interrupted) before admission, so
    /// the content ships as a fresh round's prompt instead. Idempotent on
    /// already-delivered messages — a late race can never un-deliver one.
    pub fn hold_pending_round(&mut self) {
        if self.delivery == DeliveryStatus::Queued {
            self.delivery = DeliveryStatus::HeldNextRound;
        }
    }

    /// Stamp the provider/solution id and model that produced this message.
    pub fn with_attribution(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self
    }

    /// Stamp the reasoning effort (depth) this message's model request ran
    /// with. Kept separate from [`Self::with_attribution`] so existing call
    /// sites (and their tests) keep their arity; pass `None` (or skip the
    /// call) when the channel exposes no effort.
    pub fn with_effort(mut self, effort: Option<impl Into<String>>) -> Self {
        self.effort = effort.map(Into::into);
        self
    }

    /// Stamp the enclosing user round.
    pub fn with_round(mut self, round: u64) -> Self {
        self.round = Some(round);
        self
    }

    /// Stamp the ReAct turn within the enclosing round.
    pub fn with_turn(mut self, turn: u64) -> Self {
        self.turn = Some(turn);
        self
    }

    /// ADR-0023: stamp the provider tool-call slot on a pre-dispatch step, the
    /// correlation key for the announce → collapse handshake.
    pub fn with_input_slot(mut self, slot: usize) -> Self {
        if let MessageKind::ToolStep { input_slot, .. } = &mut self.kind {
            *input_slot = Some(slot);
        }
        self
    }

    /// ADR-0026: whether this tool step was announced before its arguments
    /// finished streaming — it carries a provider slot and no dispatch id yet.
    pub fn is_announced_pending_tool_step(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep {
                id,
                input_slot: Some(_),
                ..
            } if id.is_empty()
        )
    }

    /// ADR-0026: whether this is the still-pending announced step for `slot`.
    pub fn is_announced_pending_tool_step_for_slot(&self, slot: usize) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep {
                id,
                input_slot: Some(s),
                ..
            } if id.is_empty() && *s == slot
        )
    }

    /// ADR-0026: record count-only input progress for `call_id` (the step's own
    /// dispatch id — empty while pre-dispatch). Returns whether the count moved.
    pub fn set_tool_input_bytes(&mut self, call_id: &str, bytes: usize) -> bool {
        let MessageKind::ToolStep {
            id, input_bytes, ..
        } = &mut self.kind
        else {
            return false;
        };
        if id != call_id || *input_bytes == Some(bytes) {
            return false;
        }
        *input_bytes = Some(bytes);
        true
    }

    /// ADR-0026: collapse a pre-dispatch step onto its dispatch `call_id` and complete arguments —
    /// re-key the id, record arguments, and clear the slot/byte pre-dispatch state in one step.
    pub fn collapse_tool_call(
        &mut self,
        call_id: impl Into<String>,
        arguments: impl Into<String>,
    ) -> bool {
        let call_id = call_id.into();
        let arguments = arguments.into();
        let MessageKind::ToolStep {
            id,
            arguments: step_arguments,
            input_slot,
            input_bytes,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        let was_pending = input_slot.is_some();
        *id = call_id;
        *step_arguments = arguments;
        *input_slot = None;
        *input_bytes = None;
        self.refresh_tool_step();
        was_pending
    }

    /// ADR-0026: collapse a pre-dispatch step onto its dispatch `call_id` —
    /// re-key the id and clear the slot/byte pre-dispatch state in one step.
    pub fn rekey_tool_step(&mut self, call_id: impl Into<String>) -> bool {
        let call_id = call_id.into();
        let MessageKind::ToolStep {
            id,
            input_slot,
            input_bytes,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        let was_pending = input_slot.is_some();
        *id = call_id;
        *input_slot = None;
        *input_bytes = None;
        self.refresh_tool_step();
        was_pending
    }

    /// ADR-0026: mark an announced-but-never-dispatched step as cancelled so it
    /// never hangs. Dispatched steps (with an id) are left untouched.
    pub fn cancel_pending_announced_tool_step(&mut self) -> bool {
        let MessageKind::ToolStep {
            id,
            input_slot,
            input_bytes,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if !id.is_empty() || input_slot.is_none() {
            return false;
        }
        *status = ToolStepStatus::Cancelled;
        *input_slot = None;
        *input_bytes = None;
        true
    }

    /// Stamp the visible send time for a user-authored message.
    pub fn with_sent_at_ms(mut self, sent_at_ms: u64) -> Self {
        self.sent_at_ms = Some(sent_at_ms);
        self
    }

    /// Whether this message is a notice with `Error` severity (indicating an
    /// unrecovered turn error, provider failure, or error notice).
    pub fn is_error_notice(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Notice {
                severity: NoticeSeverity::Error,
                ..
            }
        )
    }

    /// The `(provider, model)` pair to show as an attribution badge, when this
    /// message carries at least a model. Used by the renderer to label which
    /// model produced a turn; `None` when the message has no attribution
    /// (user/system messages, or untagged history).
    #[cfg(test)]
    pub fn attribution_label(&self) -> Option<(String, String)> {
        let model = self.model.clone()?;
        let provider = self.provider.clone().unwrap_or_default();
        Some((provider, model))
    }

    pub fn tool_step(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        let mut message = Self {
            id: next_message_id(),
            role: Role::Tool,
            blocks: Vec::new(),
            raw: String::new(),
            kind: MessageKind::ToolStep {
                id: id.into(),
                name: name.into(),
                profile: None,
                arguments: arguments.into(),
                output: None,
                structured: None,
                status: ToolStepStatus::Running,
                expanded: false,
                user_pinned: false,
                duration_ms: None,
                started_at: Some(std::time::Instant::now()),
                awaiting: false,
                activity: None,
                input_bytes: None,
                input_slot: None,
                children: Vec::new(),
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        };
        message.refresh_tool_step();
        message
    }

    /// A slash-command invocation with its typed result (ADR-0091). `name` is
    /// the command word without the leading slash (`"search"`), `"shell"` for
    /// a `!command` passthrough. The collapsed row shows the invocation; the
    /// expandable body shows `result.to_text()`. The row starts `Completed`.
    pub fn command_result(
        name: impl Into<String>,
        args: impl Into<String>,
        result: Option<nuo_wire::CommandResult>,
    ) -> Self {
        Self::command_result_in_phase(name, args, result, CommandPhase::Completed)
    }

    /// The optimistic dispatch row (ADR-0108): the user just sent the command
    /// and no result exists yet. Renders as the pending input half of the
    /// command component; the `RoundEvent::CommandResult` handler settles it
    /// in place via [`Self::settle_command_result`].
    pub fn pending_command(name: impl Into<String>, args: impl Into<String>) -> Self {
        Self::command_result_in_phase(name, args, None, CommandPhase::Pending)
    }

    fn command_result_in_phase(
        name: impl Into<String>,
        args: impl Into<String>,
        result: Option<nuo_wire::CommandResult>,
        phase: CommandPhase,
    ) -> Self {
        let name = name.into();
        let args = args.into();
        let invocation = if args.is_empty() {
            format!("/{}", name)
        } else {
            format!("/{} {}", name, args)
        };
        let result_text = result
            .as_ref()
            .map(|result| result.to_text())
            .unwrap_or_default();
        // An ack's newlines are its chosen structure (headline + muted detail
        // lines, ADR-0106), so it parses plain — the markdown parser's
        // soft-break rule would squeeze those lines onto one row. Every other
        // result keeps the markdown block renderer (lists, tables, code).
        let is_ack = matches!(result, Some(nuo_wire::CommandResult::Ack { .. }));
        Self {
            id: next_message_id(),
            // A harness artifact, not user or model prose — the renderer gives
            // it its own dimmed command-row treatment.
            role: Role::Tool,
            blocks: if is_ack {
                parse_blocks_plain(&result_text)
            } else {
                parse_blocks(&result_text)
            },
            raw: sanitize_text(&invocation).into_owned(),
            kind: MessageKind::CommandResult {
                invocation: CommandInvocation { name, args },
                result: result.map(Box::new),
                phase,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        }
    }

    /// Construct a structured, inspectable compaction checkpoint card (ADR-0296).
    pub fn compacted_card(
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
        summary: Option<String>,
        tracked_files: Vec<String>,
    ) -> Self {
        let summary_text = summary.clone().unwrap_or_default();
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks: parse_blocks(&summary_text),
            raw: summary_text,
            kind: MessageKind::CompactedCard {
                archived_messages,
                window_tokens_before,
                window_tokens_after,
                summary,
                tracked_files,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        }
    }

    pub fn is_compacted_card(&self) -> bool {
        matches!(self.kind, MessageKind::CompactedCard { .. })
    }

    pub fn compacted_card_data(
        &self,
    ) -> Option<(usize, usize, usize, Option<&str>, &[String], bool)> {
        match &self.kind {
            MessageKind::CompactedCard {
                archived_messages,
                window_tokens_before,
                window_tokens_after,
                summary,
                tracked_files,
                expanded,
                ..
            } => Some((
                *archived_messages,
                *window_tokens_before,
                *window_tokens_after,
                summary.as_deref(),
                tracked_files.as_slice(),
                *expanded,
            )),
            _ => None,
        }
    }

    /// Settle a pending command component with its typed result (ADR-0108):
    /// this is the *only* live path that turns [`CommandPhase::Pending`] into
    /// [`CommandPhase::Completed`], and it reuses the existing message id so
    /// the row is updated in place — one component, input and output, no
    /// second row and no seam in the transcript. Returns `false` when the
    /// message is not a pending command (an id mismatch — the pending row was
    /// dropped by a transcript rebuild — so the caller may push a fresh
    /// completed row instead).
    pub fn settle_command_result(&mut self, result: nuo_wire::CommandResult) -> bool {
        let MessageKind::CommandResult {
            result: slot,
            phase,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if *phase != CommandPhase::Pending {
            return false;
        }
        // Keep the parsed body in sync with the stored typed result, the same
        // way the constructor derives it (acks parse plain — their newlines
        // are structure, not markdown soft breaks).
        let result_text = result.to_text();
        let is_ack = matches!(result, nuo_wire::CommandResult::Ack { .. });
        self.blocks = if is_ack {
            parse_blocks_plain(&result_text)
        } else {
            parse_blocks(&result_text)
        };
        // Command results never stream; freeze the whole body so a stray
        // `push_stream` degrades to a safe full re-parse.
        self.resume_byte = self.raw.len();
        self.live_blocks = 0;
        *slot = Some(Box::new(result));
        *phase = CommandPhase::Completed;
        true
    }

    /// Mark a pending command component as never receiving a reply
    /// (ADR-0108) — the input half stays readable but stops promising an
    /// output. Returns whether the transition applied.
    pub fn cancel_pending_command(&mut self) -> bool {
        if let MessageKind::CommandResult { phase, .. } = &mut self.kind
            && *phase == CommandPhase::Pending
        {
            *phase = CommandPhase::Cancelled;
            true
        } else {
            false
        }
    }

    pub fn finish_tool_step(
        &mut self,
        id: &str,
        output: impl Into<String>,
        structured: nuo_wire::ToolOutput,
        duration_ms: u64,
    ) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            output: step_output,
            structured: step_structured,
            status,
            duration_ms: step_duration,
            awaiting,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        *awaiting = false;
        let output = output.into();
        // Classify from the structured result (data-level: a non-zero shell
        // exit, an explicit `ToolOutput::Error`, a `failed` subagent). The
        // legacy `starts_with("Error")` text fallback was removed once tool
        // error sites migrated to `ToolOutput::Error` and subagents carried
        // an explicit `failed` flag — classification is now fully data-driven.
        // Permission denial gets its own status so the UI shows it distinctly
        // from a runtime error.
        *status = if matches!(
            structured,
            nuo_wire::ToolOutput::PermissionDenied { .. }
        ) {
            ToolStepStatus::Denied
        } else if matches!(
            &structured,
            nuo_wire::ToolOutput::Subagent {
                interrupted: true,
                ..
            }
        ) {
            // A cooperatively-drained subagent: the user interrupted the turn,
            // but the partial transcript was preserved. Classified before
            // `is_error()` because interruption is not a failure.
            ToolStepStatus::Interrupted
        } else if structured.is_error() {
            ToolStepStatus::Failed
        } else {
            ToolStepStatus::Ok
        };
        *step_output = Some(output);
        *step_structured = Some(Box::new(structured));
        *step_duration = Some(duration_ms);
        self.refresh_tool_step();
        true
    }

    /// Accumulate an incremental stream chunk into a still-running tool step,
    /// so the UI can render partial output (e.g. bash stdout) live. The first
    /// chunk initializes a partial [`nuo_wire::ToolOutput::Shell`]; the
    /// terminal `finish_tool_step` later overwrites it with the final result.
    /// Returns `false` if this isn't a matching running step.
    pub fn push_tool_stream(&mut self, id: &str, stream: &nuo_wire::ToolStream) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            arguments,
            structured,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        if !matches!(
            structured.as_deref(),
            Some(nuo_wire::ToolOutput::Shell { .. })
        ) {
            let cmd = parse_arguments_kv(arguments)
                .into_iter()
                .find(|(k, _)| k == "command")
                .map(|(_, v)| v)
                .unwrap_or_default();
            *structured = Some(Box::new(nuo_wire::ToolOutput::Shell {
                command: cmd,
                stdout: String::new(),
                stderr: String::new(),
                lines: Vec::new(),
                exit: None,
                truncated: false,
                // Still-streaming seed: the real termination lands with the
                // final result (`finish_tool_step`). Default until then.
                termination: nuo_wire::tool_output::ShellTermination::default(),
                detached_job_id: None,
            }));
        }
        if let Some(nuo_wire::ToolOutput::Shell {
            stdout,
            stderr,
            lines,
            ..
        }) = structured.as_deref_mut()
        {
            // Build the TUI-authoritative `lines` view alongside the flat
            // strings so the streaming view matches the final result: stdout/stderr
            // keep their true arrival interleaving, instead of the all-stdout-then-all-stderr
            // degraded band the empty-`lines` fallback used to force.
            //
            // Each stream chunk is one complete `\n`-terminated line (bash's
            // capture is line-buffered and emits `format!("{text}\n")`), so
            // split on `\n` and tag each non-empty piece with its source
            // stream. Trailing empties (from the terminal `\n`) are dropped so
            // they don't paint phantom blank rows.
            let stream_tag = match stream {
                nuo_wire::ToolStream::Stdout(_) => {
                    nuo_wire::tool_output::ShellStream::Out
                }
                nuo_wire::ToolStream::Stderr(_) => {
                    nuo_wire::tool_output::ShellStream::Err
                }
            };
            let text = match stream {
                nuo_wire::ToolStream::Stdout(s) | nuo_wire::ToolStream::Stderr(s) => s,
            };
            for piece in text.split('\n') {
                if !piece.is_empty() {
                    lines.push(nuo_wire::tool_output::ShellLine {
                        stream: stream_tag,
                        text: piece.to_string(),
                    });
                }
            }
            match stream {
                nuo_wire::ToolStream::Stdout(s) => stdout.push_str(s),
                nuo_wire::ToolStream::Stderr(s) => stderr.push_str(s),
            }
        }
        self.refresh_tool_step();
        true
    }

    /// Mark a still-running tool step as cancelled. Idempotent: a step that
    /// already reached a terminal state (`Ok` / `Failed` / `Cancelled`) is left
    /// untouched and returns `false`. When the step is a `task` (subagent),
    /// its still-running nested tool children are cancelled too, so an aborted
    /// subagent never leaves a "running" child step behind.
    pub fn cancel_tool_step(&mut self, id: &str) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        // Apply the transition through `cancel_all_running`, which also handles
        // the nested-children sweep and refreshes the rendered view in one
        // place.
        self.cancel_all_running()
    }

    /// Recursively cancel every still-running tool step within this message
    /// (used for subagent children and as a defensive sweep). Returns `true`
    /// if anything transitioned.
    pub fn cancel_all_running(&mut self) -> bool {
        let (step_running, child_changed) = {
            let MessageKind::ToolStep {
                status,
                started_at,
                duration_ms,
                awaiting,
                children,
                ..
            } = &mut self.kind
            else {
                return false;
            };
            let mut changed = false;
            if status.is_running() {
                *status = ToolStepStatus::Cancelled;
                *awaiting = false;
                // Freeze the elapsed time at the moment of cancellation so the
                // step stops showing a live-running timer.
                if duration_ms.is_none() {
                    *duration_ms = started_at
                        .map(|started| started.elapsed().as_millis() as u64)
                        .or(Some(0));
                }
                changed = true;
            }
            let mut child_changed = changed;
            for child in children.iter_mut() {
                child_changed |= child.cancel_all_running();
            }
            (changed, child_changed)
        };
        if step_running || child_changed {
            self.refresh_tool_step();
        }
        step_running || child_changed
    }

    /// The explicit lifecycle of a tool step, or `None` for non-tool messages.
    pub fn tool_step_status(&self) -> Option<ToolStepStatus> {
        match &self.kind {
            MessageKind::ToolStep { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Append a subagent event as a nested child of this tool step.
    ///
    /// Returns `true` if this message is a tool step and the event was stored.
    pub fn push_subagent_event(&mut self, event: &SubagentEvent) -> bool {
        let MessageKind::ToolStep {
            children,
            profile,
            awaiting,
            activity,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        // Progress events clear a parked human-decision wait; request events
        // (permission / ask-user / input) park it, so the peek row can say
        // `awaiting approval` instead of replaying the last tool activity.
        match event {
            SubagentEvent::PermissionRequest(_)
            | SubagentEvent::UserQuestionRequest(_)
            | SubagentEvent::StdinRequest(_) => *awaiting = true,
            SubagentEvent::ToolCall { .. }
            | SubagentEvent::ToolResult { .. }
            | SubagentEvent::StreamStart { .. }
            | SubagentEvent::StreamDelta(_)
            | SubagentEvent::StreamEnd(_)
            | SubagentEvent::StreamReasoningStart { .. }
            | SubagentEvent::StreamReasoningDelta(_)
            | SubagentEvent::StreamReasoningEnd(_) => *awaiting = false,
            _ => {}
        }
        match event {
            // The subagent announced its role — stamp it on the step so the
            // renderer can draw an `[explore]` / `[debug]` role badge in front
            // of the summary instead of a generic `[SUBAGENT]`.
            // No child message is produced.
            SubagentEvent::Started { profile: name } => {
                *profile = Some(name.clone());
            }
            SubagentEvent::StreamStart { round, turn } => {
                children.push(
                    TranscriptMessage::new(Role::Assistant, "")
                        .with_round(*round)
                        // `turn` is the subagent's 0-indexed model-request
                        // position; the transcript's `turn` is 1-indexed.
                        .with_turn((*turn as u64) + 1),
                );
            }
            SubagentEvent::StreamDelta(delta) => {
                // Identity-addressed (ADR-0114): fold the delta into the
                // latest assistant-text child of the *same* stream turn, not
                // merely the last child — a tool-call/result child can be
                // appended between two deltas and would otherwise fork the
                // text into a second entry.
                let target = children
                    .iter_mut()
                    .rfind(|m| m.role == Role::Assistant && matches!(m.kind, MessageKind::Text));
                if let Some(last) = target {
                    last.push_stream(&sanitize_text(delta));
                } else {
                    let mut msg = TranscriptMessage::new(Role::Assistant, "");
                    msg.push_stream(&sanitize_text(delta));
                    children.push(msg);
                }
            }
            SubagentEvent::StreamEnd(content) => {
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.role == Role::Assistant && matches!(m.kind, MessageKind::Text))
                {
                    last.raw = content.clone();
                    last.reparse();
                } else {
                    children.push(TranscriptMessage::new(Role::Assistant, content.clone()));
                }
            }
            // The subagent's live reasoning chain, folded into the same
            // `MessageKind::Reasoning` message a resumed session restores from
            // `reasoning_content` — so a live drill-in and a reloaded one show
            // the same children. Placement mirrors the wire order the child
            // emits (reasoning precedes its turn's assistant text and tool
            // calls), so the trace lands in the right turn band. Disclosed
            // chains only: the sender gates hidden-chain models out at the
            // source, so no phantom summary trace can appear here.
            SubagentEvent::StreamReasoningStart { round, turn } => {
                children.push(
                    TranscriptMessage::reasoning("")
                        .with_round(*round)
                        .with_turn((*turn as u64) + 1),
                );
            }
            SubagentEvent::StreamReasoningDelta(delta) => {
                // Identity-addressed (ADR-0114): fold into the latest still-
                // streaming thinking child. `StreamReasoningStart` pushes a
                // stamped Thinking child; a tool-call child landing between
                // two deltas must not fork the trace into a second entry.
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.is_reasoning() && m.is_reasoning_streaming())
                {
                    last.push_reasoning_delta(delta);
                } else {
                    children.push(TranscriptMessage::reasoning(delta));
                }
            }
            SubagentEvent::StreamReasoningEnd(content) => {
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.is_reasoning() && m.is_reasoning_streaming())
                {
                    last.finalize_reasoning(content);
                    // No wall clock is available on the folding path; 0 is
                    // the same terminal stamp a resumed session applies, and
                    // what matters is that the trace stops "streaming" so the
                    // spinner freezes.
                    last.set_reasoning_duration(0);
                } else if !content.is_empty() {
                    children.push(TranscriptMessage::reasoning(content));
                }
            }
            SubagentEvent::ToolCall {
                id,
                name,
                arguments,
                round,
                turn,
            } => {
                children.push(
                    TranscriptMessage::tool_step(id.clone(), name.clone(), arguments.clone())
                        .with_round(*round)
                        .with_turn((*turn as u64) + 1),
                );
            }
            SubagentEvent::ToolResult {
                id,
                output,
                duration_ms,
                ..
            } => {
                if let Some(child) = children.iter_mut().find(|m| {
                    m.is_tool_step()
                        && if let MessageKind::ToolStep {
                            id: step_id,
                            output: None,
                            ..
                        } = &m.kind
                        {
                            step_id == id
                        } else {
                            false
                        }
                }) {
                    child.finish_tool_step(
                        id,
                        output.clone(),
                        nuo_wire::ToolOutput::text(output.clone()),
                        *duration_ms,
                    );
                } else {
                    let mut msg = TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                    msg.finish_tool_step(
                        id,
                        output.clone(),
                        nuo_wire::ToolOutput::text(output.clone()),
                        *duration_ms,
                    );
                    children.push(msg);
                }
            }
            SubagentEvent::Notice(notice) => {
                children.push(TranscriptMessage::notice_from_core(notice));
            }
            // The subagent reported a free-text activity line (`waiting for
            // model`, `waiting to retry (3s)`). Stored for the peek row so a
            // stretch with no child events still reads as alive. No child
            // message is produced.
            SubagentEvent::Activity(text) => *activity = Some(text.clone()),
            // Full-duplex (ADR-0029): a subagent surfaced a permission /
            // ask_user request up through the subagent tool. The down-direction
            // reply (registry → handle → reply_permission / reply_user_question)
            // is wired at the agent layer; rendering the nested prompt in the
            // TUI and routing the user's answer back down is the harness↔TUI
            // integration step that follows. Until then these are observed but
            // not rendered as a nested child step (the request still reaches
            // the harness via the `RoundEvent::Subagent` envelope, so a future
            // handler can attach without changing the event shape).
            SubagentEvent::PermissionRequest(_)
            | SubagentEvent::UserQuestionRequest(_)
            | SubagentEvent::StdinRequest(_) => {}
        }
        true
    }

    pub fn is_tool_step(&self) -> bool {
        matches!(self.kind, MessageKind::ToolStep { .. })
    }

    pub fn is_command_result(&self) -> bool {
        matches!(self.kind, MessageKind::CommandResult { .. })
    }

    /// The command invocation metadata.
    pub fn command_invocation(&self) -> Option<&CommandInvocation> {
        match &self.kind {
            MessageKind::CommandResult { invocation, .. } => Some(invocation),
            _ => None,
        }
    }

    pub fn command_name(&self) -> Option<&str> {
        self.command_invocation().map(|c| c.name.as_str())
    }

    pub fn command_args(&self) -> Option<&str> {
        self.command_invocation().map(|c| c.args.as_str())
    }

    /// Whether this row is a round-interrupt marker (C11).
    pub fn is_round_interrupt(&self) -> bool {
        match &self.kind {
            MessageKind::Notice {
                parts: Some(parts), ..
            } => {
                matches!(parts.topic.as_deref(), Some("interrupted") | Some("error"))
            }
            _ => false,
        }
    }

    pub fn command_result_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::CommandResult { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// The lifecycle phase of a command component (ADR-0108).
    pub fn command_result_phase(&self) -> Option<CommandPhase> {
        match &self.kind {
            MessageKind::CommandResult { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// The render layout for this command row (ADR-0106): `Plain` when there
    /// is no result, `Inline` when a single-line reply fits beside the
    /// invocation, `Disclose` otherwise. A `Pending` row has no result yet and
    /// always classifies `Plain` — the phase owns its presentation until the
    /// reply settles. `available_width` is the row's usable columns.
    ///
    /// A present `sent_at_ms` renders a trailing `HH:MM` timestamp,
    /// so the method reserves that span before classifying — the free
    /// classifier stays purely shape-based and timestamp-blind.
    pub fn command_row_layout(&self, available_width: usize) -> Option<CommandRowLayout> {
        match &self.kind {
            MessageKind::CommandResult { result, phase, .. } => {
                if *phase == CommandPhase::Pending {
                    return Some(CommandRowLayout::Plain);
                }
                let usable = if self.sent_at_ms.is_some() {
                    available_width.saturating_sub(SENT_TIME_LABEL_COLS)
                } else {
                    available_width
                };
                Some(command_row_layout(result.as_deref(), &self.raw, usable))
            }
            _ => None,
        }
    }

    /// User-driven disclosure change: force `expanded` and mark it pinned so
    /// later transitions leave it alone.
    pub fn pin_command_result_expanded(&mut self, expanded: bool) {
        use super::interactive::InteractiveEntry;
        self.pin_expanded(expanded);
    }

    /// The invocation text shown on the collapsed command row (`/search foo`,
    /// `!ls -la`), from the message `raw`.
    pub fn command_result_summary(&self) -> Option<String> {
        if self.is_command_result() {
            Some(self.raw.clone())
        } else {
            None
        }
    }

    /// The typed result body text (ADR-0091 `to_text`), when the record
    /// carries a result.
    pub fn command_result_text(&self) -> Option<String> {
        match &self.kind {
            MessageKind::CommandResult {
                result: Some(result),
                ..
            } => Some(result.to_text()),
            _ => None,
        }
    }

    /// The typed result itself, when this row carries one.
    pub fn command_result_payload(&self) -> Option<&nuo_wire::CommandResult> {
        match &self.kind {
            MessageKind::CommandResult { result, .. } => result.as_deref(),
            _ => None,
        }
    }

    /// The headline/detail split for an ack reply (ADR-0106 two-tone ack):
    /// the title alone when there is no detail, `None` for non-acks.
    pub fn command_ack_split(&self) -> Option<(&str, &[String])> {
        command_ack_split(self.command_result_payload())
    }

    pub fn tool_step_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ToolStep { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// Auto/system disclosure setter: sets `expanded` **unless** the user has
    /// pinned the step (in which case it's a no-op). This is what lifecycle
    /// transitions (start / finish / cancel) and step creation call, so the
    /// derived default never fights a manual choice. User-driven toggles go
    /// through [`Self::pin_tool_step_expanded`].
    pub fn set_tool_step_expanded(&mut self, expanded: bool) {
        if let MessageKind::ToolStep {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            if *user_pinned {
                return;
            }
            *current = expanded;
            self.refresh_tool_step();
        }
    }

    /// User-driven disclosure change: force `expanded` and mark it pinned so
    /// later lifecycle transitions leave it alone.
    pub fn pin_tool_step_expanded(&mut self, expanded: bool) {
        use super::interactive::InteractiveEntry;
        self.pin_expanded(expanded);
    }

    /// A tool step that spawns a subagent — the read-only `subagent` tool or the
    /// write-capable `delegate_code` tool. Such steps render as a compact,
    /// non-expandable line that navigates into a dedicated subagent view on
    /// activation (see the TUI focus stack) rather than expanding inline.
    pub fn is_subagent_task(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep { name, .. }
                if matches!(
                    name.as_str(),
                    "spawn_agent" | "delegate_code"
                )
        )
    }

    /// The bound subagent profile name (`explore` / `plan` / `verify` / …), used
    /// by the inline step's role badge. `None` until the `Started` event lands
    /// (or for non-subagent steps); the renderer falls back to a generic
    /// `[SUBAGENT]` badge then.
    pub fn subagent_profile(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.as_deref(),
            _ => None,
        }
    }

    /// The call id of a tool step, used as the addressable identity of a
    /// subagent task for the focus stack.
    pub fn tool_step_call_id(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The nested child messages emitted by a subagent task. Returns `None`
    /// for non-tool-step messages.
    pub fn subagent_children(&self) -> Option<&[TranscriptMessage]> {
        match &self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// Mutable access to a tool step's child messages (used when the view is
    /// zoomed into a subagent and its children are the active message stream).
    pub fn subagent_children_mut(&mut self) -> Option<&mut Vec<TranscriptMessage>> {
        match &mut self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// The subagent's role (`explore` / `plan` / `verify` / …), identified by
    /// the `Started` event. `None` for non-task steps and before the role is
    /// known. The Subagent page header renders this as the `[ROLE]` tag between
    /// the `SUBAGENT` identity and the task title.
    pub fn subagent_role(&self) -> Option<String> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.clone(),
            _ => None,
        }
    }

    /// The subagent's task description (the `description` argument), truncated
    /// for display. Shown as the title of the Subagent page header.
    pub fn subagent_description(&self) -> String {
        let MessageKind::ToolStep { arguments, .. } = &self.kind else {
            return "Subagent".to_string();
        };
        let label = parse_arguments_kv(arguments)
            .into_iter()
            .find(|(k, _)| k == "description")
            .map(|(_, v)| v)
            .unwrap_or_else(|| "Subagent".to_string());
        truncate(&label, 48)
    }

    /// One-line live "peek" at the subagent's current activity, e.g.
    /// `running Grep "foo"  12s` or `running thinking  8s`. Shown as the
    /// step's second row while the subagent runs and replaced in place by
    /// [`Self::subagent_outcome_line`] when the step terminates. Returns `None`
    /// for non-task steps and for terminal steps (the outcome row owns the
    /// second row then). The elapsed timer is derived from `started_at` at
    /// render time, so the line stays fresh on every animation tick without
    /// storing any ticking state.
    pub fn subagent_status_line(&self) -> Option<String> {
        if !self.is_subagent_task() {
            return None;
        }
        let MessageKind::ToolStep {
            status,
            started_at,
            awaiting,
            activity,
            children,
            ..
        } = &self.kind
        else {
            return None;
        };
        if !status.is_running() {
            return None;
        }
        let elapsed = started_at.map(|started| {
            let ms = started.elapsed().as_millis() as u64;
            if ms < 1000 {
                format!("{}ms", ms)
            } else if ms < 60_000 {
                format!("{}s", ms / 1000)
            } else {
                duration_text(Some(ms))
            }
        });
        // A parked human-decision wait outranks replaying the last tool
        // activity: the subagent is blocked on the user, not making progress.
        // It keeps the bare phrase — no `running` prefix — because nothing
        // is moving while the subagent waits.
        let activity = if *awaiting {
            "awaiting approval".to_string()
        } else {
            let current = match children.last() {
                Some(child)
                    if child.is_tool_step()
                        && child.tool_step_status() == Some(ToolStepStatus::Running) =>
                {
                    // A tool step still in flight — name the tool so the
                    // row says *what* is being done, not just that the
                    // parent is busy.
                    Some(
                        child
                            .tool_step_summary()
                            .unwrap_or_else(|| "tool".to_string()),
                    )
                }
                // Assistant text has streamed but no tool call followed it:
                // the subagent is composing between tools. A bare `starting`
                // here read as "possibly stuck" during long model calls,
                // which is exactly what the `running` prefix disambiguates.
                Some(child) if child.role == Role::Assistant && !child.raw.is_empty() => {
                    Some("thinking".to_string())
                }
                // Nothing observable has landed yet. Prefer the subagent's own
                // reported activity (`waiting for model`, …) over the
                // generic `starting`: it proves the subagent is alive during
                // the model call that precedes the first child event.
                _ => activity.clone(),
            };
            // A transport wait (provider backoff) is a pause, not progress:
            // bare phrase, same rule as `awaiting approval` above — `running`
            // would falsely claim forward motion.
            match current {
                Some(current) if current.starts_with("waiting to retry") => current,
                Some(current) => format!("running {current}"),
                None => "running".to_string(),
            }
        };
        // The activity and its elapsed time are same-rank metadata — plain
        // whitespace (R2 on the join ladder), never a `·` glyph.
        Some(match elapsed {
            Some(elapsed) => format!("{activity}  {elapsed}"),
            None => activity,
        })
    }

    /// One-line outcome replacing the peek row once the subagent terminates: the
    /// first non-empty line of its conclusion (`ToolOutput::Subagent.summary`,
    /// falling back to the legacy `output` text for restored sessions).
    /// Returns `None` for non-task steps, running steps, and terminal steps
    /// with no conclusion text.
    pub fn subagent_outcome_line(&self) -> Option<String> {
        if !self.is_subagent_task() {
            return None;
        }
        let MessageKind::ToolStep {
            status,
            output,
            structured,
            ..
        } = &self.kind
        else {
            return None;
        };
        if status.is_running() {
            return None;
        }
        let source: &str = match structured.as_deref() {
            Some(nuo_wire::ToolOutput::Subagent { summary, .. }) => summary,
            _ => output.as_deref()?,
        };
        source
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    }

    pub fn reasoning(content: impl Into<String>) -> Self {
        let content = sanitize_text(&content.into()).into_owned();
        let milestones = count_milestones(&content);
        let mut message = Self {
            id: next_message_id(),
            role: Role::Assistant,
            blocks: Vec::new(),
            raw: String::new(),
            kind: MessageKind::Reasoning {
                content: content.clone(),
                duration_ms: None,
                expanded: false,
                user_pinned: false,
                milestones,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        };
        message.raw = content;
        message.relayout();
        message
    }

    /// Append a reasoning delta to a still-streaming Reasoning message.
    pub fn push_reasoning_delta(&mut self, delta: &str) {
        let sanitized = sanitize_text(delta);
        let MessageKind::Reasoning { content, .. } = &mut self.kind else {
            return;
        };
        content.push_str(&sanitized);
        self.raw.push_str(&sanitized);
        self.bump_rev();
    }

    /// Replace a streaming Reasoning message's content with the authoritative
    /// full text and close the trace (stream end / restored session).
    pub fn finalize_reasoning(&mut self, content: &str) {
        let content = sanitize_text(content).into_owned();
        let milestones = count_milestones(&content);
        self.raw = content.clone();
        if let MessageKind::Reasoning {
            content: current,
            milestones: slot,
            ..
        } = &mut self.kind
        {
            *current = content;
            *slot = milestones;
        }
        self.reparse();
    }

    pub fn is_reasoning(&self) -> bool {
        matches!(self.kind, MessageKind::Reasoning { .. })
    }

    /// Whether this is the live provider-retry entry.
    pub fn is_provider_retry(&self) -> bool {
        matches!(self.kind, MessageKind::ProviderRetry { .. })
    }

    /// Whether this message is a harness notice (error / turn-pause / status / retry).
    pub fn is_notice(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Notice { .. } | MessageKind::ProviderRetry { .. }
        )
    }

    pub fn notice_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::Notice { expanded, .. } => Some(*expanded),
            MessageKind::ProviderRetry { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    pub fn pin_notice_expanded(&mut self, expanded: bool) {
        use super::interactive::InteractiveEntry;
        self.pin_expanded(expanded);
    }

    /// Construct a round-interrupt marker row (C11) unified as a Notice entry.
    pub fn round_interrupted(record: nuo_wire::RoundInterrupt) -> Self {
        let severity = match record.reason {
            nuo_wire::RoundInterruptReason::Error => NoticeSeverity::Error,
            _ => NoticeSeverity::Warning,
        };
        let raw = match record.reason {
            nuo_wire::RoundInterruptReason::Error => {
                record.detail.clone().unwrap_or_else(|| match record.round {
                    Some(round) => format!("Round {round} — failed with error"),
                    None => "Round failed with error".to_string(),
                })
            }
            _ => match record.round {
                Some(round) => match record.reason {
                    nuo_wire::RoundInterruptReason::User => {
                        format!("Round {round} — cancelled via [Esc Esc]")
                    }
                    nuo_wire::RoundInterruptReason::Superseded => {
                        format!("Round {round} — superseded by new message")
                    }
                    nuo_wire::RoundInterruptReason::Terminated => {
                        format!("Round {round} — process exited")
                    }
                    nuo_wire::RoundInterruptReason::Error => unreachable!(),
                },
                None => match record.reason {
                    nuo_wire::RoundInterruptReason::User => {
                        "Cancelled via [Esc Esc]".to_string()
                    }
                    nuo_wire::RoundInterruptReason::Superseded => {
                        "Superseded by new message".to_string()
                    }
                    nuo_wire::RoundInterruptReason::Terminated => {
                        "Process exited".to_string()
                    }
                    nuo_wire::RoundInterruptReason::Error => unreachable!(),
                },
            },
        };
        let parts = match record.reason {
            nuo_wire::RoundInterruptReason::Error => {
                let parsed = crate::components::notice::parse_notice_content(&raw);
                NoticeParts {
                    origin: Some(NoticeOrigin::Provider {
                        provider_name: None,
                        attempt: None,
                    }),
                    topic: Some("error".to_string()),
                    title: parsed.header,
                    detail: parsed.detail,
                }
            }
            _ => NoticeParts {
                origin: Some(NoticeOrigin::System {
                    topic: SystemNoticeTopic::Interrupted,
                }),
                topic: Some("interrupted".to_string()),
                title: raw.clone(),
                detail: None,
            },
        };
        Self::notice(severity, raw).with_notice_parts(parts)
    }

    /// Construct a retry-resolution marker row: the success-side twin of
    /// [`Self::round_interrupted`]. One compact system notice per round that
    /// recovered from transient provider faults; the per-attempt fault lines
    /// ride as the expandable detail.
    pub fn retry_resolved(record: nuo_wire::RetryResolution) -> Self {
        Self::notice(NoticeSeverity::Info, record.summary_line())
            .with_notice_parts(NoticeParts {
                origin: Some(NoticeOrigin::System {
                    topic: SystemNoticeTopic::Interrupted,
                }),
                topic: Some("recovered".to_string()),
                title: record.summary_line(),
                detail: if record.faults.is_empty() {
                    None
                } else {
                    Some(record.faults.join("\n"))
                },
            })
            .with_sent_at_ms(record.at_ms)
    }

    /// Construct a notice message. Replaces the ad-hoc
    /// `TranscriptMessage::new(Role::System, format!("Error: …"))` pattern with
    /// a typed severity so the renderer can pick color/icon from one place.
    /// Leaves the structured parts unset — the renderer falls back to its
    /// heuristic parse of `raw`. Core (`AgentNotice`) notices should use
    /// [`Self::notice_from_core`] instead so the topic/title/detail split
    /// survives the boundary instead of being re-derived from text.
    pub fn notice(severity: NoticeSeverity, raw: impl Into<String>) -> Self {
        // Notices receive transport and harness errors directly. HTTP proxy
        // bodies commonly use CRLF, and allowing the raw `\r` through to the
        // terminal moves its physical cursor back to column zero while the
        // retained grid still believes it advanced normally. The next diff
        // then paints over unrelated transcript cells. Keep this constructor
        // on the same sanitized boundary as ordinary and retry messages.
        let raw = sanitize_text(&raw.into()).into_owned();
        let blocks = parse_blocks(&raw);
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks,
            raw,
            kind: MessageKind::Notice {
                severity,
                parts: None,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        }
    }

    /// Construct a notice from its contract form, keeping the
    /// architecture-agreed two-part split intact: the topic label from
    /// [`notice_topic_label`] and the `title`/`body` detail pair. `raw` still
    /// carries the flattened `render_text()` form for copy fidelity and as
    /// the renderer's fallback, but the renderer never has to guess the
    /// split back out of it.
    pub fn notice_from_core(notice: &nuo_wire::AgentNotice) -> Self {
        // Same sanitized boundary as `raw`: provider HTTP bodies commonly
        // carry CRLF, and a raw `\r` reaching the grid moves the physical
        // cursor while the retained grid believes it advanced.
        let origin = match notice.kind {
            nuo_wire::NoticeKind::ProviderRetry => Some(NoticeOrigin::Provider {
                provider_name: None,
                attempt: None,
            }),
            nuo_wire::NoticeKind::NudgeInjected => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::TurnGuard,
            }),
            nuo_wire::NoticeKind::ReviewAlert => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Review,
            }),
            nuo_wire::NoticeKind::TrustChanged => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Trust,
            }),
            nuo_wire::NoticeKind::CommandAck => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::CommandAck,
            }),
            nuo_wire::NoticeKind::ImageInputWithheld => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Images,
            }),
        };
        let parts = NoticeParts {
            origin,
            topic: Some(notice_topic_label(notice.kind).to_string()),
            title: sanitize_text(&notice.title).into_owned(),
            detail: notice
                .body
                .as_deref()
                .filter(|body| !body.trim().is_empty())
                .map(|body| sanitize_text(body).into_owned()),
        };
        Self::notice(
            notice_severity_from_core(notice.severity),
            notice.render_text(),
        )
        .with_notice_parts(parts)
    }

    /// Attach the structured topic/detail split to a notice. No-op on
    /// non-notice messages.
    pub fn with_notice_parts(mut self, parts: NoticeParts) -> Self {
        if let MessageKind::Notice { parts: slot, .. } = &mut self.kind {
            *slot = Some(Box::new(parts));
        }
        self
    }

    /// The structured topic/detail split, when this notice carries one.
    pub fn notice_parts(&self) -> Option<&NoticeParts> {
        match &self.kind {
            MessageKind::Notice { parts, .. } => parts.as_deref(),
            _ => None,
        }
    }

    /// Construct a transient provider-retry notice message.
    pub fn provider_retry(
        attempt: usize,
        max_attempts: usize,
        retry_at: std::time::Instant,
        failure: impl Into<String>,
    ) -> Self {
        let failure = sanitize_text(&failure.into()).into_owned();
        let blocks = parse_blocks(&failure);
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks,
            raw: failure.clone(),
            kind: MessageKind::ProviderRetry {
                attempt,
                max_attempts,
                failure,
                retry_at,
                expanded: false,
                user_pinned: false,
            },
            origin: UserMessageOrigin::Chat,
            delivery: DeliveryStatus::Delivered,
            insert_id: None,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        }
    }

    /// In-place update for an ongoing provider retry.
    pub fn update_provider_retry(
        &mut self,
        attempt: usize,
        max_attempts: usize,
        retry_at: std::time::Instant,
        failure: impl Into<String>,
    ) {
        let failure = sanitize_text(&failure.into()).into_owned();
        self.blocks = parse_blocks(&failure);
        self.raw = failure.clone();
        // Retry notices never stream; freeze the body (safe default for any
        // stray suffix parse).
        self.resume_byte = self.raw.len();
        self.live_blocks = 0;
        if let MessageKind::ProviderRetry {
            attempt: a,
            max_attempts: m,
            retry_at: r,
            failure: f,
            ..
        } = &mut self.kind
        {
            *a = attempt;
            *m = max_attempts;
            *r = retry_at;
            *f = failure;
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// Settle a transient provider-retry entry into a static failure notice when
    /// the round is interrupted.
    ///
    /// Freezes the dynamic countdown and preserves the retry failure diagnostics
    /// so the user knows why the round was interrupted.
    pub fn settle_interrupted_provider_retry(&mut self) {
        if let MessageKind::ProviderRetry {
            attempt,
            max_attempts,
            failure,
            expanded,
            user_pinned,
            ..
        } = &self.kind
        {
            let attempt = *attempt;
            let max_attempts = *max_attempts;
            let failure = failure.clone();
            let expanded = *expanded;
            let user_pinned = *user_pinned;

            let title = format!("Provider request failed (attempt {attempt}/{max_attempts})");
            let raw = if failure.trim().is_empty() {
                title.clone()
            } else {
                format!("{title}: {failure}")
            };
            self.blocks = parse_blocks(&raw);
            self.raw = raw;
            self.resume_byte = self.raw.len();
            self.live_blocks = 0;

            let parts = NoticeParts {
                origin: Some(NoticeOrigin::Provider {
                    provider_name: None,
                    attempt: Some((attempt, max_attempts)),
                }),
                topic: Some("retry".to_string()),
                title,
                detail: if failure.trim().is_empty() {
                    None
                } else {
                    Some(failure)
                },
            };
            self.kind = MessageKind::Notice {
                severity: NoticeSeverity::Warning,
                parts: Some(Box::new(parts)),
                expanded,
                user_pinned,
            };
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// A reasoning trace that has not yet been stamped with a duration — i.e.
    /// its stream is still open. The renderer treats this as the "spinner
    /// should keep breathing" state, and `finalize_streaming_reasoning` uses
    /// it to find orphaned traces to freeze after an interrupt.
    pub fn is_reasoning_streaming(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Reasoning {
                duration_ms: None,
                ..
            }
        )
    }

    pub fn reasoning_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::Reasoning { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// Auto/system disclosure setter — respects a user pin. See
    /// [`Self::set_tool_step_expanded`] for the rationale.
    pub fn set_reasoning_expanded(&mut self, expanded: bool) {
        if let MessageKind::Reasoning {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            if *user_pinned {
                return;
            }
            *current = expanded;
        }
    }

    /// User-driven disclosure change: force `expanded` and pin it.
    pub fn pin_reasoning_expanded(&mut self, expanded: bool) {
        use super::interactive::InteractiveEntry;
        self.pin_expanded(expanded);
    }

    pub fn set_reasoning_duration(&mut self, duration_ms: u64) {
        if let MessageKind::Reasoning { duration_ms: d, .. } = &mut self.kind {
            *d = Some(duration_ms);
        }
    }

    /// Human-readable summary for the reasoning trace (always one line).
    ///
    /// For models reporting structured thought milestones/outlines (e.g. GPT-5.6
    /// Sol, ChatGPT Responses `reasoning_summary_text`, Claude thinking headings),
    /// this dynamically extracts the active milestone header while streaming
    /// (`Thinking through security architecture components`) and reports the
    /// completed milestone state once finished (`Thought through security
    /// architecture components (1.2s)`).
    ///
    /// Deliberately **no token count** (ADR-0191): a local cl100k count of the
    /// visible chain is not the billed reasoning volume — hidden-CoT models
    /// bill far more than they show, and a number that ranges from exact to
    /// 10× off is noise. The authoritative reasoning-token figure lives in the
    /// usage/performance surfaces via `TokenUsage.reasoning_tokens`.
    pub fn reasoning_summary(&self) -> Option<String> {
        let MessageKind::Reasoning {
            content,
            duration_ms,
            milestones,
            ..
        } = &self.kind
        else {
            return None;
        };
        let active_milestone = extract_active_milestone(content);

        Some(match duration_ms {
            None => match active_milestone {
                Some(milestone) => {
                    let topic = normalize_thinking_topic(&milestone);
                    format!("Thinking through {topic}")
                }
                // No milestone to show: an activity word, not a metric.
                None => "Thinking…".to_string(),
            },
            Some(ms) => {
                let duration = duration_text(Some(*ms));
                let milestones = *milestones;
                if milestones > 1 {
                    format!("Thought through {milestones} steps ({duration})")
                } else if milestones == 1
                    && let Some(milestone) = active_milestone
                {
                    let topic = normalize_thinking_topic(&milestone);
                    format!("Thought through {topic} ({duration})")
                } else {
                    format!("Thought ({duration})")
                }
            }
        })
    }

    /// Human-readable header for the tool step (always one line).
    ///
    /// Shows only what the tool did and a duration suffix for finished
    /// states — the technical tool name lives inside the expanded body to
    /// reduce cognitive load.
    pub fn tool_step_summary(&self) -> Option<String> {
        let MessageKind::ToolStep {
            name,
            profile,
            arguments,
            status,
            duration_ms,
            input_bytes,
            input_slot,
            ..
        } = &self.kind
        else {
            return None;
        };
        let summary = crate::tools::summary_for(name, arguments, profile.as_deref());
        // ADR-0026: while the call's arguments are still streaming (announced,
        // not yet dispatched) append a count-only progress clause. It is a
        // static clause — never a `+`/`-` disclosure marker, never animated.
        let streaming = match (input_slot, input_bytes) {
            (Some(_), Some(bytes)) => format!(" · receiving input ({} KB)", bytes / 1024),
            (Some(_), None) => " · receiving input".to_string(),
            (None, _) => String::new(),
        };
        Some(match status {
            ToolStepStatus::Running => format!("{summary}{streaming}"),
            ToolStepStatus::Ok => {
                format!("{summary}{streaming} ({})", duration_text(*duration_ms))
            }
            ToolStepStatus::Failed => {
                format!("{summary}{streaming} (failed {})", duration_text(*duration_ms))
            }
            ToolStepStatus::Denied => {
                format!("{summary}{streaming} (denied {})", duration_text(*duration_ms))
            }
            ToolStepStatus::Cancelled => {
                format!("{summary}{streaming} (cancelled {})", duration_text(*duration_ms))
            }
            ToolStepStatus::Interrupted => {
                format!("{summary}{streaming} (interrupted {})", duration_text(*duration_ms))
            }
        })
    }

    /// Structured semantic header for the tool step and optional trailing status badge (ADR-0206).
    pub fn tool_step_semantic_summary(
        &self,
        workspace_root: Option<&std::path::Path>,
    ) -> Option<(
        crate::components::inline_layout::SemanticLine<'static>,
        Option<String>,
    )> {
        let MessageKind::ToolStep {
            name,
            profile,
            arguments,
            status,
            duration_ms,
            structured,
            ..
        } = &self.kind
        else {
            return None;
        };
        let effective_args_buf;
        let effective_args = if arguments.trim().is_empty() {
            if let Some(box_structured) = structured {
                match &**box_structured {
                    nuo_wire::ToolOutput::Patch { path, old, new, .. } => {
                        effective_args_buf = serde_json::json!({
                            "path": path,
                            "old_string": old,
                            "new_string": new,
                        }).to_string();
                        &effective_args_buf
                    }
                    _ => arguments.as_str(),
                }
            } else {
                arguments.as_str()
            }
        } else {
            arguments.as_str()
        };
        let semantic_line =
            crate::tools::semantic_summary_for(name, effective_args, profile.as_deref(), workspace_root);
        let suffix = match status {
            ToolStepStatus::Running => None,
            ToolStepStatus::Ok => Some(format!(" ({})", duration_text(*duration_ms))),
            ToolStepStatus::Failed => Some(format!(" (failed {})", duration_text(*duration_ms))),
            ToolStepStatus::Denied => Some(format!(" (denied {})", duration_text(*duration_ms))),
            ToolStepStatus::Cancelled => {
                Some(format!(" (cancelled {})", duration_text(*duration_ms)))
            }
            ToolStepStatus::Interrupted => {
                Some(format!(" (interrupted {})", duration_text(*duration_ms)))
            }
        };
        Some((semantic_line, suffix))
    }

    /// Alias for [`Self::is_tool_step`].
    pub fn is_tool_invocation(&self) -> bool {
        self.is_tool_step()
    }

    /// Alias for [`Self::tool_step_summary`].
    pub fn tool_invocation_summary(&self) -> Option<String> {
        self.tool_step_summary()
    }

    /// Alias for [`Self::tool_step_status`].
    pub fn tool_invocation_status(&self) -> Option<ToolInvocationStatus> {
        self.tool_step_status()
    }

    /// Alias for [`Self::tool_step_expanded`].
    pub fn tool_invocation_expanded(&self) -> Option<bool> {
        self.tool_step_expanded()
    }

    /// Alias for [`Self::pin_tool_step_expanded`].
    pub fn pin_tool_invocation_expanded(&mut self, expanded: bool) {
        self.pin_tool_step_expanded(expanded);
    }

    /// Extract structured [`ToolInvocation`] if this message is a tool step.
    pub fn as_tool_invocation(&self) -> Option<ToolInvocation> {
        let MessageKind::ToolStep {
            id,
            name,
            profile,
            arguments,
            output,
            structured,
            status,
            expanded,
            user_pinned,
            duration_ms,
            started_at,
            awaiting,
            activity,
            input_bytes,
            input_slot,
            children,
        } = &self.kind
        else {
            return None;
        };
        Some(ToolInvocation {
            id: id.clone(),
            name: name.clone(),
            profile: profile.clone(),
            arguments: arguments.clone(),
            output: output.clone(),
            structured: structured.clone(),
            status: *status,
            expanded: *expanded,
            user_pinned: *user_pinned,
            duration_ms: *duration_ms,
            started_at: *started_at,
            awaiting: *awaiting,
            activity: activity.clone(),
            input_bytes: *input_bytes,
            input_slot: *input_slot,
            children: children.clone(),
        })
    }

    pub(crate) fn refresh_tool_step(&mut self) {
        let MessageKind::ToolStep {
            id: _,
            name,
            profile,
            arguments,
            output,
            structured,
            status,
            expanded,
            user_pinned: _,
            duration_ms,
            started_at: _,
            awaiting: _,
            activity: _,
            input_bytes: _,
            input_slot: _,
            children: _,
        } = &self.kind
        else {
            return;
        };
        if *expanded {
            // Expanded tool-step bodies are rendered directly from the
            // structured data (see draw_tool_step), not from parsed
            // markdown. We still populate `blocks` so semantic selection and
            // copy work: block 0 = display arguments, block 1 = output.
            let kv = parse_arguments_kv(arguments);
            let display_args: String = kv
                .iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            self.raw = display_args.clone();
            let mut blocks = vec![Block::Text(Inline::plain(display_args))];
            if let Some(out) = output {
                self.raw.push_str("\n\n");
                self.raw.push_str(out);
                blocks.push(Block::Text(Inline::plain(out.clone())));
            }
            self.blocks = blocks;
            self.resume_byte = self.raw.len();
            self.live_blocks = 0;
        } else {
            let effective_args_buf;
            let effective_args = if arguments.trim().is_empty() {
                if let Some(box_structured) = structured {
                    match &**box_structured {
                        nuo_wire::ToolOutput::Patch { path, old, new, .. } => {
                            effective_args_buf = serde_json::json!({
                                "path": path,
                                "old_string": old,
                                "new_string": new,
                            }).to_string();
                            &effective_args_buf
                        }
                        _ => arguments.as_str(),
                    }
                } else {
                    arguments.as_str()
                }
            } else {
                arguments.as_str()
            };
            let summary = crate::tools::summary_for(name, effective_args, profile.as_deref());
            let suffix = match status {
                ToolStepStatus::Running => String::new(),
                ToolStepStatus::Ok => format!(" ({})", duration_text(*duration_ms)),
                ToolStepStatus::Failed => format!(" (failed {})", duration_text(*duration_ms)),
                ToolStepStatus::Denied => format!(" (denied {})", duration_text(*duration_ms)),
                ToolStepStatus::Cancelled => {
                    format!(" (cancelled {})", duration_text(*duration_ms))
                }
                ToolStepStatus::Interrupted => {
                    format!(" (interrupted {})", duration_text(*duration_ms))
                }
            };
            self.raw = format!("{}{}", summary, suffix);
            self.blocks = parse_blocks(&self.raw);
            // Tool summaries never stream; freeze the body (safe default for
            // any stray suffix parse).
            self.resume_byte = self.raw.len();
            self.live_blocks = 0;
        }
    }

    /// Re-parse blocks from raw text (e.g. after streaming append).
    pub fn reparse(&mut self) {
        self.relayout();
        self.bump_rev();
    }

    /// Full tracked re-layout of `raw` without bumping the revision (used at
    /// construction, where the rev is still fresh).
    fn relayout(&mut self) {
        let (blocks, resume) = parse_blocks_tracked(&self.raw);
        self.blocks = blocks;
        self.resume_byte = resume.resume_offset;
        self.live_blocks = resume.live_len;
    }

    /// Append streaming text and re-parse **incrementally** (ADR-0184).
    ///
    /// Only the still-open tail construct is re-parsed: everything before
    /// `resume_byte` is a frozen prefix whose blocks can never be modified by
    /// future input (markdown blocks are line-oriented and, once terminated
    /// by a blank line / closing fence / non-continuation line, immutable).
    /// Parsing every accumulated chunk keeps the live layout structurally
    /// consistent with the final layout — the previous append-only Text block
    /// path delayed all Markdown structure until StreamEnd, causing the whole
    /// response to jump when headings, lists, and code fences were discovered
    /// — but per-frame cost now grows with the delta, not the message.
    pub fn push_stream(&mut self, delta: &str) {
        self.raw.push_str(&sanitize_text(delta));
        self.refresh_stream_tail();
    }

    /// Re-parse only the live tail region and splice it onto the frozen
    /// prefix. The junction [`Block::Break`] between the frozen prefix and
    /// the suffix is re-decided here with the same pair rule the parser's
    /// `push_block` applies, so the result is byte-for-byte what a full
    /// `parse_blocks(&self.raw)` would produce.
    fn refresh_stream_tail(&mut self) {
        if self.resume_byte > self.raw.len() {
            // Defensive: raw was replaced without a relayout — fall back to
            // a full re-parse.
            self.resume_byte = 0;
            self.live_blocks = 0;
        }
        let frozen = self.blocks.len().saturating_sub(self.live_blocks);
        // Immutable borrow of `raw` while `blocks` is mutated — disjoint
        // fields, so the live region parses in place without a copy.
        let (tail, resume) = parse_blocks_tracked(&self.raw[self.resume_byte..]);
        let junction_break = match (
            frozen.checked_sub(1).and_then(|i| self.blocks.get(i)),
            tail.first(),
        ) {
            (Some(prev), Some(next)) => {
                !(matches!(prev, Block::Break)
                    || matches!(prev, Block::ListItem { .. })
                        && matches!(next, Block::ListItem { .. }))
            }
            _ => false,
        };
        self.blocks.truncate(frozen);
        let live_starts_at_zero = tail.len() == resume.live_len;
        if junction_break {
            self.blocks.push(Block::Break);
        }
        self.blocks.extend(tail);
        // The junction Break belongs to the live region only when it sits
        // directly in front of it (the suffix's live region starts at 0);
        // otherwise it precedes frozen suffix content.
        self.live_blocks = resume.live_len + usize::from(junction_break && live_starts_at_zero);
        self.resume_byte += resume.resume_offset;
        self.bump_rev();
    }
}

/// Strip control characters (except \n, \t) to prevent Ratatui from rendering
/// them as block characters (█).
fn sanitize_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(|c: char| c.is_control() && c != '\n' && c != '\t') {
        std::borrow::Cow::Owned(
            text.replace(|c: char| c.is_control() && c != '\n' && c != '\t', ""),
        )
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Parse a JSON arguments string into ordered `(key, display_value)` pairs
/// suitable for compact rendering in the tool step body.
///
/// String values are shown unquoted; other JSON types keep their native
/// representation. Non-JSON input falls back to a single pair.
pub fn parse_arguments_kv(arguments: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return vec![("raw".to_string(), arguments.trim().to_string())];
    };
    let Some(object) = value.as_object() else {
        return vec![("value".to_string(), arguments.trim().to_string())];
    };
    object
        .iter()
        .map(|(key, val)| {
            let display = match val {
                serde_json::Value::String(s) => s.clone(),
                _ => val.to_string(),
            };
            (key.clone(), display)
        })
        .collect()
}

fn duration_text(duration_ms: Option<u64>) -> String {
    match duration_ms {
        None => "...".to_string(),
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => {
            let total_secs = ms / 1000;
            let h = total_secs / 3600;
            let m = (total_secs % 3600) / 60;
            let s = total_secs % 60;
            if h > 0 {
                format!("{}h {}m", h, m)
            } else {
                format!("{}m {}s", m, s)
            }
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", prefix)
    } else {
        prefix
    }
}

/// Extract the latest active milestone or heading from reasoning content.
pub fn extract_active_milestone(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("**") {
            let title = if let Some(end) = rest.find("**") {
                &rest[..end]
            } else {
                rest
            };
            let clean = title.trim().trim_matches(':').trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            let header = rest.trim_start_matches('#').trim();
            if !header.is_empty() {
                return Some(header.to_string());
            }
        }
    }
    None
}

/// Canonical normalization for thinking milestone topics.
///
/// Strips redundant action participles (e.g. "Deconstructing", "Analyzing",
/// "Evaluating") and normalizes casing while preserving acronyms and mixed-case
/// identifiers (API, SQL, OAuth, macOS). No article is injected: the milestone
/// heading is already the topic, so forcing `the` in front of it produces odd
/// output for code-like headings (`the derive(Clone, copy, debug, PartialEq,
/// eq)`). The summary's verb ("Thinking through" / "Thought through") supplies
/// the grammatical frame.
pub fn normalize_thinking_topic(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches(':').trim();
    if trimmed.is_empty() {
        return "the solution".to_string();
    }

    // Strip trailing ellipsis or punctuation
    let topic = trimmed.trim_end_matches(['.', '…', ':']).trim();

    // Redundant participle prefixes that models prepend to milestone headings
    const REDUNDANT_PREFIXES: &[&str] = &[
        "deconstructing ",
        "analyzing ",
        "evaluating ",
        "inspecting ",
        "reviewing ",
        "mapping out ",
        "breaking down ",
        "thinking about ",
        "thinking through ",
        "working on ",
        "looking into ",
        "investigating ",
        "exploring ",
        "examining ",
        "considering ",
        "understanding ",
        "identifying ",
        "determining ",
        "formulating ",
        "planning ",
        "executing ",
        "validating ",
        "verifying ",
        "checking ",
    ];

    let mut stripped = topic;
    for prefix in REDUNDANT_PREFIXES {
        // `get` returns None when the prefix length lands mid-UTF-8-char;
        // byte-length slicing (`stripped[..prefix.len()]`) would panic there.
        let head = match stripped.get(..prefix.len()) {
            Some(head) => head,
            None => continue,
        };
        if stripped.len() > prefix.len() && head.eq_ignore_ascii_case(prefix) {
            let candidate = stripped[prefix.len()..].trim();
            if !candidate.is_empty() {
                stripped = candidate;
                break;
            }
        }
    }

    let words: Vec<&str> = stripped.split_whitespace().collect();
    if words.is_empty() {
        return "the solution".to_string();
    }

    let mut normalized_words = Vec::with_capacity(words.len());
    for (i, word) in words.iter().enumerate() {
        let is_acronym = word.len() > 1
            && word.chars().any(|c| c.is_alphabetic())
            && word.chars().all(|c| c.is_uppercase() || !c.is_alphabetic());
        let has_internal_caps = word.chars().skip(1).any(|c| c.is_uppercase());

        if is_acronym || has_internal_caps {
            normalized_words.push(word.to_string());
        } else if i == 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                let lower = first.to_lowercase().to_string() + chars.as_str();
                normalized_words.push(lower);
            }
        } else {
            let is_capitalized = word.chars().next().is_some_and(|c| c.is_uppercase());
            let is_all_caps = word.chars().all(|c| c.is_uppercase() || !c.is_alphabetic());
            if is_capitalized && !is_all_caps {
                normalized_words.push(word.to_lowercase());
            } else {
                normalized_words.push(word.to_string());
            }
        }
    }

    // No article is injected here: the milestone heading is already the topic,
    // and prefixing `the` reads oddly for code-like identifiers
    // (`the derive(Clone, Copy, Debug)`). The summary's verb ("Thinking
    // through" / "Thought through") supplies the grammatical frame.
    normalized_words.join(" ")
}

/// Count distinct milestone/heading sections in reasoning content.
pub fn count_milestones(text: &str) -> usize {
    let mut count = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("**") && trimmed.len() > 4 {
            let rest = &trimmed[2..];
            if rest.contains("**") {
                count += 1;
            }
        } else if trimmed.starts_with('#') {
            let header = trimmed.trim_start_matches('#').trim();
            if !header.is_empty() {
                count += 1;
            }
        }
    }
    count
}

use super::markdown::{ParseResume, parse_blocks_tracked};
/// Parse raw markdown-like text into semantic blocks.
///
/// This is intentionally lightweight — it splits on major block boundaries
/// (code fences, headings, rules, blockquotes) while preserving the original
/// text so copying yields exact source.
pub(crate) use super::markdown::{clamp_link_ranges, clamp_ranges, scan_inline};
pub use super::markdown::{parse_blocks, parse_blocks_plain};

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
