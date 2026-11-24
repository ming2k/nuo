//! Translation between the harness's persistent [`Message`] stream and the
//! TUI's semantic [`TranscriptMessage`] document model. Also hosts the small
//! parsing helpers for the textual `` `[<tool> result]`: `` envelope and the
//! matching `` `Calling \`<tool>\`` `` formatter used when no display content
//! is available for a restored assistant turn.
//!
//! [`Message`]: tool::Message
use std::collections::HashMap;
use std::collections::VecDeque;

use tool::{Message, Role};

use crate::shell::step_interaction;
use view::disclosure_config::DisclosureSource;
use view::model::document::{TranscriptMessage, UserMessageOrigin};

pub(super) fn transcript_message_from_core(message: Message) -> Option<TranscriptMessage> {
    // A compaction checkpoint is classified by its provenance and read through
    // the one envelope reader; the bytes are not a classifier (ADR-0232,
    // ADR-0226). A checkpoint is hidden from the ordinary message stream but
    // still owns a card, so this check precedes the hidden filter.
    if message
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == tool::InjectionKind::CompactionCheckpoint)
        && let Some((envelope, body)) = tool::parse_envelope_with_body(&message.content)
        && envelope.tag == tool::Tag::Checkpoint
    {
        let mut card =
            TranscriptMessage::compacted_card(0, 0, 0, Some(body.trim().to_string()), Vec::new());
        card.provider = message.provider;
        card.model = message.model;
        card.sent_at_ms = message.sent_at_ms.or_else(|| {
            message
                .timestamp
                .map(|seconds| seconds.saturating_mul(1000))
        });
        return Some(card);
    }
    if message.hidden {
        return None;
    }
    if message.role == Role::System {
        return None;
    }
    let provider = message.provider.clone();
    let model = message.model.clone();
    let sent_at_ms = message.sent_at_ms.or_else(|| {
        message
            .timestamp
            .map(|seconds| seconds.saturating_mul(1000))
    });
    let is_insert = message
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == tool::InjectionKind::UserSteer);
    let content = if let Some(display_content) = message.display_content {
        display_content
    } else if message.content.is_empty() {
        message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|call| format_tool_call(&call.name, &call.arguments))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        message.content
    };
    if content.is_empty() {
        None
    } else {
        let mut msg = TranscriptMessage::new(message.role, content);
        msg.provider = provider;
        msg.model = model;
        msg.effort = message
            .effort
            .clone()
            .filter(|effort| !effort.is_empty() && !effort.eq_ignore_ascii_case("none"));
        msg.sent_at_ms = sent_at_ms;
        msg.injection_origin = message.origin;
        // Infer the turn origin for restored user messages. There is no slash
        // case: a command is a ledger row, never a message, so nothing to
        // classify here (ADR-0233).
        if message.role == Role::User && is_insert {
            msg.origin = UserMessageOrigin::Steer;
            // ADR-0137: durable steers were persisted only at the turn seam, so
            // every restored steer is an in-round interjection. The render tag is
            // rebuilt here so any consumer of the restored document sees the
            // one-body-row form.
            msg.origin_tags = vec![view::model::document::SteerOriginTag::InRound];
        }
        Some(msg)
    }
}

pub(super) fn transcript_messages_from_core(
    messages: Vec<Message>,
    config: &impl DisclosureSource,
) -> Vec<TranscriptMessage> {
    transcript_from_core_inner(messages, config, true)
}

/// Rebuild the round-interrupt marker rows of the durable record list (C11)
/// into the TUI document model: one compact warning entry per stopped round,
/// placed at its timestamp seam by [`merge_round_interrupt_rows`]. Like the
/// command ledger, the records are the source of truth on resume — the
/// message stream is pure dialogue — and each row is stamped with the
/// record's `at_ms` (via `sent_at_ms`) as its seam anchor.
pub(super) fn transcript_interrupts_from_records(
    records: Vec<protocol::RoundInterrupt>,
) -> Vec<TranscriptMessage> {
    records
        .into_iter()
        .map(|record| {
            let at_ms = record.at_ms;
            TranscriptMessage::round_interrupted(record).with_sent_at_ms(at_ms)
        })
        .collect()
}

/// Whether `message` is an assistant turn component (mirrors
/// `view::layout::is_turn_component`), used so timeline merging never splits
/// the components of a single `(round, turn)` band.
fn is_turn_component(message: &TranscriptMessage) -> bool {
    message.is_tool_step()
        || message.is_subagent_task()
        || message.is_reasoning()
        || message.role == Role::Assistant
}

