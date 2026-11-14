use super::*;
use futures::stream::{self, BoxStream};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TestProvider;
struct HintProvider;
struct PermissionTestProvider(AtomicUsize);
struct StreamingToolProvider(AtomicUsize);
struct WriteTestTool;
struct ShadowTodoTool;
struct StreamingReadTool(Arc<AtomicUsize>);

#[async_trait]
impl Provider for TestProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        // The default `stream_chat_events` wraps this into a single
        // `TextDelta("done")`, so the streaming ReAct loop sees the same
        // terminal answer as `chat()`.
        Ok(Box::pin(stream::once(async { Ok("done".to_string()) })))
    }
}

#[async_trait]
impl Provider for HintProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    fn prompt_hints(&self) -> nuo_protocol::ProviderPromptHints {
        nuo_protocol::ProviderPromptHints {
            system_guidance: "Provider protocol hint.",
        }
    }
}

#[async_trait]
impl Provider for PermissionTestProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        unreachable!("streaming path should be used")
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        unreachable!("stream_chat_events should be called directly")
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let events = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call".to_string()),
                    name: Some("write_test".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[async_trait]
impl Provider for StreamingToolProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let events = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: Some("stream_".to_string()),
                    arguments: "{\"value\":".to_string(),
                }),
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: Some("read".to_string()),
                    arguments: "1}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("do".to_string())),
                Ok(ProviderStreamEvent::TextDelta("ne".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[async_trait]
impl Tool for WriteTestTool {
    fn name(&self) -> &str {
        "write_test"
    }

    fn description(&self) -> &str {
        "test write tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn scope_target(&self, _arguments: &str) -> nuo_tool::ScopeTarget {
        nuo_tool::ScopeTarget::Path(std::path::PathBuf::from("/tmp/test"))
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("should not run".to_string())
    }
}

#[async_trait]
impl Tool for StreamingReadTool {
    fn name(&self) -> &str {
        "stream_read"
    }

    fn description(&self) -> &str {
        "streaming test tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        assert_eq!(arguments, "{\"value\":1}");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("read".to_string())
    }
}

#[async_trait]
impl Tool for ShadowTodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "caller-owned shadow"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("wrong state".to_string())
    }
}

fn agent() -> Arc<Agent> {
    Arc::new(Agent::new(
        Arc::new(TestProvider),
        Vec::new(),
        crate::AgentIdentity::default(),
    ))
}

// Golden-transcript harness
//
// `ScriptedProvider` replays a fixed list of streamed events — one script
// per ReAct turn — so a whole agent round runs deterministically and its
// emitted `AgentEvent` stream can be asserted as a stable golden
// transcript. This pins the loop's externally-visible contract (tool-call
// ordering, native vs text-fallback dispatch, concurrent result ordering,
// the repeated-call guard, and permission gating) independently of any real
// provider, so the refactors that follow can lean on it as a safety net.

/// A ReAct turn that streams a single chunk of assistant text.
fn text_turn(text: &str) -> Vec<ProviderStreamEvent> {
    vec![ProviderStreamEvent::TextDelta(text.to_string())]
}

/// A ReAct turn that streams native tool calls as `(id, name, arguments)`.
fn turn(calls: &[(&str, &str, &str)]) -> Vec<ProviderStreamEvent> {
    calls
        .iter()
        .enumerate()
        .map(
            |(index, (id, name, arguments))| ProviderStreamEvent::ToolCallDelta {
                index,
                id: Some(id.to_string()),
                name: Some(name.to_string()),
                arguments: arguments.to_string(),
            },
        )
        .collect()
}

struct ScriptedProvider {
    turns: std::sync::Mutex<std::collections::VecDeque<Vec<ProviderStreamEvent>>>,
    cognitive_replies: std::sync::Mutex<std::collections::VecDeque<String>>,
    cognitive_requests: std::sync::Mutex<Vec<nuo_protocol::ModelRequest>>,
    /// Pace every stream event with a small gap so the runtime interleaves
    /// spawned cognitive loop consults with stream consumption — the
    /// network cadence an instantly-ready scripted stream omits. Without
    /// it, a current-thread test runtime consumes the whole script before
    /// the detached consult ever runs, so mid-stream verdict cuts can never
    /// be exercised.
    paced: bool,
}

impl ScriptedProvider {
    fn new(turns: Vec<Vec<ProviderStreamEvent>>) -> Self {
        Self {
            turns: std::sync::Mutex::new(turns.into_iter().collect()),
            cognitive_replies: std::sync::Mutex::new(std::collections::VecDeque::new()),
            cognitive_requests: std::sync::Mutex::new(Vec::new()),
            paced: false,
        }
    }

