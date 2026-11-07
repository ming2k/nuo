//! One versioned runtime context policy. Legacy conversion belongs
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
/// Default effective input ceiling, in tokens.
///
/// A model's advertised context window is a *hard* ceiling, not a performance
/// target: prefill and attention cost scale with the tokens actually sent, so a
/// 1M-token route that never compacts turns every ReAct turn into a multi-second
/// wait. The policy derives every watermark from the **effective** window
/// `min(declared_window, max_input_tokens)`, which keeps the working set inside
/// the region where a turn stays interactive.
pub const DEFAULT_MAX_INPUT_TOKENS: u64 = 128_000;
/// Default horizon, in tokens, for tool-result retention.
///
/// Tool results younger than this (measured from the tail) are kept in a bounded
/// projected shape; older ones are cleared to an informative placeholder. The
/// newest [`ContextPolicy::recent_observation_protect_tokens`] stay verbatim.
pub const DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS: u64 = 24_000;
/// Default verbatim window for the newest tool output, in tokens.
pub const DEFAULT_RECENT_OBSERVATION_PROTECT_TOKENS: u64 = 6_000;
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

/// The single versioned context policy.
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
    /// Hard cap on the effective input window used for budget derivation.
    ///
    /// Watermarks are fractions of this capped window, never of the model's
    /// advertised maximum. Without it, a 1M-token route compacts at 850k tokens
    /// and every turn pays the prefill cost of a small book.
    #[serde(default = "default_max_input_tokens")]
    pub max_input_tokens: u64,
    pub recent_observation_protect_tokens: u64,
    /// Absolute token horizon for tool-result retention, measured from the tail
    /// of the projected request. Tool results beyond this horizon are cleared to
    /// an informative placeholder.
    #[serde(default = "default_tool_output_retention_tokens")]
    pub tool_output_retention_tokens: u64,
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
            max_input_tokens: DEFAULT_MAX_INPUT_TOKENS,
            recent_observation_protect_tokens: DEFAULT_RECENT_OBSERVATION_PROTECT_TOKENS,
            tool_output_retention_tokens: DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS,
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
    /// The tool-output retention horizon is shorter than the verbatim window.
    RetentionBelowProtect {
        /// The configured retention horizon.
        retention: u64,
        /// The configured verbatim window.
        protect: u64,
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
            PolicyError::RetentionBelowProtect { retention, protect } => write!(
                f,
                "tool-output retention horizon {retention} is below the verbatim window {protect}"
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
    /// Validate every size setting and its relations.
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
            ("max_input_tokens", self.max_input_tokens),
            (
                "recent_observation_protect_tokens",
                self.recent_observation_protect_tokens,
            ),
            (
                "tool_output_retention_tokens",
                self.tool_output_retention_tokens,
            ),
        ] {
            if value == 0 {
                return Err(PolicyError::InvalidLimit { name: name.into() });
            }
        }
        if self.tool_output_retention_tokens < self.recent_observation_protect_tokens {
            return Err(PolicyError::RetentionBelowProtect {
                retention: self.tool_output_retention_tokens,
                protect: self.recent_observation_protect_tokens,
            });
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
    ///
    /// The declared window is capped by [`Self::max_input_tokens`] before any
    /// watermark is derived. The cap is what keeps a 1M-token route from
    /// advertising a 850k-token compaction threshold it can never usefully
    /// reach: `W` is a capability ceiling, while the budget is a *performance*
    /// contract.
    pub fn resolve_budget(&self, window_tokens: usize) -> crate::ContextBudget {
        let declared = if window_tokens == 0 {
            self.fallback_window_tokens
        } else {
            u64::try_from(window_tokens).unwrap_or(u64::MAX)
        };
        let window = declared.min(self.max_input_tokens).max(1);
        let threshold = |fraction: f64| (window as f64 * fraction) as usize;
        let quantum_floor = 512.max(threshold(0.05));
        let soft_frac = self.watermarks.soft_bp as f64 / 10_000.0;
        let hard_frac = self.watermarks.hard_bp as f64 / 10_000.0;
        let target_frac = self.watermarks.target_bp as f64 / 10_000.0;
        crate::ContextBudget {
            window_tokens: window as usize,
            prune_threshold_tokens: threshold(soft_frac),
            compaction_threshold_tokens: threshold(hard_frac),
            target_tokens: threshold(target_frac),
            quantum_floor_tokens: quantum_floor,
            cruise_low_tokens: threshold(soft_frac * 0.8),
        }
    }
}

fn default_max_input_tokens() -> u64 {
    DEFAULT_MAX_INPUT_TOKENS
}

fn default_tool_output_retention_tokens() -> u64 {
    DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS
}

/// A legacy key the runtime refuses (`INV-POLICY-01`).
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

    #[test]
    fn declared_window_is_capped_by_the_effective_input_ceiling() {
        let policy = ContextPolicy::default();
        // A 1M-token route is capped at the policy's 128k working set: the
        // watermarks are fractions of the *effective* window, so compaction
        // engages while turns are still interactive.
        let budget = policy.resolve_budget(1_000_000);
        assert_eq!(budget.window_tokens, 128_000);
        assert_eq!(budget.prune_threshold_tokens, 83_200); // 65%
        assert_eq!(budget.compaction_threshold_tokens, 108_800); // 85%
        assert_eq!(budget.target_tokens, 64_000); // 50%

        // A route narrower than the cap keeps its own window.
        let narrow = policy.resolve_budget(64_000);
        assert_eq!(narrow.window_tokens, 64_000);
        assert_eq!(narrow.prune_threshold_tokens, 41_600);

        // An unknown window still falls back, then caps.
        let unknown = policy.resolve_budget(0);
        assert_eq!(unknown.window_tokens, 32_000);
    }

    #[test]
    fn a_retention_horizon_below_the_verbatim_window_is_rejected() {
        let policy = ContextPolicy {
            recent_observation_protect_tokens: 8_000,
            tool_output_retention_tokens: 6_000,
            ..ContextPolicy::default()
        };
        assert!(matches!(
            policy.validate().unwrap_err(),
            PolicyError::RetentionBelowProtect {
                retention: 6_000,
                protect: 8_000,
            }
        ));
    }

    #[test]
    fn new_limits_default_when_absent_from_an_existing_policy_document() {
        // A v1 policy document written before the performance ceiling existed
        // still parses; the new knobs take their documented defaults rather than
        // silently inheriting a 1M-token working set.
        let source = r#"
            schema_version = 1
            preferred_recent_rounds = 6
            checkpoint_enabled = true
            lightweight_degradation_enabled = true
            fallback_window_tokens = 32000
            recent_observation_protect_tokens = 6000
            checkpoint_max_calls = 16
            checkpoint_max_reduction_depth = 4
            capture_chunk_bytes = 65536
            capture_queue_chunks = 16
            capture_bytes_per_execution = 268435456
            session_artifact_quota_bytes = 2147483648
            attempt_diagnostic_ttl_ms = 86400000
            inspect_page_tokens = 2048
            checkpoint_call_timeout_ms = 45000
            checkpoint_total_timeout_ms = 120000
            gc_batch_objects = 1000
            gc_batch_ms = 100

            [watermarks]
            soft_bp = 6500
            hard_bp = 8500
            target_bp = 5000
        "#;
        let policy: ContextPolicy = toml::from_str(source).expect("v1 document parses");
        assert_eq!(policy.max_input_tokens, DEFAULT_MAX_INPUT_TOKENS);
        assert_eq!(
            policy.tool_output_retention_tokens,
            DEFAULT_TOOL_OUTPUT_RETENTION_TOKENS
        );
        assert_eq!(
            policy.resolve_budget(1_000_000).prune_threshold_tokens,
            83_200
        );
    }
}
