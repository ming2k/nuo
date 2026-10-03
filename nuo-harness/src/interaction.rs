//! Unified Interaction Controller & Posture Management (Phase 4).
//!
//! Consolidates previously fragmented interaction flags (the delegated flag,
//! `human_posture`, `skip_interactive_input`, `allow_model_stdin`, `human_channel`,
//! and `autonomous_fallback`) into a coherent, single-source-of-truth interaction
//! state machine.

use nuo_wire::human_request::{
    AutonomousFallbackPolicy, HumanChannelAccountant, HumanChannelPosture,
};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Interaction configuration parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionConfig {
    pub allow_model_stdin: bool,
    pub skip_interactive_input: bool,
    pub fallback_policy: AutonomousFallbackPolicy,
}

impl Default for InteractionConfig {
    fn default() -> Self {
        Self {
            allow_model_stdin: false,
            skip_interactive_input: false,
            fallback_policy: AutonomousFallbackPolicy::FailClosed,
        }
    }
}

/// Unified controller governing how the agent interacts with humans.
pub struct InteractionController {
    /// Baseline posture (Interactive vs Autonomous vs AutoReject).
    posture: Arc<AtomicU8>,
    /// Unattended mode flag (when true, auto-approves all tool permissions).
    unattended: Arc<AtomicBool>,
    /// Model-provided stdin support for shell tools.
    allow_model_stdin: Arc<AtomicBool>,
    /// Skip interactive shell input popups.
    skip_interactive_input: Arc<AtomicBool>,
    /// Fallback strategy when autonomous.
    fallback_policy: Mutex<AutonomousFallbackPolicy>,
    /// Live channel source when connected to a multi-client daemon session.
    human_channel: Mutex<Option<Arc<HumanChannelAccountant>>>,
}

impl Default for InteractionController {
    fn default() -> Self {
        Self::new(InteractionConfig::default())
    }
}

impl InteractionController {
    pub fn new(config: InteractionConfig) -> Self {
        Self {
            posture: Arc::new(AtomicU8::new(HumanChannelPosture::Interactive as u8)),
            unattended: Arc::new(AtomicBool::new(false)),
            allow_model_stdin: Arc::new(AtomicBool::new(config.allow_model_stdin)),
            skip_interactive_input: Arc::new(AtomicBool::new(config.skip_interactive_input)),
            fallback_policy: Mutex::new(config.fallback_policy),
            human_channel: Mutex::new(None),
        }
    }

    pub fn human_posture(&self) -> HumanChannelPosture {
        if let Ok(guard) = self.human_channel.lock()
            && let Some(accountant) = guard.as_ref()
        {
            return accountant.effective();
        }
        if self.posture.load(Ordering::Relaxed) == HumanChannelPosture::Autonomous as u8 {
            HumanChannelPosture::Autonomous
        } else {
            HumanChannelPosture::Interactive
        }
    }

    pub fn set_human_posture(&self, posture: HumanChannelPosture) {
        self.posture.store(posture as u8, Ordering::Relaxed);
    }

    pub fn unattended(&self) -> bool {
        self.unattended.load(Ordering::Relaxed)
    }

    pub fn set_unattended(&self, value: bool) {
        self.unattended.store(value, Ordering::Relaxed);
    }

    pub fn allow_model_stdin(&self) -> bool {
        self.allow_model_stdin.load(Ordering::Relaxed)
    }

    pub fn set_allow_model_stdin(&self, value: bool) {
        self.allow_model_stdin.store(value, Ordering::Relaxed);
    }

    pub fn skip_interactive_input(&self) -> bool {
        self.skip_interactive_input.load(Ordering::Relaxed)
    }

    pub fn set_skip_interactive_input(&self, value: bool) {
        self.skip_interactive_input.store(value, Ordering::Relaxed);
    }

    pub fn autonomous_fallback_policy(&self) -> AutonomousFallbackPolicy {
        self.fallback_policy
            .lock()
            .map(|g| *g)
            .unwrap_or(AutonomousFallbackPolicy::FailClosed)
    }

    pub fn set_autonomous_fallback_policy(&self, policy: AutonomousFallbackPolicy) {
        if let Ok(mut guard) = self.fallback_policy.lock() {
            *guard = policy;
        }
    }

    pub fn set_human_channel(&self, channel: Option<Arc<HumanChannelAccountant>>) {
        if let Ok(mut guard) = self.human_channel.lock() {
            *guard = channel;
        }
    }

    pub fn allow_model_stdin_handle(&self) -> Arc<AtomicBool> {
        self.allow_model_stdin.clone()
    }

    pub fn skip_interactive_input_handle(&self) -> Arc<AtomicBool> {
        self.skip_interactive_input.clone()
    }

    pub fn human_posture_handle(&self) -> Arc<AtomicU8> {
        self.posture.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_controller_defaults() {
        let c = InteractionController::default();
        assert_eq!(c.human_posture(), HumanChannelPosture::Interactive);
        assert!(!c.unattended());
        assert!(!c.allow_model_stdin());
        assert!(!c.skip_interactive_input());
        assert_eq!(
            c.autonomous_fallback_policy(),
            AutonomousFallbackPolicy::FailClosed
        );
    }

    #[test]
    fn interaction_controller_mutations() {
        let c = InteractionController::default();
        c.set_unattended(true);
        assert!(c.unattended());
        c.set_allow_model_stdin(true);
        assert!(c.allow_model_stdin());
        c.set_skip_interactive_input(true);
        assert!(c.skip_interactive_input());
        c.set_human_posture(HumanChannelPosture::Autonomous);
        assert_eq!(c.human_posture(), HumanChannelPosture::Autonomous);
        c.set_autonomous_fallback_policy(AutonomousFallbackPolicy::RecommendedLabeled);
        assert_eq!(
            c.autonomous_fallback_policy(),
            AutonomousFallbackPolicy::RecommendedLabeled
        );
    }
}
