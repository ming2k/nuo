//! Subagent lifecycle + human-request event vocabulary (ADR-0008 §3).
//!
//! Moved to the tool leaf so the single tool contract's event-streaming
//! method can live here without the leaf depending on the harness.
use serde::{Deserialize, Serialize};

/// A user-visible notice emitted by the agent or harness.
///
/// This is distinct from state-sync events such as `RoundEvent::TodosUpdated`
/// and blocking interaction events such as `RoundEvent::PermissionRequest`:
/// those events update UI state or require a reply, while a notice means
/// "surface this fact to the user".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNotice {
    pub id: String,
    pub kind: NoticeKind,
    pub severity: NoticeSeverity,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub source: NoticeSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A retried model request replayed a tool call whose result is already
    /// checkpointed, so the harness did not run it again. A **receipt**: it
    /// explains a missing tool execution and belongs in a surface log, never in
    /// the conversation.
    ///
    /// This value used to be `ProviderRetry`, which carried the live retry
    /// countdown. That countdown is not a notice: it is chrome with its own wire
    /// event (`nuo_wire::RoundEvent::RetryScheduled`) and its own typed state
    /// machine (`nuo_view::retry`). Keeping a notice kind for it meant every
    /// retry was published twice — once as a receipt the router absorbed and
    /// once as a transcript row — and the two counted attempts differently.
    CheckpointReplay,
    NudgeInjected,
    /// Generic "needs human attention" alert. Historically also carried
    /// workspace-trust notices; those moved to the first-class
    /// [`NoticeKind::TrustChanged`] in, leaving this value free for
    /// review-subsystem alerts.
    ReviewAlert,
    /// Workspace trust state changed or needs a decision: project-authored
    /// content changed on disk after being trusted (quarantined pending
    /// review), or a project entry shadows a user-scope entry..
    TrustChanged,
    /// A harness-level acknowledgment of a slash command / configuration change
    /// (e.g. `/delegate on`, `--delegate`, `/permissions clear`). These are
    /// status confirmations, not model output: they carry no conversational
    /// content, so frontends should surface them as a transient notification
    /// (toast) rather than appending them to the transcript as if the model
    /// had spoken. See ADR-0050 for the durable-vs-ephemeral boundary — the
    /// command *invocation* stays durable; this *reply* is ephemeral.
    CommandAck,
    /// Image attachments were withheld from a request because the route cannot
    /// take them: either a layer declared no image input, or the
    /// provider rejected an image-bearing request and the harness learned the
    /// route's limit.
    ///
    /// Deliberately first-class rather than folded into a transient-fault
    /// channel: this is not a fault to wait out
    /// but a durable fact about the route, and the user's remedy differs
    /// (switch model, or set the `Vision` override in the model editor). The
    /// transcript keeps its images either way — only the request projection
    /// drops them.
    ImageInputWithheld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSource {
    Agent,
    TurnGuard,
    Todo,
    Review,
    Harness,
}

