//! Claim-check invoicing engine and context compaction.

use super::budget::{CompactionPolicy, OffloadMode, TokenBudget};
use super::counter::TokenCounter;
use super::observation::ObservationStore;
use crate::error::Result;
use crate::message::{Message, Role, ToolResult};
use std::sync::Arc;

/// Pluggable context compaction and message retention strategy interface.
#[async_trait::async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Executes context compaction on active dialogue messages.
    ///
    /// Returns `Ok(true)` if messages were compressed/modified.
    async fn compact(
        &self,
        policy: &CompactionPolicy,
        messages: &mut Vec<Message>,
        budget: &TokenBudget,
        observation_store: Option<&Arc<dyn ObservationStore>>,
    ) -> Result<bool>;
}

/// Standard turn-folding compaction strategy.
#[derive(Debug, Clone, Default)]
pub struct StandardCompactionStrategy;

#[async_trait::async_trait]
impl CompactionStrategy for StandardCompactionStrategy {
    async fn compact(
        &self,
        policy: &CompactionPolicy,
        messages: &mut Vec<Message>,
        budget: &TokenBudget,
        _observation_store: Option<&Arc<dyn ObservationStore>>,
    ) -> Result<bool> {
        Self::execute_standard(policy, messages, budget)
    }
}

impl StandardCompactionStrategy {
    fn execute_standard(
        policy: &CompactionPolicy,
        messages: &mut Vec<Message>,
        budget: &TokenBudget,
    ) -> Result<bool> {
        let current_tokens = TokenCounter::estimate_messages(messages);
        let ratio = current_tokens as f32 / budget.max_context_tokens as f32;

        if ratio < policy.utilization || messages.len() <= policy.preserve_rounds + 1 {
            return Ok(false);
        }

        // Identify system prefix
        let mut has_system = false;
        let mut system_msg = None;
        if let Some(first) = messages.first()
            && first.role == Role::System
        {
            has_system = true;
            system_msg = Some(first.clone());
        }

        let start_idx = if has_system { 1 } else { 0 };
        let end_idx = messages.len().saturating_sub(policy.preserve_rounds);

        if start_idx >= end_idx {
            return Ok(false);
        }

        // Build summary of evicted turns
        let evicted = &messages[start_idx..end_idx];
        let mut summary_lines = Vec::new();
        summary_lines.push("[Context Compaction Summary of Previous Rounds]:".to_string());

        for msg in evicted.iter() {
            let role_name = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
                Role::System => "System",
            };

            let snippet = if msg.content.len() > 120 {
                format!("{}...", &msg.content[..120])
            } else {
                msg.content.clone()
            };

            summary_lines.push(format!("- {role_name}: {snippet}"));
        }

        let summary_text = summary_lines.join("\n");
        let mut new_messages = Vec::new();

        if let Some(sys) = system_msg {
            new_messages.push(sys);
        }

        new_messages.push(Message::system(summary_text));
        new_messages.extend_from_slice(&messages[end_idx..]);

        *messages = new_messages;
        Ok(true)
    }
}

/// Offloads a single tool result into the observation store according to the specified mode.
///
/// - `[INV-CTX-01]`: Tool offload strictly operates on `ToolResult` outputs.
/// - `[INV-CTX-02]`: Offloaded tool results emit an immutable claim-check invoice handle (`call:<call_id>`).
/// - `[INV-CTX-03]`: `OffloadMode::Partial` retains head and tail previews; `OffloadMode::Full` retains zero body characters.
///
/// Returns the number of characters elided from context.
pub async fn offload_tool_result(
    tr: &mut ToolResult,
    mode: OffloadMode,
    store: Option<&Arc<dyn ObservationStore>>,
) -> usize {
    let total_len = tr.output.len();
    let handle = format!("call:{}", tr.call_id);

    // Offload raw output to observation store if present
    if let Some(s) = store {
        let _ = s.store(&handle, tr.output.clone()).await;
    }

    match mode {
        OffloadMode::Partial => {
            let preview_len = 150.min(total_len / 4);
            let head: String = tr.output.chars().take(preview_len).collect();
            let tail: String = tr
                .output
                .chars()
                .rev()
                .take(preview_len)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let elided = total_len.saturating_sub(preview_len * 2);

            tr.output = format!(
                "{head}\n\n[... Claim-Check Invoice: {elided} bytes elided to relieve context pressure ...]\n\
                - Invoice Handle: \"{handle}\"\n\
                - Tail Preview: {tail}\n\
                - To inspect the complete raw output, invoke: `inspect(target: \"{handle}\", offset: 0, limit: 2000)`"
            );

            elided
        }
        OffloadMode::Full => {
            tr.output = format!(
                "[Claim-Check Invoice: {total_len} bytes offloaded to store]\n\
                - Invoice Handle: \"{handle}\"\n\
                - To inspect the complete raw output, invoke: `inspect(target: \"{handle}\", offset: 0, limit: 2000)`"
            );

            total_len
        }
    }
}

/// Context compaction and offload engine.
#[derive(Clone, Default)]
pub struct Compactor {
    pub policy: CompactionPolicy,
    pub observation_store: Option<Arc<dyn ObservationStore>>,
    pub strategy: Option<Arc<dyn CompactionStrategy>>,
}

