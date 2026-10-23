//! Objective token classification and distribution breakdown (ADR-0066).
//!
//! Provides pure physical telemetry facts (`[INV-CODEC-01]`) by normalizing heterogenous
//! provider counters (Gemini, Claude, OpenAI) into unambiguous, non-overlapping classes.
//! Contains zero monetary or pricing concepts.

use serde::{Deserialize, Serialize};

/// Semantic classification of tokens crossing provider wire protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenClass {
    /// Uncached standard prompt input text tokens.
    UncachedInput,
    /// Prompt tokens served from cache (read discount tier).
    CacheRead,
    /// Prompt tokens written to context cache (creation surcharge tier).
    CacheWrite,
    /// Standard generated assistant completion tokens.
    StandardOutput,
    /// Extended thinking / deep reasoning generation tokens.
    ReasoningOutput,
    /// Audio modality input tokens.
    AudioInput,
}

/// Canonical, de-duplicated distribution of billable token counts across standardized classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenBreakdown {
    /// Net uncached input text tokens.
    pub uncached_input: u64,
    /// Cached input prompt tokens read from storage.
    pub cache_read: u64,
    /// Input prompt tokens written to context cache.
    pub cache_write: u64,
    /// Standard completion tokens generated.
    pub standard_output: u64,
    /// Thinking / deep reasoning tokens generated.
    pub reasoning_output: u64,
    /// Audio input tokens.
    pub audio_input: u64,
}

impl TokenBreakdown {
    /// Normalize raw token counters into an unambiguous, non-overlapping breakdown.
    ///
    /// Resolves the upstream semantic ambiguity where some providers (Gemini, OpenAI) include
    /// cached tokens in `prompt_tokens`, while others (Anthropic) report only the uncached suffix.
    pub fn from_counts(
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
        reasoning_tokens: i64,
    ) -> Self {
        let cache_read = cache_read_input_tokens.max(0) as u64;
        let cache_write = cache_creation_input_tokens.max(0) as u64;
        let prompt_total = prompt_tokens.max(0) as u64;

        // Disambiguate prompt tokens: net uncached input is prompt_total minus cached & written.
        let uncached_input = prompt_total
            .saturating_sub(cache_read)
            .saturating_sub(cache_write);

        let reasoning_output = reasoning_tokens.max(0) as u64;
        let completion_total = completion_tokens.max(0) as u64;
        let standard_output = completion_total.saturating_sub(reasoning_output);

        Self {
            uncached_input,
            cache_read,
            cache_write,
            standard_output,
            reasoning_output,
            audio_input: 0,
        }
    }

    /// Total billable input tokens (uncached + cached + cache writes).
    pub fn total_input(&self) -> u64 {
        self.uncached_input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Total billable output tokens (standard + reasoning).
    pub fn total_output(&self) -> u64 {
        self.standard_output.saturating_add(self.reasoning_output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_openai_inclusive_prompt_normalization() {
        let breakdown = TokenBreakdown::from_counts(100_000, 2_000, 80_000, 0, 500);
        assert_eq!(breakdown.uncached_input, 20_000);
        assert_eq!(breakdown.cache_read, 80_000);
        assert_eq!(breakdown.standard_output, 1_500);
        assert_eq!(breakdown.reasoning_output, 500);
        assert_eq!(breakdown.total_input(), 100_000);
        assert_eq!(breakdown.total_output(), 2_000);
    }
}
