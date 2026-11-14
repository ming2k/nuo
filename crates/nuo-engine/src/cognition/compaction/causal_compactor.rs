//! Native causal compactor for durable threads.
//!
//! Replaces the retired `SessionIR` compactor with topological operations on a
//! [`Thread`]'s fact DAG. A compaction appends one `Compaction` fact and moves
//! the selected branch's horizon; sibling branches keep traversing the original
//! facts.
//!
//! Features:
//! - Turn-internal atomic cut point selection (never cuts on an observation).
//! - Deterministic file-operation tracking across folded facts.
//! - LLM structured summarization with deterministic excerpt fallback.
//! - Branch-local horizon advancement only.
use super::file_tracker::FileOperations;
use super::split_compaction::{
    compact_facts, find_cut_point_facts, find_tail_preserving_cut_point,
};
use nuo_protocol::{BranchId, FactId, FactPayload, Provider, Thread};
use nuo_tool::Role;

use std::sync::Arc;

/// Strategy determining where the compaction boundary cuts the live branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionCutMode {
    /// Token budget preserving mode: preserves ~target_tokens of recent
    /// context (used for auto-compaction backstops).
    TargetTokens(usize),
    /// Tail rounds preserving mode: preserves the last N complete user rounds
    /// (used for manual `/compact`).
    PreserveTailRounds(usize),
}

/// Outcome of a successful causal compaction on a thread branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalCompactionOutcome {
    /// Identity of the newly created Compaction fact.
    pub compaction_fact_id: FactId,
    /// Identity of the first fact preserved in the active tail.
    pub first_kept_fact_id: FactId,
    /// Identity of the last folded fact.
    pub split_parent_fact_id: Option<FactId>,
    /// Number of facts folded into the compaction checkpoint.
    pub nodes_folded: usize,
    /// Sequence of the last folded fact.
    pub folded_through_seq: u64,
    /// Token weight of the folded prefix.
    pub tokens_before: usize,
    /// Generated structured summary.
    pub summary: String,
    /// Artifact file paths touched across the compacted lineage.
    pub tracked_files: Vec<String>,
    /// Files the folded range read.
    pub read_files: Vec<String>,
    /// Files the folded range modified.
    pub modified_files: Vec<String>,
}

/// Native engine executing causal graph compaction on a thread branch.
pub struct CausalCompactor;

impl CausalCompactor {
    /// Compact one branch of a thread into a structured checkpoint.
    ///
    /// The returned thread mutation is in memory. The caller commits it through
    /// the thread store; a failed summarization leaves the thread untouched.
    pub async fn compact_thread(
        thread: &mut Thread,
        branch: &BranchId,
        provider: Option<Arc<dyn Provider>>,
        cut_mode: CompactionCutMode,
        extra_context: Vec<String>,
        summary_budget: usize,
    ) -> Result<Option<CausalCompactionOutcome>, String> {
        let active_facts = thread.path(branch).map_err(|error| error.to_string())?;
        if active_facts.is_empty() {
            return Ok(None);
        }
        let horizon_fact = thread
            .latest_compaction(branch)
            .map_err(|error| error.to_string())?;

        let cut_point = match cut_mode {
            CompactionCutMode::TargetTokens(target_tokens) => {
                find_cut_point_facts(&active_facts, target_tokens)
            }
            CompactionCutMode::PreserveTailRounds(rounds) => {
                find_tail_preserving_cut_point(&active_facts, rounds).or_else(|| {
                    // When only one round exists, fall back to an intra-round
                    // token cut above 4,000 tokens.
                    find_cut_point_facts(&active_facts, 4000)
                })
            }
        };
        let Some(cut_point) = cut_point else {
            return Ok(None);
        };
        if cut_point.first_kept_index == 0 {
            return Ok(None);
        }

        let nodes_to_fold = &active_facts[..cut_point.first_kept_index];
        let nodes_folded = nodes_to_fold.len();
        let folded_through_seq = cut_point.folded_through_seq;

        let previous_summary = horizon_fact
            .and_then(|fact| match &fact.payload {
                FactPayload::Compaction { summary, .. } => Some(summary.as_str()),
                _ => None,
            })
            .or_else(|| {
                nodes_to_fold
                    .iter()
                    .rev()
                    .find_map(|fact| match &fact.payload {
                        FactPayload::Compaction { summary, .. } => Some(summary.as_str()),
                        _ => None,
                    })
            });

        let mut file_tracker = FileOperations::new();
        if let Some(FactPayload::Compaction {
            read_files,
            modified_files,
            ..
        }) = horizon_fact.map(|fact| &fact.payload)
        {
            file_tracker.read.extend(read_files.iter().cloned());
            file_tracker.modified.extend(modified_files.iter().cloned());
        }
        for fact in nodes_to_fold {
            file_tracker.extract_from_fact(fact);
        }

        let summary = match provider {
            Some(provider) => match compact_facts(
                provider,
                &active_facts,
                &cut_point,
                previous_summary,
                &extra_context,
            )
            .await
            {
                Ok(FactPayload::Compaction { summary, .. }) => summary,
                Ok(_) => {
                    build_deterministic_excerpt(nodes_to_fold, previous_summary, &file_tracker)
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "LLM compaction failed; falling back to deterministic excerpt compaction"
                    );
                    build_deterministic_excerpt(nodes_to_fold, previous_summary, &file_tracker)
                }
            },
            None => build_deterministic_excerpt(nodes_to_fold, previous_summary, &file_tracker),
        };

