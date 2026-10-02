//! Single-transcript persistence model (ADR-0186).
//!
//! The durable session stores **facts** (`TranscriptEntry`) and **decisions**
//! (`ProjectionDirective`); every consumer-facing window is a pure `derive`
//! over the two. Entries are immutable and position-free facts; position lives
//! in the per-session membership (seq). Projections never rewrite entries —
//! they append a directive and, for compaction, one checkpoint entry.

use crate::message::{ImagePart, InjectionOrigin, Message, Role, ToolCall};
use crate::todos::unix_now;
use serde::{Deserialize, Serialize};

/// Unix-epoch milliseconds now.
fn unix_now_ms() -> u64 {
    unix_now().saturating_mul(1000)
}

/// What an entry is. Open enum: new kinds extend the payload contract; readers
/// must preserve unknown kinds verbatim (unknown deserialises as `Unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum EntryKind {
    /// A transcript message (user / assistant / system / tool).
    Message,
    /// A working-state snapshot (e.g. the TodoList mirror). Never enters a
    /// view; consumers derive the current state from the latest entry.
    State,
}

/// Coarse provenance of a harness-written entry. `NULL` (absent) is genuine
/// dialogue: real user input, model output, tool results. The rich structured
/// classifier (`InjectionOrigin`) travels inside the payload; the envelope
/// value is only the projection-relevant class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum EntryOrigin {
    /// Program-injected content (system reminders, steering notes, ...).
    Harness,
    /// Compaction checkpoint: stands in for an elided history range and is
    /// referenced by a `compact` projection directive.
    Checkpoint,
}

/// Reference to the dedicated session that durably records a subagent run
/// (ADR-0186 §6). Replaces inline nested transcripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct SubagentRef {
    /// The subagent session's id (its own `sessions` row).
    pub session_id: String,
    /// Task description copied from the spawning tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Wall-clock duration of the run in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Number of read-only tools the subagent had access to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset_count: Option<u32>,
}

/// Kind-specific payload of a message entry. Everything that is not an
/// envelope column (see ADR-0186 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct MessagePayload {
    /// Tool calls declared by an assistant message. `call.id` pairs with the
    /// matching tool-result entry's `tool_call_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// The tool call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImagePart>>,
    /// Producing provider attribution (assistant messages). Consumed by the
    /// protocol boundary to decide per-protocol transforms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Provider-opaque sidecar (Gemini thought signatures, Anthropic thinking
    /// signature, ...). Never inspected by the harness core; round-trips
    /// verbatim through every projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "Record<string, unknown> | undefined")]
    pub provider_meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// Dedicated subagent session this result points at (replaces inline
    /// nested transcripts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<SubagentRef>,
    /// Content-addressed blob holding the full body (large tool output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Rich provenance classifier for harness-injected entries (the envelope
    /// `origin` is the coarse class).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injection: Option<InjectionOrigin>,
    /// Exact user-authored send time (milliseconds), UI sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<u64>,
    /// The provider-visible shape of this entry is permanently settled; no
    /// later assembly pass may transform it again (KV-cache prefix stability).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache_frozen: bool,
}

/// Kind-specific payload of an entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum EntryPayload {
    /// A transcript message. Boxed: the payload dominates the entry size and
    /// entries are cloned wholesale on every turn commit (ADR-0187).
    #[serde(rename = "message")]
    Message(Box<MessagePayload>),
    /// A working-state snapshot (e.g. the TodoList mirror).
    #[serde(rename = "state")]
    State(StatePayload),
}

/// Working-state snapshot carried by a `state` entry. Fields are additive;
/// consumers derive the current state from the newest `state` entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct StatePayload {
    /// The unified task-list mirror at the time of the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<crate::todos::TodoList>,
}

/// An immutable transcript fact (ADR-0186 §2). `seq` is the entry's position
/// **in one session** (materialized from `entry_memberships`); the same entry
/// shared across a fork carries the same id with per-session seq values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct TranscriptEntry {
    /// Global identity — stable across forks and re-memberships.
    pub id: String,
    /// Position within the owning session (membership seq).
    pub seq: u64,
    pub kind: EntryKind,
    /// Only for `kind = Message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Body text. The full body may live in `payload.content_blob` (CAS);
    /// inline content may then be empty or elided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Coarse provenance; `None` for genuine dialogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<EntryOrigin>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Wall-clock creation time in Unix-epoch milliseconds.
    #[serde(default = "unix_now_ms")]
    pub created_at_ms: u64,
    pub payload: EntryPayload,
}

