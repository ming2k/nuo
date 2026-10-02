use serde::{Deserialize, Serialize};

/// Token and round boundaries defining resource governance for an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum context window capacity in tokens
    pub max_context_tokens: usize,
    /// Maximum completion tokens per round
    pub max_completion_tokens: usize,
    /// Maximum cognitive rounds allowed in a single session/turn
    pub max_rounds: u32,
    /// Watermark ratio triggering warning (default: 0.75)
    pub warning_watermark: f32,
    /// Watermark ratio triggering automatic history compaction (default: 0.85)
    pub compaction_watermark: f32,
    /// Watermark ratio where round aborts to avoid context overflow (default: 0.95)
    pub hard_limit_watermark: f32,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            max_completion_tokens: 4_096,
            max_rounds: 25,
            warning_watermark: 0.75,
            compaction_watermark: 0.85,
            hard_limit_watermark: 0.95,
        }
    }
}

impl TokenBudget {
    pub fn new(max_context_tokens: usize, max_rounds: u32) -> Self {
        Self {
            max_context_tokens,
            max_rounds,
            ..Default::default()
        }
    }

    /// Evaluates current pressure based on token count
    pub fn evaluate_pressure(&self, current_tokens: usize) -> PressureLevel {
        let ratio = current_tokens as f32 / self.max_context_tokens as f32;
        if ratio >= self.hard_limit_watermark {
            PressureLevel::HardLimitExceeded
        } else if ratio >= self.compaction_watermark {
            PressureLevel::CompactionRequired
        } else if ratio >= self.warning_watermark {
            PressureLevel::Warning
        } else {
            PressureLevel::Normal
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureLevel {
    Normal,
    Warning,
    CompactionRequired,
    HardLimitExceeded,
}

/// Execution mode for offloading tool results into claim-check storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OffloadMode {
    /// Preserves head and tail previews for fast model inference without rehydration.
    Partial,
    /// Elides all body text, retaining only the invoice handle and byte size metadata.
    Full,
}

/// Declarative policy governing tool output offloading and conversation compaction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompactionPolicy {
    /// Optional mode for automatic tool output offloading (`Partial` or `Full`).
    /// When set and an `ObservationStore` is configured, tool results exceeding
    /// `max_tool_output_chars` are offloaded into the store.
    pub auto_offload: Option<OffloadMode>,
    /// Maximum character length before a tool result is considered for offloading (default: 1,500 chars).
    pub max_tool_output_chars: usize,
    /// Context window utilization ratio triggering conversation compaction (default: 0.85).
    pub utilization: f32,
    /// Target utilization ratio after conversation compaction (default: 0.25).
    pub target_utilization: f32,
    /// Number of recent complete rounds preserved verbatim during conversation compaction (default: 6).
    pub preserve_rounds: usize,
    /// Whether conversation history compaction is enabled.
    pub compact: bool,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            auto_offload: Some(OffloadMode::Partial),
            max_tool_output_chars: 1_500,
            utilization: 0.85,
            target_utilization: 0.25,
            preserve_rounds: 6,
            compact: true,
        }
    }
}

impl CompactionPolicy {
    /// Disables all automatic offloading and compaction.
    pub fn none() -> Self {
        Self {
            auto_offload: None,
            compact: false,
            ..Default::default()
        }
    }
}
