//! Native Causal Compactor for Session IR (ADR-0255).
//!
//! Replaces legacy transcript-based compaction with native topological operations
//! on the [`SessionIR`] causal graph (`CausalGraph` and `SessionState`).
//!
//! Features:
//! - Turn-internal atomic cut point selection (never cuts on Tool Result).
//! - Deterministic file operations tracking across folded nodes.
//! - LLM structured summarization with automatic deterministic excerpt fallback.
//! - Direct horizon advancement on `SessionState.compaction_horizon`.

use super::file_tracker::FileOperations;
use super::split_compaction::{
    compact_causal_nodes, find_cut_point_nodes, find_tail_preserving_cut_point,
};
use nuo_wire::{CausalNode, NodePayload, Provider, Role, SessionIR};
use std::sync::Arc;

/// Strategy determining where the compaction boundary cuts the active lineage (ADR-0296).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionCutMode {
    /// Token budget preserving mode: preserves ~target_tokens of recent context (used for auto-compaction backstops).
    TargetTokens(usize),
    /// Tail rounds preserving mode: preserves the last N complete user rounds (used for manual /compact).
    PreserveTailRounds(usize),
}

/// Outcome of a successful causal compaction on Session IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalCompactionOutcome {
    /// ID of the newly created Compaction causal node.
    pub compaction_node_id: String,
    /// ID of the first node preserved in the active tail.
    pub first_kept_node_id: String,
    /// Number of causal nodes folded into the compaction checkpoint.
    pub nodes_folded: usize,
    /// Estimated tokens before compaction.
    pub tokens_before: usize,
    /// Generated structured summary (ADR-0296).
    pub summary: String,
    /// Artifact file paths touched across the compacted lineage (ADR-0296).
    pub tracked_files: Vec<String>,
}

/// Native engine executing causal graph compaction on Session IR.
pub struct CausalCompactor;

impl CausalCompactor {
    /// Compact the active branch of a Session IR into a structured checkpoint.
    pub async fn compact_session_ir(
        ir: &mut SessionIR,
        provider: Option<Arc<dyn Provider>>,
        cut_mode: CompactionCutMode,
        extra_context: Vec<String>,
    ) -> Result<Option<CausalCompactionOutcome>, String> {
        let Some(leaf_id) = ir.state.active_leaf.clone() else {
            return Ok(None);
        };

        let (cut_point, summary, split_parent, nodes_folded, read_files, modified_files) = {
            let active_nodes = ir.history.linear_path(&leaf_id);
            if active_nodes.is_empty() {
                return Ok(None);
            }

            // 1. Find atomic cut point according to chosen mode (ADR-0296)
            let cut_point = match cut_mode {
                CompactionCutMode::TargetTokens(target_tokens) => {
                    find_cut_point_nodes(&active_nodes, target_tokens)
                }
                CompactionCutMode::PreserveTailRounds(rounds) => {
                    find_tail_preserving_cut_point(&active_nodes, rounds).or_else(|| {
                        // When only 1 round exists, fallback to intra-round token cut if pressure exceeds 4,000 tokens
                        find_cut_point_nodes(&active_nodes, 4000)
                    })
                }
            };

            let Some(cut_point) = cut_point else {
                return Ok(None); // Context fits criteria; no compaction needed
            };

            if cut_point.first_kept_index == 0 {
                return Ok(None);
            }

            let nodes_to_fold = &active_nodes[..cut_point.first_kept_index];
            let nodes_folded = nodes_to_fold.len();

            // 2. Extract prior summary if present in folded nodes
            let previous_summary = nodes_to_fold.iter().rev().find_map(|n| {
                if let NodePayload::Compaction { summary, .. } = &n.payload {
                    Some(summary.as_str())
                } else {
                    None
                }
            });

            // 3. Track file operations across folded nodes programmatically (ADR-0255)
            let mut file_tracker = FileOperations::new();
            for node in nodes_to_fold {
                file_tracker.extract_from_node(node);
            }

            // 4. Summarize: LLM first, falling back to deterministic excerpt on failure
            let summary = match provider {
                Some(p) => {
                    match compact_causal_nodes(
                        p,
                        &active_nodes,
                        &cut_point,
                        previous_summary,
                        &extra_context,
                    )
                    .await
                    {
                        Ok(NodePayload::Compaction { summary, .. }) => summary,
                        Ok(_) => build_deterministic_causal_excerpt(
                            nodes_to_fold,
                            previous_summary,
                            &file_tracker,
                        ),
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "LLM compaction failed; falling back to deterministic excerpt compaction (ADR-0255)"
                            );
                            build_deterministic_causal_excerpt(
                                nodes_to_fold,
                                previous_summary,
                                &file_tracker,
                            )
                        }
                    }
                }
                None => build_deterministic_causal_excerpt(
                    nodes_to_fold,
                    previous_summary,
                    &file_tracker,
                ),
            };