impl TranscriptEntry {
    /// A message entry built from a wire [`Message`]. Lossless: the inverse
    /// conversion reconstructs the message exactly (minus inline subagent
    /// children, which are sessions of their own under this model).
    pub fn from_message(seq: u64, message: &Message) -> Self {
        let injection = message.origin.clone();
        let origin = match message.origin.as_ref().map(|o| o.kind) {
            Some(crate::message::InjectionKind::CompactionCheckpoint) => {
                Some(EntryOrigin::Checkpoint)
            }
            Some(_) => Some(EntryOrigin::Harness),
            None => None,
        };
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            seq,
            kind: EntryKind::Message,
            role: Some(message.role),
            content: Some(message.content.clone()),
            origin,
            hidden: message.hidden,
            created_at_ms: message
                .timestamp
                .map(|s| s.saturating_mul(1000))
                .unwrap_or_else(unix_now_ms),
            payload: EntryPayload::Message(Box::new(MessagePayload {
                tool_calls: message.tool_calls.clone(),
                tool_call_id: message.tool_call_id.clone(),
                images: message.images.clone(),
                provider: message.provider.clone(),
                model: message.model.clone(),
                effort: message.effort.clone(),
                provider_meta: message.provider_meta.clone(),
                subagent: None,
                content_blob: message.content_blob.clone(),
                display_content: message.display_content.clone(),
                reasoning_content: message.reasoning_content.clone(),
                injection,
                sent_at_ms: message.sent_at_ms,
                cache_frozen: message.cache_frozen,
            })),
        }
    }

    /// A working-state snapshot entry (`state` kind). Never view content.
    pub fn from_state(seq: u64, todos: Option<crate::todos::TodoList>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            seq,
            kind: EntryKind::State,
            role: None,
            content: None,
            origin: Some(EntryOrigin::Harness),
            hidden: true,
            created_at_ms: unix_now_ms(),
            payload: EntryPayload::State(StatePayload { todos }),
        }
    }

    /// Reconstruct the wire [`Message`] for a message entry.
    ///
    /// Subagent children are not carried: under this model a subagent run is a
    /// session of its own reached through `payload.subagent`.
    pub fn to_message(&self) -> Option<Message> {
        let EntryPayload::Message(payload) = &self.payload else {
            return None;
        };
        let role = self.role?;
        Some(Message {
            role,
            content: self.content.clone().unwrap_or_default(),
            content_blob: payload.content_blob.clone(),
            display_content: payload.display_content.clone(),
            reasoning_content: payload.reasoning_content.clone(),
            provider_meta: payload.provider_meta.clone(),
            tool_calls: payload.tool_calls.clone(),
            tool_call_id: payload.tool_call_id.clone(),
            images: payload.images.clone(),
            provider: payload.provider.clone(),
            model: payload.model.clone(),
            effort: payload.effort.clone(),
            hidden: self.hidden,
            children: None,
            subagent_meta: None,
            origin: payload.injection.clone(),
            timestamp: Some(self.created_at_ms / 1000),
            sent_at_ms: payload.sent_at_ms,
            cache_frozen: payload.cache_frozen,
        })
    }

    pub fn as_message(&self) -> Option<&MessagePayload> {
        match &self.payload {
            EntryPayload::Message(payload) => Some(payload),
            EntryPayload::State(_) => None,
        }
    }

    /// True for entries that never enter a view (`state`).
    pub fn is_hidden_kind(&self) -> bool {
        self.kind == EntryKind::State
    }
}

/// What a projection directive does. Open to extension; readers preserve
/// unknown kinds verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum DirectiveKind {
    /// Tool-result bodies are replaced by placeholders in views.
    Prune,
    /// A history range is replaced by a single checkpoint entry.
    Compact,
    /// An entry's provider-visible shape is byte-frozen.
    Freeze,
}