/// Merge timestamped out-of-band rows (commands and/or round interrupts) into
/// a restored dialogue transcript at their chronological turn seams.
///
/// Ordering rule, stable and total:
///
/// 1. Sidecar rows are stably sorted by `sent_at_ms` ascending, with
///    un-timestamped rows (`None`) placed last so they never block timestamped
///    rows from reaching earlier seams.
/// 2. Each timestamped row lands **before the first valid timeline seam whose
///    timestamp is `>=` the row's timestamp** — i.e. before a user message,
///    compaction card, or the start of a new assistant turn, never inside one
///    turn's components.
/// 3. Rows newer than every seam, or carrying no timestamp, land at the tail
///    in chronological (or original) order.
fn merge_sidecar_rows(
    dialogue: Vec<TranscriptMessage>,
    mut rows: Vec<TranscriptMessage>,
) -> Vec<TranscriptMessage> {
    if rows.is_empty() {
        return dialogue;
    }
    rows.sort_by(|a, b| match (a.sent_at_ms, b.sent_at_ms) {
        (Some(a_ms), Some(b_ms)) => a_ms.cmp(&b_ms),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    let mut out = Vec::with_capacity(dialogue.len() + rows.len());
    let mut rows = rows.into_iter().peekable();
    let mut prev_turn_key: Option<(Option<u64>, Option<u64>)> = None;

    for message in dialogue {
        let curr_turn_key = is_turn_component(&message).then_some((message.round, message.turn));
        let same_turn = matches!(
            (prev_turn_key, curr_turn_key),
            (Some(prev), Some(curr)) if curr.1.is_some() && prev == curr
        );
        if !same_turn && let Some(seam_ms) = message.sent_at_ms {
            while let Some(item) = rows.peek() {
                if item.sent_at_ms.is_some_and(|ms| ms <= seam_ms) {
                    #[allow(clippy::expect_used)]
                    out.push(rows.next().expect("peeked timeline row exists"));
                } else {
                    break;
                }
            }
        }
        prev_turn_key = curr_turn_key;
        out.push(message);
    }
    // Rows after the last seam (or with no timestamp) land at the tail.
    out.extend(rows);
    out
}

/// Merge rebuilt command and round-interrupt rows together into a restored
/// dialogue transcript in a single chronological pass.
///
/// Merging both ledgers in one pass guarantees that commands and interrupts
/// occurring at the same seam (or at the tail) interleave by timestamp rather
/// than grouping all commands before all interrupts. On an exact-millisecond
/// tie, interrupts precede commands (e.g. a round error precedes `/retry`).
pub(super) fn merge_timeline_rows(
    dialogue: Vec<TranscriptMessage>,
    commands: Vec<TranscriptMessage>,
    interrupts: Vec<TranscriptMessage>,
) -> Vec<TranscriptMessage> {
    if commands.is_empty() && interrupts.is_empty() {
        return dialogue;
    }
    let mut combined = Vec::with_capacity(interrupts.len() + commands.len());
    combined.extend(interrupts);
    combined.extend(commands);
    merge_sidecar_rows(dialogue, combined)
}

/// Merge rebuilt round-interrupt rows into a restored transcript (C11) at
/// their chronological turn seams.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn merge_round_interrupt_rows(
    dialogue: Vec<TranscriptMessage>,
    interrupts: Vec<TranscriptMessage>,
) -> Vec<TranscriptMessage> {
    merge_sidecar_rows(dialogue, interrupts)
}

/// Rebuild the act rows of the durable command ledger into the
/// TUI document model: one compact, dimmed, non-conversational row per act, with
/// the typed result as its expandable body. The ledger is the source of truth for
/// acts on resume — the message stream is pure dialogue, and a *reading* has no
/// row here at all (ADR-0140: a measurement of the world is a receipt, never a
/// statement that stays true). Rows carry no round/turn position (a command is
/// not a turn); the caller rebases round positions over the returned slice as
/// usual.
///
/// Each row is stamped with the record's invocation `timestamp` (via
/// `sent_at_ms`) so [`merge_command_rows`] can place it at its turn seam; the
/// stamp is projection-only state and never reaches the ledger or the model.
pub(super) fn transcript_commands_from_ledger(
    commands: Vec<protocol::CommandRecord>,
) -> Vec<TranscriptMessage> {
    commands
        .into_iter()
        .map(|record| {
            TranscriptMessage::command_result(record.name, record.args, record.result)
                .with_sent_at_ms(record.timestamp)
        })
        .collect()
}

/// Merge rebuilt command rows into a restored dialogue transcript at their
/// chronological turn seams.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn merge_command_rows(
    dialogue: Vec<TranscriptMessage>,
    commands: Vec<TranscriptMessage>,
) -> Vec<TranscriptMessage> {
    merge_sidecar_rows(dialogue, commands)
}

