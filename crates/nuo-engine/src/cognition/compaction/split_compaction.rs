//! Turn-aware split compaction on durable thread facts.
//!
//! Features:
//! - Turn-aware atomic cut point selection that never cuts on an observation.
//! - Conservative token estimation per `ThreadFact`.
//! - LLM structured summarization into dense context checkpoints.
use super::file_tracker::FileOperations;
use nuo_protocol::{FactId, FactPayload, ModelRequest, ObservationLifecycle, Provider, ThreadFact};
use nuo_tool::{Message, Role};

use std::sync::Arc;
use tokio::time::Duration;

const COMPACTION_TIMEOUT: Duration = Duration::from_secs(45);

const COMPACTION_SYSTEM_PROMPT: &str = "\
You are a context compaction engine for an AI coding assistant. \
Your task is to summarize older conversation turns into a dense, factual context summary \
so that ongoing work can continue with minimal token overhead without losing crucial facts.";

const COMPACTION_USER_INSTRUCTIONS: &str = "\
Summarize the conversation history above into a structured context checkpoint.

Follow this format:

## Primary Goal & Current Objective
[What is the overall goal and what was being worked on?]

## Key Facts & Architecture Notes
- [Crucial decisions, paths, port numbers, dependencies, or architectural facts]

## Completed Actions
- [x] [What was accomplished and verified]

## Current State & In-Flight Context
- [What was in progress right before this checkpoint]

## Critical Error Messages & Observations
- [Any specific error messages or unexpected behavior observed]

Preserve exact identifiers, function names, file paths, and technical details.";

/// Result of finding a valid cut point for compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index in facts slice of the first fact to KEEP in active context.
    pub first_kept_index: usize,
    /// Identity of the first fact kept.
    pub first_kept_fact_id: FactId,
    /// Whether the cut splits an in-flight multi-step turn.
    pub is_split_turn: bool,
    /// Estimated tokens in the facts before the cut point.
    pub tokens_before: usize,
    /// Sequence of the last folded fact.
    pub folded_through_seq: u64,
}

/// Conservative token estimation for a `ThreadFact` (chars / 4 heuristic).
pub fn estimate_fact_tokens(fact: &ThreadFact) -> usize {
    match &fact.payload {
        FactPayload::Message { message } => {
            let mut chars = message.content.len();
            if let Some(ref calls) = message.tool_calls {
                for c in calls {
                    chars += c.name.len() + c.arguments.len() + 16;
                }
            }
            chars.div_ceil(4)
        }
        FactPayload::Compaction { summary, .. } => summary.len().div_ceil(4),
        FactPayload::Observation { lifecycle, .. } => match lifecycle {
            ObservationLifecycle::Raw { content } => content.len().div_ceil(4),
            ObservationLifecycle::ActiveTruncated {
                head_preview,
                tail_preview,
                ..
            } => (head_preview.len() + tail_preview.len() + 100).div_ceil(4),
            ObservationLifecycle::Retired { .. } => 32,
        },
        FactPayload::Termination { partial_output, .. } => partial_output
            .as_ref()
            .map(|s| s.len().div_ceil(4))
            .unwrap_or(32),
        FactPayload::SystemNotice { content, .. } => content.len().div_ceil(4),
    }
}

fn is_user_prompt(fact: &ThreadFact) -> bool {
    matches!(
        &fact.payload,
        FactPayload::Message { message } if message.role == Role::User
    )
}

fn is_tool_result(fact: &ThreadFact) -> bool {
    matches!(fact.payload, FactPayload::Observation { .. })
        || matches!(
            &fact.payload,
            FactPayload::Message { message } if message.role == Role::Tool
        )
}

/// Find a turn-aware cut point that keeps **at most** `keep_recent_tokens` of
/// newest context.
pub fn find_cut_point_facts(
    facts: &[&ThreadFact],
    keep_recent_tokens: usize,
) -> Option<CutPointResult> {
    if facts.is_empty() {
        return None;
    }

    let token_counts: Vec<usize> = facts.iter().map(|f| estimate_fact_tokens(f)).collect();
    let total_tokens: usize = token_counts.iter().sum();

    if total_tokens <= keep_recent_tokens {
        return None;
    }

    let mut accumulated_tokens = 0usize;
    let mut cut_index = facts.len();
    for i in (0..facts.len()).rev() {
        let next = accumulated_tokens.saturating_add(token_counts[i]);
        if next > keep_recent_tokens {
            break;
        }
        accumulated_tokens = next;
        cut_index = i;
    }
    if cut_index == facts.len() {
        cut_index = facts.len() - 1;
    }

    // Never cut between an assistant tool call and its observation.
    while cut_index > 0 && is_tool_result(facts[cut_index]) {
        cut_index -= 1;
    }

    if cut_index == 0 || cut_index >= facts.len() {
        return None;
    }

    let first_kept = facts[cut_index];
    let is_split_turn = matches!(
        &first_kept.payload,
        FactPayload::Message { message } if message.role == Role::Assistant
    );

    let tokens_before: usize = token_counts[..cut_index].iter().sum();
    let folded_through_seq = facts[cut_index - 1].seq;

    Some(CutPointResult {
        first_kept_index: cut_index,
        first_kept_fact_id: first_kept.id.clone(),
        is_split_turn,
        tokens_before,
        folded_through_seq,
    })
}