    fn with_cognitive_replies(self, replies: &[&str]) -> Self {
        *self
            .cognitive_replies
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            replies.iter().map(|reply| (*reply).to_string()).collect();
        self
    }

    fn paced(mut self) -> Self {
        self.paced = true;
        self
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat(
        &self,
        request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        self.cognitive_requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        let reply = self
            .cognitive_replies
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .unwrap_or_else(|| "no".to_string());
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            reply,
        )))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        // A turn that runs past its script gets a terminal "done" so the
        // loop exits rather than hanging on a missing turn.
        let mut turn = self
            .turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .unwrap_or_else(|| text_turn("done"));
        if !turn
            .iter()
            .any(|e| matches!(e, ProviderStreamEvent::Completed(_)))
        {
            turn.push(ProviderStreamEvent::Completed(
                nuo_protocol::ProviderCompletionMeta::default(),
            ));
        }
        if self.paced {
            use futures::StreamExt as _;
            let paced = stream::iter(turn.into_iter().map(Ok)).then(|event| async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                event
            });
            return Ok(Box::pin(paced));
        }
        Ok(Box::pin(stream::iter(turn.into_iter().map(Ok))))
    }
}

/// A tool that records every invocation's arguments and returns canned
/// output. The `write` variant declares a [`ScopeTarget::Path`] so the
/// permission broker fires for it; the `read` variant leaves the default
/// [`ScopeTarget::Unspecified`] and skips the broker.
struct RecordingTool {
    name: &'static str,
    output: String,
    declares_target: bool,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl RecordingTool {
    fn read(name: &'static str, output: &str) -> Self {
        Self {
            name,
            output: output.to_string(),
            declares_target: false,
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn write(name: &'static str, output: &str) -> Self {
        Self {
            declares_target: true,
            ..Self::read(name, output)
        }
    }

    fn calls_handle(&self) -> Arc<std::sync::Mutex<Vec<String>>> {
        Arc::clone(&self.calls)
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "recording test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn scope_target(&self, arguments: &str) -> nuo_tool::ScopeTarget {
        if self.name == "execute_command" {
            let command = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    v.get("command")
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| arguments.to_string());
            nuo_tool::ScopeTarget::Command(command)
        } else if self.declares_target {
            // Pull a path from the args if present, else a fixed sentinel, so
            // the broker fires for the `write` variant.
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
                .unwrap_or_else(|| "/tmp/recording".to_string());
            nuo_tool::ScopeTarget::Path(std::path::PathBuf::from(path))
        } else {
            nuo_tool::ScopeTarget::Unspecified
        }
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(arguments.to_string());
        Ok(self.output.clone())
    }
}

/// Normalise an event stream into a stable, assertable transcript by
/// dropping non-deterministic fields (generated call ids and durations).
fn transcript(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            // High-resolution performance samples are non-deterministic by
            // construction; dropped so transcripts stay stable across runs.
            AgentEvent::TurnPerformance(_) => None,
            AgentEvent::BackgroundJobReady { .. } => None,
            AgentEvent::Notice(notice) => {
                Some(format!("notice {:?} {:?}", notice.kind, notice.title))
            }
            AgentEvent::ModelRequestStarted { turn, .. } => {
                Some(format!("model-request turn={turn}"))
            }
            AgentEvent::ContextTokens(_) => Some("context-tokens".to_string()),
            AgentEvent::SteerAdmitted { input, .. } => {
                Some(format!("steer-admitted {:?}", input.text))
            }
            AgentEvent::AssistantDelta { delta, start } => {
                Some(format!("assistant-delta start={start} {delta:?}"))
            }
            AgentEvent::AssistantEnd(content) => Some(format!("assistant-end {content:?}")),
            AgentEvent::AssistantDiscard => Some("assistant-discard".to_string()),
            AgentEvent::ReasoningDelta { delta, start } => {
                Some(format!("reasoning-delta start={start} {delta:?}"))
            }
            AgentEvent::ReasoningEnd(content) => Some(format!("reasoning-end {content:?}")),
            AgentEvent::ToolCall {
                name, arguments, ..
            } => Some(format!("tool-call {name} {arguments}")),
            // ADR-0166: the retraction names call ids; the assertion stream
            // carries its shape (how many provisional rows died) rather than the
            // ids themselves, which are provider-supplied or minted.
            AgentEvent::ToolCallsRetracted { ids } => {
                Some(format!("tool-calls-retracted {}", ids.len()))
            }
            AgentEvent::ToolCallStarted { id, name } => {
                Some(format!("tool-call-started {id} {name}"))
            }
            // ADR-0125: previews are deterministic function of the workspace, so
            // they belong in the transcript-normalized assertion stream — a
            // behavioural test can then assert "the diff was shown before the
            // write", which is the property that matters.
            AgentEvent::ToolPreview { name, preview, .. } => Some(format!(
                "tool-preview {name} {} +{} -{}",
                preview.path,
                preview.line_counts().0,
                preview.line_counts().1
            )),
            AgentEvent::ToolInputProgress { id, bytes, .. } => {
                Some(format!("tool-input-progress {id} {bytes}"))
            }
            AgentEvent::ToolResult { name, output, .. } => {
                Some(format!("tool-result {name} {output:?}"))
            }
            AgentEvent::ToolStream { id, stream } => {
                Some(format!("tool-stream {} {:?}", id, stream))
            }
            AgentEvent::ToolCancelled { name, .. } => Some(format!("tool-cancelled {name}")),
            AgentEvent::UnattendedChanged(enabled) => Some(format!("unattended {enabled}")),
            AgentEvent::ConfinementChanged(enabled) => Some(format!("confinement {enabled}")),
            AgentEvent::PermissionRequest(request) => Some(format!(
                "permission-request {} {}",
                request.tool, request.scope
            )),
            AgentEvent::UserQuestionRequest(request) => {
                Some(format!("user-question {}", request.questions.len()))
            }
            AgentEvent::StdinRequest(request) => Some(format!(
                "stdin-request {} (secret={})",
                request.command, request.secret
            )),
            AgentEvent::Subagent { .. } => Some("subtask".to_string()),
            AgentEvent::TodosUpdated(list) => Some(format!("todos {} items", list.len())),
            AgentEvent::BackgroundJobStarted(info) => {
                Some(format!("background-job-started {}", info.id.0))
            }
            AgentEvent::BackgroundJobProgress { job_id, .. } => {
                Some(format!("background-job-progress {}", job_id.0))
            }
            AgentEvent::BackgroundJobCompleted(outcome) => {
                Some(format!("background-job-completed {}", outcome.job_id.0))
            }
            AgentEvent::CatalogInvalidated => Some("catalog-invalidated".to_string()),
        })
        .collect()
}

