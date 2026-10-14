//! One versioned runtime context policy (ADR-0280). Legacy conversion belongs
//! exclusively to the offline migration tool.
//!
//! The legacy ratios are fractions of the model window `W`; the new watermarks
//! are fractions of the input ceiling `I = W - O - F`. A rename therefore
//! changes behavior unless the number is remapped, so the offline migrator —
//! not the runtime — converts values, and the runtime rejects legacy keys.

use crate::context_lifecycle::budget::{AdmissionError, WatermarkPolicy};

/// Schema version stamped onto every persisted `context` policy.
///
/// The runtime rejects a policy whose version it does not know, and keeps no
/// synonym aliases for older keys (`INV-POLICY-01`).
pub const CONTEXT_POLICY_SCHEMA_VERSION: u32 = 1;

/// Default capture chunk size: 64 KiB.
pub const DEFAULT_CAPTURE_CHUNK_BYTES: u64 = 64 * 1024;
/// Default capture queue capacity, in chunks, per active stream.
pub const DEFAULT_CAPTURE_QUEUE_CHUNKS: u64 = 16;
/// Default per-execution capture ceiling: 256 MiB.
pub const DEFAULT_CAPTURE_BYTES_PER_EXECUTION: u64 = 256 * 1024 * 1024;
/// Default per-session artifact quota: 2 GiB (logically referenced bytes).
pub const DEFAULT_SESSION_ARTIFACT_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Default TTL for non-fact attempt diagnostics: 24 hours.
pub const DEFAULT_ATTEMPT_DIAGNOSTIC_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
/// Default inspect page ceiling, in tokens.
pub const DEFAULT_INSPECT_PAGE_TOKENS: u64 = 2048;
/// Maximum inspect page ceiling, in tokens.
pub const MAX_INSPECT_PAGE_TOKENS: u64 = 8192;
/// Default single-checkpoint-call timeout: 45 s.
pub const DEFAULT_CHECKPOINT_CALL_TIMEOUT_MS: u64 = 45_000;
/// Default total checkpoint-build budget: 120 s.
pub const DEFAULT_CHECKPOINT_TOTAL_TIMEOUT_MS: u64 = 120_000;
/// Default GC batch object ceiling.
pub const DEFAULT_GC_BATCH_OBJECTS: u32 = 1000;
/// Default GC batch time ceiling, in milliseconds.
pub const DEFAULT_GC_BATCH_MS: u64 = 100;

/// The single versioned context policy (ADR-0280 §1–§2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    /// Schema version of this policy record.
    pub schema_version: u32,
    /// Soft/hard/target watermarks as fractions of `I`.
    pub watermarks: WatermarkPolicy,
    /// Rounds the planner *prefers* to keep; never a hard protection quantity.
    pub preferred_recent_rounds: u32,
    pub checkpoint_enabled: bool,
    pub lightweight_degradation_enabled: bool,
    pub fallback_window_tokens: u64,
    pub recent_observation_protect_tokens: u64,
    pub checkpoint_max_calls: u32,
    pub checkpoint_max_reduction_depth: u32,
    /// Raw-artifact capture chunk size.
    pub capture_chunk_bytes: u64,
    /// Raw-artifact capture queue capacity, in chunks.
    pub capture_queue_chunks: u64,
    /// Per-execution raw capture ceiling.
    pub capture_bytes_per_execution: u64,
    /// Per-session artifact quota, in logically referenced bytes.
    pub session_artifact_quota_bytes: u64,
    /// Non-fact attempt-diagnostic TTL.
    pub attempt_diagnostic_ttl_ms: u64,
    /// Default inspect page size.
    pub inspect_page_tokens: u64,
    /// A single checkpoint call's timeout.
    pub checkpoint_call_timeout_ms: u64,
    /// The whole checkpoint build's timeout.
    pub checkpoint_total_timeout_ms: u64,
    /// GC objects per batch.
    pub gc_batch_objects: u32,
    /// GC milliseconds per batch.
    pub gc_batch_ms: u64,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            schema_version: CONTEXT_POLICY_SCHEMA_VERSION,
            watermarks: WatermarkPolicy::DEFAULT,
            preferred_recent_rounds: 6,
            checkpoint_enabled: true,
            lightweight_degradation_enabled: true,
            fallback_window_tokens: 32_000,
            recent_observation_protect_tokens: 6_000,
            checkpoint_max_calls: 16,
            checkpoint_max_reduction_depth: 4,
            capture_chunk_bytes: DEFAULT_CAPTURE_CHUNK_BYTES,
            capture_queue_chunks: DEFAULT_CAPTURE_QUEUE_CHUNKS,
            capture_bytes_per_execution: DEFAULT_CAPTURE_BYTES_PER_EXECUTION,
            session_artifact_quota_bytes: DEFAULT_SESSION_ARTIFACT_QUOTA_BYTES,
            attempt_diagnostic_ttl_ms: DEFAULT_ATTEMPT_DIAGNOSTIC_TTL_MS,
            inspect_page_tokens: DEFAULT_INSPECT_PAGE_TOKENS,
            checkpoint_call_timeout_ms: DEFAULT_CHECKPOINT_CALL_TIMEOUT_MS,
            checkpoint_total_timeout_ms: DEFAULT_CHECKPOINT_TOTAL_TIMEOUT_MS,
            gc_batch_objects: DEFAULT_GC_BATCH_OBJECTS,
            gc_batch_ms: DEFAULT_GC_BATCH_MS,
        }
    }
}

