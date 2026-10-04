//! Translation between the harness's persistent [`Message`] stream and the
//! TUI's semantic [`TranscriptMessage`] document model. Also hosts the small
//! parsing helpers for the textual `` `[<tool> result]`: `` envelope and the
//! matching `` `Calling \`<tool>\`` `` formatter used when no display content
//! is available for a restored assistant turn.
//!
//! [`Message`]: nuo_wire::Message

use std::collections::HashMap;
use std::collections::VecDeque;

use nuo_wire::{Message, Role};

use crate::config::{self, TuiConfig};
use crate::model::document::{TranscriptMessage, UserMessageOrigin};
use crate::step_interaction;

pub(super) fn transcript_message_from_core(message: Message) -> Option<TranscriptMessage> {
    if message.hidden {
        return None;
    }
    if message.role == Role::System {
        if message.content.starts_with("[Conversation Summary Checkpoint") {
            let mut card = TranscriptMessage::compacted_card(
                0,
                0,
                0,
                Some(message.content),
                Vec::new(),
            );
            card.provider = message.provider;
            card.model = message.model;
            card.sent_at_ms = message.sent_at_ms.or_else(|| {
                message
                    .timestamp
                    .map(|seconds| seconds.saturating_mul(1000))
            });
            return Some(card);
        }
        return None;
    }
    let provider = message.provider.clone();
    let model = message.model.clone();
    let sent_at_ms = message.sent_at_ms.or_else(|| {
        message
            .timestamp
            .map(|seconds| seconds.saturating_mul(1000))
    });
    // Whether the harness carried a curated `display_content` for this user
    // message — slash commands set this to the literal `/cmd` (their `content`
    // is the harness-expanded form), so its presence is the signal that the
    // turn was a slash command rather than a genuine chat prompt.
    let had_display_content = message.display_content.is_some();
    // Capture non-driving provenance before `content` is moved (ADR-0050): a
    // durable `CommandEcho` origin unambiguously marks the message as a
    // non-driving command regardless of text shape.
    let is_echo = message.is_command_echo();
    let is_insert = message
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == nuo_wire::InjectionKind::UserSteer);
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
        // Infer the turn origin for restored user messages so a resumed
        // session's Activity modal still skips slash/shell turns. The durable
        // `origin` field is consulted first (ADR-0050): a `CommandEcho`
        // provenance unambiguously marks the message as a non-driving command,
        // regardless of its text shape. When `origin` is `None` (legacy
        // sessions predating the field, or genuine chat prompts) the shape
        // heuristic below applies: slash commands carry a `display_content`
        // that is the literal `/cmd` (the `content` is the harness-expanded
        // form); shell passthroughs persist as the `!command` the user typed.
        // Only a real prompt surfaces a leading `/` or `!` *not* followed by a
        // word char would still be misclassified — but in practice both are
        // followed by a command word, so the heuristic is exact for the shapes
        // the harness produces.
        if message.role == Role::User {
            // Slash origin: a durable `CommandEcho` provenance (ADR-0050, which
            // unambiguously marks the message as a non-driving command
            // regardless of text shape) OR the legacy shape signal of a
            // `display_content` whose text is the literal `/cmd`. Both fold to
            // Slash; the echo check runs first so a durable echo without
            // `display_content` is still classified correctly.
            let is_slash = is_echo
                || (had_display_content
                    && msg.raw.strip_prefix('/').is_some_and(|rest| {
                        rest.chars().next().is_some_and(|c| c.is_alphanumeric())
                    }));
            if is_slash {
                msg.origin = UserMessageOrigin::Slash;
            } else if is_insert {
                msg.origin = UserMessageOrigin::Steer;
            }
        }
        Some(msg)
    }
}

pub(super) fn transcript_messages_from_core(
    messages: Vec<Message>,
    config: &TuiConfig,
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
    records: Vec<nuo_wire::RoundInterrupt>,
) -> Vec<TranscriptMessage> {
    records
        .into_iter()
        .map(|record| {
            let at_ms = record.at_ms;
            TranscriptMessage::round_interrupted(record).with_sent_at_ms(at_ms)
        })
        .collect()
}

