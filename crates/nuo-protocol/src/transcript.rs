//! Single-transcript persistence model.
//!
//! The durable session stores **facts** (`TranscriptEntry`) and **decisions**
//! (`ProjectionDirective`); every consumer-facing window is a pure `derive`
//! over the two. Entries are immutable and position-free facts; position lives
//! in the per-session membership (seq). Projections never rewrite entries —
//! they append a directive and, for compaction, one checkpoint entry.
use nuo_tool::todos::unix_now;
use nuo_tool::{ImagePart, InjectionKind, InjectionOrigin, Message, Role, ToolCall};
use serde::{Deserialize, Serialize};

/// Unix-epoch milliseconds now.
fn unix_now_ms() -> u64 {
    unix_now().saturating_mul(1000)
}

/// Rendered prefix of a compaction checkpoint.
///
/// Owned by the protocol because three surfaces must agree on it: the
/// transcript projection, the IR's active-window rendering, and the legacy
/// selector that strips a prior summary. The structure lives in
/// [`CompactionMeta`]; this is only the model-facing envelope.
pub const CHECKPOINT_HEADER: &str = "[Conversation checkpoint]\n\
     Earlier complete rounds were compacted. Treat this as durable context, not a new user request.\n\n";

/// Build the model-facing checkpoint message for `summary`.
pub fn checkpoint_message(summary: &str) -> Message {
    Message::injected(
        Role::User,
        format!("{CHECKPOINT_HEADER}{summary}"),
        InjectionOrigin::new(InjectionKind::CompactionCheckpoint),
    )
}

/// What an entry is. Open enum: new kinds extend the payload contract; readers
/// must preserve unknown kinds verbatim (unknown deserialises as `Unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryOrigin {
    /// Program-injected content (system reminders, steering notes, ...).
    Harness,
    /// Compaction checkpoint: stands in for an elided history range and is
    /// referenced by a `compact` projection directive.
    Checkpoint,
}

/// Reference to the dedicated session that durably records a subagent run
/// Replaces inline nested transcripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
/// envelope column (see).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Provider-opaque replay sidecar — the transport facts a vendor requires to
    /// reconstruct a prior turn (on `GoogleGenerateContent`, the thought
    /// signatures carried on `text` / `functionCall` parts and the wire identity
    /// that minted them; on `AnthropicMessages`, the thinking-block `signature`).
    ///
    /// Key names and their semantics are owned *entirely* by the provider that
    /// writes them. Never inspected here, never interpreted here, and never
    /// extended with a provider-specific accessor: the moment such a helper lands
    /// on this crate, this field stops being opaque and the protocol layer
    /// inherits a wire contract whose key constants it cannot even see (they live
    /// in the provider). It round-trips verbatim through every projection so a
    /// resumed session replays exactly what it recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
}

/// Kind-specific payload of an entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryPayload {
    /// A transcript message. Boxed: the payload dominates the entry size and
    /// entries are cloned wholesale on every turn commit.
    #[serde(rename = "message")]
    Message(Box<MessagePayload>),
    /// A working-state snapshot (e.g. the TodoList mirror).
    #[serde(rename = "state")]
    State(StatePayload),
    /// A durable compaction fact.
    ///
    /// The rendered checkpoint text lives in the entry's `content`; this
    /// payload carries the structure the causal graph must be able to rebuild
    /// without any other store: which tail was kept, which node the horizon
    /// hangs from, and where the archived range ends. The entry *is* the fold —
    /// `causal_nodes` is a derive of it (ADR-0160).
    #[serde(rename = "compaction")]
    Compaction(Box<CompactionMeta>),
}

/// Structural facts of one compaction, stored inside its checkpoint entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionMeta {
    /// ID of the first causal node preserved after the fold. The horizon walk
    /// anchors here when it injects the checkpoint.
    pub first_kept_node_id: String,
    /// ID of the last folded node: the horizon node's parent. It is a sibling
    /// anchor, not an ancestor of the kept tail (`INV-CHK-01`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_parent_node_id: Option<String>,
    /// Membership seq of the last folded entry. The `compact` directive hides
    /// entries through here (snapped down to an existing entry).
    pub folded_through_seq: u64,
    /// Token weight of the folded prefix, for the checkpoint card.
    pub tokens_before: usize,
    /// Files the folded range read / modified, carried into the next summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,
}