/// Drive one full round, auto-answering any permission prompt with `decision`
/// so write-capable tools don't deadlock the loop.
async fn run_golden_round(
    agent: &Arc<Agent>,
    prompt: &str,
    decision: PermissionDecision,
) -> (Vec<AgentEvent>, Result<RoundOutcome, HarnessError>) {
    let mut messages = vec![Message::new(Role::User, prompt)];
    let mut events = Vec::new();
    let outcome = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |event| {
            if let AgentEvent::PermissionRequest(request) = &event {
                agent.reply_permission(&request.id, decision);
            }
            events.push(event);
        })
        .await;
    (events, outcome)
}

/// A read-only tool for the subagent child in the drain test.
struct SubagentReadTool;

#[async_trait]
impl Tool for SubagentReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "test read tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("file contents".to_string())
    }
}

/// A provider whose first request returns a `read` tool call and whose
/// second request flips the gate and then stalls forever — so the subagent is
/// parked mid-flight and can only stop when its cancellation token fires.
struct GatedSubagentProvider {
    requests: AtomicUsize,
    gate: tokio::sync::watch::Sender<bool>,
}

#[async_trait]
impl Provider for GatedSubagentProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "gated",
        )))
    }
    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }
    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<nuo_protocol::ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(nuo_protocol::ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("inner_1".to_string()),
                    name: Some("read".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(nuo_protocol::ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        } else {
            let _ = self.gate.send(true);
            Ok(Box::pin(stream::pending()))
        }
    }
}

/// Build N distinct read-only `alpha` turns (each with a different
/// path so they count as distinct calls rather than repeats), optionally
/// followed by a final text turn. Drives the turn count past a review line
/// without accumulating repeated-call counts.
fn distinct_read_turns(n: usize, suffix: Option<&str>) -> Vec<Vec<ProviderStreamEvent>> {
    let mut turns: Vec<Vec<ProviderStreamEvent>> = (0..n)
        .map(|i| turn(&[("c", "alpha", &format!("{{\"path\":\"f{i}\"}}"))]))
        .collect();
    if let Some(s) = suffix {
        turns.push(text_turn(s));
    }
    turns
}

mod agent_config;
mod guards;
mod permissions;
mod scheduling;
mod stream_loops;
mod streaming;
