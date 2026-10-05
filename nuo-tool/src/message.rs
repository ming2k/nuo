//! Conversation message types shared across the harness, providers, and UI.

use crate::todos::unix_now;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEventKind {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    PreCompact,
    PostCompact,
    Turn,
    #[serde(alias = "RoundStart")]
    TurnStart,
    PermissionRequest,
    UserQuestion,
}

impl HookEventKind {
    /// Whether this event filters on a tool name and so honours a matcher.
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            Self::PreToolUse
                | Self::PostToolUse
                | Self::PostToolUseFailure
                | Self::PermissionRequest
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

pub const CLEARED_TOOL_PREFIX: &str = "[cleared tool result:";

pub fn estimate_tokens(messages: &[Message]) -> usize {
    let mut tokens: usize = 0;
    for m in messages {
        tokens += crate::tokenizer::count_tokens(&m.content) + 4;
        if let Some(children) = m.children.as_ref() {
            tokens += estimate_tokens(children);
        }
    }
    tokens.max(1)
}

/// Provenance of a message that was inserted by the harness rather than
/// produced by the model or typed by the user. Stamped at every injection
/// site so a persisted transcript can answer "what was injected, where did
/// it come from, and why" — exactly reconstructing the live turn without
/// fragile string-sniffing.
///
/// `origin: None` is the default for every genuine message: real user input,
/// assistant replies, and tool results. Only harness-injected messages carry
/// an origin.
///
/// Kept as `Option<InjectionOrigin>` on [`Message`] with
/// `#[serde(default, skip_serializing_if = "Option::is_none")]` so the wire
/// shape of a default message is unchanged and legacy snapshots / event-log
/// lines load as `origin: None` with no migration (per ADR-0017 / ADR-0022
/// backward-compat contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectionOrigin {
    /// Structured source classifier.
    pub kind: InjectionKind,
    /// Free-form reason — e.g. the hook name, the steering cause, the skill
    /// that fired. `None` when the kind alone is self-describing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl InjectionOrigin {
    pub fn new(kind: InjectionKind) -> Self {
        Self { kind, reason: None }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Closed classifier for every harness injection path. Adding a variant
/// requires stamping it at the corresponding call site; the enum exhaustiveness
/// is the design lever that forces every injection to be traceable.
///
/// Each variant maps 1:1 to a concrete injection site in the harness; the
/// doc-link in each arm is the single source of truth for "where does this
/// come from".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionKind {
    /// A user-configured hook returned `HookOutcome::Inject`. Carries the
    /// lifecycle event so "which hook axis injected this" is recoverable.
    /// Sites: `HookRegistry::{session_start, run_post_tool_use,
    /// run_post_tool_use_failure, check_stop, run_turn, run_turn_start}`.
    Hook(HookEventKind),
    /// Inter-agent steering note (`AgentOp::InterAgentMessage`, codex
    /// `InterAgentCommunication` analogue). Site: `Agent::drain_inbox`.
    InterAgent,
    /// Visible parent→child steering payload (`AgentOp::InjectUserMessage`,
    /// codex `inject_if_running` analogue). Site: `Agent::drain_inbox`.
    /// Lands as a *visible* user message, hence distinct from `InterAgent`.
    SubagentSteer,
    /// Visible human-authored steering input admitted into the principal (or a
    /// side conversation) at a safe turn boundary. Unlike `SubagentSteer`, the
    /// author is the user; the origin records its mid-round placement so a
    /// restored transcript can render it as an insert rather than a new round.
    UserSteer,
    /// The initial visible task handed to a newly spawned subagent. Unlike
    /// `SubagentSteer`, this opens the child transcript rather than steering an
    /// already-running child. Site: `SubagentTool::run`.
    SubagentTask,
    /// Legacy: the transcript snapshot once handed to the bounded
    /// session-review subagent (the `/review` diagnostic, now retired). No
    /// production site constructs it anymore; the variant stays so old
    /// session files carrying injected rows still deserialize.
    SessionReviewInput,
    /// Implicit skill auto-load: the latest user round mentioned a skill name,
    /// so the skill body was injected in-context. Site:
    /// `nuo-agent`'s conversation-context skill injection policy.
    ImplicitSkill,
    /// Implicit file auto-load: the latest user round referenced a path via
    /// `@file:` / `@files:`, so the file's contents were injected in-context
    /// (sandboxed to the workspace root and capped in size). The companion to
    /// `ImplicitSkill` for source files. Site: `nuo-agent`'s
    /// conversation-context file injection policy.
    ImplicitFile,
    /// System-prompt assembly: the harness rebuilt the head system message
    /// from live identity, model/provider, and tool state through the
    /// agent's request-scoped `SystemPromptRegistry` assembly.
    SystemPrompt,
    /// Built-in anti-anchoring nudge fired by the deterministic read-loop guard
    /// when the model repeats the same read (a single page or a two-page thrash)
    /// without progress. Detection is pure signature bookkeeping — no model call
    /// — and the nudge is non-terminating: it steers off the loop, the hard
    /// backstops (`hard_stop_turns`, `abort`, `Esc`) still cap. This is a
    /// harness-internal steering injection, distinct from the user-configurable
    /// `Hook(Turn)` axis. Site: `Agent::maybe_inject_loop_nudge`
    /// (`muta_agent::guard`).
    LoopReviewNudge,
    /// Context-compaction checkpoint: an LLM summary of archived rounds wrapped
    /// under the stable checkpoint header. Site: `checkpoint_message`.
    CompactionCheckpoint,
    /// A harness-internal prompt admitted through the orchestration layer as a
    /// hidden user round (for example resume/replay input). Site:
    /// `execute_round`'s `input.hidden` branch.
    #[serde(alias = "hidden_turn_input")]
    HiddenRoundInput,
    /// A non-driving command echo: the literal text of a user invocation that
    /// is recorded in the durable transcript for resume/export/audit
    /// faithfulness but is **never sent to the model**. Covers commands
    /// (e.g. `/session …`) which the harness handles directly without an LLM
    /// roundtrip. Distinct from `HiddenRoundInput` (which *is* a driving
    /// hidden prompt): a `CommandEcho` carries no instruction for the model.
    /// Projected out before the wire by model-request assembly.
    /// Site: `handlers_slash::dispatch`. (ADR-0050.)
    CommandEcho,
    /// A user-role image companion projected from a tool result for providers
    /// that accept image inputs. Site: `conversation_context::tool_image`.
    ToolImage,
    /// An **authoritative** harness directive wrapped in a `<system-reminder>`
    /// block. Unlike the stable head `SystemPrompt`, a system reminder is
    /// event-driven and mid-turn: it carries a transient, situation-specific
    /// instruction the model MUST follow (it may override normal behavior —
    /// e.g. "you are now read-only"). (ADR-0068.)
    SystemReminder,
    /// User-provided or foreign task data wrapped in an `<untrusted_…>` block
    /// (objective text, pasted content) that the model must treat as **data,
    /// not instructions** — it must not override system messages, tool schemas,
    /// or permission rules. (ADR-0068.)
    UntrustedDirective,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Content-addressed storage hash for large payloads. When present the
    /// inline `content` may be empty on disk and is rehydrated from the blob
    /// store on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Provider-opaque sidecar for **wire-protocol detail that does not map to
    /// a cross-provider semantic concept** and therefore has no business as a
    /// named field on this struct. The canonical example is Anthropic's
    /// extended-thinking `signature` — a cryptographic credential the server
    /// requires to reconstruct a prior `thinking` block on multi-turn replay.
    /// It is meaningless to OpenAI/Google, so instead of a named
    /// `thinking_signature` field (which would pollute this provider-agnostic
    /// type with one protocol's transport detail), Anthropic-specific values
    /// live under a `"thinking_signature"` key inside this map. Each concrete
    /// provider owns the contract for the keys it reads/writes here; `core`
    /// treats the whole map as an opaque blob that round-trips through
    /// `session.json` (so a resumed session replays thinking correctly) but is
    /// never inspected outside the provider that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// Optional tool calls attached to an assistant message. Marked
    /// `#[serde(default)]` so hand-written or stripped JSON messages (e.g. test
    /// fixtures, externally generated snapshots) can omit the key entirely
    /// instead of having to spell out `"tool_calls": null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Inline image attachments (typically pasted into the prompt). Each part
    /// carries a MIME type and already-base64-encoded bytes so it can be
    /// emitted directly as an OpenAI `image_url` data URL or a Google
    /// `inline_data` part. Only user messages normally carry images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImagePart>>,
    /// Identifier of the provider/solution that produced this assistant
    /// message (e.g. `"kimi-code"`, `"google"`). Stamped by the harness so a
    /// session that mixes multiple models stays traceable after resume. Other
    /// roles leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model identifier that produced this assistant message (e.g.
    /// `"kimi-code"`). Companion to [`Message::provider`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The reasoning effort (depth) this message's model request ran with
    /// (`"high"`, `"max"`, …), when the active channel exposes one. Stamped
    /// next to [`Message::provider`]/[`Message::model`] so the transcript can
    /// show the depth each turn actually ran at after resume. Storage/UI
    /// metadata only — stripped by [`Message::to_wire`] and never sent to the
    /// provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    /// Nested subagent transcript. Populated only on the `Tool`-role result
    /// message of a `task` tool call (see `SubagentTool`). Each entry is a
    /// `Message` from the subagent's own conversation (System, User,
    /// Assistant with tool_calls, Tool results, …), in chronological order.
    /// Recursive: a subagent's own `task` results carry their own `children`,
    /// so arbitrarily deep subagent trees round-trip through session.json.
    ///
    /// `None` for every message that is not a subagent's tool result; this
    /// keeps the legacy flat shape unchanged for non-task messages and lets
    /// old session.json files (which predate the field) deserialize as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Message>>,
    /// Metadata about the subagent run that produced [`Message::children`].
    /// Populated only on the same message that has `children = Some(_)`. The
    /// two fields are convention-paired (presence of one implies presence of
    /// the other); they are kept separate rather than bundled into a single
    /// `subagent: Option<Payload>` field so the schema stays backward-
    /// compatible without a custom deserializer — old session.json files
    /// simply have `subagent_meta = None` and `children = Some(...)`, and the
    /// harness fills in best-effort defaults on read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_meta: Option<SubagentMeta>,
    /// Provenance of a harness-injected message (`None` for genuine user input,
    /// assistant replies, and tool results). See [`InjectionOrigin`] / the
    /// closed [`InjectionKind`] classifier. `#[serde(default,
    /// skip_serializing_if = "Option::is_none")]` keeps the wire shape of a
    /// default message unchanged so legacy snapshots and event-log lines load
    /// as `origin: None` without migration (ADR-0017 / ADR-0022).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InjectionOrigin>,
    /// Wall-clock time this message was produced, in Unix-epoch seconds.
    /// Stamped at construction so it survives event-log compaction — which
    /// rewrites the `.jsonl` to a single seed event and thereby drops every
    /// `EventEnvelope::timestamp` — giving each message a durable timestamp
    /// that travels with it through the snapshot, the archive, and
    /// context-projection. Kept off the provider wire (`to_wire` zeroes it).
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` lets legacy
    /// snapshots load unchanged as `timestamp: None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    /// Exact wall-clock send time for user-authored transcript messages, in
    /// Unix-epoch milliseconds. This is storage/UI metadata only: it preserves
    /// the TUI's displayed send time across resume without changing provider
    /// requests, which continue to use [`Message::to_wire`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<u64>,
    /// This message's provider-visible shape is **permanently settled**: every
    /// byte that will ever reach the provider for it is already fixed, and no
    /// later assembly pass may transform it again.
    ///
    /// Set exactly once, when tool-output compaction freezes a historical
    /// `Tool` message into its final truncated form (`trim_tool_output`). The
    /// freeze is the *only* mutation a tool result ever takes: one turn after
    /// the result lands the shape is chosen, and from then on the provider
    /// sees byte-identical content every round. This is what makes the
    /// server-side KV-cache prefix stable — without the flag, every assembly
    /// pass re-derived the "historical" truncation from an ever-shifting
    /// recency window and the prefix broke on two consecutive rounds.
    ///
    /// `to_wire` strips the flag (it is harness bookkeeping, not wire data);
    /// session/event-log persistence keeps it so a resumed session replays
    /// with the same frozen shapes.
    #[serde(default)]
    pub cache_frozen: bool,
}

/// Sidecar metadata for a subagent run. Lives next to
/// [`Message::children`] on the same `Tool`-role result message. Captures
/// information that the live event stream knows but the bare transcript
/// cannot reconstruct on resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SubagentMeta {
    /// The task description supplied by the parent agent (from the `task`
    /// tool_call's `arguments.description` field). Cached here so the TUI
    /// does not have to re-parse the JSON arguments to label the subagent
    /// view's navigation bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Wall-clock duration of the subagent run in milliseconds. Filled from
    /// the parent `record_tool_result`'s `duration_ms` parameter (which
    /// already measures the full subagent run because the `task` tool blocks
    /// until the subagent finishes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Number of read-only tools the subagent had access to. Useful as a
    /// debugging signal when reviewing archived runs.
    #[serde(default)]
    pub toolset_count: u32,
    /// Provider / model that served the subagent. Currently always equal to
    /// the parent's provider/model (SubagentTool clones the parent's provider),
    /// but persisted separately so a future "cheaper model for subagents"
    /// feature does not require a schema change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether the subagent finished by hitting an error path (32-turn
    /// limit, repeated-call guard, provider error). Mirrors
    /// `ToolOutput::Subagent { summary.starts_with("Error") }` but stored
    /// explicitly so consumers do not have to string-sniff.
    #[serde(default)]
    pub failed: bool,
    /// Whether the subagent was stopped by the parent (the turn was interrupted)
    /// before completing. The partial transcript in [`Message::children`] is
    /// preserved either way; this flag lets the TUI classify the restored
    /// step as `Interrupted` rather than `Failed` or `Ok`.
    #[serde(default)]
    pub interrupted: bool,
}