        let summary = if summary_budget == 0 {
            summary
        } else {
            nuo_tool::tokenizer::truncate_to_tokens(&summary, summary_budget)
                .0
                .trim_end()
                .to_string()
        };

        let split_parent_fact_id = nodes_to_fold.last().map(|fact| fact.id.clone());
        let read_files: Vec<String> = file_tracker.read.into_iter().collect();
        let modified_files: Vec<String> = file_tracker.modified.into_iter().collect();
        let tracked_files: Vec<String> = read_files
            .iter()
            .chain(modified_files.iter())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        let now_ms = crate::runtime::time::unix_epoch_ms();
        let compaction_fact_id = thread
            .compact_with_files(
                branch,
                summary.clone(),
                cut_point.first_kept_fact_id.clone(),
                cut_point.tokens_before,
                read_files.clone(),
                modified_files.clone(),
                now_ms,
            )
            .map_err(|error| error.to_string())?;

        Ok(Some(CausalCompactionOutcome {
            compaction_fact_id,
            first_kept_fact_id: cut_point.first_kept_fact_id,
            split_parent_fact_id,
            nodes_folded,
            folded_through_seq,
            tokens_before: cut_point.tokens_before,
            summary,
            tracked_files,
            read_files,
            modified_files,
        }))
    }
}