/// Rebuild the nested transcript of a subagent run (the `children` carried by a
/// Tool-role message) into the TUI document model. Mirrors the live
/// event-driven build — assistant text, reasoning traces, and nested tool
/// steps with their own results — and recurses for arbitrarily deep subagent
/// trees, so the drill-in subagent view works after a resume.
fn transcript_children_from_core(
    messages: Vec<Message>,
    config: &impl DisclosureSource,
) -> Vec<TranscriptMessage> {
    transcript_from_core_inner(messages, config, false)
}

/// Shared restore engine. With `track_rounds` the caller gets the canonical
/// Round → Turn reconstruction used for the top-level transcript; with it off
/// (subagent children) round/turn attribution is skipped, since nested transcripts
/// render inside their parent step rather than against the session's global
/// round counter.
fn transcript_from_core_inner(
    messages: Vec<Message>,
    config: &impl DisclosureSource,
    track_rounds: bool,
) -> Vec<TranscriptMessage> {
    let mut restored = Vec::new();
    // Reconstruct the canonical Round -> Turn position from the durable
    // transcript. A driving visible user message opens a round; every
    // assistant response advances the ReAct turn within it. Steering inserts
    // do not open rounds.
    let mut restored_round: u64 = 0;
    let mut restored_turn: u64 = 0;
    // ADR-0166 `[INV-TOOL-ID-01]`: a restored step carries the call's real id.
    // A typed transcript carries it on both halves — the assistant
    // `tool_calls[].id` and the tool message's `tool_call_id` — so pairing is
    // by identity, O(1).
    let mut pending_by_id: HashMap<String, usize> = HashMap::new();
    // The per-name FIFO remains **only** for a result whose persisted
    // `tool_call_id` is absent or empty: that shape has no identity to pair on.
    // It is a fallback over *unfinished* steps, so an id-paired step is skipped
    // rather than removed (which would make same-name restore quadratic).
    let mut pending_by_name: HashMap<String, VecDeque<usize>> = HashMap::new();
    // A call whose persisted id is empty gets a synthetic unique id rather than
    // borrowing the tool name: an id identifies one call, and two same-named
    // calls must never share one.
    let mut restored_seq: usize = 0;
    for mut message in messages {
        // A compaction checkpoint is *hidden* dialogue but still owns a card:
        // it is the one row that explains why the model's window is shorter
        // than the thread. Classifying it before the hidden filter is what
        // keeps a resumed transcript from looking like history was deleted
        // (ADR-0246 `[INV-CMP-10]`).
        if message
            .origin
            .as_ref()
            .is_some_and(|origin| origin.kind == tool::InjectionKind::CompactionCheckpoint)
        {
            if let Some(card) = transcript_message_from_core(message) {
                restored.push(card);
            }
            continue;
        }
        if message.hidden || message.role == Role::System {
            continue;
        }
        if track_rounds {
            let is_insert = message
                .origin
                .as_ref()
                .is_some_and(|origin| origin.kind == tool::InjectionKind::UserSteer);
            let opens_round = message.role == Role::User && !is_insert;
            if opens_round {
                restored_round = restored_round.saturating_add(1);
                restored_turn = 0;
            }
        }
        // Attribution travels on every part so a resumed session that mixed
        // models still shows which model produced each turn.
        let provider = message.provider.clone();
        let model = message.model.clone();
        // Reasoning depth is assistant-turn attribution too: keep it alongside
        // provider/model, dropping the empty/`none` spellings (a channel whose
        // effort resolved to the `None` tier renders no depth chip).
        let effort = message
            .effort
            .clone()
            .filter(|effort| !effort.is_empty() && !effort.eq_ignore_ascii_case("none"));
        let message_sent_at_ms = message.sent_at_ms.or_else(|| {
            message
                .timestamp
                .map(|seconds| seconds.saturating_mul(1000))
        });
        if message.role == Role::Assistant {
            if track_rounds {
                if restored_round == 0 {
                    // Defensive compatibility for imported assistant-first
                    // transcripts that predate a driving user message.
                    restored_round = 1;
                }
                restored_turn = restored_turn.saturating_add(1);
            }
            // Mirrors the live path's `StreamReasoningDelta` gate: a hidden-chain
            // model (`ReasoningSummary`, e.g. GPT-5.x) never disclosed its full
            // reasoning chain, so its persisted `reasoning_content` is only a
            // summary. Restoring it as a `MessageKind::Reasoning` message would
            // resurrect a phantom entry the live stream never created — leaking
            // into layout counts, selection math, and scroll state. Skip it.
            // `model` is the persisted `Option<String>` attribution. Use
            // `model_by_id` (not `resolve`): `resolve` falls back to a model
            // with `ReasoningSupport::None` for unrecognized ids, whose
            // `chain_disclosed()` is `false`, which would suppress restoration
            // of reasoning traces for local/user-defined models that DO reason
            // — a regression for legacy transcripts. `model_by_id` returns
            // `None` for unknown ids, and we default to `true` (disclosed) so
            // only known hidden-chain models (`ReasoningSummary`, GPT-5.x) are
            // gated.
            let chain_disclosed = model
                .as_deref()
                .and_then(protocol::model_by_id)
                .map(|m| m.thinking.chain_disclosed())
                .unwrap_or(true);
            if chain_disclosed && let Some(reasoning) = message.reasoning_content.take() {
                let mut thinking = TranscriptMessage::reasoning(reasoning);
                thinking.provider = provider.clone();
                thinking.model = model.clone();
                thinking.effort = effort.clone();
                // The trace is a component of *this* assistant message's turn, so
                // it carries that message's send time. Without this the restored
                // band header lost its `HH:MM` whenever thinking was the turn's
                // first component — the live path stamps every component from the
                // turn's own clock, and resume must agree.
                thinking.sent_at_ms = message_sent_at_ms;
                if track_rounds {
                    thinking.round = Some(restored_round);
                    thinking.turn = Some(restored_turn);
                }
                thinking.set_reasoning_duration(0);
                // Honor the configured default expand state for reasoning
                // traces so resumed sessions match live behavior.
                if view::disclosure_config::reasoning_expanded(config) {
                    thinking.set_reasoning_expanded(true);
                }
                restored.push(thinking);
            }
            if let Some(calls) = message.tool_calls.take() {
                for call in calls {
                    // ADR-0166: the step's id is the call's id, never the tool
                    // name. An id-less legacy entry gets a synthetic unique id so
                    // two same-named calls still cannot collide.
                    let real_id = (!call.id.is_empty()).then(|| call.id.clone());
                    let step_id = match &real_id {
                        Some(id) => id.clone(),
                        None => {
                            restored_seq += 1;
                            format!("restored_call_{restored_seq}")
                        }
                    };
                    let mut step =
                        TranscriptMessage::tool_step(step_id, call.name.clone(), call.arguments);
                    step.provider = provider.clone();
                    step.model = model.clone();
                    step.effort = effort.clone();
                    if track_rounds {
                        step.round = Some(restored_round);
                        step.turn = Some(restored_turn);
                    }
                    step.sent_at_ms = message_sent_at_ms;
                    if let Some(id) = real_id {
                        pending_by_id.insert(id, restored.len());
                    }
                    pending_by_name
                        .entry(call.name)
                        .or_default()
                        .push_back(restored.len());
                    restored.push(step);
                }
                if message.content.is_empty() {
                    continue;
                }
            }
        }
        if message.role == Role::Tool
            && let Some((name, output)) = parse_tool_result(&message.content)
        {
            // Identity first: a tool message carries the `tool_call_id` of the
            // call it answers. Fall back to the per-name FIFO only when that id
            // is absent or empty, and to nothing when no step is pending — an
            // orphan result is then rendered as a plain message below.
            let paired = message
                .tool_call_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .and_then(|id| pending_by_id.remove(id))
                .or_else(|| {
                    let queue = pending_by_name.get_mut(name)?;
                    while let Some(idx) = queue.pop_front() {
                        if restored[idx]
                            .tool_step_status()
                            .is_some_and(|status| status.is_running())
                        {
                            return Some(idx);
                        }
                    }
                    None
                });
            if let Some(idx) = paired {
                let item = &mut restored[idx];
                let step_id = item.tool_step_call_id().unwrap_or(name).to_string();
                let meta = message.subagent_meta.take();
                let children = message.children.take();
                // Rebuild a structured output that preserves the subagent's true
                // classification (failed / interrupted / ok) and real duration;
                // the bare summary text cannot distinguish them. `children`
                // round-trip separately below so the drill-in view works after
                // resume.
                let (structured, duration_ms) = match &meta {
                    Some(meta) => (
                        ::tool::ToolOutput::Subagent {
                            summary: output.to_string(),
                            messages: Vec::new(),
                            usage: tool::TokenUsage::default(),
                            generation_ms: 0,
                            failed: meta.failed,
                            interrupted: meta.interrupted,
                        },
                        meta.duration_ms.unwrap_or(0),
                    ),
                    None => (::tool::ToolOutput::text(output), 0),
                };
                if item.finish_tool_step(&step_id, output, structured, duration_ms) {
                    // Restore the subagent's nested transcript so its partial /
                    // completed work is drillable after resume.
                    if let Some(child_messages) = children {
                        let grand = transcript_children_from_core(child_messages, config);
                        if !grand.is_empty()
                            && let Some(slot) = item.subagent_children_mut()
                        {
                            *slot = grand;
                        }
                    }
                    // Apply the lifecycle-aware default disclosure so
                    // restored steps match live (Failed/Denied expand,
                    // Ok follows per-tool config).
                    if let Some(status) = item.tool_step_status() {
                        let default = step_interaction::default_tool_expanded(status, name, config);
                        item.set_tool_step_expanded(default);
                    }
                    continue;
                }
            }
        }
        if let Some(mut transcript_message) = transcript_message_from_core(message) {
            if track_rounds {
                if transcript_message.role == Role::Assistant {
                    transcript_message.round = Some(restored_round);
                    transcript_message.turn = Some(restored_turn);
                } else if transcript_message.role == Role::User
                    && transcript_message.origin == UserMessageOrigin::Chat
                {
                    transcript_message.round = Some(restored_round);
                } else if transcript_message.role == Role::User
                    && (transcript_message.origin == UserMessageOrigin::Steer
                        || transcript_message.origin == UserMessageOrigin::FollowUp)
                {
                    transcript_message.round = (restored_round > 0).then_some(restored_round);
                }
                // ADR-0137: the durable store only ever contains *admitted*
                // steers — the harness persists them at the turn seam — so a
                // restored steer is always an in-round interjection, and the
                // restore engine rebuilds the render tag the live path stamps
                // from `SteerAdmitted`. Follow-ups admitted as fresh rounds
                // keep round-panel weight (no tag).
                if transcript_message.role == Role::User
                    && transcript_message.origin == UserMessageOrigin::Steer
                {
                    transcript_message.origin_tags =
                        vec![view::model::document::SteerOriginTag::InRound];
                }
            }
            restored.push(transcript_message);
        }
    }
    restored
}

