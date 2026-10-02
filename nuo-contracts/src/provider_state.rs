//! Conversation revisions and provider continuation state.
//!
//! These values make remote response chains explicit session data. Provider
//! instances remain stateless transports and can therefore be shared safely by
//! forks, subagents, retries, and concurrently active sessions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONTINUATION_ARTIFACT_KEY: &str = "muta.continuation";
pub const OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY: &str = "openai.response.output";
pub const OPENAI_RESPONSE_ID_ARTIFACT_KEY: &str = "openai.response.id";

/// Conversation delivery strategy implemented by a provider route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationMode {
    FullReplay,
    RemoteStored,
    OpaqueReplay,
}

/// Version of the semantic conversation graph visible to a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRevision {
    pub sequence: u64,
    pub head: Option<String>,
}

impl ContextRevision {
    pub const fn empty() -> Self {
        Self {
            sequence: 0,
            head: None,
        }
    }
}

/// Version of request-scoped instructions, tool declarations, and controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeRevision {
    pub sequence: u64,
    pub fingerprint: String,
}

impl EnvelopeRevision {
    pub fn ephemeral() -> Self {
        Self {
            sequence: 0,
            fingerprint: String::new(),
        }
    }
}

/// Relationship between the current context and the prior provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRelation {
    Initial,
    AppendOnly,
    Rewritten,
    Truncated,
    Projected,
    Forked,
}

/// Opaque stable key for one endpoint/model/account/protocol/storage route.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RouteFingerprint(pub String);

/// How this immutable request carries conversation state on the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RequestDelivery {
    #[default]
    FullReplay,
    RemoteContinuation {
        previous_response_id: String,
        /// Index in [`crate::ModelRequest::messages`] where new input begins.
        input_start: usize,
    },
    /// Replay the complete local context, preferring provider-owned output
    /// items attached to assistant nodes over lossy neutral reconstruction.
    OpaqueReplay,
}

/// A server-side response anchor valid for one exact local graph node/route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationCursor {
    pub route: RouteFingerprint,
    pub local_head: String,
    pub response_id: String,
}

/// Why a previously usable cursor can no longer be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorInvalidationReason {
    ContextRewritten,
    ContextProjected,
    ContextTruncated,
    RouteChanged,
    StorageDisabled,
    RemoteExpired,
    RemoteRejected,
}

/// Route-scoped continuation state persisted by the conversation owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderCursorState {
    Unsupported,
    Empty,
    Ready {
        cursor: ContinuationCursor,
    },
    Stale {
        prior: ContinuationCursor,
        reason: CursorInvalidationReason,
    },
}

/// Provider-private replay material attached to the assistant graph node.
pub type ProviderArtifacts = serde_json::Map<String, serde_json::Value>;

/// Stable semantic fingerprint of the provider-visible conversation. System
/// instructions and provider-private metadata are intentionally excluded:
/// instructions are a request envelope, while artifacts are credentials for
/// replaying the semantic node rather than part of its meaning.
#[allow(clippy::expect_used)] // `serde_json::Value` serialization is structurally infallible.
pub fn semantic_context_head<'a>(messages: impl IntoIterator<Item = &'a crate::Message>) -> String {
    let mut digest = Sha256::new();
    for message in messages {
        update_semantic_message(&mut digest, message);
    }
    format!("sha256:{:x}", digest.finalize())
}

/// Fold one message's semantic content (role, text, tool trace, images) into a
/// digest. Instructions and provider-private metadata are excluded; see
/// [`semantic_context_head`].
#[allow(clippy::expect_used)] // `serde_json::Value` serialization is structurally infallible.
fn update_semantic_message(digest: &mut Sha256, message: &crate::Message) {
    if message.role == crate::Role::System {
        return;
    }
    let semantic = serde_json::json!({
        "role": message.role,
        "content": message.content,
        "content_blob": message.content_blob,
        "reasoning_content": message.reasoning_content,
        "tool_calls": message.tool_calls,
        "tool_call_id": message.tool_call_id,
        "images": message.images,
    });
    digest.update(serde_json::to_vec(&semantic).expect("semantic message serializes"));
    digest.update([0xff]);
}