            let split_parent = if cut_point.first_kept_index > 0 {
                nodes_to_fold.last().map(|n| n.id.clone())
            } else {
                None
            };

            let read_files: Vec<String> = file_tracker.read.into_iter().collect();
            let modified_files: Vec<String> = file_tracker.modified.into_iter().collect();

            (
                cut_point,
                summary,
                split_parent,
                nodes_folded,
                read_files,
                modified_files,
            )
        };

        // 5. Mutate Session IR Causal Graph
        let compaction_node_id = format!("compact_{}", uuid::Uuid::new_v4().simple());
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let tracked_files = {
            let mut set = std::collections::BTreeSet::new();
            for f in &read_files {
                set.insert(f.clone());
            }
            for f in &modified_files {
                set.insert(f.clone());
            }
            set.into_iter().collect::<Vec<_>>()
        };

        ir.append_compaction(
            &compaction_node_id,
            split_parent,
            now_ms,
            summary.clone(),
            cut_point.first_kept_entry_id.clone(),
            cut_point.tokens_before,
            read_files,
            modified_files,
        );

        // ADR-0275 / ADR-0278: Facts are immutable and parent edges never mutate.
        // A checkpoint references a source interval; the preserved tail's head
        // node parent_id is strictly preserved and never reparented (INV-CHK-01).

        Ok(Some(CausalCompactionOutcome {
            compaction_node_id,
            first_kept_node_id: cut_point.first_kept_entry_id,
            nodes_folded,
            tokens_before: cut_point.tokens_before,
            summary,
            tracked_files,
        }))
    }
}

/// Deterministic, zero-LLM excerpt summarizer (ADR-0255).
///
/// Collects user prompts, tool accomplishments, and file touch manifests
/// without model hallucinations or failure modes.
fn build_deterministic_causal_excerpt(
    nodes: &[&CausalNode],
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
    for node in nodes {
        if let NodePayload::Message { message } = &node.payload
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
    use nuo_wire::{Message, NodeKind, Role, SessionPolicy};

    #[tokio::test]
    async fn test_causal_compactor_deterministic_fallback() {
        let mut ir = SessionIR::new("test_session", SessionPolicy::default(), 1000);

        // Add 5 user & assistant turns
        for i in 1..=5 {
            ir.append_message(
                format!("u{i}"),
                1000 + i * 10,
                Message::new(Role::User, format!("User message {i}")),
            );
            ir.append_message(
                format!("a{i}"),
                1000 + i * 10 + 5,
                Message::new(Role::Assistant, format!("Assistant response {i}")),
            );
        }

        assert_eq!(ir.history.nodes.len(), 10);

        // Run CausalCompactor with provider = None (deterministic excerpt fallback)
        // target_tokens small so it cuts early nodes
        let outcome = CausalCompactor::compact_session_ir(
            &mut ir,
            None,
            CompactionCutMode::TargetTokens(10),
            Vec::new(),
        )
        .await
        .unwrap();

        assert!(outcome.is_some());
        let outcome = outcome.unwrap();
        assert!(outcome.nodes_folded > 0);
        assert!(!outcome.summary.is_empty());
        assert!(ir.state.compaction_horizon.is_some());
        assert_eq!(
            ir.state.compaction_horizon.as_deref(),
            Some(outcome.compaction_node_id.as_str())
        );

        // Verify compiler stops at compaction horizon
        let compiled = ir.history.linear_path_with_horizon(
            ir.state.active_leaf.as_deref().unwrap(),
            ir.state.compaction_horizon.as_deref(),
        );
        // The compiled path contains the Compaction node at its head, followed only by kept tail nodes
        assert_eq!(compiled[0].kind, NodeKind::Compaction);
    }

    #[tokio::test]
    async fn test_causal_compactor_preserve_tail_rounds() {
        let mut ir = SessionIR::new("test_manual_session", SessionPolicy::default(), 1000);

        // Add 3 complete rounds (u1/a1, u2/a2, u3/a3)
        for i in 1..=3 {
            ir.append_message(
                format!("u{i}"),
                1000 + i * 10,
                Message::new(Role::User, format!("User message {i}")),
            );
            ir.append_message(
                format!("a{i}"),
                1000 + i * 10 + 5,
                Message::new(Role::Assistant, format!("Assistant response {i}")),
            );
        }

        // Compact with manual mode: preserve 1 tail round (u3/a3)
        let outcome = CausalCompactor::compact_session_ir(
            &mut ir,
            None,
            CompactionCutMode::PreserveTailRounds(1),
            vec!["User note".to_string()],
        )
        .await
        .unwrap()
        .expect("Should compact successfully with 3 rounds when preserving 1");

        assert_eq!(outcome.first_kept_node_id, "u3");
        assert_eq!(outcome.nodes_folded, 4); // u1, a1, u2, a2
        assert!(!outcome.summary.is_empty());
    }
}
