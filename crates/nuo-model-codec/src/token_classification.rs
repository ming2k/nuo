//! Objective token classification and distribution breakdown (ADR-0066).
//!
//! Provides pure physical telemetry facts (`[INV-CODEC-01]`) by normalizing heterogenous
//! provider counters (Gemini, Claude, OpenAI) into unambiguous, non-overlapping classes.
//! Contains zero monetary or pricing concepts.

use serde::{Deserialize, Serialize};
use nuo_tool::usage::TokenUsage;

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

/// Normalized token distribution across semantic classes reported by wire protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenBreakdown {
    /// Net uncached standard input tokens.
    pub uncached_input: u64,
    /// Tokens read from prompt cache.
    pub cache_read: u64,
    /// Tokens written to create or retain prompt cache.
    pub cache_write: u64,
    /// Net standard generation output tokens (excluding reasoning).
    pub standard_output: u64,
    /// Reasoning / extended thinking tokens.
    pub reasoning_output: u64,
    /// Audio input tokens.
    pub audio_input: u64,
}

impl TokenBreakdown {
    /// Normalize an upstream [`TokenUsage`] into an unambiguous, non-overlapping breakdown.
    ///
    /// Resolves the upstream semantic ambiguity where some providers (Gemini, OpenAI) include
    /// cached tokens in `prompt_tokens`, while others (Anthropic) report only the uncached suffix.
    pub fn from_usage(usage: &TokenUsage) -> Self {
        let cache_read = usage.cache_read_input_tokens.max(0) as u64;
        let cache_write = usage.cache_creation_input_tokens.max(0) as u64;
        let prompt_total = usage.prompt_tokens.max(0) as u64;

        // Disambiguate prompt tokens: net uncached input is prompt_total minus cached & written.
        let uncached_input = prompt_total
            .saturating_sub(cache_read)
            .saturating_sub(cache_write);

        let reasoning_output = usage.reasoning_tokens.max(0) as u64;
        let completion_total = usage.completion_tokens.max(0) as u64;
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
        // Gemini/OpenAI style: prompt_tokens=100k, cachedContentTokenCount=80k
        let usage = TokenUsage {
            prompt_tokens: 100_000,
            completion_tokens: 2_000,
            total_tokens: 102_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 80_000,
            cache_miss_input_tokens: 20_000,
            reasoning_tokens: 500,
        };

        let breakdown = TokenBreakdown::from_usage(&usage);
        // Net uncached input must be 20k (not 100k) to prevent double counting
        assert_eq!(breakdown.uncached_input, 20_000);
        assert_eq!(breakdown.cache_read, 80_000);
        assert_eq!(breakdown.standard_output, 1_500);
        assert_eq!(breakdown.reasoning_output, 500);
        assert_eq!(breakdown.total_input(), 100_000);
        assert_eq!(breakdown.total_output(), 2_000);
    }
}