/// A policy invariant was violated by an operator-supplied value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyError {
    /// A finite resource limit is zero or cannot be represented.
    InvalidLimit { name: String },
    /// A watermark ordering rule failed.
    Watermarks(AdmissionError),
    /// The per-execution capture ceiling exceeds the session quota.
    CaptureExceedsSessionQuota {
        /// The capture ceiling.
        per_execution: u64,
        /// The session quota.
        session_quota: u64,
    },
    /// A capture chunk size of zero is meaningless.
    ZeroCaptureChunkBytes,
    /// The inspect page ceiling exceeds the hard maximum.
    InspectPageTooLarge {
        /// The requested page size.
        requested: u64,
    },
    /// Total checkpoint budget is below a single call's timeout.
    CheckpointBudgetBelowCallTimeout {
        /// The total budget.
        total: u64,
        /// The single-call timeout.
        call: u64,
    },
    /// The stamped schema version is not the one this runtime knows.
    UnknownSchemaVersion {
        /// The version found on the record.
        found: u32,
        /// The version this runtime understands.
        expected: u32,
    },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimit { name } => write!(f, "invalid finite context limit: {name}"),
            PolicyError::Watermarks(e) => write!(f, "watermark policy invalid: {e}"),
            PolicyError::CaptureExceedsSessionQuota {
                per_execution,
                session_quota,
            } => write!(
                f,
                "per-execution capture {per_execution} exceeds session quota {session_quota}"
            ),
            PolicyError::ZeroCaptureChunkBytes => write!(f, "capture chunk size must be non-zero"),
            PolicyError::InspectPageTooLarge { requested } => write!(
                f,
                "inspect page {requested} exceeds max {MAX_INSPECT_PAGE_TOKENS}"
            ),
            PolicyError::CheckpointBudgetBelowCallTimeout { total, call } => write!(
                f,
                "checkpoint total budget {total} is below a single call timeout {call}"
            ),
            PolicyError::UnknownSchemaVersion { found, expected } => write!(
                f,
                "unknown context policy schema version {found} (expected {expected})"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

impl ContextPolicy {
    /// Validate every size setting and its relations (ADR-0280 §2).
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.schema_version != CONTEXT_POLICY_SCHEMA_VERSION {
            return Err(PolicyError::UnknownSchemaVersion {
                found: self.schema_version,
                expected: CONTEXT_POLICY_SCHEMA_VERSION,
            });
        }
        for (name, value) in [
            ("capture_queue_chunks", self.capture_queue_chunks),
            (
                "capture_bytes_per_execution",
                self.capture_bytes_per_execution,
            ),
            (
                "session_artifact_quota_bytes",
                self.session_artifact_quota_bytes,
            ),
            (
                "checkpoint_call_timeout_ms",
                self.checkpoint_call_timeout_ms,
            ),
            ("checkpoint_max_calls", self.checkpoint_max_calls as u64),
            (
                "checkpoint_max_reduction_depth",
                self.checkpoint_max_reduction_depth as u64,
            ),
            ("gc_batch_objects", self.gc_batch_objects as u64),
            ("gc_batch_ms", self.gc_batch_ms),
            ("fallback_window_tokens", self.fallback_window_tokens),
        ] {
            if value == 0 {
                return Err(PolicyError::InvalidLimit { name: name.into() });
            }
        }
        if self
            .capture_chunk_bytes
            .checked_mul(self.capture_queue_chunks)
            .is_none()
        {
            return Err(PolicyError::InvalidLimit {
                name: "capture queue capacity".into(),
            });
        }
        self.watermarks
            .validate()
            .map_err(PolicyError::Watermarks)?;
        if self.capture_chunk_bytes == 0 {
            return Err(PolicyError::ZeroCaptureChunkBytes);
        }
        if self.capture_bytes_per_execution > self.session_artifact_quota_bytes {
            return Err(PolicyError::CaptureExceedsSessionQuota {
                per_execution: self.capture_bytes_per_execution,
                session_quota: self.session_artifact_quota_bytes,
            });
        }
        if self.inspect_page_tokens > MAX_INSPECT_PAGE_TOKENS || self.inspect_page_tokens == 0 {
            return Err(PolicyError::InspectPageTooLarge {
                requested: self.inspect_page_tokens,
            });
        }
        if self.checkpoint_total_timeout_ms < self.checkpoint_call_timeout_ms {
            return Err(PolicyError::CheckpointBudgetBelowCallTimeout {
                total: self.checkpoint_total_timeout_ms,
                call: self.checkpoint_call_timeout_ms,
            });
        }
        Ok(())
    }

    /// Pending bytes a single active stream pipe may hold: chunk × queue.
    pub const fn capture_pending_bytes_per_stream(&self) -> u64 {
        self.capture_chunk_bytes
            .saturating_mul(self.capture_queue_chunks)
    }

    /// Resolve model window into runtime context budget thresholds (ADR-0031).
    pub fn resolve_budget(&self, window_tokens: usize) -> crate::ContextBudget {
        let window = if window_tokens == 0 {
            self.fallback_window_tokens as usize
        } else {
            window_tokens
        }
        .max(1);
        let threshold = |fraction: f64| (window as f64 * fraction) as usize;
        let quantum_floor = 512.max(threshold(0.05));
        let soft_frac = self.watermarks.soft_bp as f64 / 10_000.0;
        let hard_frac = self.watermarks.hard_bp as f64 / 10_000.0;
        let target_frac = self.watermarks.target_bp as f64 / 10_000.0;
        crate::ContextBudget {
            window_tokens: window,
            prune_threshold_tokens: threshold(soft_frac),
            compaction_threshold_tokens: threshold(hard_frac),
            target_tokens: threshold(target_frac),
            quantum_floor_tokens: quantum_floor,
            cruise_low_tokens: threshold(soft_frac * 0.8),
        }
    }
}

