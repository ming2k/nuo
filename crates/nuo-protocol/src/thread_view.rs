//! Disposable request projections over durable thread facts.
//!
//! A [`ThreadWindow`] is a bounded, branch-scoped slice of facts. A
//! [`RequestView`] is the model-facing message projection built from that
//! window. Neither type is durable and neither may write back into a thread.
use crate::message::Message;
use crate::observation::{ObservationLifecycle, ObservationMetrics};
use crate::thread::{
    BlobRef, BranchId, FactId, FactPayload, Thread, ThreadError, ThreadFact, ThreadRevision,
};
use crate::{Role, estimate_message_tokens};

/// Bounded branch-scoped fact window.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadWindow {
    pub facts: Vec<ThreadFact>,
    pub head: Option<FactId>,
    pub compaction_horizon: Option<FactId>,
    pub revision: ThreadRevision,
    pub oldest_loaded: Option<FactId>,
}

impl ThreadWindow {
    /// Project the last `max_facts` facts of one branch path.
    pub fn from_thread(
        thread: &Thread,
        branch: &BranchId,
        max_facts: usize,
    ) -> Result<Self, ThreadError> {
        let branch_state = thread.state.branch(branch)?;
        let mut facts: Vec<ThreadFact> = thread.path(branch)?.into_iter().cloned().collect();
        if facts.len() > max_facts.max(1) {
            // Keep the newest facts, but never drop the latest compaction
            // summary that defines the window's oldest visible point.
            let split = facts.len() - max_facts.max(1);
            let compaction = facts
                .iter()
                .rev()
                .find(|fact| matches!(fact.payload, FactPayload::Compaction { .. }))
                .cloned();
            facts = facts.split_off(split);
            if let Some(compaction) = compaction
                && !facts.iter().any(|fact| fact.id == compaction.id)
            {
                facts.insert(0, compaction);
            }
        }
        let oldest_loaded = facts.first().map(|fact| fact.id.clone());
        Ok(Self {
            facts,
            head: branch_state.head.clone(),
            compaction_horizon: branch_state.compaction_horizon.clone(),
            revision: thread.revision,
            oldest_loaded,
        })
    }
}

/// One request-time fold, kept only so telemetry can explain a shortened view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRecord {
    pub dropped_messages: usize,
    pub dropped_tokens: usize,
    pub reason: FoldReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldReason {
    Budget,
}

/// Message projection for one request.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestView {
    pub messages: Vec<Message>,
    pub revision: ThreadRevision,
    pub folded: Vec<FoldRecord>,
}

impl RequestView {
    /// Build the model-facing messages from a branch window without a token
    /// budget. Used by tests and by callers that already bounded the window.
    pub fn from_window(window: &ThreadWindow) -> Self {
        Self::from_window_with_budget(window, None)
    }

