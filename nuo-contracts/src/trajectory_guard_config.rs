//! Trajectory-guard configuration (ADR-0247).
//!
//! [`TrajectoryGuardConfig`] is the serializable DTO that governs the
//! pre-dispatch trajectory loop guard (`muta_agent::trajectory_guard`).
//! Canonical TOML sub-table is `[agent.trajectory_guard]`.

use serde::{Deserialize, Serialize};

/// User-tunable trajectory-guard behaviour (ADR-0247).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrajectoryGuardConfig {
    /// Master switch for the trajectory loop guard.
    pub enabled: bool,
    /// Sliding-window size: how many recent watched tool-call signatures are tracked.
    pub window: usize,
    /// Base occurrence threshold before a repetition triggers review or block.
    /// Acts as Tier 1 (default 4) in the escalating backoff ladder (4 -> 8 -> 12).
    pub threshold: usize,
    /// Whether to engage the Steward L2 cognitive arbitration pipeline.
    /// When enabled, candidate repetitions are reviewed by the Steward; acquittals
    /// escalate along the backoff ladder (4 -> 8 -> 12).
    pub cognitive_review: bool,
}

impl TrajectoryGuardConfig {
    /// A disabled config with default window — the canonical "off" state.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Construct a configuration with Steward cognitive arbitration enabled.
    pub fn cognitive() -> Self {
        Self {
            enabled: true,
            window: 16,
            threshold: 4,
            cognitive_review: true,
        }
    }
}

impl Default for TrajectoryGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window: 16,
            threshold: 4,
            cognitive_review: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_enabled_with_cognitive_ladder() {
        let cfg = TrajectoryGuardConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.window, 16);
        assert_eq!(cfg.threshold, 4);
        assert!(cfg.cognitive_review);
    }

    #[test]
    fn disabled_helper_keeps_default_window() {
        let off = TrajectoryGuardConfig::disabled();
        assert!(!off.enabled);
        assert_eq!(off.window, 16);
        assert_eq!(off.threshold, 4);
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = TrajectoryGuardConfig {
            enabled: true,
            window: 12,
            threshold: 4,
            cognitive_review: false,
        };
        let s = toml::to_string(&cfg).unwrap();
        let parsed: TrajectoryGuardConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed, cfg);
    }
}