/// An inline image attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePart {
    /// MIME type, e.g. `"image/png"`.
    pub mime: String,
    /// Base64-encoded image bytes.
    pub data: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: None,
            tool_call_id: None,
            images: None,
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: Some(unix_now()),
            sent_at_ms: None,
            cache_frozen: false,
        }
    }

    pub fn hidden(role: Role, content: impl Into<String>) -> Self {
        let mut message = Self::new(role, content);
        message.hidden = true;
        message
    }

    /// Construct a hidden user/system message with an explicit injection
    /// origin. This is the canonical constructor for every harness injection
    /// site — it stamps provenance at construction so it can never drift from
    /// the content. Use [`Message::with_origin`] to stamp an existing message.
    pub fn injected(role: Role, content: impl Into<String>, origin: InjectionOrigin) -> Self {
        let mut message = Self::hidden(role, content);
        message.origin = Some(origin);
        message
    }

    /// Construct a **non-driving** command echo: a visible (`hidden = false`)
    /// user message stamped with the `CommandEcho` provenance. Recorded in the
    /// durable transcript for resume/export/audit faithfulness but projected
    /// out before the provider wire (see model-request assembly, ADR-0050).
    /// Unlike [`Message::injected`] it is *visible* — the echo must show on
    /// resume — and unlike a driving prompt it never reaches the model.
    pub fn command_echo(text: impl Into<String>) -> Self {
        let mut message = Self::new(Role::User, text);
        message.origin = Some(InjectionOrigin::new(InjectionKind::CommandEcho));
        message
    }

    /// Whether this message is a non-driving command echo — recorded durably
    /// for resume/export but excluded from the provider wire and from
    /// compaction turn-counting. The single predicate consulted at both the
    /// wire funnel and `select_compaction`; keep them in sync (ADR-0050).
    pub fn is_command_echo(&self) -> bool {
        self.origin
            .as_ref()
            .is_some_and(|o| o.kind == InjectionKind::CommandEcho)
    }

    /// Stamp / overwrite the injection origin on this message. Builder-style
    /// companion to [`Message::injected`] for sites that build a message via
    /// another constructor first (e.g. `Message::hidden(...).with_origin(...)`).
    pub fn with_origin(mut self, origin: InjectionOrigin) -> Self {
        self.origin = Some(origin);
        self
    }

    pub fn with_display_content(mut self, content: impl Into<String>) -> Self {
        self.display_content = Some(content.into());
        self
    }

    pub fn with_images(mut self, images: Vec<ImagePart>) -> Self {
        self.images = if images.is_empty() {
            None
        } else {
            Some(images)
        };
        self
    }

    pub fn with_sent_at_ms(mut self, sent_at_ms: u64) -> Self {
        self.sent_at_ms = Some(sent_at_ms);
        self
    }

    /// Stamp the provider/solution id and model that produced this message,
    /// so the transcript stays traceable when a session spans multiple models.
    pub fn with_attribution(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self
    }

    pub fn tool_result(call: &ToolCall, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: None,
            tool_call_id: Some(call.id.clone()),
            images: None,
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: Some(unix_now()),
            sent_at_ms: None,
            cache_frozen: false,
        }
    }

    /// Attach a subagent's full internal transcript to a `Tool`-role result
    /// message. Builder-style companion to [`Message::tool_result`]. Storing
    /// the nested transcript on the result message (rather than on the
    /// assistant `tool_calls` message) keeps the data close to where it was
    /// produced and lets resume reconstruct the subagent view by reading a
    /// single message.
    pub fn with_children(mut self, children: Vec<Message>) -> Self {
        self.children = if children.is_empty() {
            None
        } else {
            Some(children)
        };
        self
    }

    /// Attach subagent sidecar metadata to a `Tool`-role result message.
    /// Pair with [`Message::with_children`]; the two fields travel together
    /// but are kept separate for schema-backward-compat (see
    /// [`Message::subagent_meta`] docs).
    pub fn with_subagent_meta(mut self, meta: SubagentMeta) -> Self {
        self.subagent_meta = Some(meta);
        self
    }

    /// Retrieve the Google Gemini thought signature for a given tool call ID or name (ADR-0297).
    pub fn gemini_thought_signature(&self, call_key: &str) -> Option<&str> {
        let meta = self.provider_meta.as_ref()?;
        if let Some(signatures) = meta
            .get("gemini_thought_signatures")
            .and_then(serde_json::Value::as_object)
        {
            if let Some(sig) = signatures.get(call_key).and_then(serde_json::Value::as_str) {
                return Some(sig);
            }
        }
        meta.get("gemini_text_thought_signature")
            .and_then(serde_json::Value::as_str)
    }

    /// Check if this assistant message carries a valid Gemini thought signature for the given tool call (ADR-0297).
    pub fn has_gemini_thought_signature_for(&self, call: &ToolCall) -> bool {
        self.gemini_thought_signature(&call.id).is_some()
            || self.gemini_thought_signature(&call.name).is_some()
    }

    /// Check if this assistant message contains at least one Google Gemini thought signature (ADR-0297).
    pub fn has_gemini_thought_signatures(&self) -> bool {
        let Some(meta) = self.provider_meta.as_ref() else {
            return false;
        };
        meta.get("gemini_thought_signatures")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|m| !m.is_empty())
            || meta.get("gemini_text_thought_signature").is_some()
    }

    /// Project this message to its provider-**wire** form: the minimal shape a
    /// provider request body serializes. Strips every out-of-band field —
    /// nested subagent [`children`](Self::children),
    /// [`subagent_meta`](Self::subagent_meta), injection [`origin`](Self::origin),
    /// [`hidden`](Self::hidden), [`provider`](Self::provider)/[`model`](Self::model)
    /// attribution, [`provider_meta`](Self::provider_meta), and the storage/UI
    /// sidecars [`content_blob`](Self::content_blob)/[`display_content`](Self::display_content) — keeping only
    /// `role`, `content`, `tool_calls`, `tool_call_id`, `images`, and
    /// `reasoning_content` (which Anthropic replays as a `thinking` block).
    ///
    /// Each concrete provider's request builder ignores the out-of-band fields
    /// by construction (it simply never reads them); `to_wire` makes that
    /// projection explicit and reusable. It is the single source of truth for
    /// "what the model actually sees", independent of any one SDK's
    /// `message_obj` — used by `/debug preview`, which must dump the wire
    /// request body, not the internal [`Message`] struct that also carries
    /// durable-session sidecars.
    pub fn to_wire(&self) -> Message {
        Message {
            role: self.role,
            content: self.content.clone(),
            content_blob: None,
            display_content: None,
            reasoning_content: self.reasoning_content.clone(),
            provider_meta: None,
            tool_calls: self.tool_calls.clone(),
            tool_call_id: self.tool_call_id.clone(),
            images: self.images.clone(),
            provider: None,
            model: None,
            effort: None,
            hidden: false,
            children: None,
            subagent_meta: None,
            origin: None,
            timestamp: None,
            sent_at_ms: None,
            cache_frozen: false,
        }
    }

    /// Compare two messages for wire-level semantic equivalence without allocating.
    ///
    /// Semantics are identical to `self.to_wire() == other.to_wire()`, but performs
    /// zero heap allocations and avoids copying contents or sidecars.
    #[inline]
    pub fn semantic_wire_eq(&self, other: &Self) -> bool {
        self.role == other.role
            && self.content == other.content
            && self.reasoning_content == other.reasoning_content
            && self.tool_calls == other.tool_calls
            && self.tool_call_id == other.tool_call_id
            && self.images == other.images
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_without_children_omits_field_in_json() {
        // Legacy compatibility: a normal Message must still serialise without
        // a `children` key so old consumers / tests that match the literal
        // JSON keep working.
        let m = Message::new(Role::User, "hi");
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            !json.contains("children"),
            "json should omit children: {json}"
        );
    }

    #[test]
    fn legacy_json_without_children_deserialises_to_none() {
        // Pre-Phase-3 snapshots must load unchanged.
        let json = r#"{"role":"User","content":"hi","hidden":false}"#;
        let m: Message = serde_json::from_str(json).unwrap();
        assert_eq!(m.content, "hi");
        assert!(m.children.is_none());
        assert!(m.sent_at_ms.is_none());
    }

    #[test]
    fn children_round_trip_through_json() {
        // A tool result with a subagent transcript must survive a
        // serialise → deserialise round trip with the nested messages intact,
        // including their own nested children (sub-subagents).
        let call = ToolCall {
            id: "call_root".to_string(),
            name: "subagent".to_string(),
            arguments: "{}".to_string(),
        };
        let nested_call = ToolCall {
            id: "call_inner".to_string(),
            name: "search_text".to_string(),
            arguments: r#"{"pattern":"foo"}"#.to_string(),
        };
        let inner_child = Message::new(Role::Tool, "match at a.rs:1")
            .with_children(vec![Message::new(Role::Assistant, "deeply nested note")]);
        let subagent_transcript = vec![
            Message::new(Role::System, "subagent system"),
            Message::new(Role::User, "subagent task"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                content_blob: None,
                display_content: None,
                reasoning_content: None,
                provider_meta: None,
                tool_calls: Some(vec![nested_call]),
                tool_call_id: None,
                images: None,
                provider: None,
                model: None,
                effort: None,
                hidden: false,
                children: None,
                subagent_meta: None,
                origin: None,
                timestamp: None,
                sent_at_ms: None,
                cache_frozen: false,
            },
            inner_child,
        ];
        let parent = Message::tool_result(&call, "[task result]:\nfound it")
            .with_children(subagent_transcript);

        let json = serde_json::to_string_pretty(&parent).unwrap();
        let restored: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.role, Role::Tool);
        assert_eq!(restored.tool_call_id.as_deref(), Some("call_root"));
        let children = restored.children.expect("children round-trip");
        assert_eq!(children.len(), 4);
        // The grep call inside the subagent kept its tool_calls.
        assert!(children[2].tool_calls.is_some());
        // The inner Tool message kept its own nested children (sub-subagent).
        let inner = &children[3];
        assert_eq!(inner.role, Role::Tool);
        assert!(
            inner.children.is_some(),
            "sub-subagent children must survive"
        );
        assert_eq!(inner.children.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn with_children_empty_vec_is_none() {
        let call = ToolCall {
            id: "c".to_string(),
            name: "subagent".to_string(),
            arguments: "{}".to_string(),
        };
        let m = Message::tool_result(&call, "x").with_children(Vec::new());
        assert!(
            m.children.is_none(),
            "empty children should collapse to None"
        );
    }

    #[test]
    fn to_wire_strips_children_and_sidecars_keeps_wire_fields() {
        // A Tool-role subagent result carries a heavy nested transcript + meta +
        // attribution + origin. `to_wire` must drop all of those (the provider
        // never sees them) while keeping role/content/tool_call_id. This is the
        // contract `/debug preview` relies on to dump the real request body
        // rather than the internal `Message` struct that also carries
        // durable-session sidecars.
        let call = ToolCall {
            id: "c".to_string(),
            name: "subagent".to_string(),
            arguments: "{}".to_string(),
        };
        let m = Message::tool_result(&call, "[subagent result]: summary")
            .with_children(vec![Message::new(
                Role::Assistant,
                "subagent internal turn",
            )])
            .with_subagent_meta(SubagentMeta::default())
            .with_attribution("kimi", "kimi-code")
            .with_origin(InjectionOrigin::new(InjectionKind::SubagentSteer))
            .with_sent_at_ms(1_700_000_000_123);

        let w = m.to_wire();
        assert_eq!(w.role, Role::Tool);
        assert_eq!(w.content, "[subagent result]: summary");
        assert_eq!(w.tool_call_id.as_deref(), Some("c"));
        assert!(w.children.is_none(), "children must be stripped");
        assert!(w.subagent_meta.is_none(), "subagent_meta must be stripped");
        assert!(
            w.provider.is_none() && w.model.is_none(),
            "attribution stripped"
        );
        assert!(w.origin.is_none(), "origin stripped");
        assert!(w.sent_at_ms.is_none(), "UI timestamp stripped");
        assert!(!w.hidden, "hidden reset to false");
        assert!(
            w.content_blob.is_none() && w.display_content.is_none(),
            "storage/UI sidecars stripped"
        );
    }

    #[test]
    fn semantic_wire_eq_matches_to_wire_equality() {
        let m1 = Message::new(Role::User, "hello")
            .with_attribution("kimi", "kimi-code")
            .with_sent_at_ms(12345);
        let m2 = Message::new(Role::User, "hello")
            .with_attribution("openai", "gpt-4o")
            .with_sent_at_ms(67890);
        let m3 = Message::new(Role::User, "hello world");

        assert!(m1.semantic_wire_eq(&m2));
        assert_eq!(m1.to_wire() == m2.to_wire(), m1.semantic_wire_eq(&m2));
        assert!(!m1.semantic_wire_eq(&m3));
        assert_eq!(m1.to_wire() == m3.to_wire(), m1.semantic_wire_eq(&m3));
    }

    #[test]
    fn default_message_omits_origin_key() {
        // A genuine message (user input / assistant / tool result) must
        // serialise WITHOUT an `origin` key, so the wire shape is unchanged
        // and legacy consumers / snapshot matchers keep working. Mirrors the
        // `children` compat contract.
        let m = Message::new(Role::User, "hi");
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            !json.contains("origin"),
            "default message must omit origin: {json}"
        );
        assert!(
            !json.contains("injection"),
            "default message must omit injection fields: {json}"
        );
    }

    #[test]
    fn legacy_json_without_origin_loads_as_none() {
        // A pre-C4 snapshot / event-log line has no `origin` key and must
        // deserialise to `origin: None`. This is the load-side of the
        // backward-compat contract.
        let json = r#"{"role":"User","content":"hi","hidden":false}"#;
        let m: Message = serde_json::from_str(json).unwrap();
        assert_eq!(m.content, "hi");
        assert!(m.origin.is_none());
    }

    #[test]
    fn injection_origin_round_trips() {
        // A stamped origin must survive a serialise → deserialise round trip
        // with its kind, reason, and the nested HookEventKind intact. This is
        // the contract that makes the persisted transcript faithfully
        // reconstruct injection provenance.
        use super::HookEventKind;
        let msg = Message::injected(
            Role::User,
            "remember X",
            InjectionOrigin::new(InjectionKind::Hook(HookEventKind::PostToolUse))
                .with_reason("my_hook.sh"),
        );
        let json = serde_json::to_string(&msg).unwrap();
        // The origin object is present in the wire form.
        assert!(json.contains("\"origin\""), "origin must serialise: {json}");
        assert!(
            json.contains("\"reason\":\"my_hook.sh\""),
            "reason must serialise: {json}"
        );
        let restored: Message = serde_json::from_str(&json).unwrap();
        let origin = restored.origin.expect("origin round-trip");
        assert_eq!(origin.kind, InjectionKind::Hook(HookEventKind::PostToolUse));
        assert_eq!(origin.reason.as_deref(), Some("my_hook.sh"));
    }

    #[test]
    fn every_injection_kind_serialises_distinctly() {
        // The closed classifier must serialise to distinct wire forms so a
        // persisted transcript can discriminate injection sources without
        // ambiguity. Regression guard: adding a variant without a distinct
        // serde tag would silently collapse provenance. We compare the full
        // serialised `kind` (not just a prefix) because `Hook(HookEventKind)`
        // serialises as a map `{"hook":"session_start"}` while unit variants
        // serialise as a bare string.
        use super::HookEventKind;
        let cases: Vec<InjectionKind> = vec![
            InjectionKind::Hook(HookEventKind::SessionStart),
            InjectionKind::Hook(HookEventKind::PostToolUse),
            InjectionKind::Hook(HookEventKind::Stop),
            InjectionKind::Hook(HookEventKind::Turn),
            InjectionKind::Hook(HookEventKind::TurnStart),
            InjectionKind::InterAgent,
            InjectionKind::SubagentSteer,
            InjectionKind::SubagentTask,
            InjectionKind::SessionReviewInput,
            InjectionKind::ImplicitSkill,
            InjectionKind::SystemPrompt,
            InjectionKind::CompactionCheckpoint,
            InjectionKind::HiddenRoundInput,
            InjectionKind::LoopReviewNudge,
            InjectionKind::CommandEcho,
            InjectionKind::ToolImage,
            InjectionKind::SystemReminder,
            InjectionKind::UntrustedDirective,
        ];
        let mut forms = Vec::new();
        for kind in cases {
            // Round-trip each kind in isolation.
            let json = serde_json::to_string(&kind).unwrap();
            let restored: InjectionKind = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, kind, "kind {kind:?} must round-trip");
            forms.push(json);
        }
        let mut sorted = forms.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            forms.len(),
            "injection kinds must serialise to distinct wire forms: {forms:?}"
        );
    }

    #[test]
    fn hidden_round_input_writes_canonical_name_and_reads_legacy_name() {
        let legacy: InjectionKind = serde_json::from_str("\"hidden_turn_input\"").unwrap();
        assert_eq!(legacy, InjectionKind::HiddenRoundInput);
        assert_eq!(
            serde_json::to_string(&legacy).unwrap(),
            "\"hidden_round_input\""
        );
    }

    #[test]
    fn injected_constructor_stamps_origin_and_hidden() {
        // `Message::injected` must set BOTH hidden=true (display contract) and
        // origin=Some (provenance contract). The two are orthogonal: hidden
        // governs visibility, origin governs "why is this here".
        let m = Message::injected(
            Role::User,
            "nudge",
            InjectionOrigin::new(InjectionKind::LoopReviewNudge),
        );
        assert!(m.hidden, "injected message must be hidden");
        assert!(m.origin.is_some(), "injected message must carry origin");
        assert_eq!(
            m.origin.as_ref().unwrap().kind,
            InjectionKind::LoopReviewNudge
        );
    }

    #[test]
    fn command_echo_is_visible_and_non_driving() {
        // A command echo is VISIBLE (hidden=false, so it shows on resume/export)
        // yet non-driving (is_command_echo=true, so it is projected out before
        // the wire). It must not be conflated with an injected/hidden message,
        // whose `hidden` axis means the opposite half (ADR-0050).
        let m = Message::command_echo("/session list");
        assert!(!m.hidden, "command echo must be visible on resume/export");
        assert_eq!(m.role, Role::User);
        assert!(m.is_command_echo());
        // Round-trips with provenance intact (the durable record must survive).
        let restored: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert!(
            restored.is_command_echo(),
            "echo origin survives round-trip"
        );
        // A plain user message is not an echo.
        assert!(!Message::new(Role::User, "hello").is_command_echo());
        // An injected (hidden) message is not an echo even though it has origin.
        assert!(
            !Message::injected(
                Role::User,
                "x",
                InjectionOrigin::new(InjectionKind::HiddenRoundInput)
            )
            .is_command_echo()
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
}
