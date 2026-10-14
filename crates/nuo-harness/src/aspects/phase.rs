//! Spatiotemporal Aspect Engine contracts (ADR-0183).
//!
//! Governs the five deterministic lifecycle phases of the Agent Harness:
//! 1. Pre-flight (intent evaluation & model tier selection)
//! 2. Turn-Intake (silent environment observation & dynamic reminder injection)
//! 3. In-flight Stream (token-stream monitoring & semantic loop interception)
//! 4. Tool-Gating (anti-derailment: repeated calls, doom mutation, token budgets)
//! 5. Round-EOL (memory digest accumulation, title generation, compaction)

use serde::{Deserialize, Serialize};

/// The five canonical lifecycle phases of the Spatiotemporal Aspect Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AspectPhase {
    /// Evaluates prompt intent before any model dispatch (0-100ms).
    PreFlight,
    /// Inspects workspace/environmental state before context assembly.
    TurnIntake,
    /// Observes real-time streaming tokens and reasoning channels.
    InFlightStream,
    /// Intercepts tool invocations before physical OS/filesystem execution.
    ToolGating,
    /// Post-round convergence: persists digests and synthesizes metadata.
    RoundEol,
}

impl AspectPhase {
    pub const ALL: &'static [AspectPhase] = &[
        Self::PreFlight,
        Self::TurnIntake,
        Self::InFlightStream,
        Self::ToolGating,
        Self::RoundEol,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::PreFlight => "pre_flight",
            Self::TurnIntake => "turn_intake",
            Self::InFlightStream => "in_flight_stream",
            Self::ToolGating => "tool_gating",
            Self::RoundEol => "round_eol",
        }
    }
}

/// The deterministic verdict returned by an aspect hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AspectVerdict<T> {
    /// Proceed with normal execution without modification.
    Continue,
    /// Proceed with mutated payload or injected context.
    Mutate(T),
    /// Abort or reject execution with an explicit, auditable reason.
    Abort {
        reason: String,
        error_code: &'static str,
    },
}

impl<T> AspectVerdict<T> {
    pub fn is_continue(&self) -> bool {
        matches!(self, Self::Continue)
    }

    pub fn is_abort(&self) -> bool {
        matches!(self, Self::Abort { .. })
    }
}

/// Baseline trait implemented by all deterministic aspect hooks.
pub trait AspectHook: Send + Sync {
    /// Unique, stable identifier for diagnostics and telemetry.
    fn id(&self) -> &'static str;

    /// The lifecycle phase this hook binds to.
    fn phase(&self) -> AspectPhase;
}
