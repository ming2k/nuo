//! Cognitive execution engine: typed, resilient out-of-band subagent for Agent Harness (ADR-0167).
//!
//! # Architecture
//!
//! The [`CognitivePipeline`] provides internal cognitive execution for the Agent Harness.
//!
//! Key design invariants:
//! - **Typed Tasks**: Dispatches any task implementing `CognitiveTask`.
//! - **Timeout Bounds**: Strict timeouts on every consult call, preventing background task leaks.
//! - **Fail-Open Resilience**: Helper methods guarantee graceful fallback if model calls fail or timeout.
//! - **JSON Normalization**: Extracts structured JSON payloads even if the model wraps them in markdown.

use std::sync::Arc;
use std::time::Duration;

use nuo_wire::{
    EnvironmentReminderOutput, EnvironmentSensorInput, EnvironmentSensorTask, ExecutionTier,
    Message, ModelRequest, PreFlightRouteInput, PreFlightRouteOutput, PreFlightRouterTask,
    Provider, Role, SessionTitleInput, SessionTitleTask, StreamLoopReviewInput,
    StreamLoopReviewerTask, StreamLoopVerdict, TrajectoryLoopReviewInput,
    TrajectoryLoopReviewerTask, TrajectoryLoopVerdict,
};

/// Errors that can occur during a harness task consultation (ADR-0211).
#[derive(Debug)]
pub enum HarnessTaskError {
    Timeout(Duration),
    ProviderError(String),
    DeserializationError { error: String, raw: String },
    EmptyResponse,
}

impl std::fmt::Display for HarnessTaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(d) => write!(f, "Harness task timed out after {d:?}"),
            Self::ProviderError(e) => write!(f, "Provider failed: {e}"),
            Self::DeserializationError { error, raw } => {
                write!(
                    f,
                    "Failed to deserialize structured output: {error}, raw: {raw}"
                )
            }
            Self::EmptyResponse => write!(f, "Model returned an empty response"),
        }
    }
}

impl std::error::Error for HarnessTaskError {}

/// Legacy alias for [`HarnessTaskError`] (ADR-0211).
pub type CognitiveError = HarnessTaskError;

/// The Harness internal task execution pipeline (ADR-0211).
#[derive(Clone)]
pub struct HarnessTaskPipeline {
    provider: Arc<dyn Provider>,
}

/// Legacy alias for [`HarnessTaskPipeline`] (ADR-0211).
pub type CognitivePipeline = HarnessTaskPipeline;

impl HarnessTaskPipeline {
    /// Create a new harness task pipeline bound to `provider`.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }

    /// Access the underlying provider.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Consult the harness task pipeline with a typed [`nuo_wire::HarnessTask`].
    pub async fn consult<T: nuo_wire::HarnessTask>(
        &self,
        task: T,
        input: T::Input,
    ) -> Result<T::Output, HarnessTaskError> {
        let timeout = Duration::from_millis(task.timeout_ms());
        let instructions =
            nuo_wire::InstructionBundle::new(vec![nuo_wire::InstructionSlice::new(
                "harness.cognitive_task",
                nuo_wire::InstructionTier::Task,
                task.system_prompt(),
            )]);
        let messages = vec![Message::new(Role::User, task.render_prompt(&input))];
        tracing::debug!(task = task.name(), "cognitive pipeline consult");

        let response = tokio::time::timeout(
            timeout,
            self.provider
                .chat(ModelRequest::ephemeral(messages).with_instructions(instructions)),
        )
        .await
        .map_err(|_| CognitiveError::Timeout(timeout))?
        .map_err(|e| CognitiveError::ProviderError(e.to_string()))?;

        let content = response.message.content.as_str();
        if content.trim().is_empty() {
            return Err(CognitiveError::EmptyResponse);
        }

        task.parse_output(content)
            .map_err(|error| CognitiveError::DeserializationError {
                error,
                raw: content.to_string(),
            })
    }