/// Deterministic, zero-LLM excerpt summarizer.
fn build_deterministic_excerpt(
    facts: &[&nuo_protocol::ThreadFact],
    previous_summary: Option<&str>,
    file_tracker: &FileOperations,
) -> String {
    let mut out = String::from("## Conversation Checkpoint (Deterministic Excerpt)\n\n");

    if let Some(prev) = previous_summary {
        out.push_str("### Prior Context\n");
        out.push_str(prev);
        out.push_str("\n\n");
    }

    out.push_str("### User Instructions & Prompts\n");
    let mut user_count = 0;
    for fact in facts {
        if let FactPayload::Message { message } = &fact.payload
            && message.role == Role::User
            && !message.content.starts_with("[Conversation")
            && !message.is_command_echo()
        {
            user_count += 1;
            let snippet = if message.content.len() > 300 {
                format!("{}...", &message.content[..300])
            } else {
                message.content.clone()
            };
            out.push_str(&format!("{}. {}\n", user_count, snippet.trim()));
        }
    }
    if user_count == 0 {
        out.push_str("- (no explicit user prompts in this segment)\n");
    }

    out.push_str(&file_tracker.format_markdown());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::{FactContext, ThreadId, ThreadStatus};
    use nuo_tool::Message;

    fn thread_with_rounds(rounds: u64) -> Thread {
        let mut thread = Thread::new(ThreadId::new("test-thread"), 1000);
        thread.state.status = ThreadStatus::Idle;
        for i in 1..=rounds {
            thread
                .append(
                    FactPayload::Message {
                        message: Message::new(Role::User, format!("User message {i}")),
                    },
                    FactContext::turn(i, 0),
                    1000 + i * 10,
                )
                .unwrap();
            thread
                .append(
                    FactPayload::Message {
                        message: Message::new(Role::Assistant, format!("Assistant response {i}")),
                    },
                    FactContext::turn(i, 1),
                    1000 + i * 10 + 5,
                )
                .unwrap();
        }
        thread
    }

    #[tokio::test]
    async fn deterministic_fallback_advances_only_its_branch() {
        let mut thread = thread_with_rounds(5);
        assert_eq!(thread.path(&BranchId::main()).unwrap().len(), 10);

        let outcome = CausalCompactor::compact_thread(
            &mut thread,
            &BranchId::main(),
            None,
            CompactionCutMode::TargetTokens(10),
            Vec::new(),
            0,
        )
        .await
        .unwrap()
        .expect("the budget must cut");

        assert!(outcome.nodes_folded > 0);
        assert!(!outcome.summary.is_empty());
        let horizon = thread
            .state
            .branch(&BranchId::main())
            .unwrap()
            .compaction_horizon
            .clone();
        assert_eq!(horizon, Some(outcome.first_kept_fact_id));
        assert!(
            thread
                .path(&BranchId::main())
                .unwrap()
                .iter()
                .any(|fact| { matches!(fact.payload, FactPayload::Compaction { .. }) })
        );
    }

    #[tokio::test]
    async fn preserve_tail_rounds_keeps_the_newest_round() {
        let mut thread = thread_with_rounds(3);
        let outcome = CausalCompactor::compact_thread(
            &mut thread,
            &BranchId::main(),
            None,
            CompactionCutMode::PreserveTailRounds(1),
            vec!["User note".to_string()],
            0,
        )
        .await
        .unwrap()
        .expect("3 rounds with preserve=1 must fold");

        assert_eq!(outcome.nodes_folded, 4);
        let path = thread.path(&BranchId::main()).unwrap();
        let kept_user = path.iter().find_map(|fact| match &fact.payload {
            FactPayload::Message { message } if message.role == Role::User => {
                Some(message.content.as_str())
            }
            _ => None,
        });
        assert_eq!(kept_user, Some("User message 3"));
    }

    #[tokio::test]
    async fn a_summary_is_bounded_by_its_budget() {
        let mut thread = Thread::new(ThreadId::new("bounded-summary"), 1000);
        for i in 1..=8 {
            thread
                .append(
                    FactPayload::Message {
                        message: Message::new(
                            Role::User,
                            format!("User message {i} {}", "filler ".repeat(40)),
                        ),
                    },
                    FactContext::default(),
                    1000 + i * 10,
                )
                .unwrap();
            thread
                .append(
                    FactPayload::Message {
                        message: Message::new(
                            Role::Assistant,
                            format!("Assistant response {i} {}", "filler ".repeat(40)),
                        ),
                    },
                    FactContext::default(),
                    1000 + i * 10 + 5,
                )
                .unwrap();
        }
        let outcome = CausalCompactor::compact_thread(
            &mut thread,
            &BranchId::main(),
            None,
            CompactionCutMode::TargetTokens(50),
            Vec::new(),
            30,
        )
        .await
        .unwrap()
        .expect("the fold must engage");
        assert!(
            nuo_tool::tokenizer::count_tokens(&outcome.summary) <= 30,
            "summary exceeded its budget: {} tokens",
            nuo_tool::tokenizer::count_tokens(&outcome.summary)
        );
    }
}