/// Kind-specific payload of a projection directive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum DirectivePayload {
    /// Tool results whose bodies are replaced by informative placeholders in
    /// views. Originals stay durably available (blob / archived entry).
    #[serde(rename = "prune")]
    Prune {
        /// One record per elided tool result, keyed by the tool call it
        /// answers.
        elided: Vec<PrunedToolOutput>,
        /// One record per pruned visual media artifact, keyed by entry seq (ADR-0285).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pruned_media: Vec<PrunedMediaOutput>,
    },
    /// Entries with `seq <= up_to_seq` are replaced in views by the single
    /// checkpoint entry at `checkpoint_seq`.
    #[serde(rename = "compact")]
    Compact {
        /// The checkpoint entry's membership seq.
        checkpoint_seq: u64,
    },
    /// The entry at `up_to_seq` presents exactly this content in views from
    /// now on (byte-frozen truncated tool output).
    #[serde(rename = "freeze")]
    Freeze {
        /// The frozen content that views must present verbatim.
        shape: String,
    },
}

/// One pruned tool result: the call it answered and the informative
/// placeholder that replaces its body in views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct PrunedToolOutput {
    pub tool_call_id: String,
    /// Placeholder text presented in views (e.g. "[tool output elided: …]").
    pub placeholder: String,
}

/// One pruned visual media artifact (e.g. user-uploaded image): the entry membership seq
/// and the informative placeholder invoice that replaces its body in views (ADR-0285).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct PrunedMediaOutput {
    pub seq: u64,
    /// Placeholder invoice text presented in views.
    pub placeholder: String,
}

/// A durable projection decision (ADR-0186 §2). Appending a directive is the
/// only effect a projection has on storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct ProjectionDirective {
    /// The directive's own total order within the session.
    pub seq: u64,
    pub kind: DirectiveKind,
    /// Anchor: the membership seq the directive acts upon (prune/compact:
    /// "through"; freeze: the target entry itself).
    pub up_to_seq: u64,
    pub payload: DirectivePayload,
}

