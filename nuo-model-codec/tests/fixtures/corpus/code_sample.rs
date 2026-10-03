//! Static code sample fixture for tokenizer verification.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenizerState {
    Uninitialized,
    Ready {
        vocab_size: usize,
        loaded_at_ms: u64,
    },
    Failed(String),
}

pub struct TokenBudget {
    pub max_tokens: usize,
    pub warning_threshold: usize,
    pub allocated: HashMap<String, usize>,
}

impl TokenBudget {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            warning_threshold: (max_tokens * 80) / 100,
            allocated: HashMap::new(),
        }
    }

    pub fn can_admit(&self, session_id: &str, incoming_tokens: usize) -> bool {
        let current: usize = self.allocated.get(session_id).copied().unwrap_or(0);
        current + incoming_tokens <= self.max_tokens
    }

    pub fn record_usage(&mut self, session_id: &str, count: usize) {
        let entry = self.allocated.entry(session_id.to_string()).or_insert(0);
        *entry = entry.saturating_add(count);
    }
}
