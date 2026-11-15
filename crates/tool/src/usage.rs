//! Per-turn token telemetry shared across the stack.
//!
//! [`TokenUsage`] is reported by every provider turn and consumed by the core
//! token ledger, the LLM client, the CLI usage meters, and the agent loop.

use serde::{Deserialize, Serialize};

/// Token usage reported by a single turn.
///
/// Per-turn telemetry.
///
/// `cache_creation_input_tokens` / `cache_read_input_tokens` carry prompt-cache
/// counts. Anthropic reports both: its `input_tokens` is ONLY the uncached
/// dynamic suffix, so the cache write/read counts must be tracked separately
/// (and added into `prompt_tokens`/`total_tokens`) or the context meter would
/// undercount every cached turn. Other routes may report reads, writes, or
/// misses independently. Protocol adapters normalize those counters through
/// the codec crate's `read_prompt_cache_usage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Tokens written to the prompt cache this turn (billed at a premium by
    /// Anthropic; absent on providers without explicit breakpoint caching).
    pub cache_creation_input_tokens: i64,
    /// Tokens served from the prompt cache this turn (billed at a steep
    /// discount by Anthropic; surfaced as `cached_tokens` /
    /// `cachedContentTokenCount` by the auto-caching providers).
    pub cache_read_input_tokens: i64,
    /// Provider-reported input tokens that missed the prompt cache. This is a
    /// provider-specific diagnostic breakout and is already included in
    /// `prompt_tokens` when the upstream reports it.
    #[serde(default)]
    pub cache_miss_input_tokens: i64,
    /// Provider-reported reasoning tokens (OpenAI Responses
    /// `output_tokens_details.reasoning_tokens`; chat-completions
    /// `completion_tokens_details.reasoning_tokens`). A diagnostic breakout
    /// that is already included in `completion_tokens` — not an addition to
    /// any total. `0` means the provider reported no such counter (Anthropic
    /// and Google never do); absence is never encoded as a fabricated zero
    /// for a provider that does report it.
    #[serde(default)]
    pub reasoning_tokens: i64,
}