/// Stable identity of the cacheable request prefix `S | H | I` (ADR-0217): the
/// instruction slices, the deterministically ordered tool declarations, and the
/// semantic conversation messages. Request-local temporary context (`E`) is
/// deliberately absent because callers pass only `messages`, never
/// `temporary_context`.
#[allow(clippy::expect_used)] // Tool declarations contain no fallible custom serializers.
pub fn request_prefix_fingerprint(
    instructions: &crate::InstructionBundle,
    messages: &[crate::Message],
    tools: &[crate::ToolSpec],
) -> String {
    let mut digest = Sha256::new();
    for slice in &instructions.slices {
        digest.update(slice.id.as_bytes());
        digest.update([slice.tier as u8]);
        digest.update(slice.content.as_bytes());
        digest.update([0xff]);
    }
    digest.update(serde_json::to_vec(tools).expect("tool declarations serialize"));
    digest.update([0xfe]);
    for message in messages {
        update_semantic_message(&mut digest, message);
    }
    format!("sha256:{:x}", digest.finalize())
}

#[allow(clippy::expect_used)] // Tool declarations contain no fallible custom serializers.
pub fn request_envelope_fingerprint(
    instructions: &crate::InstructionBundle,
    messages: &[crate::Message],
    tools: &[crate::ToolSpec],
) -> String {
    let mut digest = Sha256::new();
    for slice in &instructions.slices {
        digest.update(slice.id.as_bytes());
        digest.update([slice.tier as u8]);
        digest.update(slice.content.as_bytes());
        digest.update([0xff]);
    }
    for message in messages
        .iter()
        .filter(|message| message.role == crate::Role::System)
    {
        digest.update(message.content.as_bytes());
        digest.update([0xff]);
    }
    digest.update(serde_json::to_vec(tools).expect("tool declarations serialize"));
    format!("sha256:{:x}", digest.finalize())
}

/// Select a delivery plan without consulting mutable provider state. A remote
/// cursor is usable only when it belongs to the same route and its semantic
/// anchor still matches the exact local assistant prefix.
pub fn select_request_delivery(
    messages: &[crate::Message],
    route: &RouteFingerprint,
    mode: ContinuationMode,
) -> (RequestDelivery, ContextRelation) {
    match mode {
        ContinuationMode::FullReplay => (RequestDelivery::FullReplay, ContextRelation::Initial),
        ContinuationMode::OpaqueReplay => {
            (RequestDelivery::OpaqueReplay, ContextRelation::AppendOnly)
        }
        ContinuationMode::RemoteStored => {
            let mut saw_cursor = false;
            for (index, message) in messages.iter().enumerate().rev() {
                let Some(cursor) = read_continuation_cursor(message) else {
                    continue;
                };
                saw_cursor = true;
                if &cursor.route != route {
                    continue;
                }
                let actual_head = semantic_context_head(messages[..=index].iter());
                if actual_head == cursor.local_head {
                    return (
                        RequestDelivery::RemoteContinuation {
                            previous_response_id: cursor.response_id,
                            input_start: index + 1,
                        },
                        ContextRelation::AppendOnly,
                    );
                }
            }
            (
                RequestDelivery::FullReplay,
                if saw_cursor {
                    ContextRelation::Rewritten
                } else {
                    ContextRelation::Initial
                },
            )
        }
    }
}

pub fn read_continuation_cursor(message: &crate::Message) -> Option<ContinuationCursor> {
    serde_json::from_value(
        message
            .provider_meta
            .as_ref()?
            .get(CONTINUATION_ARTIFACT_KEY)?
            .clone(),
    )
    .ok()
}

#[allow(clippy::expect_used)] // The cursor contains only strings and has no fallible serializer.
pub fn write_continuation_cursor(artifacts: &mut ProviderArtifacts, cursor: &ContinuationCursor) {
    artifacts.insert(
        CONTINUATION_ARTIFACT_KEY.to_string(),
        serde_json::to_value(cursor).expect("continuation cursor serializes"),
    );
}

/// Metadata that becomes valid only when a response completed normally.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCompletionMeta {
    pub usage: Option<crate::TokenUsage>,
    pub artifacts: Option<ProviderArtifacts>,
    pub continuation: Option<ContinuationCursor>,
}

/// Non-streaming provider result. Nothing is retrieved through shared mutable
/// "last response" state after this value is returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCompletion {
    pub message: crate::Message,
    pub meta: ProviderCompletionMeta,
}

impl ProviderCompletion {
    pub fn message(message: crate::Message) -> Self {
        Self {
            message,
            meta: ProviderCompletionMeta::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_scoped_to_route_and_graph_head() {
        let cursor = ContinuationCursor {
            route: RouteFingerprint("openai:responses:gpt-5.6:stored".into()),
            local_head: "node-7".into(),
            response_id: "resp_7".into(),
        };
        let state = ProviderCursorState::Ready {
            cursor: cursor.clone(),
        };
        assert_eq!(state, ProviderCursorState::Ready { cursor });
    }
}