/// Events emitted by a subagent spawned through the `task` tool.
///
/// These are forwarded from the child agent back to the parent harness so that
/// the TUI can render nested tool steps and streaming output inside the parent
/// tool step.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentEvent {
    /// Emitted once at subagent start, carrying the bound profile's name
    /// (e.g. `"explore"`, `"plan"`, `"verify"`). Lets the TUI label the
    /// subagent by its role rather than a generic "Subagent", so a user can
    /// tell a planning subagent from a research one at a glance.
    Started { profile: String },
    /// A user-visible notice from the subagent.
    Notice(AgentNotice),
    /// The subagent started a new response stream. `round`/`turn` carry the
    /// subagent's own ReAct position (1-indexed user round, 0-indexed
    /// model-request position within it, mirroring
    /// `AgentEvent::ModelRequestStarted`) so the TUI can stamp the child
    /// message and group the zoomed subagent view into turn bands exactly like
    /// the main session view.
    StreamStart { round: u64, turn: usize },
    /// New text token from the subagent.
    StreamDelta(String),
    /// The subagent response stream finished with the final accumulated text.
    StreamEnd(String),
    /// The subagent started a reasoning (thinking) stream. `round`/`turn`
    /// identify the subagent's own ReAct position (see
    /// [`SubagentEvent::StreamStart`]) so the child thinking trace joins the
    /// same turn band as its sibling assistant text and tool calls. Emitted
    /// before the first [`SubagentEvent::StreamReasoningDelta`] of a trace, so
    /// frontends can place the trace without waiting for content.
    ///
    /// This closes a visibility gap, not a new capability: the subagent's
    /// reasoning is already captured in its persisted transcript
    /// (`Message::reasoning_content`) and renders after a session reload —
    /// but before these events it was invisible while the subagent was actually
    /// running, because the child's `AgentEvent::ReasoningDelta` had no
    /// forwarding arm. The design principle is that no agent behaviour is
    /// hidden from the user: what the principal discloses live, a subagent
    /// discloses live too.
    StreamReasoningStart { round: u64, turn: usize },
    /// New reasoning token from the subagent (a disclosed chain only — the
    /// sender gates hidden-chain models out at the source; see
    /// `ReasoningSupport::chain_disclosed`).
    StreamReasoningDelta(String),
    /// The subagent's reasoning stream finished with the final accumulated
    /// reasoning text.
    StreamReasoningEnd(String),
    /// The subagent invoked a tool. `round`/`turn` identify the subagent's own
    /// ReAct position (see [`SubagentEvent::StreamStart`]) so the child tool
    /// step joins the same turn band as its sibling calls.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        round: u64,
        turn: usize,
    },
    /// A tool invoked by the subagent returned a result.
    ToolResult {
        id: String,
        name: String,
        output: String,
        duration_ms: u64,
    },
    /// A status update from the subagent.
    Activity(String),
    /// The subagent scheduled a retry after a transient provider fault.
    ///
    /// The child's retry is the parent's business for one reason: the parent
    /// step is what the user is watching, so it must be able to say *why* the
    /// subagent went quiet. It carries the same typed triple as
    /// `nuo_wire::RoundEvent::RetryScheduled` (attempt pair, backoff, fault) so
    /// the parent renders the child's setback from data instead of parsing the
    /// `waiting to retry (3s)` string back out of [`Self::Activity`].
    RetryScheduled {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        message: String,
    },
    /// The subagent's retry loop recovered: the fault list rides here so the
    /// parent can report what the child survived, exactly as the principal
    /// round reports its own recovery.
    RetryResolved {
        attempts: usize,
        faults: Vec<String>,
    },
    /// The subagent's permission broker surfaced a write/execute tool call
    /// that needs a human decision. Full-duplex (ADR-0029): this carries the
    /// request *up* to the parent harness so the user can answer it; the
    /// reply travels back *down* through the subagent handle's
    /// `reply_permission` (resolving the parked oneshot directly), unblocking
    /// the subagent's pending tool. Only fires when
    /// the subagent's profile does not suppress the broker (e.g. via
    /// `delegated: true`) — a read-only profile never produces one.
    PermissionRequest(PermissionRequest),
    /// The subagent called `ask_user` and is blocked awaiting answers.
    /// Full-duplex (ADR-0029): carries the questions *up*; the reply travels
    /// back *down* through the subagent handle's `reply_user_question`. Only
    /// fires for profiles with `allow_user_interaction: true`.
    UserQuestionRequest(UserQuestionRequest),
    /// The subagent's `bash` tool classified a command interactive and needs
    /// operator stdin. Carries the request *up*; the reply travels
    /// back *down* through the subagent handle's `reply_stdin`.
    #[serde(alias = "InputRequest")]
    StdinRequest(StdinRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PermissionRequest {
    pub id: String,
    pub tool: String,
    /// Short human-friendly title for the prompt (e.g. `"Run tests"`).
    /// Falls back to [`tool`](Self::tool) when a tool does not override
    /// `Tool::permission_label`. The TUI renders this as the header.
    #[serde(default)]
    pub label: String,
    /// User-facing description shown in the prompt's "Details" section.
    /// Populated from `Tool::permission_description`, distinct from the
    /// model-facing `Tool::description`.
    pub description: String,
    pub arguments: String,
    pub scope: String,
    /// Whether this call is an elevation the user, not a builtin limit, is
    /// being asked to grant (ADR-0028). The TUI renders such prompts with a
    /// distinct ⚠ treatment so the operator understands they are authorising
    /// access *beyond* the configured boundary, not a routine in-scope call.
    /// `false` for ordinary broker prompts and bash-policy confirms.
    #[serde(default)]
    pub elevation: bool,
    /// Whether the decision is **one-off only**: an `Always` reply is *not*
    /// persisted, and the TUI suppresses the "Always allow" option for such
    /// prompts. Set by the bash-policy confirm gate — a dangerous-command
    /// confirmation must stay one-off unless the user writes an explicit
    /// `[bash_policy.rules] action = "allow"` override. `false` (i.e.
    /// "Always" is honoured) for ordinary broker prompts.
    #[serde(default)]
    pub one_off: bool,
    /// Origin label identifying which subagent produced this request.
    /// `None` for top-level principal calls; e.g. `Some("subagent #a1b2 · explore")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Threat / hazard level classification of this tool invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hazard: Option<crate::hazard::HazardLevel>,
    /// Structured tool-specific payload submitted to the permission handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<crate::hazard::ToolPermissionSubmission>,
    /// A pure preview of what this call would do, when the tool can compute one
    /// without performing it (ADR-0125).
    ///
    /// Present for file mutations, absent for effects that cannot be predicted
    /// (shell, network, subagent). Its absence is information: it tells the operator
    /// that authorizing this call means authorizing an effect nobody can describe in
    /// advance, which is exactly what they need to know before saying yes.
    ///
    /// It lives on the *request* rather than on a separate event so the diff and the
    /// question cannot be decoupled: every frontend that can ask for authority can
    /// also show the change, and a frontend that omits it is visibly withholding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<crate::preview::PreviewResult>,
}

/// One option offered to the user inside an `ask_user` question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A single question inside an `ask_user` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestion {
    /// Short label shown as a chip/tag above the question (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The full question text.
    pub question: String,
    /// Available choices. Must contain at least one option.
    pub options: Vec<UserQuestionOption>,
    /// Whether the user may select more than one option.
    #[serde(default)]
    pub multi_select: bool,
}