    /// Consult with automatic fallback (Fail-Open pattern).
    ///
    /// If the pipeline fails, times out, or returns invalid JSON, this logs a warning
    /// and returns `fallback` to prevent blocking the production loop.
    pub async fn consult_with_fallback<T: nuo_wire::HarnessTask>(
        &self,
        task: T,
        input: T::Input,
        fallback: T::Output,
    ) -> T::Output {
        let task_name = task.name();
        match self.consult(task, input).await {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(task = %task_name, error = %err, "Harness task consultation failed, using fail-open fallback");
                fallback
            }
        }
    }

    /// Confirm or clear an L1 in-flight stream-loop candidate.
    ///
    /// The output grammar is the strict bare-token contract owned by
    /// [`StreamLoopReviewerTask`]. Any timeout, provider failure, or malformed
    /// answer is fail-open `no`: an infrastructure judgment can authorize a
    /// cutoff only with an explicit valid `yes`.
    pub async fn review_stream_loop(&self, input: StreamLoopReviewInput) -> StreamLoopVerdict {
        self.consult_with_fallback(StreamLoopReviewerTask, input, StreamLoopVerdict::No)
            .await
    }

    /// Confirm or clear an L1 trajectory loop candidate (ADR-0247).
    ///
    /// Any timeout, provider failure, or malformed answer is fail-open `No`:
    /// an infrastructure judgment authorizes a block only with an explicit valid `Yes`.
    /// On `No` (or fail-open), the caller advances the backoff ladder.
    pub async fn review_trajectory_loop(
        &self,
        input: TrajectoryLoopReviewInput,
    ) -> TrajectoryLoopVerdict {
        self.consult_with_fallback(TrajectoryLoopReviewerTask, input, TrajectoryLoopVerdict::No)
            .await
    }

    /// Distill an excerpt into a concise session title.
    pub async fn generate_title(&self, input: SessionTitleInput) -> Option<String> {
        match self.consult(SessionTitleTask, input).await {
            Ok(title) => Some(title),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "Session title generation failed"
                );
                None
            }
        }
    }

    /// Evaluate pre-flight intent and select an execution tier with fail-open fallback.
    pub async fn route_pre_flight(&self, input: PreFlightRouteInput) -> PreFlightRouteOutput {
        self.consult_with_fallback(
            PreFlightRouterTask,
            input,
            PreFlightRouteOutput {
                tier: ExecutionTier::StandardEngineering,
                enable_thinking: false,
                estimated_complexity: 5,
            },
        )
        .await
    }

    /// Sense workspace environment facts and synthesize dynamic reminder text.
    pub async fn sense_environment(
        &self,
        input: EnvironmentSensorInput,
    ) -> EnvironmentReminderOutput {
        self.consult_with_fallback(
            EnvironmentSensorTask,
            input,
            EnvironmentReminderOutput {
                reminder_text: None,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use nuo_wire::Message;

    struct MockProvider {
        response: Result<String, String>,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(
            &self,
            _req: ModelRequest,
        ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError> {
            match &self.response {
                Ok(content) => Ok(nuo_wire::ProviderCompletion::message(Message::new(
                    Role::Assistant,
                    content,
                ))),
                Err(err) => Err(nuo_wire::ProviderError::new(
                    "mock",
                    nuo_wire::ProviderErrorKind::Other,
                    err.clone(),
                )),
            }
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
    async fn cognitive_title_parses_and_fails_open() {
        let provider = Arc::new(MockProvider {
            response: Ok("```\nFix auth loop\n```".to_string()),
        });
        let title = CognitivePipeline::new(provider)
            .generate_title(SessionTitleInput {
                excerpt: "user: fix the login loop".to_string(),
            })
            .await
            .expect("fenced title parses");
        assert_eq!(title, "Fix auth loop");

        let provider = Arc::new(MockProvider {
            response: Err("HTTP 500 error".to_string()),
        });
        assert!(
            CognitivePipeline::new(provider)
                .generate_title(SessionTitleInput {
                    excerpt: "x".to_string(),
                })
                .await
                .is_none()
        );
        let provider = Arc::new(MockProvider {
            response: Ok("   \n\t  ".to_string()),
        });
        assert!(
            CognitivePipeline::new(provider)
                .generate_title(SessionTitleInput {
                    excerpt: "x".to_string(),
                })
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn loop_reviewer_verdicts_are_fail_open_on_error() {
        let provider = Arc::new(MockProvider {
            response: Ok("yes".to_string()),
        });
        let verdict = CognitivePipeline::new(provider)
            .review_stream_loop(StreamLoopReviewInput {
                heuristic_candidate: "abab".to_string(),
                channel: nuo_wire::StreamLoopChannel::AssistantText,
                preceding_context: String::new(),
                assistant_text: "abababab".to_string(),
                reasoning_text: String::new(),
            })
            .await;
        assert_eq!(verdict, StreamLoopVerdict::Yes);

        let provider = Arc::new(MockProvider {
            response: Ok("no".to_string()),
        });
        let verdict = CognitivePipeline::new(provider)
            .review_stream_loop(StreamLoopReviewInput {
                heuristic_candidate: "abab".to_string(),
                channel: nuo_wire::StreamLoopChannel::AssistantText,
                preceding_context: String::new(),
                assistant_text: "abababab".to_string(),
                reasoning_text: String::new(),
            })
            .await;
        assert_eq!(verdict, StreamLoopVerdict::No);

        let provider = Arc::new(MockProvider {
            response: Ok("maybe or invalid".to_string()),
        });
        let verdict = CognitivePipeline::new(provider)
            .review_stream_loop(StreamLoopReviewInput {
                heuristic_candidate: "abab".to_string(),
                channel: nuo_wire::StreamLoopChannel::AssistantText,
                preceding_context: String::new(),
                assistant_text: "abababab".to_string(),
                reasoning_text: String::new(),
            })
            .await;
        assert_eq!(verdict, StreamLoopVerdict::No);
    }

    #[tokio::test]
    async fn route_pre_flight_parses_and_fails_open() {
        let json = r#"{"tier":"fast_direct","enable_thinking":false,"estimated_complexity":2}"#;
        let provider = Arc::new(MockProvider {
            response: Ok(format!("```json\n{json}\n```")),
        });
        let res = CognitivePipeline::new(provider)
            .route_pre_flight(PreFlightRouteInput {
                user_prompt: "hello".into(),
                has_active_error: false,
            })
            .await;
        assert_eq!(res.tier, ExecutionTier::FastDirect);
        assert_eq!(res.estimated_complexity, 2);

        let provider_err = Arc::new(MockProvider {
            response: Err("timeout".into()),
        });
        let fallback = CognitivePipeline::new(provider_err)
            .route_pre_flight(PreFlightRouteInput {
                user_prompt: "hello".into(),
                has_active_error: false,
            })
            .await;
        assert_eq!(fallback.tier, ExecutionTier::StandardEngineering);
    }

    #[tokio::test]
    async fn environment_sensor_parses_and_fails_open() {
        let json = r#"{"reminder_text":"Dirty branch: 3 files changed."}"#;
        let provider = Arc::new(MockProvider {
            response: Ok(json.into()),
        });
        let res = CognitivePipeline::new(provider)
            .sense_environment(EnvironmentSensorInput {
                active_branch: "main".into(),
                dirty_files_count: 3,
                dirty_files_sample: vec!["src/main.rs".into()],
                compiler_error: None,
            })
            .await;
        assert_eq!(
            res.reminder_text.as_deref(),
            Some("Dirty branch: 3 files changed.")
        );

        let provider_err = Arc::new(MockProvider {
            response: Err("failed".into()),
        });
        let fallback = CognitivePipeline::new(provider_err)
            .sense_environment(EnvironmentSensorInput {
                active_branch: "main".into(),
                dirty_files_count: 0,
                dirty_files_sample: vec![],
                compiler_error: None,
            })
            .await;
        assert_eq!(fallback.reminder_text, None);
    }
}
