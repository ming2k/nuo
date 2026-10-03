//! Offline policy conversion. No clamping or implicit decision can activate a policy.
use nuo_wire::context_lifecycle::{ContextPolicy, WatermarkPolicy};

/// The legacy `compaction.*` family, as read from an old configuration file.
///
/// Ratios are fractions of the model window `W` (ADR-0280 §3).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LegacyCompactionConfig {
    /// `compaction.utilization`
    pub utilization: f64,
    /// `compaction.prune_utilization`
    pub prune_utilization: f64,
    /// `compaction.target_utilization`
    pub target_utilization: f64,
    /// `compaction.fallback_window_tokens`
    pub fallback_window_tokens: u64,
    /// `compaction.preserve_rounds`
    pub preserve_rounds: u32,
    /// `compaction.summarize`
    pub summarize: bool,
    /// `compaction.prune`
    pub prune: bool,
    /// `compaction.prune_protect_tokens`
    pub prune_protect_tokens: u64,
}


#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionClass { Equivalent, RequiresDecision, Removed }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversionItem {
    pub key: String,
    pub classification: ConversionClass,
    pub old_threshold: Option<u64>,
    pub candidate_basis_points: Option<u32>,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PolicyConversionReport {
    pub profile: String,
    pub window: u64,
    pub input_ceiling: u64,
    pub items: Vec<ConversionItem>,
    /// Absent whenever even the candidate is outside valid watermark bounds.
    pub candidate: Option<ContextPolicy>,
}

impl PolicyConversionReport {
    pub fn requires_decision(&self) -> bool {
        self.items.iter().any(|i| i.classification == ConversionClass::RequiresDecision)
    }
}

pub fn classify_legacy_policy(
    profile: &str, legacy: &LegacyCompactionConfig, window: u64, input_ceiling: u64,
    present_removed_keys: &[String],
) -> PolicyConversionReport {
    let mut items = Vec::new();
    for (key, ratio) in [
        ("compaction.prune_utilization", legacy.prune_utilization),
        ("compaction.utilization", legacy.utilization),
        ("compaction.target_utilization", legacy.target_utilization),
    ] {
        let valid = ratio.is_finite() && (0.0..=1.0).contains(&ratio)
            && window > 0 && input_ceiling > 0 && window <= (1u64 << 53);
        let threshold = valid.then(|| (ratio * window as f64).floor() as u64);
        let candidate = threshold.and_then(|t| {
            let bp = (u128::from(t) * 10_000).div_ceil(input_ceiling as u128);
            u32::try_from(bp).ok().filter(|v| *v <= 10_000)
        });
        let equivalent = candidate.zip(threshold).is_some_and(|(bp, t)|
            (u128::from(input_ceiling) * u128::from(bp) / 10_000) == u128::from(t));
        items.push(ConversionItem {
            key: key.into(),
            classification: if equivalent { ConversionClass::Equivalent } else { ConversionClass::RequiresDecision },
            old_threshold: threshold, candidate_basis_points: candidate,
            detail: if equivalent { "integer token threshold preserved" } else {
                "unknown/invalid reference budget or threshold not exactly representable; no clamping is permitted"
            }.into(),
        });
    }
    items.push(ConversionItem { key: "compaction.preserve_rounds".into(),
        classification: ConversionClass::RequiresDecision, old_threshold: None, candidate_basis_points: None,
        detail: "hard protection becomes a preference; explicit resolution required".into() });
    for key in ["compaction.fallback_window_tokens", "compaction.summarize", "compaction.prune", "compaction.prune_protect_tokens"] {
        items.push(ConversionItem { key: key.into(), classification: ConversionClass::Equivalent,
            old_threshold: None, candidate_basis_points: None, detail: "value preserved in the candidate policy".into() });
    }
    for key in present_removed_keys {
        items.push(ConversionItem { key: key.clone(), classification: ConversionClass::RequiresDecision,
            old_threshold: None, candidate_basis_points: None,
            detail: "removed key requires a source-version behavior disposition".into() });
    }
    let candidate = items[0].candidate_basis_points.zip(items[1].candidate_basis_points)
        .zip(items[2].candidate_basis_points).and_then(|((soft_bp, hard_bp), target_bp)| {
            let policy = ContextPolicy {
                watermarks: WatermarkPolicy { soft_bp, hard_bp, target_bp },
                preferred_recent_rounds: legacy.preserve_rounds,
                fallback_window_tokens: legacy.fallback_window_tokens,
                checkpoint_enabled: legacy.summarize,
                lightweight_degradation_enabled: legacy.prune,
                recent_observation_protect_tokens: legacy.prune_protect_tokens,
                ..ContextPolicy::default()
            };
            policy.validate().ok().map(|()| policy)
        });
    if candidate.is_none() {
        items.push(ConversionItem { key: "context".into(), classification: ConversionClass::RequiresDecision,
            old_threshold: None, candidate_basis_points: None, detail: "candidate policy is not valid".into() });
    }
    PolicyConversionReport { profile: profile.into(), window, input_ceiling, items, candidate }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy() -> LegacyCompactionConfig {
        LegacyCompactionConfig { utilization: 0.85, prune_utilization: 0.65,
            target_utilization: 0.25, fallback_window_tokens: 32_000,
            preserve_rounds: 6, summarize: false, prune: false, prune_protect_tokens: 6_000 }
    }
    #[test]
    fn impossible_threshold_is_not_clamped_or_activated() {
        let report = classify_legacy_policy("model/high", &legacy(), 100_000, 70_000, &[]);
        assert!(report.requires_decision());
        assert!(report.candidate.is_none());
        assert_eq!(report.items[1].old_threshold, Some(85_000));
        assert_eq!(report.items[1].candidate_basis_points, None);
    }
    #[test]
    fn exact_thresholds_still_require_explicit_hard_to_preference_decision() {
        let report = classify_legacy_policy("model", &legacy(), 100_000, 100_000, &[]);
        assert!(report.requires_decision());
        assert_eq!(report.items[1].classification, ConversionClass::Equivalent);
        let policy = report.candidate.unwrap();
        assert!(!policy.checkpoint_enabled);
        assert!(!policy.lightweight_degradation_enabled);
        assert_eq!(policy.recent_observation_protect_tokens, 6_000);
        assert_eq!(report.items.len(), 8, "absent removed keys are not fabricated");
    }
    #[test]
    fn unknown_budgets_and_rounding_require_decisions() {
        let unknown = classify_legacy_policy("model", &legacy(), 0, 0, &[]);
        assert!(unknown.candidate.is_none());
        let rounded = classify_legacy_policy("model", &legacy(), 100_000, 99_999, &[]);
        assert_eq!(rounded.items[1].classification, ConversionClass::RequiresDecision);
    }
}
