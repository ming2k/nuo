//! Spatiotemporal Aspect Engine implementation (ADR-0183).
//!
//! Orchestrates the deterministic lifecycle phases of the Agent Harness:
//! 1. `PreFlight`: Fast intent classification and execution tier selection.
//! 2. `InFlightStream`: Token-stream monitoring & semantic loop interception.
//! 3. `ToolGating`: Anti-derailment gate (repeated call checks + doom guard + token budget).
//! 4. `RoundEol`: Post-round convergence and title synthesis.
//!
//! The former `TurnIntake` phase (an automatic git/AST/compiler scan committed
//! into history) is removed by ADR-0214: code structure and workspace change
//! information enter through scoped, on-demand tool retrieval, never as a
//! silently-committed request-time scan.

pub mod phase;
pub use phase::{AspectHook, AspectPhase, AspectVerdict};

use std::sync::Arc;

use nuo_wire::{
    ExecutionTier, PreFlightRouteInput, PreFlightRouteOutput, StreamLoopReviewInput,
    StreamLoopVerdict, TrajectoryLoopReviewInput, TrajectoryLoopVerdict,
};

use crate::cognitive::{CognitivePipeline, HarnessTaskPipeline};

/// Runtime engine managing the five spatiotemporal aspect phases (ADR-0183 / ADR-0211).
#[derive(Clone)]
pub struct AspectEngine {
    harness_tasks: HarnessTaskPipeline,
}

impl AspectEngine {
    /// Create a new aspect engine backed by `harness_tasks`.
    pub fn new(harness_tasks: HarnessTaskPipeline) -> Self {
        Self { harness_tasks }
    }

    /// Access the underlying harness internal task pipeline.
    pub fn harness_tasks(&self) -> &HarnessTaskPipeline {
        &self.harness_tasks
    }

    /// Legacy alias for [`Self::harness_tasks`].
    pub fn cognitive(&self) -> &CognitivePipeline {
        &self.harness_tasks
    }

    // Phase 1: Pre-flight

    /// Evaluate user turn intent and select the appropriate execution tier.
    pub async fn evaluate_pre_flight(
        &self,
        prompt: &str,
        has_active_error: bool,
    ) -> PreFlightRouteOutput {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return PreFlightRouteOutput {
                tier: ExecutionTier::FastDirect,
                enable_thinking: false,
                estimated_complexity: 1,
            };
        }

        self.harness_tasks
            .route_pre_flight(PreFlightRouteInput {
                user_prompt: prompt.to_string(),
                has_active_error,
            })
            .await
    }

    // Phase 3: In-flight Stream

    /// Confirm or clear an L1 in-flight stream loop candidate.
    pub async fn review_stream_loop(&self, input: StreamLoopReviewInput) -> StreamLoopVerdict {
        self.harness_tasks.review_stream_loop(input).await
    }

    /// Confirm or clear an L1 trajectory loop candidate (ADR-0247).
    pub async fn review_trajectory_loop(
        &self,
        input: TrajectoryLoopReviewInput,
    ) -> TrajectoryLoopVerdict {
        self.harness_tasks.review_trajectory_loop(input).await
    }

    // Phase 4: Tool Gating

    /// Evaluate tool invocation safety against repeated-call ruts and trajectory thresholds.
    pub fn evaluate_tool_gating(
        &self,
        tool_name: &str,
        is_repeated_rut: bool,
        is_trajectory_blocked: bool,
    ) -> AspectVerdict<()> {
        if is_repeated_rut {
            return AspectVerdict::Abort {
                reason: format!(
                    "Tool '{}' rejected by RepeatedCallGuard: same call failed repeatedly without convergence.",
                    tool_name
                ),
                error_code: "TOOL_REPEATED_RUT",
            };
        }

        if is_trajectory_blocked {
            return AspectVerdict::Abort {
                reason: format!(
                    "Tool '{}' blocked by TrajectoryLoopGuard: repeating invocation trajectory detected.",
                    tool_name
                ),
                error_code: "TOOL_TRAJECTORY_LOOP",
            };
        }

        AspectVerdict::Continue
    }

    // Phase 5: Round EOL

    /// Round-EOL hook (ADR-0183 phase 5): fires at round convergence.
    pub fn fire_round_eol(
        &self,
        _agent: &Arc<crate::Agent>,
        _session: Arc<nuo_persistence::SessionStore>,
    ) {
        // Round convergence hook. Session titling runs concurrently on
        // first-prompt admission, removing the legacy EOL digest requirement.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nuo_wire::{Message, ModelRequest, Provider, Role};
    use std::sync::Arc;

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(
            &self,
            _req: ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            Ok(nuo_wire::ProviderCompletion::message(Message::new(
                Role::Assistant,
                r#"{"tier":"fast_direct","enable_thinking":false,"estimated_complexity":1}"#,
            )))
        }

        async fn stream_chat(
            &self,
            _req: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
            nuo_wire::ProviderError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn aspect_engine_pre_flight_evaluates() {
        let engine = AspectEngine::new(CognitivePipeline::new(Arc::new(MockProvider)));
        let out = engine.evaluate_pre_flight("hi", false).await;
        assert_eq!(out.tier, ExecutionTier::FastDirect);
    }

    #[test]
    fn aspect_engine_tool_gating_rejects_rut() {
        let engine = AspectEngine::new(CognitivePipeline::new(Arc::new(MockProvider)));
        let verdict = engine.evaluate_tool_gating("bash", true, false);
        assert!(verdict.is_abort());
    }

    #[test]
    fn aspect_engine_tool_gating_continues_clean() {
        let engine = AspectEngine::new(CognitivePipeline::new(Arc::new(MockProvider)));
        let verdict = engine.evaluate_tool_gating("bash", false, false);
        assert!(verdict.is_continue());
    }
}