/// A legacy key the runtime refuses (ADR-0280 §1, `INV-POLICY-01`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LegacyKeyRejected {
    /// The offending key.
    pub key: String,
    /// The conversion hint.
    pub hint: String,
}

/// Reject a legacy runtime key with a conversion hint (`INV-POLICY-01`).
pub fn reject_legacy_runtime_key(key: &str) -> Option<LegacyKeyRejected> {
    if key.starts_with("compaction.") || key == "compaction_preserve_turns" {
        Some(LegacyKeyRejected {
            key: key.into(),
            hint: "run the offline `context` policy migrator; the runtime accepts only the versioned `context.*` policy".into(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_valid() {
        ContextPolicy::default().validate().unwrap();
    }

    #[test]
    fn relations_are_enforced() {
        // per-execution ceiling above the session quota.
        let p = ContextPolicy {
            capture_bytes_per_execution: ContextPolicy::default().session_artifact_quota_bytes + 1,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::CaptureExceedsSessionQuota { .. }
        ));

        let p = ContextPolicy {
            inspect_page_tokens: MAX_INSPECT_PAGE_TOKENS + 1,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::InspectPageTooLarge { .. }
        ));

        let p = ContextPolicy {
            checkpoint_call_timeout_ms: ContextPolicy::default().checkpoint_total_timeout_ms + 1,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::CheckpointBudgetBelowCallTimeout { .. }
        ));

        let p = ContextPolicy {
            schema_version: 999,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            p.validate().unwrap_err(),
            PolicyError::UnknownSchemaVersion { .. }
        ));
    }

    #[test]
    fn runtime_rejects_legacy_keys_with_a_hint() {
        let r = reject_legacy_runtime_key("compaction.utilization").unwrap();
        assert!(r.hint.contains("migrator"));
        assert!(reject_legacy_runtime_key("compaction_preserve_turns").is_some());
        assert!(reject_legacy_runtime_key("context.hard_watermark").is_none());
    }

    #[test]
    fn capture_pending_bytes_are_bounded() {
        let p = ContextPolicy::default();
        assert_eq!(p.capture_pending_bytes_per_stream(), 64 * 1024 * 16);
    }
}
