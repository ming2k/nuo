use super::*;

/// Retract every call this attempt announced that no `ToolCall` discharged.
///
/// ADR-0166: the retraction names the **call ids**, because that is the only
/// identity a frontend rows by. An empty set emits nothing, so the normal path
/// carries no no-op wire event.
fn retract_announced_calls<F: FnMut(AgentEvent)>(
    announced_slots: &mut std::collections::BTreeSet<usize>,
    calls: &[ToolCall],
    on_event: &mut F,
) {
    if announced_slots.is_empty() {
        return;
    }
    let slots = std::mem::take(announced_slots);
    let ids: Vec<String> = slots
        .into_iter()
        .filter_map(|slot| calls.get(slot).map(|call| call.id.clone()))
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return;
    }
    on_event(AgentEvent::ToolCallsRetracted { ids });
}

/// A stream-loop cutoff is recoverable once per user round. A second cutoff
/// ends generation so a model that ignores the guidance cannot consume the
/// context indefinitely.
const MAX_STREAM_LOOP_RECOVERIES: u8 = 1;
const STREAM_LOOP_TURN_FIELD_CHARS: usize = 24_000;
const STREAM_LOOP_PRECEDING_CONTEXT_CHARS: usize = 12_000;
/// Bound on the tool-call argument tail carried as loop-review evidence.
const TOOL_INPUT_EVIDENCE_CHARS: usize = 24_000;

/// Append `fragment` to `buf`, keeping only the last `cap` bytes. Loop-review
/// evidence is a tail, not an archive; a multi-megabyte `write_file` payload
/// must not be duplicated in memory just to be reviewed.
fn push_bounded_tail(buf: &mut String, fragment: &str, cap: usize) {
    if fragment.is_empty() {
        return;
    }
    buf.push_str(fragment);
    let excess = buf.len().saturating_sub(cap);
    if excess > 0 {
        let mut cut = excess;
        while cut < buf.len() && !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.drain(..cut);
    }
}

#[derive(Debug, Clone)]
struct StreamLoopIncident {
    pattern: nuo_guardrails::stream_loop_detector::DegeneratePattern,
    channel: nuo_protocol::StreamLoopChannel,
}

/// An in-flight Stream Sentinel review spawned off the hot stream loop.
/// Carrying the candidate's identity lets the verdict be applied — or
/// memo-cleared — whenever it lands: mid-stream at the next event boundary,
/// or after the stream ends naturally.
struct PendingStreamLoopReview {
    candidate_key: String,
    channel: nuo_protocol::StreamLoopChannel,
    pattern: nuo_guardrails::stream_loop_detector::DegeneratePattern,
    verdict_rx: tokio::sync::oneshot::Receiver<nuo_protocol::StreamLoopVerdict>,
}

/// Memo key identifying an L1 candidate: its channel and pattern identity.
fn stream_loop_candidate_key(
    channel: nuo_protocol::StreamLoopChannel,
    pattern: &nuo_guardrails::stream_loop_detector::DegeneratePattern,
) -> String {
    format!("{channel:?}:{pattern:?}")
}

/// Keep the beginning and live tail of a large field. Most turns fit in full;
/// the bound prevents one diagnostic call from exceeding the cognitive model's
/// context on an already-pathological stream. The explicit omission marker is
/// evidence too: the reviewer knows it received a projection, not a seamless
/// string.
fn project_stream_loop_field(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let head_chars = max_chars / 3;
    let tail_chars = max_chars.saturating_sub(head_chars);
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}\n\n[... {} characters omitted by harness projection ...]\n\n{tail}",
        char_count.saturating_sub(max_chars)
    )
}

/// Render the immediate conversational evidence that makes repeated output
/// meaningful: the latest user request, assistant/tool observations, and
/// command output. System and hidden control messages are excluded so a
/// reviewer judges task content rather than harness instructions.
fn stream_loop_preceding_context(messages: &[Message]) -> String {
    let mut blocks = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        if message.hidden || message.role == Role::System || message.content.trim().is_empty() {
            continue;
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::System => continue,
        };
        let remaining = STREAM_LOOP_PRECEDING_CONTEXT_CHARS.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let body = project_stream_loop_field(message.content.trim(), remaining.min(6_000));
        used = used.saturating_add(body.chars().count() + role.len() + 2);
        blocks.push(format!("{role}: {body}"));
    }
    blocks.reverse();
    blocks.join("\n\n")
}

pub(crate) struct ToolResultRecord<'a> {
    pub call: &'a ToolCall,
    pub result: &'a ToolOutput,
    pub duration_ms: u64,
    pub checkpoint_replay: bool,
    pub emit_event: bool,
}

mod lifecycle;
mod stream_loop;
mod tools;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use self::tools::format_tool_result_message;