/// Request sent from the agent to the TUI when the model calls `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionRequest {
    pub id: String,
    pub questions: Vec<UserQuestion>,
    /// Origin label identifying which subagent produced this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Request sent from the agent to the TUI when a `bash` command is classified
/// interactive and needs a line of stdin the agent cannot supply itself.
/// The TUI shows an inline input panel; the operator's reply is sent back as a `StdinReply`.
/// If the operator dismisses it (Esc), an empty reply cancels the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdinRequest {
    pub id: String,
    /// The command that needs input, shown for context.
    pub command: String,
    /// A human-readable prompt describing what to enter (e.g. "sudo password",
    /// "passphrase", "confirmation").
    pub prompt: String,
    /// Whether to mask the typed input (passwords/passphrases).
    pub secret: bool,
}
impl AgentNotice {
    pub fn new(
        kind: NoticeKind,
        severity: NoticeSeverity,
        title: impl Into<String>,
        source: NoticeSource,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            severity,
            title: title.into(),
            body: None,
            source,
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Build an ephemeral acknowledgment of a slash command or configuration
    /// change (the *reply* to a command, not its invocation).
    ///
    /// `title` is the one-line confirmation (e.g. `"Delegated mode ON: …"`). The
    /// notice is `Info` severity and carries the `CommandAck` kind, which the
    /// presentation substrate's routing policy maps to the toast layer: it
    /// carries no conversational content and never reaches the transcript.
    /// ADR-0050 keeps the command *invocation* durable; this reply is
    /// deliberately ephemeral.
    ///
    /// Prefer this constructor over `AgentNotice::new(NoticeKind::CommandAck, …)`
    /// so the kind is stamped uniformly and frontends can branch on
    /// `kind == CommandAck` (e.g. to suppress re-surfacing on reconnect).
    ///
    /// A command acknowledgment is a *session-scoped* notice: emit it wrapped
    /// in `RoundEvent::Notice` (via `round_response`), not as a
    /// top-level `AgentResponse::Notice`. Wrapping it routes the toast to the
    /// frontend over the session's broadcast tap so every attached client (the
    /// in-process TUI, `nuo attach`, `/serve`) sees the same confirmation,
    /// and it is what the TUI's toast drain actually listens for.
    pub fn command_ack(title: impl Into<String>) -> Self {
        Self::new(
            NoticeKind::CommandAck,
            NoticeSeverity::Info,
            title,
            NoticeSource::Harness,
        )
    }

    /// Build a workspace-trust notice: previously trusted (or shadowing)
    /// project-authored content changed on disk, is quarantined, or overrides
    /// a user-scope entry — and needs a human `/trust` decision or inspection.
    ///
    /// Stamps the `TrustChanged` kind, `Warning` severity, and `Harness`
    /// source uniformly so frontends can branch on `kind == TrustChanged`
    /// (topic labels, filtering, re-surfacing on reconnect) without
    /// string-matching titles. Callers supply the detail body; the layer is
    /// derived by `nuo-view`'s `model::layer` from the surface
    /// context, never chosen here.
    ///
    /// Makes this a first-class kind, revising stance that
    /// `ReviewAlert` (the closed enum's generic "needs attention" value) was
    /// good enough for trust notices.
    pub fn trust_changed(title: impl Into<String>) -> Self {
        Self::new(
            NoticeKind::TrustChanged,
            NoticeSeverity::Warning,
            title,
            NoticeSource::Harness,
        )
    }

    pub fn render_text(&self) -> String {
        match self.body.as_deref().filter(|body| !body.trim().is_empty()) {
            Some(body) => format!("{}\n{}", self.title, body),
            None => self.title.clone(),
        }
    }
}