/// Find a cut point preserving the last `preserve_rounds` user rounds.
pub fn find_tail_preserving_cut_point(
    facts: &[&ThreadFact],
    preserve_rounds: usize,
) -> Option<CutPointResult> {
    if facts.is_empty() || preserve_rounds == 0 {
        return None;
    }

    let token_counts: Vec<usize> = facts.iter().map(|f| estimate_fact_tokens(f)).collect();

    let mut user_rounds_seen = 0;
    let mut cut_index = None;
    for (i, fact) in facts.iter().enumerate().rev() {
        if is_user_prompt(fact) {
            user_rounds_seen += 1;
            if user_rounds_seen == preserve_rounds {
                cut_index = Some(i);
                break;
            }
        }
    }

    let mut cut_index = cut_index?;
    if cut_index == 0 {
        return None;
    }

    while cut_index > 0 && is_tool_result(facts[cut_index]) {
        cut_index -= 1;
    }

    if cut_index == 0 || cut_index >= facts.len() {
        return None;
    }

    let first_kept = facts[cut_index];
    let is_split_turn = matches!(
        &first_kept.payload,
        FactPayload::Message { message } if message.role == Role::Assistant
    );
    let tokens_before: usize = token_counts[..cut_index].iter().sum();
    let folded_through_seq = facts[cut_index - 1].seq;

    Some(CutPointResult {
        first_kept_index: cut_index,
        first_kept_fact_id: first_kept.id.clone(),
        is_split_turn,
        tokens_before,
        folded_through_seq,
    })
}

/// Serialize facts to clean text for LLM summarization.
pub fn serialize_facts_for_summary(facts: &[&ThreadFact]) -> String {
    let mut out = String::new();
    for fact in facts {
        match &fact.payload {
            FactPayload::Message { message } => {
                let role_label = match message.role {
                    Role::User => "User",
                    Role::Assistant => "Assistant",
                    Role::Tool => "Tool Result",
                    Role::System => "System",
                };
                out.push_str(&format!("[{}]\n{}\n\n", role_label, message.content));
            }
            FactPayload::Compaction { summary, .. } => {
                out.push_str(&format!("[Previous Summary Checkpoint]\n{}\n\n", summary));
            }
            FactPayload::Termination {
                reason,
                partial_output,
                ..
            } => {
                let reason_str = match reason {
                    nuo_protocol::TerminationReason::UserInterrupt => "Interrupted by user",
                    nuo_protocol::TerminationReason::Timeout => "Execution timed out",
                    nuo_protocol::TerminationReason::FatalError { error } => error.as_str(),
                    nuo_protocol::TerminationReason::Superseded => "Superseded by user message",
                };
                let output = partial_output.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "[Execution Stopped: {}]\n{}\n\n",
                    reason_str, output
                ));
            }
            FactPayload::SystemNotice {
                source, content, ..
            } => {
                out.push_str(&format!("[System Notice from {}]\n{}\n\n", source, content));
            }
            FactPayload::Observation {
                call_id,
                tool_name,
                lifecycle,
                ..
            } => {
                let msg = lifecycle.lower_to_message(call_id, tool_name);
                out.push_str(&format!("[Tool Result ({tool_name})]\n{}\n\n", msg.content));
            }
        }
    }
    out
}

