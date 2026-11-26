use super::*;
use futures::stream::{self, BoxStream};
use serde_json::Value;
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
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        Ok(protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
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
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        Ok(protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        Ok(Box::pin(stream::empty()))
    }

    fn prompt_hints(&self) -> protocol::ProviderPromptHints {
        protocol::ProviderPromptHints {
            system_guidance: "Provider protocol hint.",
        }
    }
}

#[async_trait]
impl Provider for PermissionTestProvider {
    async fn chat(
        &self,
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        unreachable!("streaming path should be used")
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        unreachable!("stream_chat_events should be called directly")
    }

    async fn stream_chat_events(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, protocol::ProviderError>>,
        protocol::ProviderError,
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
                    protocol::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    protocol::ProviderCompletionMeta::default(),
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
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        Err(protocol::ProviderError::new(
            "mock",
            protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, protocol::ProviderError>>,
        protocol::ProviderError,
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
                    protocol::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("do".to_string())),
                Ok(ProviderStreamEvent::TextDelta("ne".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    protocol::ProviderCompletionMeta::default(),
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

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn scope_target(&self, _arguments: &Value) -> tool::ScopeTarget {
        tool::ScopeTarget::Path(std::path::PathBuf::from("/tmp/test"))
    }

    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        Ok(::tool::ToolOutput::text("should not run".to_string()))
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

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        assert_eq!(arguments, serde_json::json!({"value":1}));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(::tool::ToolOutput::text("read".to_string()))
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

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        Ok(::tool::ToolOutput::text("wrong state".to_string()))
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
    cognitive_requests: std::sync::Mutex<Vec<protocol::ModelRequest>>,
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
        request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
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
        Ok(protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            reply,
        )))
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, protocol::ProviderError>>,
        protocol::ProviderError,
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
                protocol::ProviderCompletionMeta::default(),
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
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn scope_target(&self, arguments: &Value) -> tool::ScopeTarget {
        if self.name == "execute_command" {
            let command = arguments
                .get("command")
                .and_then(|c| c.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| arguments.to_string());
            tool::ScopeTarget::Command(command)
        } else if self.declares_target {
            // Pull a path from the args if present, else a fixed sentinel, so
            // the broker fires for the `write` variant.
            let path = arguments
                .get("path")
                .and_then(|p| p.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "/tmp/recording".to_string());
            tool::ScopeTarget::Path(std::path::PathBuf::from(path))
        } else {
            tool::ScopeTarget::Unspecified
        }
    }
    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(arguments.to_string());
        Ok(::tool::ToolOutput::text(self.output.clone()))
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
            AgentEvent::Compacted(cp) => {
                Some(format!("compacted archived={}", cp.archived_messages))
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
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(
        &self,
        _ctx: &::tool::ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
        Ok(::tool::ToolOutput::text("file contents".to_string()))
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
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        Ok(protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "gated",
        )))
    }
    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<BoxStream<'static, Result<String, protocol::ProviderError>>, protocol::ProviderError>
    {
        Ok(Box::pin(stream::empty()))
    }
    async fn stream_chat_events(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<protocol::ProviderStreamEvent, protocol::ProviderError>>,
        protocol::ProviderError,
    > {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(protocol::ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("inner_1".to_string()),
                    name: Some("read".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(protocol::ProviderStreamEvent::Completed(
                    protocol::ProviderCompletionMeta::default(),
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
mod context_measurement;
mod guards;
mod permissions;
mod scheduling;
mod stream_loops;
mod streaming;

#[tokio::test]
async fn a_tool_timeout_is_enforced_by_the_dispatcher() {
    use std::future::pending;

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &str {
            "slow"
        }
        fn description(&self) -> &str {
            "never returns"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn timeout(&self) -> Option<std::time::Duration> {
            Some(std::time::Duration::from_millis(20))
        }
        async fn execute(
            &self,
            _ctx: &::tool::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
            let _: () = pending().await;
            unreachable!("the dispatcher timeout must win")
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(SlowTool)],
        crate::AgentIdentity::default(),
    ));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let call = ToolCall::new("slow_call", "slow", "{}");
    let output = agent
        .execute_tool(&call, &tx, &CancellationToken::new())
        .await;
    assert!(output.is_error(), "got {output:?}");
    assert!(
        output.to_text().contains("timed out"),
        "{}",
        output.to_text()
    );
}

#[tokio::test]
async fn tool_search_activates_a_deferred_tool_for_the_next_turn() {
    struct DeferredStub;

    #[async_trait]
    impl Tool for DeferredStub {
        fn name(&self) -> &str {
            "deferred_capability"
        }
        fn description(&self) -> &str {
            "A capability that is only declared after discovery."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn exposure(&self) -> tool::ToolExposure {
            tool::ToolExposure::Deferred
        }
        async fn execute(
            &self,
            _ctx: &::tool::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
            Ok(::tool::ToolOutput::text("found"))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(DeferredStub)],
        crate::AgentIdentity::default(),
    ));
    assert!(
        !agent
            .visible_tools()
            .iter()
            .any(|tool| tool.name() == "deferred_capability"),
        "a deferred tool must not be declared before discovery"
    );
    agent.install_tool(Arc::new(crate::tools::ToolSearchTool::new(Arc::downgrade(
        &agent,
    ))));
    // The next model request rebuilds the declaration snapshot; do the same
    // here because the install happened after the previous request.
    agent.refresh_tool_snapshot();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let call = ToolCall::new(
        "search_1",
        "tool_search",
        r#"{"query":"deferred capability"}"#,
    );
    let output = agent
        .execute_tool(&call, &tx, &CancellationToken::new())
        .await;
    assert!(
        output.to_text().contains("deferred_capability"),
        "{}",
        output.to_text()
    );
    assert!(
        agent
            .visible_tools()
            .iter()
            .any(|tool| tool.name() == "deferred_capability"),
        "activation must make the tool visible on the next request"
    );

    let empty = ToolCall::new("search_2", "tool_search", r#"{"query":"   "}"#);
    let output = agent
        .execute_tool(&empty, &tx, &CancellationToken::new())
        .await;
    assert!(output.is_error(), "empty search must not activate tools");
    assert!(
        output.to_text().contains("must name a capability"),
        "{}",
        output.to_text()
    );
}

#[tokio::test]
async fn tool_progress_reaches_the_event_stream() {
    struct ProgressTool;

    #[async_trait]
    impl Tool for ProgressTool {
        fn name(&self) -> &str {
            "progressing"
        }
        fn description(&self) -> &str {
            "emits one progress tick"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            ctx: &::tool::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
            ctx.emit_progress(::tool::ToolProgress::new("half", Some(1), Some(2)));
            Ok(::tool::ToolOutput::text("done"))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(ProgressTool)],
        crate::AgentIdentity::default(),
    ));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let call = ToolCall::new("progress_call", "progressing", "{}");
    let output = agent
        .execute_tool(&call, &tx, &CancellationToken::new())
        .await;
    assert!(!output.is_error());
    match rx.try_recv() {
        Ok(AgentEvent::ToolStream {
            id,
            stream: tool::ToolStream::Progress(progress),
        }) => {
            assert_eq!(id, "progress_call");
            assert_eq!(progress.message, "half");
            assert_eq!(progress.current, Some(1));
        }
        other => panic!("expected a progress event, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_call_keeps_its_last_progress_tick() {
    struct FailingProgressTool;

    #[async_trait]
    impl Tool for FailingProgressTool {
        fn name(&self) -> &str {
            "failing_progress"
        }
        fn description(&self) -> &str {
            "emits progress, then fails"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            ctx: &::tool::ToolContext,
            _arguments: serde_json::Value,
        ) -> Result<::tool::ToolOutput, ::tool::ToolError> {
            ctx.emit_progress(::tool::ToolProgress::new("half", Some(1), Some(2)));
            Ok(::tool::ToolOutput::error("server stopped"))
        }
    }

    let agent = Arc::new(Agent::new(
        Arc::new(TestProvider),
        vec![Arc::new(FailingProgressTool)],
        crate::AgentIdentity::default(),
    ));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let call = ToolCall::new("failing_progress_call", "failing_progress", "{}");
    let output = agent
        .execute_tool(&call, &tx, &CancellationToken::new())
        .await;
    assert!(output.is_error());
    let text = output.to_text();
    assert!(text.contains("server stopped"), "{text}");
    assert!(text.contains("Last progress: half — 1/2 (50%)"), "{text}");
}