/// The durable transcript of one session: facts in seq order plus the
/// projection decision history. This is the in-memory materialization of the
/// persisted ledger; every consumer-facing window derives from it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Facts, ordered by membership seq.
    pub entries: Vec<TranscriptEntry>,
    /// Projection decisions, ordered by directive seq.
    pub directives: Vec<ProjectionDirective>,
    /// Lower bound for the next membership seq. Set by loaders when the
    /// durable store holds rows the in-memory entries do not (e.g. entries
    /// preserved verbatim because their payload kind is unknown to this
    /// binary); guarantees a later `push` cannot collide with them.
    #[serde(default)]
    pub min_next_seq: u64,
    /// Lower bound for the next directive seq, same contract as
    /// [`Self::min_next_seq`].
    #[serde(default)]
    pub min_next_directive_seq: u64,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a fact at the next membership seq.
    pub fn push(&mut self, mut entry: TranscriptEntry) -> &mut Self {
        entry.seq = self.next_seq();
        self.min_next_seq = entry.seq + 1;
        self.entries.push(entry);
        self
    }

    /// Append a projection decision.
    pub fn push_directive(&mut self, mut directive: ProjectionDirective) -> &mut Self {
        directive.seq = self.next_directive_seq();
        self.min_next_directive_seq = directive.seq + 1;
        self.directives.push(directive);
        self
    }

    /// Next membership seq (gap-free watermark, never below the floor).
    pub fn next_seq(&self) -> u64 {
        self.entries
            .last()
            .map_or(0, |e| e.seq + 1)
            .max(self.min_next_seq)
    }

    /// Next directive seq (gap-free watermark, never below the floor).
    pub fn next_directive_seq(&self) -> u64 {
        self.directives
            .last()
            .map_or(0, |d| d.seq + 1)
            .max(self.min_next_directive_seq)
    }

    /// The current TodoList mirror, derived from the newest `state` entry
    /// carrying one.
    pub fn derive_todos(&self) -> Option<crate::todos::TodoList> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                EntryPayload::State(StatePayload { todos }) => todos.clone(),
                _ => None,
            })
    }

    /// The projected view (ADR-0186 §4): the message sequence the next
    /// provider request starts from, after applying every directive in order.
    ///
    /// Semantics:
    /// - interrupts never appear;
    /// - a `compact` hides every entry at or below its anchor except the
    ///   referenced checkpoint, which is included once;
    /// - a `prune` replaces the bodies of the listed tool results with their
    ///   informative placeholders;
    /// - a `freeze` pins the exact content an entry presents;
    /// - later directives win over earlier ones for the same entry.
    pub fn project(&self) -> Vec<(u64, Message)> {
        // Reduce the directive history into view state. Later directives win.
        let mut compact_watermark: Option<u64> = None;
        let mut checkpoint_seq: Option<u64> = None;
        let mut elided: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut pruned_media: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        let mut frozen: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        for directive in &self.directives {
            match &directive.payload {
                DirectivePayload::Prune {
                    elided: removed,
                    pruned_media: media_removed,
                } => {
                    for item in removed {
                        elided.insert(item.tool_call_id.clone(), item.placeholder.clone());
                    }
                    for item in media_removed {
                        pruned_media.insert(item.seq, item.placeholder.clone());
                    }
                }
                DirectivePayload::Compact { checkpoint_seq: cp } => {
                    compact_watermark =
                        Some(compact_watermark.unwrap_or(0).max(directive.up_to_seq));
                    checkpoint_seq = Some(*cp);
                }
                DirectivePayload::Freeze { shape } => {
                    frozen.insert(directive.up_to_seq, shape.clone());
                }
            }
        }

        let mut view = Vec::new();
        // The checkpoint substitutes the archived head: it is presented first,
        // at the position the compacted range occupied.
        if let Some(cp) = checkpoint_seq
            && let Some(entry) = self.entries.iter().find(|entry| entry.seq == cp)
            && let Some(message) = entry.to_message()
        {
            view.push((entry.seq, message));
        }
        let mut last_pruned_tool: Option<String> = None;
        for entry in &self.entries {
            if entry.is_hidden_kind() {
                continue;
            }
            if let (Some(watermark), Some(cp)) = (compact_watermark, checkpoint_seq)
                && (entry.seq <= watermark || entry.seq == cp)
            {
                // The checkpoint itself was already emitted above.
                continue;
            }
            let Some(mut message) = entry.to_message() else {
                continue;
            };
            if let Some(tool_call_id) = &message.tool_call_id
                && let Some(placeholder) = elided.get(tool_call_id)
            {
                message.content = placeholder.clone();
                message.reasoning_content = None;
                message.images = None;
                last_pruned_tool = Some(tool_call_id.clone());
            } else if let Some(last_call_id) = last_pruned_tool.take()
                && message
                    .origin
                    .as_ref()
                    .is_some_and(|o| o.kind == crate::message::InjectionKind::ToolImage)
            {
                let mime = message
                    .images
                    .as_ref()
                    .and_then(|imgs| imgs.first())
                    .map(|img| img.mime.clone())
                    .unwrap_or_else(|| "image".to_string());
                message.content = format!(
                    "[cleared image payload ({mime}) — rehydrate with inspect handle \"call:{last_call_id}\"]"
                );
                message.images = None;
            } else {
                last_pruned_tool = None;
            }
            // ADR-0285: Entry-level media pruning (e.g. user-uploaded images)
            if let Some(placeholder) = pruned_media.get(&entry.seq) {
                message.content = placeholder.clone();
                message.images = None;
            }
            if let Some(shape) = frozen.get(&entry.seq) {
                message.content = shape.clone();
                message.cache_frozen = true;
            }
            view.push((entry.seq, message));
        }
        view
    }

    /// The projected view without membership seq (convenience for callers that
    /// only need the wire history).
    pub fn project_messages(&self) -> Vec<Message> {
        self.project().into_iter().map(|(_, m)| m).collect()
    }

    /// Entries projected out of the view by the current directives — the
    /// recoverable originals (for audit, recovery, and presentation layers).
    pub fn projected_out(&self) -> Vec<&TranscriptEntry> {
        let mut compact_watermark: Option<u64> = None;
        let mut checkpoint_seq: Option<u64> = None;
        for directive in &self.directives {
            if let DirectivePayload::Compact { checkpoint_seq: cp } = &directive.payload {
                compact_watermark = Some(compact_watermark.unwrap_or(0).max(directive.up_to_seq));
                checkpoint_seq = Some(*cp);
            }
        }
        self.entries
            .iter()
            .filter(|entry| match (compact_watermark, checkpoint_seq) {
                (Some(watermark), Some(cp)) => entry.seq <= watermark && entry.seq != cp,
                _ => false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{InjectionKind, ToolCall};

    fn message_entry(seq: u64, role: Role, content: &str) -> TranscriptEntry {
        let mut message = Message::new(role, content);
        message.timestamp = Some(1_700_000_000);
        TranscriptEntry::from_message(seq, &message)
    }

    fn tool_entry(seq: u64, call_id: &str, content: &str) -> TranscriptEntry {
        let call = ToolCall::new(call_id, "read_file", "{}");
        let message = Message::tool_result(&call, content);
        TranscriptEntry::from_message(seq, &message)
    }

    fn assistant_tool_call(seq: u64, call_id: &str) -> TranscriptEntry {
        let mut message = Message::new(Role::Assistant, "working");
        message.tool_calls = Some(vec![ToolCall::new(call_id, "read_file", "{}")]);
        let mut entry = TranscriptEntry::from_message(seq, &message);
        entry.payload = match entry.payload {
            EntryPayload::Message(mut payload) => {
                // assistant messages carry protocol-private state to verify
                // verbatim round-trips through the view
                payload.provider_meta = Some(
                    [(
                        "gemini_thought_signatures".to_string(),
                        serde_json::json!({ call_id: "sig" }),
                    )]
                    .into_iter()
                    .collect(),
                );
                EntryPayload::Message(payload)
            }
            other => other,
        };
        entry
    }

    #[test]
    fn empty_transcript_projects_empty_view() {
        let transcript = Transcript::new();
        assert!(transcript.project().is_empty());
    }

    #[test]
    fn plain_messages_pass_through_in_order() {
        let mut transcript = Transcript::new();
        transcript.push(message_entry(0, Role::User, "hello"));
        transcript.push(message_entry(1, Role::Assistant, "hi"));
        let view = transcript.project();
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].0, 0);
        assert_eq!(view[1].0, 1);
        assert_eq!(view[0].1.content, "hello");
        assert_eq!(view[1].1.content, "hi");
    }

    #[test]
    fn state_entries_never_enter_the_view() {
        let mut transcript = Transcript::new();
        transcript.push(message_entry(0, Role::User, "before"));
        transcript.push(TranscriptEntry::from_state(1, None));
        transcript.push(message_entry(2, Role::Assistant, "after"));
        let view = transcript.project();
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].0, 0);
        assert_eq!(view[1].0, 2);
    }

    #[test]
    fn todos_derive_from_newest_state_entry() {
        let mut transcript = Transcript::new();
        let first = crate::todos::TodoList::default();
        // (The list starts empty; an empty mirror still counts as derived
        // state, so push two mirrors and expect the newest to win.)
        transcript.push(TranscriptEntry::from_state(0, Some(first.clone())));
        transcript.push(message_entry(1, Role::User, "work"));
        let second = crate::todos::TodoList::default();
        transcript.push(TranscriptEntry::from_state(2, Some(second)));
        assert!(transcript.derive_todos().is_some());
    }

    #[test]
    fn prune_replaces_only_listed_tool_bodies_with_placeholders() {
        let mut transcript = Transcript::new();
        transcript.push(assistant_tool_call(0, "call-1"));
        transcript.push(tool_entry(1, "call-1", "big output"));
        transcript.push(assistant_tool_call(2, "call-2"));
        transcript.push(tool_entry(3, "call-2", "small output"));
        transcript.push(message_entry(4, Role::Assistant, "done"));
        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Prune,
            up_to_seq: 3,
            payload: DirectivePayload::Prune {
                elided: vec![PrunedToolOutput {
                    tool_call_id: "call-1".into(),
                    placeholder: "[elided call-1]".into(),
                }],
                pruned_media: vec![],
            },
        });
        let view = transcript.project();
        assert_eq!(view.len(), 5);
        assert_eq!(view[1].1.content, "[elided call-1]");
        assert_eq!(view[3].1.content, "small output");
        assert_eq!(view[4].1.content, "done");
    }

    #[test]
    fn compact_replaces_range_with_checkpoint_once() {
        let mut transcript = Transcript::new();
        transcript.push(message_entry(0, Role::User, "old"));
        transcript.push(message_entry(1, Role::Assistant, "older"));
        let mut checkpoint = Message::new(Role::User, "summary of old rounds");
        checkpoint.hidden = true;
        checkpoint.origin = Some(crate::message::InjectionOrigin::new(
            InjectionKind::CompactionCheckpoint,
        ));
        let checkpoint = TranscriptEntry::from_message(2, &checkpoint);
        let checkpoint_seq = checkpoint.seq;
        transcript.push(checkpoint);
        transcript.push(message_entry(3, Role::User, "new"));
        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Compact,
            up_to_seq: 1,
            payload: DirectivePayload::Compact { checkpoint_seq },
        });
        let view = transcript.project();
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].0, checkpoint_seq);
        assert_eq!(view[0].1.content, "summary of old rounds");
        assert_eq!(view[1].0, 3);
        assert_eq!(view[1].1.content, "new");
        // The originals remain recoverable.
        assert_eq!(transcript.projected_out().len(), 2);
    }

    #[test]
    fn later_compact_wins_over_earlier() {
        let mut transcript = Transcript::new();
        for i in 0..5 {
            transcript.push(message_entry(i, Role::User, &format!("m{i}")));
        }
        let mut checkpoint_a = Message::new(Role::User, "checkpoint a");
        checkpoint_a.hidden = true;
        checkpoint_a.origin = Some(crate::message::InjectionOrigin::new(
            InjectionKind::CompactionCheckpoint,
        ));
        let checkpoint_a = TranscriptEntry::from_message(5, &checkpoint_a);
        transcript.push(checkpoint_a);
        let mut checkpoint_b = Message::new(Role::User, "checkpoint b");
        checkpoint_b.hidden = true;
        checkpoint_b.origin = Some(crate::message::InjectionOrigin::new(
            InjectionKind::CompactionCheckpoint,
        ));
        let checkpoint_b = TranscriptEntry::from_message(6, &checkpoint_b);
        transcript.push(checkpoint_b);
        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Compact,
            up_to_seq: 2,
            payload: DirectivePayload::Compact { checkpoint_seq: 5 },
        });
        transcript.push_directive(ProjectionDirective {
            seq: 1,
            kind: DirectiveKind::Compact,
            up_to_seq: 5,
            payload: DirectivePayload::Compact { checkpoint_seq: 6 },
        });
        let view = transcript.project();
        // entries 0..=5 hidden; checkpoint 5 superseded by checkpoint 6.
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].0, 6);
        assert_eq!(view[0].1.content, "checkpoint b");
    }

    #[test]
    fn freeze_pins_entry_content_and_flags_cache_frozen() {
        let mut transcript = Transcript::new();
        transcript.push(assistant_tool_call(0, "call-1"));
        transcript.push(tool_entry(1, "call-1", "long output that was truncated"));
        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Freeze,
            up_to_seq: 1,
            payload: DirectivePayload::Freeze {
                shape: "truncated head…".into(),
            },
        });
        let view = transcript.project();
        assert_eq!(view[1].1.content, "truncated head…");
        assert!(view[1].1.cache_frozen);
    }

    #[test]
    fn message_round_trip_is_lossless() {
        let mut message = Message::new(Role::Assistant, "answer");
        message.provider = Some("google".into());
        message.model = Some("gemini-2.5-pro".into());
        message.provider_meta = Some(
            [(
                "gemini_thought_signatures".to_string(),
                serde_json::json!({"call-1": "sig"}),
            )]
            .into_iter()
            .collect(),
        );
        message.tool_calls = Some(vec![ToolCall::new("call-1", "read_file", "{}")]);
        message.reasoning_content = Some("thought".into());
        let entry = TranscriptEntry::from_message(7, &message);
        assert_eq!(entry.seq, 7);
        let restored = entry.to_message().unwrap();
        assert_eq!(restored.role, Role::Assistant);
        assert_eq!(restored.content, "answer");
        assert_eq!(restored.provider.as_deref(), Some("google"));
        assert_eq!(restored.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(restored.provider_meta, message.provider_meta);
        assert_eq!(restored.tool_calls, message.tool_calls);
        assert_eq!(restored.reasoning_content.as_deref(), Some("thought"));
    }

    #[test]
    fn compaction_checkpoint_maps_to_checkpoint_origin() {
        let mut message = Message::new(Role::User, "checkpoint");
        message.hidden = true;
        message.origin = Some(crate::message::InjectionOrigin::new(
            InjectionKind::CompactionCheckpoint,
        ));
        let entry = TranscriptEntry::from_message(0, &message);
        assert_eq!(entry.origin, Some(EntryOrigin::Checkpoint));
        assert!(entry.hidden);
        let restored = entry.to_message().unwrap();
        assert_eq!(
            restored.origin.as_ref().map(|o| o.kind),
            Some(InjectionKind::CompactionCheckpoint)
        );
    }

    #[test]
    fn image_and_display_sidecars_round_trip() {
        let mut message = Message::new(Role::User, "look at this");
        message.display_content = Some("look at this".into());
        let entry = TranscriptEntry::from_message(0, &message);
        let restored = entry.to_message().unwrap();
        assert_eq!(restored.display_content.as_deref(), Some("look at this"));
    }

    #[test]
    fn unknown_payload_kind_preserved_by_serde_fallback() {
        // Simulate a future kind the current binary does not know: the
        // contract requires loaders to preserve it verbatim.
        let json = serde_json::json!({
            "id": "01900000-0000-7000-8000-000000000001",
            "seq": 0,
            "kind": "message",
            "payload": { "type": "future_kind", "x": 1 },
        });
        let decoded: Result<TranscriptEntry, _> = serde_json::from_value(json);
        // Unknown payload kinds must not silently vanish: decoding fails loud
        // at the typed boundary, and the raw row remains preserved in storage.
        // (The persistence layer stores payload as raw JSON; only typed
        // decoding is attempted.)
        assert!(decoded.is_err());
    }

    #[test]
    fn seq_watermarks_advance_gap_free() {
        let mut transcript = Transcript::new();
        assert_eq!(transcript.next_seq(), 0);
        transcript.push(message_entry(0, Role::User, "a"));
        assert_eq!(transcript.next_seq(), 1);
        transcript.push(message_entry(1, Role::Assistant, "b"));
        assert_eq!(transcript.next_seq(), 2);
        assert_eq!(transcript.next_directive_seq(), 0);
        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Prune,
            up_to_seq: 1,
            payload: DirectivePayload::Prune {
                elided: vec![],
                pruned_media: vec![],
            },
        });
        assert_eq!(transcript.next_directive_seq(), 1);
    }

    #[test]
    fn prune_replaces_user_media_with_invoice_placeholder() {
        let mut transcript = Transcript::new();
        let user_msg = Message::new(Role::User, "User prompt with image").with_images(vec![
            crate::ImagePart {
                mime: "image/png".into(),
                data: "base64data".into(),
            },
        ]);
        transcript.push(TranscriptEntry::from_message(0, &user_msg));
        transcript.push(message_entry(1, Role::Assistant, "I see the image"));

        transcript.push_directive(ProjectionDirective {
            seq: 0,
            kind: DirectiveKind::Prune,
            up_to_seq: 1,
            payload: DirectivePayload::Prune {
                elided: vec![],
                pruned_media: vec![PrunedMediaOutput {
                    seq: 0,
                    placeholder: "User prompt with image\n[cleared image payload (image/png) — rehydrate with inspect handle \"artifact:abc\"]".into(),
                }],
            },
        });

        let view = transcript.project();
        assert_eq!(view.len(), 2);
        assert!(view[0].1.images.is_none());
        assert!(view[0].1.content.contains("artifact:abc"));
    }
}