/// Working-state snapshot carried by a `state` entry. Fields are additive;
/// consumers derive the current state from the newest `state` entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatePayload {
    /// The unified task-list mirror at the time of the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<nuo_tool::todos::TodoList>,
}

/// An immutable transcript fact. `seq` is the entry's position
/// **in one session** (materialized from `entry_memberships`); the same entry
/// shared across a fork carries the same id with per-session seq values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            Some(nuo_tool::InjectionKind::CompactionCheckpoint) => Some(EntryOrigin::Checkpoint),
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
            })),
        }
    }

    /// A working-state snapshot entry (`state` kind). Never view content.
    pub fn from_state(seq: u64, todos: Option<nuo_tool::todos::TodoList>) -> Self {
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
        let role = self.role?;
        match &self.payload {
            EntryPayload::Message(payload) => Some(Message {
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
            }),
            EntryPayload::Compaction(_) => {
                let mut message = checkpoint_message(self.content.as_deref().unwrap_or_default());
                message.hidden = self.hidden;
                message.timestamp = Some(self.created_at_ms / 1000);
                Some(message)
            }
            EntryPayload::State(_) => None,
        }
    }

    pub fn as_message(&self) -> Option<&MessagePayload> {
        match &self.payload {
            EntryPayload::Message(payload) => Some(payload),
            EntryPayload::Compaction(_) | EntryPayload::State(_) => None,
        }
    }

    /// True for entries that never enter a view (`state`).
    pub fn is_hidden_kind(&self) -> bool {
        self.kind == EntryKind::State
    }
}

/// What a projection directive does. Open to extension; readers preserve
/// unknown kinds verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveKind {
    /// Tool-result bodies are replaced by placeholders in views.
    Prune,
    /// A history range is replaced by a single checkpoint entry.
    Compact,
}

/// Kind-specific payload of a projection directive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DirectivePayload {
    /// Tool results whose bodies are replaced by informative placeholders in
    /// views. Originals stay durably available (blob / archived entry).
    #[serde(rename = "prune")]
    Prune {
        /// One record per elided tool result, keyed by the tool call it
        /// answers.
        elided: Vec<PrunedToolOutput>,
        /// One record per pruned visual media artifact, keyed by entry seq.
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
}

/// One pruned tool result: the call it answered and the informative
/// placeholder that replaces its body in views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedToolOutput {
    pub tool_call_id: String,
    /// Placeholder text presented in views (e.g. "[tool output elided: …]").
    pub placeholder: String,
}

/// One pruned visual media artifact (e.g. user-uploaded image): the entry membership seq
/// and the informative placeholder invoice that replaces its body in views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedMediaOutput {
    pub seq: u64,
    /// Placeholder invoice text presented in views.
    pub placeholder: String,
}

/// A durable projection decision. Appending a directive is the
/// only effect a projection has on storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub fn derive_todos(&self) -> Option<nuo_tool::todos::TodoList> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                EntryPayload::State(StatePayload { todos }) => todos.clone(),
                _ => None,
            })
    }

    /// The projected view: the message sequence the next
    /// provider request starts from, after applying every directive in order.
    ///
    /// Semantics:
    /// - interrupts never appear;
    /// - a `compact` hides every entry at or below its anchor except the
    ///   referenced checkpoint, which is included once;
    /// - a `prune` replaces the bodies of the listed tool results with their
    ///   informative placeholders;
    /// - later directives win over earlier ones for the same entry.
    pub fn project(&self) -> Vec<(u64, Message)> {
        // Reduce the directive history into view state. Later directives win.
        let mut compact_watermark: Option<u64> = None;
        let mut checkpoint_seq: Option<u64> = None;
        let mut elided: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut pruned_media: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
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
            } else if let Some(_last_call_id) = last_pruned_tool.take()
                && message
                    .origin
                    .as_ref()
                    .is_some_and(|o| o.kind == nuo_tool::InjectionKind::ToolImage)
            {
                let mime = message
                    .images
                    .as_ref()
                    .and_then(|imgs| imgs.first())
                    .map(|img| img.mime.clone())
                    .unwrap_or_else(|| "image".to_string());
                message.content =
                    format!("[cleared image payload ({mime}) to protect context budget]");
                message.images = None;
            } else {
                last_pruned_tool = None;
            }
            // Entry-level media pruning (e.g. user-uploaded images)
            if let Some(placeholder) = pruned_media.get(&entry.seq) {
                message.content = placeholder.clone();
                message.images = None;
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
mod tests;
