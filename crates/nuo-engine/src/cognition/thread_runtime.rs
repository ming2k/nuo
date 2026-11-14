//! Bounded runtime projection of a durable `Thread`.
//!
//! `ThreadRuntime` is per `(thread, branch)`. It holds a bounded `ThreadWindow`
//! of facts and a `RequestView` of messages. It does not own the full thread,
//! blob bytes, sibling branches, or request context. The window and view are
//! pure projections: they are rebuilt from `ThreadStore` and never write back.
pub use nuo_protocol::{RequestView, ThreadWindow};

use nuo_protocol::{BranchId, FactId, Thread, ThreadError, ThreadId, ThreadRevision};

/// Bounded runtime projection for one `(thread, branch)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRuntime {
    pub thread_id: ThreadId,
    pub branch_id: BranchId,
    pub window: ThreadWindow,
    pub view: RequestView,
}

impl ThreadRuntime {
    pub fn from_thread(
        thread: &Thread,
        branch: &BranchId,
        max_facts: usize,
    ) -> Result<Self, ThreadError> {
        Self::from_thread_with_budget(thread, branch, max_facts, None)
    }

    pub fn from_thread_with_budget(
        thread: &Thread,
        branch: &BranchId,
        max_facts: usize,
        max_tokens: Option<usize>,
    ) -> Result<Self, ThreadError> {
        let window = ThreadWindow::from_thread(thread, branch, max_facts)?;
        let view = RequestView::from_window_with_budget(&window, max_tokens);
        Ok(Self {
            thread_id: thread.id.clone(),
            branch_id: branch.clone(),
            window,
            view,
        })
    }

    /// Rebuild the window and view after a thread mutation.
    pub fn refresh(
        &mut self,
        thread: &Thread,
        max_facts: usize,
        max_tokens: Option<usize>,
    ) -> Result<(), ThreadError> {
        if thread.id != self.thread_id {
            return Err(ThreadError::InvalidFact {
                fact: FactId::from_raw("<runtime>"),
                reason: "thread id does not match the runtime".to_string(),
            });
        }
        self.window = ThreadWindow::from_thread(thread, &self.branch_id, max_facts)?;
        self.view = RequestView::from_window_with_budget(&self.window, max_tokens);
        Ok(())
    }

    pub fn revision(&self) -> ThreadRevision {
        self.view.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::{
        BlobRef, FactContext, FactPayload, Message, ObservationLifecycle, ObservationMetrics, Role,
    };

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
        let runtime = ThreadRuntime::from_thread(&thread, &BranchId::main(), 32).unwrap();
        assert_eq!(runtime.view.messages.len(), 2);
        assert_eq!(runtime.view.messages[0].role, Role::User);
        assert_eq!(runtime.view.messages[1].role, Role::Tool);
        assert_eq!(
            runtime.view.messages[1].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(runtime.view.messages[1].content, "file body");
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
        let runtime = ThreadRuntime::from_thread(&thread, &BranchId::main(), 32).unwrap();
        assert!(runtime.view.messages[0].hidden);
        assert!(
            runtime.view.messages[0]
                .content
                .contains("earlier conversation")
        );
        assert_eq!(
            runtime
                .view
                .messages
                .iter()
                .filter(|message| message.content.contains("earlier conversation"))
                .count(),
            1
        );
    }

    #[test]
    fn fork_runtime_only_sees_its_own_branch_path() {
        let mut thread = thread_with_messages();
        let path = thread.path(&BranchId::main()).unwrap();
        let fork_point = path[0].id.clone();
        let branch = thread
            .fork(
                fork_point,
                "side",
                nuo_protocol::BranchPolicy::default(),
                1_003,
            )
            .unwrap();
        thread
            .append_to(&branch, notice("side"), FactContext::default(), 1_004)
            .unwrap();

        let main = ThreadRuntime::from_thread(&thread, &BranchId::main(), 32).unwrap();
        let side = ThreadRuntime::from_thread(&thread, &branch, 32).unwrap();
        assert_eq!(main.view.messages.len(), 2);
        assert_eq!(side.view.messages.len(), 1);
        assert_eq!(side.view.messages[0].content, "hello");
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
        let runtime = ThreadRuntime::from_thread(&thread, &BranchId::main(), 3).unwrap();
        assert_eq!(runtime.window.facts.len(), 3);
        assert_eq!(runtime.view.messages.len(), 3);
        assert_eq!(runtime.view.messages[2].content, "m9");
    }

    #[test]
    fn refresh_uses_the_new_revision() {
        let mut thread = thread_with_messages();
        let mut runtime = ThreadRuntime::from_thread(&thread, &BranchId::main(), 32).unwrap();
        let before = runtime.view.revision;
        thread
            .append(message("next"), FactContext::default(), 1_003)
            .unwrap();
        runtime.refresh(&thread, 32, None).unwrap();
        assert!(runtime.view.revision > before);
        assert_eq!(runtime.view.messages.last().unwrap().content, "next");
    }
}
