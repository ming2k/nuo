#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use nuo_agent::message::Message;
use nuo_agent::token::{
    CompactionPolicy, CompactionStrategy, Compactor, ObservationStore, TokenBudget,
};
use std::sync::Arc;

struct MockCausalStrategy;

#[async_trait]
impl CompactionStrategy for MockCausalStrategy {
    async fn compact(
        &self,
        _policy: &CompactionPolicy,
        messages: &mut Vec<Message>,
        _budget: &TokenBudget,
        _observation_store: Option<&Arc<dyn ObservationStore>>,
    ) -> nuo_agent::error::Result<bool> {
        // Custom strategy: keep system and user messages, compress assistant turns
        messages.retain(|m| m.role != nuo_agent::message::Role::Assistant);
        messages.push(Message::system("[Causal Compaction: folded assistant lineage]"));
        Ok(true)
    }
}

#[tokio::test]
async fn test_custom_compaction_strategy_invocation() {
    let policy = CompactionPolicy {
        compact: true,
        utilization: 0.1,
        ..Default::default()
    };

    let compactor = Compactor::new(policy).with_strategy(Arc::new(MockCausalStrategy));

    let mut messages = vec![
        Message::system("System instructions"),
        Message::user("Hello 1"),
        Message::assistant("Assistant reply 1"),
        Message::user("Hello 2"),
        Message::assistant("Assistant reply 2"),
    ];

    let budget = TokenBudget {
        max_context_tokens: 100,
        ..Default::default()
    };

    let modified = compactor
        .compact_messages(&mut messages, &budget)
        .await
        .expect("compaction execution");

    assert!(modified);
    assert_eq!(messages.len(), 4);
    assert!(messages.iter().any(|m| m.content.contains("Causal Compaction")));
    assert!(!messages.iter().any(|m| m.content.contains("Assistant reply")));
}
