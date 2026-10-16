//! Local spend circuit breaker guardrail (ADR-0065).
//!
//! Enforces pre-flight interception before HTTP egress when local connection budget
//! ceilings are reached, and atomically records cost post-flight (`[INV-BREAKER-01]`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use nuo_wire::budget::ConnectionBudget;
use nuo_wire::pricing::NanoUsd;
use crate::error::AgentError;

/// Thread-safe local spend circuit breaker.
#[derive(Debug, Clone)]
pub struct SpendCircuitBreaker {
    budget: ConnectionBudget,
    accumulated_nano_usd: Arc<AtomicU64>,
    tripped: Arc<AtomicBool>,
}

impl SpendCircuitBreaker {
    /// Construct a new circuit breaker with the given connection budget policy.
    pub fn new(budget: ConnectionBudget) -> Self {
        Self {
            budget,
            accumulated_nano_usd: Arc::new(AtomicU64::new(0)),
            tripped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Construct with an existing accumulated balance (e.g. daily counter from persistence).
    pub fn with_initial_spend(budget: ConnectionBudget, initial_spend: NanoUsd) -> Self {
        let is_exceeded = budget.is_exceeded(initial_spend);
        Self {
            budget,
            accumulated_nano_usd: Arc::new(AtomicU64::new(initial_spend.0)),
            tripped: Arc::new(AtomicBool::new(is_exceeded)),
        }
    }

    /// Pre-flight gate: verify spend capacity before dispatching model inference (`[INV-BREAKER-01]`).
    pub fn check_preflight(&self) -> Result<(), AgentError> {
        if !self.budget.is_local() {
            return Ok(());
        }

        let current = NanoUsd(self.accumulated_nano_usd.load(Ordering::Relaxed));
        if self.tripped.load(Ordering::Relaxed) || self.budget.is_exceeded(current) {
            let limit = self.budget.limit_nano_usd().unwrap_or(NanoUsd::ZERO);
            let period = self
                .budget
                .period()
                .map(|p| p.label().to_string())
                .unwrap_or_else(|| "Session".to_string());

            return Err(AgentError::SpendBudgetExceeded {
                limit_usd: limit.format_usd(),
                spend_usd: current.format_usd(),
                period,
            });
        }

        Ok(())
    }

    /// Post-flight settlement: record settled turn cost and trip the breaker if limit exceeded.
    ///
    /// Returns `true` if this turn caused the circuit breaker to trip.
    pub fn record_spend(&self, turn_cost: NanoUsd) -> bool {
        if !self.budget.is_local() {
            return false;
        }

        let prev = self
            .accumulated_nano_usd
            .fetch_add(turn_cost.0, Ordering::SeqCst);
        let new_total = NanoUsd(prev.saturating_add(turn_cost.0));

        if self.budget.is_exceeded(new_total) {
            self.tripped.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Return current accumulated spend in nano-USD.
    pub fn accumulated_spend(&self) -> NanoUsd {
        NanoUsd(self.accumulated_nano_usd.load(Ordering::Relaxed))
    }

    /// Whether the circuit breaker is currently tripped.
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Relaxed)
    }

    /// Reset breaker state (e.g. user manually overrides or session resets).
    pub fn reset(&self) {
        self.accumulated_nano_usd.store(0, Ordering::SeqCst);
        self.tripped.store(false, Ordering::SeqCst);
    }

    /// Override the budget limit in-place.
    pub fn update_budget(&mut self, new_budget: ConnectionBudget) {
        let current = self.accumulated_spend();
        let exceeded = new_budget.is_exceeded(current);
        self.tripped.store(exceeded, Ordering::SeqCst);
        self.budget = new_budget;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::budget::BudgetPeriod;

    #[test]
    fn test_spend_circuit_breaker_preflight_and_settlement() {
        // Setup local budget of $1.00
        let budget = ConnectionBudget::local(1.00, BudgetPeriod::Session);
        let breaker = SpendCircuitBreaker::new(budget);

        // Preflight passes initially
        assert!(breaker.check_preflight().is_ok());

        // First turn spends $0.40 -> still passes
        let cost1 = NanoUsd::from_usd_f64(0.40);
        assert!(!breaker.record_spend(cost1));
        assert!(breaker.check_preflight().is_ok());

        // Second turn spends $0.65 -> total $1.05 -> trips
        let cost2 = NanoUsd::from_usd_f64(0.65);
        assert!(breaker.record_spend(cost2));
        assert!(breaker.is_tripped());

        // Preflight is now intercepted and prevents further egress
        let err = breaker.check_preflight().unwrap_err();
        match err {
            AgentError::SpendBudgetExceeded {
                limit_usd,
                spend_usd,
                period,
            } => {
                assert_eq!(limit_usd, "$1.00");
                assert_eq!(spend_usd, "$1.05");
                assert_eq!(period, "Session");
            }
            other => panic!("expected SpendBudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn test_remote_budget_never_trips_locally() {
        let breaker = SpendCircuitBreaker::new(ConnectionBudget::Remote);
        assert!(breaker.check_preflight().is_ok());

        // Spend large amount
        assert!(!breaker.record_spend(NanoUsd::from_usd_f64(100.0)));
        assert!(!breaker.is_tripped());
        assert!(breaker.check_preflight().is_ok());
    }
}