impl Compactor {
    pub fn new(policy: CompactionPolicy) -> Self {
        Self {
            policy,
            observation_store: None,
            strategy: None,
        }
    }

    pub fn with_observation_store(mut self, store: Arc<dyn ObservationStore>) -> Self {
        self.observation_store = Some(store);
        self
    }

    pub fn with_strategy(mut self, strategy: Arc<dyn CompactionStrategy>) -> Self {
        self.strategy = Some(strategy);
        self
    }

    /// Offloads oversized tool results within messages using the specified mode.
    ///
    /// Iterates over messages and offloads any `ToolResult` whose output length
    /// exceeds `max_chars`. User and assistant dialogue turns are never modified.
    ///
    /// Returns total characters elided from context.
    pub async fn offload_messages(
        &self,
        messages: &mut [Message],
        mode: OffloadMode,
        max_chars: usize,
    ) -> usize {
        let mut elided_total = 0;
        for msg in messages.iter_mut() {
            for tr in msg.tool_results.iter_mut() {
                if tr.output.len() > max_chars {
                    elided_total += offload_tool_result(
                        tr,
                        mode,
                        self.observation_store.as_ref(),
                    )
                    .await;
                }
            }
        }
        elided_total
    }

    /// Compresses a sequence of messages delegating to configured strategy or default standard compactor.
    pub async fn compact_messages(
        &self,
        messages: &mut Vec<Message>,
        budget: &TokenBudget,
    ) -> Result<bool> {
        if !self.policy.compact {
            return Ok(false);
        }

        if let Some(ref strategy) = self.strategy {
            strategy
                .compact(
                    &self.policy,
                    messages,
                    budget,
                    self.observation_store.as_ref(),
                )
                .await
        } else {
            StandardCompactionStrategy::execute_standard(&self.policy, messages, budget)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolResult;
    use crate::token::observation::InMemoryObservationStore;

    #[tokio::test]
    async fn test_partial_offload_preserves_previews_and_stores_in_observation_store() {
        let obs_store: Arc<dyn ObservationStore> = Arc::new(InMemoryObservationStore::new());
        let mut tr = ToolResult {
            call_id: "call_abc123".into(),
            name: "big_query".into(),
            output: "A".repeat(500),
            is_error: false,
        };

        let elided = offload_tool_result(&mut tr, OffloadMode::Partial, Some(&obs_store)).await;
        assert!(elided > 0);
        assert!(tr.output.contains("Claim-Check Invoice"));
        assert!(tr.output.contains("call:call_abc123"));
        assert!(tr.output.contains("Tail Preview"));

        // Verify that raw output was stored in observation store
        assert!(obs_store.contains("call:call_abc123").await);
        let slice = obs_store
            .fetch_slice("call:call_abc123", 0, 10)
            .await
            .unwrap();
        assert_eq!(slice, "AAAAAAAAAA");
    }

    #[tokio::test]
    async fn test_full_offload_elides_body_and_stores_in_observation_store() {
        let obs_store: Arc<dyn ObservationStore> = Arc::new(InMemoryObservationStore::new());
        let mut tr = ToolResult {
            call_id: "call_full99".into(),
            name: "giant_log".into(),
            output: "X".repeat(1000),
            is_error: false,
        };

        let elided = offload_tool_result(&mut tr, OffloadMode::Full, Some(&obs_store)).await;
        assert_eq!(elided, 1000);
        assert!(tr.output.contains("[Claim-Check Invoice: 1000 bytes offloaded to store]"));
        assert!(tr.output.contains("call:call_full99"));
        // Full mode MUST NOT contain head or tail preview
        assert!(!tr.output.contains("Tail Preview"));

        // Raw output still 100% accessible via observation store
        assert!(obs_store.contains("call:call_full99").await);
        let slice = obs_store
            .fetch_slice("call:call_full99", 0, 5)
            .await
            .unwrap();
        assert_eq!(slice, "XXXXX");
    }

    #[tokio::test]
    async fn test_offload_messages_strictly_targets_tools_and_preserves_dialogue() {
        let obs_store = Arc::new(InMemoryObservationStore::new());
        let policy = CompactionPolicy {
            auto_offload: Some(OffloadMode::Partial),
            max_tool_output_chars: 100,
            ..Default::default()
        };
        let compactor = Compactor::new(policy).with_observation_store(obs_store.clone());

        let user_content = "Please analyze this large dump: ".to_string() + &"U".repeat(500);
        let mut user_msg = Message::user(&user_content);
        user_msg.tool_results.push(ToolResult {
            call_id: "call_tool_1".into(),
            name: "dump".into(),
            output: "T".repeat(300),
            is_error: false,
        });

        let mut messages = vec![user_msg];
        let elided = compactor
            .offload_messages(&mut messages, OffloadMode::Partial, 100)
            .await;

        assert!(elided > 0);
        // User dialogue turn must remain 100% untouched ([INV-CTX-01])
        assert_eq!(messages[0].content, user_content);
        // Only tool result was offloaded
        assert!(messages[0].tool_results[0].output.contains("call:call_tool_1"));
    }
}