/// Build durable retry-resolution rows from the session store's records —
/// the success-side twin of [`transcript_interrupts_from_records`].
pub(super) fn transcript_retry_resolutions_from_records(
    _records: Vec<nuo_wire::RetryResolution>,
) -> Vec<TranscriptMessage> {
    // ADR-0194: Retired. Transient transport retries that recovered successfully
    // are implementation details; they never project onto the transcript history.
    Vec::new()
}

/// Merge rebuilt round-interrupt rows into a restored transcript (C11),
/// mirroring [`merge_command_rows`]'s seam rule: each marker lands **before
/// the first user message whose send time is later than the stop** — at the
/// seam between conversation turns, never inside one. An interrupt older
/// than every seam, or carrying no comparable seam, lands at the tail: it
/// was the last thing that happened before the transcript ended, which is
/// exactly the "the process died mid-round" case the marker exists to show.
pub(super) fn merge_round_interrupt_rows(
    dialogue: Vec<TranscriptMessage>,
    interrupts: Vec<TranscriptMessage>,
) -> Vec<TranscriptMessage> {
    use nuo_wire::Role;

    if interrupts.is_empty() {
        return dialogue;
    }
    let mut out = Vec::with_capacity(dialogue.len() + interrupts.len());
    let mut interrupts = interrupts.into_iter().peekable();

    for message in dialogue {
        if message.role == Role::User
            && let Some(seam_ms) = message.sent_at_ms
        {
            while let Some(marker) = interrupts.peek() {
                if marker.sent_at_ms.is_some_and(|ms| ms <= seam_ms) {
                    #[allow(clippy::expect_used)]
                    out.push(interrupts.next().expect("peeked interrupt exists"));
                } else {
                    break;
                }
            }
        }
        out.push(message);
    }
    // Markers after the last seam land at the tail, in record order.
    out.extend(interrupts);
    out
}

/// Rebuild the command rows of the ADR-0091 durable command ledger into the
/// TUI document model: one compact, dimmed, non-conversational row per
/// invocation, with the typed result as its expandable body. The ledger is the
/// source of truth for commands on resume — the message stream is pure
/// dialogue. Rows carry no round/turn position (a command is not a turn); the
/// caller rebases round positions over the returned slice as usual.
///
/// Each row is stamped with the record's invocation `timestamp` (via
/// `sent_at_ms`) so [`merge_command_rows`] can place it at its turn seam; the
/// stamp is projection-only state and never reaches the ledger or the model.
pub(super) fn transcript_commands_from_ledger(
    commands: Vec<nuo_wire::CommandRecord>,
) -> Vec<TranscriptMessage> {
    commands
        .into_iter()
        .map(|record| {
            TranscriptMessage::command_result(record.name, record.args, record.result)
                .with_sent_at_ms(record.timestamp)
        })
        .collect()
}

/// Merge rebuilt command rows into a restored dialogue transcript (ADR-0106
/// §2: the transcript is a projection — command rows render *at the moment
/// they happened*, not appended to the tail).
///
/// Ordering rule, stable and total:
///
/// 1. Each command row lands **before the first user message whose send time
///    is later than the command's invocation** — i.e. at the seam between
///    conversation turns, never inside one. A command issued between turn 2
///    and turn 3 renders after turn 2's assistant reply and before turn 3's
///    prompt, exactly where it appeared live.
/// 2. Commands older than every user seam, or carrying no timestamp, keep
///    their ledger order at the **tail** (they were the last thing run), and
///    dialogue with no user timestamps at all simply takes the tail too —
///    dialogue order is never disturbed.
/// 3. Ties (a command and the next prompt within the same millisecond) place
///    the command before the seam, matching how a command is dispatched
///    before the round it precedes opens.
///
/// Timestamps compare against user messages' `sent_at_ms` — assistant/tool
/// parts of a turn are never split, because a turn's parts share the turn
/// boundary established by its user message.
pub(super) fn merge_command_rows(
    dialogue: Vec<TranscriptMessage>,
    commands: Vec<TranscriptMessage>,
) -> Vec<TranscriptMessage> {
    use nuo_wire::Role;

    if commands.is_empty() {
        return dialogue;
    }
    let mut out = Vec::with_capacity(dialogue.len() + commands.len());
    let mut commands = commands.into_iter().peekable();

    for message in dialogue {
        // Spill every command whose invocation precedes this turn seam.
        if message.role == Role::User
            && let Some(seam_ms) = message.sent_at_ms
        {
            while let Some(cmd) = commands.peek() {
                if cmd.sent_at_ms.is_some_and(|ms| ms < seam_ms) {
                    // The peek above guarantees the item; `expect` documents
                    // the invariant without a fallible API change.
                    #[allow(clippy::expect_used)]
                    out.push(commands.next().expect("peeked command exists"));
                } else {
                    break;
                }
            }
        }
        out.push(message);
    }
    // Commands after the last seam (or with no timestamp) land at the tail,
    // still in ledger order.
    out.extend(commands);
    out
}