/// Align a reconstructed transcript tail with the session's authoritative
/// monotonic round counter.
///
/// Legacy messages do not persist round positions, and compaction may remove
/// older visible rounds. Reconstruction therefore yields a relative `1..N`
/// tail. The session counter identifies which real round `N` represents.
pub fn rebase_transcript_rounds(messages: &mut [TranscriptMessage], authoritative_round: u64) {
    let Some(restored_round) = messages.iter().filter_map(|message| message.round).max() else {
        return;
    };
    let Some(offset) = authoritative_round.checked_sub(restored_round) else {
        // Never move a transcript backwards when handed a stale snapshot.
        return;
    };
    if offset == 0 {
        return;
    }
    for message in messages {
        if let Some(round) = &mut message.round {
            *round = round.saturating_add(offset);
        }
    }
}

/// Freeze any in-flight reasoning traces in `messages`.
///
/// A reasoning trace is rendered as "running" (breathing spinner) for as
/// long as its `duration_ms` is `None`. The trace normally reaches that
/// terminal state when `StreamReasoningEnd` arrives. But when a turn ends
/// first — the user interrupts, the provider errors mid-stream, or a fresh
/// turn supersedes a still-streaming one — that event never arrives and the
/// spinner would breathe forever. This sweep stamps `duration_ms` on every
/// still-streaming trace so the marker freezes on its last token.
///
/// `duration_ms` is the elapsed reasoning time if known (e.g. captured from
/// the stream start); `None` means the start instant was already consumed
/// or never recorded, in which case `0` is used so the trace still leaves
/// the streaming state.
pub fn finalize_streaming_reasoning(messages: &mut [TranscriptMessage], duration_ms: Option<u64>) {
    let stamped = duration_ms.unwrap_or(0);
    for message in messages.iter_mut() {
        if message.is_reasoning_streaming() {
            message.set_reasoning_duration(stamped);
        }
    }
}

fn parse_tool_result(content: &str) -> Option<(&str, &str)> {
    tool::tool_result_header::parse(content)
}

pub(super) fn format_tool_call(name: &str, arguments: &str) -> String {
    let arguments = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| arguments.to_string());
    format!("Calling `{}`\n\n```json\n{}\n```", name, arguments)
}

#[cfg(test)]
mod tests;
