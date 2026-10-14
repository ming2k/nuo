use crate::message::{Message, Role, ToolCall};
use crate::provider::TokenUsage;
use crate::token::{PressureLevel, TokenBudget, TokenCounter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// State, history, and token metrics for a single conversational session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
    pub budget: TokenBudget,
    pub total_usage: TokenUsage,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            budget: TokenBudget::default(),
            total_usage: TokenUsage::default(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            messages: Vec::new(),
            budget: TokenBudget::default(),
            total_usage: TokenUsage::default(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.messages.insert(0, Message::system(prompt));
        self
    }

    pub fn with_budget(mut self, budget: TokenBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn add_user_message(&mut self, text: impl Into<String>) {
        self.add_message(Message::user(text));
    }

    pub fn add_assistant_message(&mut self, text: impl Into<String>) {
        self.add_message(Message::assistant(text));
    }

    pub fn add_assistant_with_thinking(
        &mut self,
        text: impl Into<String>,
        thinking: impl Into<String>,
    ) {
        self.add_message(Message::assistant(text).with_thinking(thinking));
    }

    pub fn add_assistant_tools(&mut self, text: impl Into<String>, tool_calls: Vec<ToolCall>) {
        self.add_message(Message::assistant_with_tools(text, tool_calls));
    }

    pub fn add_tool_result(
        &mut self,
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
    ) {
        self.add_message(Message::tool_response(call_id, name, output));
    }

    pub fn add_tool_error(
        &mut self,
        call_id: impl Into<String>,
        name: impl Into<String>,
        error: impl Into<String>,
    ) {
        self.add_message(Message::tool_error(call_id, name, error));
    }

    /// Computes the current estimated token length of the active context.
    pub fn current_tokens(&self) -> usize {
        TokenCounter::estimate_messages(&self.messages)
    }

    /// Evaluates current context pressure against budget.
    pub fn pressure(&self) -> PressureLevel {
        self.budget.evaluate_pressure(self.current_tokens())
    }

    /// Returns the last assistant response content if available.
    pub fn last_assistant_reply(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.content.is_empty())
            .map(|m| m.content.as_str())
    }

    /// Attaches a metadata entry.
    pub fn add_metadata(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.metadata.insert(key.into(), value);
    }

    /// Replaces the leading system prompt with a freshly built one.
    ///
    /// Called when restoring a session: the world changes while a conversation is
    /// dormant, so its view of peers and channels must be refreshed rather than
    /// frozen at creation. Any other system messages appearing later in the
    /// history (for example compaction summaries) are left untouched.
    pub fn refresh_system_prompt(&mut self, prompt: impl Into<String>) {
        let prompt = prompt.into();
        match self.messages.first_mut() {
            Some(first) if first.role == Role::System => {
                first.content = prompt;
            }
            _ => self.messages.insert(0, Message::system(prompt)),
        }
    }

    /// Explicitly compresses conversation history using the provided compactor.
    ///
    /// This is an application-level orchestration operation and is NEVER executed
    /// implicitly inside the active inference loop, preserving prompt cache prefixes
    /// and deterministic message causality.
    pub async fn compact(&mut self, compactor: &crate::token::Compactor) -> crate::error::Result<bool> {
        compactor.compact_messages(&mut self.messages, &self.budget).await
    }
}