    /// Build the model-facing messages from a branch window and fold the oldest
    /// conversation turns until the estimate fits `max_tokens`.
    ///
    /// `Observation` facts are canonical tool results; this is the only place a
    /// provider-facing tool message is synthesized. A compaction summary is
    /// emitted first, once, so a folded prefix is represented without replaying
    /// the hidden facts.
    pub fn from_window_with_budget(window: &ThreadWindow, max_tokens: Option<usize>) -> Self {
        let summary = window
            .facts
            .iter()
            .rev()
            .find_map(|fact| match &fact.payload {
                FactPayload::Compaction { summary, .. } => Some(summary.clone()),
                _ => None,
            });
        let mut messages = Vec::new();
        if let Some(summary) = summary {
            let mut message = Message::new(
                Role::System,
                format!("[Context Compaction Summary]\n{summary}"),
            );
            message.hidden = true;
            messages.push(message);
        }
        for fact in &window.facts {
            match &fact.payload {
                FactPayload::Message { message } => messages.push(message.clone()),
                FactPayload::Observation {
                    call_id,
                    tool_name,
                    output_ref,
                    lifecycle,
                    ..
                } => {
                    let mut message = lifecycle.lower_to_message(call_id, tool_name);
                    if message.content.trim().is_empty()
                        && let Some(preview) = output_ref.preview.as_deref()
                    {
                        message.content = preview.to_string();
                    }
                    messages.push(message);
                }
                FactPayload::Compaction { .. }
                | FactPayload::Termination { .. }
                | FactPayload::SystemNotice { .. } => {}
            }
        }

        let mut folded = Vec::new();
        if let Some(max_tokens) = max_tokens {
            // Never fold the compaction summary; it is the representation of
            // everything already folded. Keep at least the last message.
            let floor = usize::from(!messages.is_empty() && messages[0].hidden);
            while messages.len() > floor + 1
                && messages
                    .iter()
                    .map(|message| estimate_message_tokens(message) as usize)
                    .sum::<usize>()
                    > max_tokens
            {
                let removed = messages.remove(floor);
                folded.push(FoldRecord {
                    dropped_messages: 1,
                    dropped_tokens: estimate_message_tokens(&removed) as usize,
                    reason: FoldReason::Budget,
                });
            }
        }

        Self {
            messages,
            revision: window.revision,
            folded,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Facts that a client transcript view needs without materializing every
/// observation body.
#[derive(Debug, Clone, PartialEq)]
pub struct FactSummary {
    pub id: FactId,
    pub parent_id: Option<FactId>,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: crate::thread::FactKind,
    pub context: crate::thread::FactContext,
    pub output_ref: Option<BlobRef>,
    pub metrics: Option<ObservationMetrics>,
    pub lifecycle: Option<ObservationLifecycle>,
}

impl FactSummary {
    pub fn from_fact(fact: &ThreadFact) -> Self {
        let (output_ref, metrics, lifecycle) = match &fact.payload {
            FactPayload::Observation {
                output_ref,
                metrics,
                lifecycle,
                ..
            } => (
                Some(output_ref.clone()),
                Some(*metrics),
                Some(lifecycle.clone()),
            ),
            _ => (None, None, None),
        };
        Self {
            id: fact.id.clone(),
            parent_id: fact.parent_id.clone(),
            seq: fact.seq,
            timestamp_ms: fact.timestamp_ms,
            kind: fact.kind,
            context: fact.context,
            output_ref,
            metrics,
            lifecycle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::{BranchPolicy, FactContext, ThreadId};

    fn message(text: &str) -> FactPayload {
        FactPayload::Message {
            message: Message::new(Role::User, text),
        }
    }

    fn notice(text: &str) -> FactPayload {
        FactPayload::SystemNotice {
            source: "test".to_string(),
            notice_type: "note".to_string(),
            content: text.to_string(),
        }
    }

    fn thread_with_messages() -> Thread {
        let mut thread = Thread::new(ThreadId::new("thread-1"), 1_000);
        thread
            .append(message("hello"), FactContext::default(), 1_001)
            .unwrap();
        thread
            .append(
                FactPayload::Observation {
                    call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    output_ref: BlobRef::new("abc", 12, "text/plain").with_preview("file body"),
                    metrics: ObservationMetrics::default(),
                    lifecycle: ObservationLifecycle::Raw {
                        content: "file body".to_string(),
                    },
                },
                FactContext::turn(1, 0),
                1_002,
            )
            .unwrap();
        thread
    }

    #[test]
    fn request_view_projects_messages_and_synthesizes_tool_results() {
        let thread = thread_with_messages();
        let window = ThreadWindow::from_thread(&thread, &BranchId::main(), 32).unwrap();
        let view = RequestView::from_window(&window);
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[0].role, Role::User);
        assert_eq!(view.messages[1].role, Role::Tool);
        assert_eq!(view.messages[1].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(view.messages[1].content, "file body");
    }

    #[test]
    fn compaction_summary_is_first_and_not_duplicated() {
        let mut thread = thread_with_messages();
        let path = thread.path(&BranchId::main()).unwrap();
        let first_kept = path[0].id.clone();
        thread
            .compact(
                &BranchId::main(),
                "earlier conversation",
                first_kept,
                100,
                1_003,
            )
            .unwrap();
        let window = ThreadWindow::from_thread(&thread, &BranchId::main(), 32).unwrap();
        let view = RequestView::from_window(&window);
        assert!(view.messages[0].hidden);
        assert!(view.messages[0].content.contains("earlier conversation"));
        assert_eq!(
            view.messages
                .iter()
                .filter(|message| message.content.contains("earlier conversation"))
                .count(),
            1
        );
    }

    #[test]
    fn fork_view_only_sees_its_own_branch_path() {
        let mut thread = thread_with_messages();
        let path = thread.path(&BranchId::main()).unwrap();
        let fork_point = path[0].id.clone();
        let branch = thread
            .fork(fork_point, "side", BranchPolicy::default(), 1_003)
            .unwrap();
        thread
            .append_to(&branch, notice("side"), FactContext::default(), 1_004)
            .unwrap();

        let main_window = ThreadWindow::from_thread(&thread, &BranchId::main(), 32).unwrap();
        let side_window = ThreadWindow::from_thread(&thread, &branch, 32).unwrap();
        let main = RequestView::from_window(&main_window);
        let side = RequestView::from_window(&side_window);
        assert_eq!(main.messages.len(), 2);
        assert_eq!(side.messages.len(), 1);
        assert_eq!(side.messages[0].content, "hello");
    }

    #[test]
    fn window_is_bounded() {
        let mut thread = Thread::new(ThreadId::new("thread-1"), 1_000);
        for i in 0..10 {
            thread
                .append(
                    FactPayload::Message {
                        message: Message::new(Role::User, format!("m{i}")),
                    },
                    FactContext::default(),
                    1_001 + i,
                )
                .unwrap();
        }
        let window = ThreadWindow::from_thread(&thread, &BranchId::main(), 3).unwrap();
        assert_eq!(window.facts.len(), 3);
        let view = RequestView::from_window(&window);
        assert_eq!(view.messages.len(), 3);
        assert_eq!(view.messages[2].content, "m9");
    }

    #[test]
    fn budget_folding_never_changes_the_fact_window() {
        let mut thread = Thread::new(ThreadId::new("thread-1"), 1_000);
        for i in 0..10 {
            thread
                .append(
                    FactPayload::Message {
                        message: Message::new(Role::User, format!("message number {i}")),
                    },
                    FactContext::default(),
                    1_001 + i,
                )
                .unwrap();
        }
        let window = ThreadWindow::from_thread(&thread, &BranchId::main(), 32).unwrap();
        let before = window.facts.len();
        let view = RequestView::from_window_with_budget(&window, Some(8));
        assert!(!view.folded.is_empty());
        assert_eq!(window.facts.len(), before);
        assert_eq!(thread.path(&BranchId::main()).unwrap().len(), before);
    }
}