/// Execute turn-aware split compaction over a fact lineage.
pub async fn compact_facts(
    provider: Arc<dyn Provider>,
    facts: &[&ThreadFact],
    cut_point: &CutPointResult,
    previous_summary: Option<&str>,
    extra_context: &[String],
) -> Result<FactPayload, String> {
    let facts_to_compact = &facts[..cut_point.first_kept_index];
    if facts_to_compact.is_empty() {
        return Err("No facts to compact".to_string());
    }

    let mut file_tracker = FileOperations::new();
    for fact in facts_to_compact {
        file_tracker.extract_from_fact(fact);
    }

    let conversation_text = serialize_facts_for_summary(facts_to_compact);
    let mut prompt_body = format!(
        "<conversation_to_compact>\n{}\n</conversation_to_compact>\n\n",
        conversation_text
    );
    if let Some(prev) = previous_summary {
        prompt_body.push_str(&format!(
            "<prior_summary>\n{}\n</prior_summary>\n\nMerge the above prior summary with the new conversation delta.\n\n",
            prev
        ));
    }
    for ctx in extra_context {
        prompt_body.push_str(&format!(
            "<additional_context>\n{}\n</additional_context>\n\n",
            ctx
        ));
    }
    prompt_body.push_str(COMPACTION_USER_INSTRUCTIONS);

    let instructions = nuo_protocol::InstructionText::fixed(COMPACTION_SYSTEM_PROMPT);
    let messages = vec![Message::new(Role::User, prompt_body)];
    let request = ModelRequest::ephemeral(messages).with_instructions(instructions);

    let response = match tokio::time::timeout(COMPACTION_TIMEOUT, provider.chat(&request)).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(err)) => return Err(format!("Compaction LLM call failed: {}", err)),
        Err(_) => return Err("Compaction LLM call timed out".to_string()),
    };

    let mut summary = response.message.content.trim().to_string();
    let file_section = file_tracker.format_markdown();
    summary.push_str(&file_section);

    let read_files: Vec<String> = file_tracker.read.into_iter().collect();
    let modified_files: Vec<String> = file_tracker.modified.into_iter().collect();

    Ok(FactPayload::Compaction {
        summary,
        first_kept_fact_id: cut_point.first_kept_fact_id.clone(),
        tokens_before: cut_point.tokens_before,
        read_files,
        modified_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::{FactContext, ThreadFact};

    fn fact(id: u64, role: Role, content: &str) -> ThreadFact {
        ThreadFact::new(
            FactId::from_raw(id.to_string()),
            (id > 1).then(|| FactId::from_raw((id - 1).to_string())),
            id,
            100 + id,
            FactPayload::Message {
                message: Message::new(role, content),
            },
            FactContext::default(),
        )
    }

    #[test]
    fn find_cut_point_never_cuts_on_observation() {
        let n1 = fact(1, Role::User, "run test");
        let n2 = fact(2, Role::Assistant, "calling bash");
        let n3 = ThreadFact::new(
            FactId::from_raw("3"),
            Some(FactId::from_raw("2")),
            3,
            103,
            FactPayload::Observation {
                call_id: "call-1".into(),
                tool_name: "bash".into(),
                output_ref: nuo_protocol::BlobRef::new("h", 1, "text/plain"),
                metrics: Default::default(),
                lifecycle: ObservationLifecycle::Raw {
                    content: "output of bash".into(),
                },
            },
            FactContext::default(),
        );
        let n4 = fact(4, Role::Assistant, "all done");
        let facts = vec![&n1, &n2, &n3, &n4];
        let cut = find_cut_point_facts(&facts, 5).unwrap();
        assert_ne!(cut.first_kept_index, 2);
    }

    #[test]
    fn find_tail_preserving_cut_point_folds_prior_rounds_and_preserves_tail() {
        let n1 = fact(1, Role::User, "round 1 question");
        let n2 = fact(2, Role::Assistant, "round 1 answer");
        let n3 = fact(3, Role::User, "round 2 question");
        let n4 = fact(4, Role::Assistant, "round 2 answer");

        let one_round = vec![&n1, &n2];
        assert!(find_tail_preserving_cut_point(&one_round, 1).is_none());

        let two_rounds = vec![&n1, &n2, &n3, &n4];
        let cut = find_tail_preserving_cut_point(&two_rounds, 1).unwrap();
        assert_eq!(cut.first_kept_index, 2);
        assert_eq!(cut.first_kept_fact_id, FactId::from_raw("3"));
        assert_eq!(cut.folded_through_seq, 2);
    }

    #[test]
    fn find_cut_point_keeps_at_most_the_budget() {
        let nodes: Vec<ThreadFact> = (1..=10u64)
            .map(|i| fact(i, Role::User, &"word ".repeat(100)))
            .collect();
        let refs: Vec<&ThreadFact> = nodes.iter().collect();
        let cut = find_cut_point_facts(&refs, 250).expect("a tenth-node budget must cut");
        let kept = refs.len() - cut.first_kept_index;
        assert!(kept <= 2, "the kept tail exceeded the budget: {kept} nodes");
    }
}