/// Rebuild the nested transcript of a subagent run (the `children` carried by a
/// Tool-role message) into the TUI document model. Mirrors the live
/// event-driven build — assistant text, reasoning traces, and nested tool
/// steps with their own results — and recurses for arbitrarily deep subagent
/// trees, so the drill-in subagent view works after a resume.
fn transcript_children_from_core(
    messages: Vec<Message>,
    config: &TuiConfig,
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
    config: &TuiConfig,
    track_rounds: bool,
) -> Vec<TranscriptMessage> {
    let mut restored = Vec::new();
    // Reconstruct the canonical Round -> Turn position from the durable
    // transcript. A driving visible user message opens a round; every
    // assistant response advances the ReAct turn within it. Inserts and
    // non-driving command echoes do not open rounds.
    let mut restored_round: u64 = 0;
    let mut restored_turn: u64 = 0;
    // Index of every still-unfinished tool step, queued per tool name. A tool
    // result pairs with the *earliest* unfinished step of the same name (live
    // order), so a per-name FIFO reproduces the old forward-scan semantics in
    // O(1) per result instead of rescanning the whole `restored` vec — turning
    // the restore of a tool-heavy session from O(n²) into O(n).
    let mut pending_steps: HashMap<String, VecDeque<usize>> = HashMap::new();
    for mut message in messages {
        if message.hidden || message.role == Role::System {
            continue;
        }
        if track_rounds {
            let is_insert = message
                .origin
                .as_ref()
                .is_some_and(|origin| origin.kind == nuo_wire::InjectionKind::UserSteer);
            let opens_round =
                message.role == Role::User && !is_insert && !message.is_command_echo();
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
                .and_then(nuo_wire::model_by_id)
                .map(|m| m.thinking.chain_disclosed())
                .unwrap_or(true);
            if chain_disclosed && let Some(reasoning) = message.reasoning_content.take() {
                let mut thinking = TranscriptMessage::reasoning(reasoning);
                thinking.provider = provider.clone();
                thinking.model = model.clone();
                thinking.effort = effort.clone();
                if track_rounds {
                    thinking.round = Some(restored_round);
                    thinking.turn = Some(restored_turn);
                }
                thinking.set_reasoning_duration(0);
                // Honor the configured default expand state for reasoning
                // traces so resumed sessions match live behavior.
                if config::reasoning_default_expanded(config) {
                    thinking.set_reasoning_expanded(true);
                }
                restored.push(thinking);
            }
            if let Some(calls) = message.tool_calls.take() {
                for call in calls {
                    // Historical results match by tool name, so use it as the id.
                    // Disclosure is applied when the matching result finishes
                    // the step below (lifecycle-aware default), mirroring live.
                    let mut step = TranscriptMessage::tool_step(
                        call.name.clone(),
                        call.name.clone(),
                        call.arguments,
                    );
                    step.provider = provider.clone();
                    step.model = model.clone();
                    step.effort = effort.clone();
                    if track_rounds {
                        step.round = Some(restored_round);
                        step.turn = Some(restored_turn);
                    }
                    step.sent_at_ms = message_sent_at_ms;
                    pending_steps
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
            // Pair with the earliest unfinished step of this name (O(1) via the
            // per-name queue). Fall back to nothing if no step is pending — an
            // orphan result is then rendered as a plain message below.
            if let Some(idx) = pending_steps.get_mut(name).and_then(|q| q.pop_front()) {
                let item = &mut restored[idx];
                let meta = message.subagent_meta.take();
                let children = message.children.take();
                // Rebuild a structured output that preserves the subagent's true
                // classification (failed / interrupted / ok) and real duration;
                // the bare summary text cannot distinguish them. `children`
                // round-trip separately below so the drill-in view works after
                // resume.
                let (structured, duration_ms) = match &meta {
                    Some(meta) => (
                        nuo_wire::ToolOutput::Subagent {
                            summary: output.to_string(),
                            messages: Vec::new(),
                            usage: nuo_wire::TokenUsage::default(),
                            generation_ms: 0,
                            failed: meta.failed,
                            interrupted: meta.interrupted,
                        },
                        meta.duration_ms.unwrap_or(0),
                    ),
                    None => (nuo_wire::ToolOutput::text(output), 0),
                };
                if item.finish_tool_step(name, output, structured, duration_ms) {
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
                        let default =
                            step_interaction::default_tool_expanded(status, name, config, false);
                        item.set_tool_step_expanded(default);
                    }
                    continue;
                }
            }
        }
        // Fold a restored slash/shell echo into the command ledger
        // projection (ADR-0108): the invocation belongs to the command
        // component (`⌘ /cmd`), never to a second user bubble — the ledger
        // row for the same invocation is merged in later by
        // `merge_command_rows`, so dropping the echo keeps exactly one row
        // per command after resume, matching the live path. Classification
        // mirrors `transcript_message_from_core` exactly: a durable
        // `CommandEcho` provenance, or the legacy shape signal of a
        // `display_content` that is the literal `/cmd` (its `content` is the
        // harness-expanded form). Round bookkeeping is unaffected — echoes
        // never opened rounds (`opens_round` already excluded them).
        if message.role == Role::User {
            let is_echo = message.is_command_echo();
            let slash_shaped = message
                .display_content
                .as_deref()
                .and_then(|raw| raw.strip_prefix('/'))
                .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_alphanumeric()));
            if is_echo || slash_shaped {
                continue;
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
pub(crate) fn rebase_transcript_rounds(
    messages: &mut [TranscriptMessage],
    authoritative_round: u64,
) {
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
pub(crate) fn finalize_streaming_reasoning(
    messages: &mut [TranscriptMessage],
    duration_ms: Option<u64>,
) {
    let stamped = duration_ms.unwrap_or(0);
    for message in messages.iter_mut() {
        if message.is_reasoning_streaming() {
            message.set_reasoning_duration(stamped);
        }
    }
}

pub(super) fn parse_tool_result(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('[')?;
    let (name, output) = content.split_once(" result]:")?;
    Some((name, output.trim_start_matches('\n')))
}

pub(super) fn format_tool_call(name: &str, arguments: &str) -> String {
    let arguments = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| arguments.to_string());
    format!("Calling `{}`\n\n```json\n{}\n```", name, arguments)
}

#[cfg(test)]
mod tests {
    use super::{
        merge_round_interrupt_rows, rebase_transcript_rounds, transcript_interrupts_from_records,
    };
    use crate::model::document::TranscriptMessage;
    use nuo_wire::Role;

    #[test]
    fn rebases_a_compacted_relative_tail_to_the_session_round_counter() {
        let mut messages = vec![
            TranscriptMessage::new(Role::User, "older").with_round(1),
            TranscriptMessage::new(Role::Assistant, "reply").with_round(2),
        ];

        rebase_transcript_rounds(&mut messages, 42);

        assert_eq!(messages[0].round, Some(41));
        assert_eq!(messages[1].round, Some(42));
    }

    /// An interrupted subagent survives a resume with its true classification and
    /// its partial children intact: the persisted `subagent_meta.interrupted` flag
    /// drives the `Interrupted` status (not `Ok` — the bare summary text cannot
    /// say it), and the nested transcript rebuilds the drill-in view.
    #[test]
    fn restores_interrupted_subagent_with_children_and_status() {
        use crate::config::TuiConfig;
        use nuo_wire::message::SubagentMeta;
        use nuo_wire::{Message, ToolCall};

        let call = ToolCall {
            id: "call_9".to_string(),
            name: "spawn_agent".to_string(),
            arguments: r#"{"description":"d","prompt":"p"}"#.to_string(),
        };
        let inner_call = ToolCall {
            id: "inner_1".to_string(),
            name: "read_text".to_string(),
            arguments: "{}".to_string(),
        };
        // The subagent's partial internal transcript: one completed read round.
        let children = vec![
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: Some(vec![inner_call.clone()]),
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(&inner_call, "[read_text result]:\nfound 1 of 3"),
        ];
        let messages = vec![
            Message::new(Role::User, "research the handlers"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: Some(vec![call.clone()]),
                ..Message::new(Role::Assistant, "")
            },
            Message::tool_result(
                &call,
                "[spawn_agent result]:\nInterrupted: stopped by the user",
            )
            .with_children(children)
            .with_subagent_meta(SubagentMeta {
                duration_ms: Some(42),
                failed: false,
                interrupted: true,
                ..Default::default()
            }),
        ];

        let restored = super::transcript_messages_from_core(messages, &TuiConfig::default());
        let step = restored
            .iter()
            .find(|m| m.is_subagent_task())
            .expect("the subagent step must be restored");
        assert_eq!(
            step.tool_step_status(),
            Some(crate::model::document::ToolStepStatus::Interrupted),
            "restored interrupted subagent must classify as Interrupted, not Ok/Failed"
        );
        let kids = step.subagent_children().expect("children must be restored");
        assert_eq!(kids.len(), 1, "one completed child tool step expected");
        assert!(kids[0].is_tool_step());
        assert_eq!(
            kids[0].tool_step_status(),
            Some(crate::model::document::ToolStepStatus::Ok),
            "the child read_text step completed normally"
        );
    }

    /// C11: interrupt markers re-project at their timestamp seam — before the
    /// first user message sent *after* the stop — and a marker newer than
    /// every seam lands at the tail (the process died mid-round).
    #[test]
    fn round_interrupt_markers_land_at_their_seams() {
        use nuo_wire::{RoundInterrupt, RoundInterruptReason};

        let dialogue = vec![
            TranscriptMessage::new(Role::User, "first").with_sent_at_ms(1_000),
            TranscriptMessage::new(Role::Assistant, "reply 1"),
            TranscriptMessage::new(Role::User, "second").with_sent_at_ms(5_000),
            TranscriptMessage::new(Role::Assistant, "reply 2"),
        ];
        let markers = transcript_interrupts_from_records(vec![
            RoundInterrupt {
                reason: RoundInterruptReason::User,
                at_ms: 3_000,
                round: Some(1),
                detail: None,
            },
            RoundInterrupt {
                reason: RoundInterruptReason::Terminated,
                at_ms: 9_000,
                round: Some(2),
                detail: None,
            },
        ]);

        let merged = merge_round_interrupt_rows(dialogue, markers);

        // 6 rows: u1, a1, marker1, u2, a2, marker2(tail).
        assert_eq!(merged.len(), 6);
        assert!(merged[2].is_round_interrupt());
        assert_eq!(
            merged[2].sent_at_ms,
            Some(3_000),
            "mid-transcript marker keeps its own timestamp"
        );
        assert!(
            merged[5].is_round_interrupt(),
            "post-tail marker at the end"
        );
        assert!(!merged[1].is_round_interrupt(), "seam order preserved");
    }

    /// The marker row's raw text carries the round + reason vocabulary shared
    /// with the live path.
    #[test]
    fn round_interrupt_marker_text_uses_shared_vocabulary() {
        use nuo_wire::{RoundInterrupt, RoundInterruptReason};

        let marker = TranscriptMessage::round_interrupted(RoundInterrupt {
            reason: RoundInterruptReason::User,
            at_ms: 42,
            round: Some(3),
            detail: None,
        });
        assert_eq!(marker.raw, "Round 3 — cancelled via [Esc Esc]");

        let unnumbered = TranscriptMessage::round_interrupted(RoundInterrupt {
            reason: RoundInterruptReason::Terminated,
            at_ms: 42,
            round: None,
            detail: None,
        });
        assert_eq!(unnumbered.raw, "Process exited");

        let err_marker = TranscriptMessage::round_interrupted(RoundInterrupt {
            reason: RoundInterruptReason::Error,
            at_ms: 42,
            round: Some(4),
            detail: Some(
                "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests".to_string(),
            ),
        });
        assert_eq!(
            err_marker.raw,
            "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests"
        );
    }
}
