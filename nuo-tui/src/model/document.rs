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
            