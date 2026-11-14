//! Subagent dispatch tests.

use super::*;

use futures::stream::{self, BoxStream};
use nuo_protocol::{Provider, ProviderStreamEvent, SubAgentProfile};
use nuo_tool::{Message, Role};

struct CannedProvider;

#[async_trait::async_trait]
impl Provider for CannedProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "found 3 relevant files",
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
        Ok(Box::pin(stream::once(async {
            Ok("found 3 relevant files".to_string())
        })))
    }
}

#[derive(Default)]
struct RecordingProvider {
    request: std::sync::Mutex<Option<nuo_protocol::ModelRequest>>,
}

/// Fails the first `stream_chat_events` call with a retryable transport
/// error, succeeds afterwards — the exact shape of a GLM long SSE stream
/// cut off mid-body (`Kind::Decode` → `[NUO_RETRYABLE]`), which before
/// the subagent retry loop killed the sub-task outright.
struct FlakyThenOkProvider {
    calls: std::sync::atomic::AtomicUsize,
}

impl FlakyThenOkProvider {
    fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl Provider for FlakyThenOkProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "recovered",
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
        Ok(Box::pin(stream::once(async {
            Ok("recovered".to_string())
        })))
    }

    async fn stream_chat_events(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let seen = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if seen == 0 {
            return Err(nuo_protocol::ProviderError::new(
                "OpenAI",
                nuo_protocol::ProviderErrorKind::Transport,
                "OpenAI transport error: error decoding response body (connection closed before message completed)",
            ).retryable(None));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderStreamEvent::TextDelta("recovered".to_string())),
            Ok(ProviderStreamEvent::Completed(
                nuo_protocol::ProviderCompletionMeta::default(),
            )),
        ])))
    }
}
#[async_trait::async_trait]
impl Provider for RecordingProvider {
    async fn chat(
        &self,
        request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        *self.request.lock().unwrap() = Some(request.clone());
        Ok(nuo_protocol::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "found 3 relevant files",
        )))
    }

    async fn stream_chat(
        &self,
        request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        *self.request.lock().unwrap() = Some(request.clone());
        Ok(Box::pin(stream::once(async {
            Ok("found 3 relevant files".to_string())
        })))
    }
}

struct EchoReadTool;

#[async_trait::async_trait]
impl Tool for EchoReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "test read tool"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("echo".to_string())
    }
}

/// A terse `read` variant and a write tool, to prove a subagent
/// resolves the *model's* variant (override axis) and then narrows to the
/// *profile's* scope (scope axis) — the two are orthogonal.
struct TerseReadTool;
#[async_trait::async_trait]
impl Tool for TerseReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn variant(&self) -> &str {
        "terse"
    }
    fn description(&self) -> &str {
        "terse read tool"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("terse".to_string())
    }
}
#[test]
fn subagent_inherits_model_variant_then_applies_profile_scope() {
    // `StubWriteTool` (name "stub_write") is not in SubAgentProfile::EXPLORE's read-only
    // scope, so it is always excluded; `read` has two variants.
    let toolset = nuo_protocol::ToolSet::from_tools([
        std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
        std::sync::Arc::new(TerseReadTool) as std::sync::Arc<dyn Tool>,
        std::sync::Arc::new(StubWriteTool) as std::sync::Arc<dyn Tool>,
    ]);
    let tool = SubagentTool::new(
        std::sync::Arc::new(CannedProvider),
        toolset,
        &SubAgentProfile::EXPLORE,
    );

    let resolve = |tool: &SubagentTool| {
        let model = nuo_protocol::resolve_model(&CannedProvider.model());
        let model_sel =
            nuo_protocol::ToolSelection::unrestricted().with_variants(tool.variant_snapshot());
        tool.profile
            .resolve_tools(&tool.toolset, &model, &model_sel)
    };

    // Unbound (no model override) → read resolves to its default
    // variant; the out-of-scope write tool is excluded regardless.
    let scoped = resolve(&tool);
    let read = scoped.iter().find(|t| t.name() == "read");
    assert_eq!(read.map(|t| t.variant()), Some("default"));
    assert!(scoped.iter().all(|t| t.name() != "stub_write"));

    // Bind a model selection pinning read=terse: the subagent inherits
    // the override (terse), while scope is still profile-driven.
    let mut sel = nuo_protocol::VariantSelection::new();
    sel.insert("read".to_string(), "terse".to_string());
    tool.bind_variant_selection(std::sync::Arc::new(std::sync::Mutex::new(sel)));
    let scoped = resolve(&tool);
    let read = scoped.iter().find(|t| t.name() == "read");
    assert_eq!(read.map(|t| t.variant()), Some("terse"));
    assert!(scoped.iter().all(|t| t.name() != "stub_write"));
}

#[tokio::test]
async fn subagent_retries_after_transient_stream_failure() {
    let provider = std::sync::Arc::new(FlakyThenOkProvider::new());
    let tool = SubagentTool::new(
        std::sync::Arc::clone(&provider) as std::sync::Arc<dyn Provider>,
        nuo_protocol::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
        ]),
        &SubAgentProfile::EXPLORE,
    );

    let output = tool
        .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
        .await
        .expect("the subagent must recover from one transient stream failure");

    assert_eq!(
        output, "recovered",
        "the retry must reach the successful attempt's answer"
    );
    assert_eq!(
        provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "exactly one transient failure then one success"
    );
}

#[tokio::test]
async fn subagent_inherits_and_respects_custom_retry_policy() {
    let provider = std::sync::Arc::new(FlakyThenOkProvider::new());
    let tool = SubagentTool::new(
        std::sync::Arc::clone(&provider) as std::sync::Arc<dyn Provider>,
        nuo_protocol::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
        ]),
        &SubAgentProfile::EXPLORE,
    );
    tool.bind_retry_policy(1, 10, 10); // only 1 attempt

    let output = tool
        .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
        .await
        .expect("tool call returns error string in outcome");

    assert!(
        output.starts_with("Error:"),
        "should return error string when retry limit is 1 and first call fails: {output}"
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn task_tool_runs_read_only_subagent_and_returns_answer() {
    let tool = SubagentTool::new(
        std::sync::Arc::new(CannedProvider),
        nuo_protocol::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
        ]),
        &SubAgentProfile::EXPLORE,
    );

    let output = tool
        .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
        .await
        .unwrap();

    assert_eq!(output, "found 3 relevant files");
}

/// A provider that lets the test control when the *second* model request
/// is in flight: the first request returns a `read` tool call (which
/// the subagent executes), the second flips `second_request_started` and
/// then never produces a stream event — so the subagent is parked mid-flight
/// until its cancellation token fires.
struct GatedProvider {
    requests: std::sync::atomic::AtomicUsize,
    second_request_started: tokio::sync::watch::Sender<bool>,
}

#[async_trait]
impl Provider for GatedProvider {
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
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        if self
            .requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            // First request: ask the subagent to run its `read` tool.
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("subagent_inner_1".to_string()),
                    name: Some("read".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_protocol::ProviderCompletionMeta::default(),
                )),
            ])))
        } else {
            // Second request: tell the test the subagent is mid-flight, then
            // stall forever. The subagent's streaming loop races its
            // cancellation token against `stream.next()`, so cancelling
            // the child token resolves this immediately.
            let _ = self.second_request_started.send(true);
            Ok(Box::pin(stream::pending()))
        }
    }
}

/// Regression for cooperative interruption: when the parent cancels a
/// running subagent, the partial transcript is preserved as an *interrupted*
/// outcome (not dropped, not a failure). The child's completed tool call,
/// the task message, and a model-facing "Interrupted:" summary all survive
/// so the parent can record them and the user can resume.
#[tokio::test]
async fn interrupting_subagent_preserves_partial_transcript() {
    let (started_tx, started_rx) = tokio::sync::watch::channel(false);
    let provider = std::sync::Arc::new(GatedProvider {
        requests: std::sync::atomic::AtomicUsize::new(0),
        second_request_started: started_tx,
    });
    let tool = std::sync::Arc::new(SubagentTool::new(
        provider,
        nuo_protocol::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
        ]),
        &SubAgentProfile::EXPLORE,
    ));

    let tool_for_run = tool.clone();
    let run = tokio::spawn(async move {
        tool_for_run
            .run_subagent_outcome(
                Some("call_interrupt"),
                r#"{"description":"interrupt me","prompt":"find the handlers"}"#,
                Box::new(|_event: nuo_tool::SubagentEvent| {}),
            )
            .await
    });

    // Wait until the subagent is genuinely mid-flight (its second model
    // request is in the air), then interrupt it the way the harness's
    // executor does: via `Tool::request_cancel` keyed by the call id.
    let mut started_rx = started_rx;
    started_rx
        .changed()
        .await
        .expect("subagent reached its second request");
    assert!(
        tool.request_cancel("call_interrupt"),
        "an in-flight subagent must accept the cancel request"
    );

    let outcome = run.await.expect("subagent run task").expect("outcome");
    assert!(outcome.interrupted, "interruption must be flagged");
    assert!(!outcome.failed, "interruption is not a failure");
    assert!(
        outcome.final_content.starts_with("Interrupted:"),
        "model-facing summary should say Interrupted, got: {}",
        outcome.final_content
    );
    // The partial transcript must contain the completed read round.
    let tool_result_msgs = outcome
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .count();
    assert_eq!(
        tool_result_msgs, 1,
        "the completed child tool call must survive in the partial transcript"
    );
    assert!(outcome.messages[0].role == Role::User);
    // A late cancel for a finished call degrades to a no-op, not an error.
    assert!(
        !tool.request_cancel("call_interrupt"),
        "a finished call must reject a late cancel"
    );
}

/// The subagent persona belongs to the immutable provider request, not its
/// durable child transcript. The delegated task remains a user message.
#[tokio::test]
async fn subagent_head_system_message_has_no_dead_task_line() {
    let provider = std::sync::Arc::new(RecordingProvider::default());
    let tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
        ]),
        &SubAgentProfile::EXPLORE,
    );
    let outcome = tool
        .run_subagent_outcome(
            None,
            r#"{"description":"find files","prompt":"where are the handlers?"}"#,
            Box::new(|_event: nuo_tool::SubagentEvent| {}),
        )
        .await
        .unwrap();

    let request = provider
        .request
        .lock()
        .unwrap()
        .clone()
        .expect("subagent request captured");
    let system_content = request.instructions.fixed.clone();
    assert!(
        system_content.starts_with("You are a delegated research subagent"),
        "system instructions should open with the SubAgentProfile::EXPLORE task prompt"
    );
    assert!(
        !system_content.contains("Task: find files"),
        "the dead `Task: {{description}}` line must not appear (ADR-0039)"
    );

    assert!(
        outcome
            .messages
            .iter()
            .all(|message| message.role != nuo_tool::Role::System),
        "request-scoped policy must not be persisted in the child transcript"
    );

    // The task is the first durable user message.
    assert_eq!(outcome.messages[0].role, nuo_tool::Role::User);
    assert_eq!(outcome.messages[0].content, "where are the handlers?");
    assert_eq!(
        outcome.messages[0]
            .origin
            .as_ref()
            .map(|origin| origin.kind),
        Some(nuo_tool::InjectionKind::SubagentTask)
    );
}

#[tokio::test]
async fn task_tool_rejects_missing_fields() {
    let tool = SubagentTool::new(
        std::sync::Arc::new(CannedProvider),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );
    assert!(tool.call(r#"{"description":"x"}"#).await.is_err());
    assert!(tool.call(r#"{"prompt":"x"}"#).await.is_err());
}

/// The tool cannot dispatch a background child (the call awaits
/// the child round and returns its result), so both the schema and an
/// explicit request must say so instead of promising a notification that
/// never arrives.
#[tokio::test]
async fn subagent_schema_hides_and_rejects_background_dispatch() {
    let tool = SubagentTool::new(
        std::sync::Arc::new(CannedProvider),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );
    assert!(
        tool.parameters()
            .get("properties")
            .and_then(|p| p.get("background"))
            .is_none(),
        "the unsupported background parameter must not be advertised"
    );
    let err = tool
        .call(r#"{"description":"x","prompt":"y","background":true}"#)
        .await
        .expect_err("background dispatch must be rejected explicitly");
    assert!(
        err.contains("not available") && err.contains("without `background`"),
        "rejection must be actionable: {err}"
    );
}

/// A non-whitelisted stub, used to prove the explore profile rejects tools
/// by name (it is not in READ_ONLY_TOOLS).
struct StubWriteTool;

#[async_trait::async_trait]
impl Tool for StubWriteTool {
    fn name(&self) -> &str {
        "stub_write"
    }
    fn description(&self) -> &str {
        "test write tool"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("write".to_string())
    }
}

/// Regression for the deadlock fixed in ADR-0011: the explore profile
/// must exclude (a) the real `ask_user` tool — Read but interactive,
/// (b) any write tool, and (c) `task` itself — Read but a dispatch tool
/// that would recurse. Built with the real tool instances the harness
/// registers, not stubs, so a future capability-bit regression on either
/// side is caught here.
#[test]
fn explore_profile_excludes_user_write_and_recursion_using_real_tools() {
    let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
    let subagent_tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let toolset = nuo_protocol::ToolSet::from_tools(vec![
        std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
        std::sync::Arc::new(crate::tools::AskUserTool),
        std::sync::Arc::new(StubWriteTool),
        std::sync::Arc::new(subagent_tool),
    ]);

    let model = nuo_protocol::resolve_model(&CannedProvider.model());
    let model_sel = nuo_protocol::ToolSelection::unrestricted();
    let admitted = SubAgentProfile::EXPLORE.resolve_tools(&toolset, &model, &model_sel);
    let admitted_names: Vec<&str> = admitted.iter().map(|t| t.name()).collect();

    assert_eq!(admitted_names, vec!["read"]);
}

/// Cross-cut regression: `SubAgentProfile::EXPLORE` admits only its whitelisted read tools —
/// `ask_user`, the non-whitelisted write stub, and recursion are all
/// excluded. The read stub is admitted because it is named `read`,
/// which is in [`READ_ONLY_TOOLS`].
#[test]
fn explore_profile_excludes_bash_writes_user_and_recursion() {
    let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
    let subagent_tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let exec_ctx = std::sync::Arc::new(nuo_tool_exec::ExecToolContext::new("."));
    let toolset = nuo_protocol::ToolSet::from_tools(vec![
        std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
        std::sync::Arc::new(crate::tools::ExecuteCommandTool::new(exec_ctx)),
        std::sync::Arc::new(crate::tools::AskUserTool),
        std::sync::Arc::new(StubWriteTool),
        std::sync::Arc::new(subagent_tool),
    ]);

    // SubAgentProfile::EXPLORE: only the whitelisted read tool survives (bash, ask_user,
    // the write stub, and recursion are all excluded).
    let model = nuo_protocol::resolve_model(&CannedProvider.model());
    let model_sel = nuo_protocol::ToolSelection::unrestricted();
    let explore_selected = SubAgentProfile::EXPLORE.resolve_tools(&toolset, &model, &model_sel);
    let explore_names: Vec<&str> = explore_selected.iter().map(|t| t.name()).collect();
    assert_eq!(explore_names, vec!["read"]);
}

/// A diagnostic `delegate_debug` tool (bound to [`nuo_protocol::SubAgentProfile::DEBUG`])
/// admits read tools and command execution/process tools, but strictly excludes
/// workspace writes, human interaction, and recursion. Built with the real tools the
/// harness registers so a future capability regression is caught here —
/// mirrors `explore_profile_excludes_bash_writes_user_and_recursion` for
/// the inverse contract.
#[test]
fn debug_profile_admits_command_and_read_tools_excluding_writes_and_recursion() {
    let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
    let delegate_debug_arc = std::sync::Arc::new(SubagentTool::named(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &nuo_protocol::SubAgentProfile::DEBUG,
        "delegate_debug",
        "debugging subagent",
    ));

    let sys_ctx = std::sync::Arc::new(nuo_tool_fs::SystemToolContext::new("."));
    let exec_ctx = std::sync::Arc::new(nuo_tool_exec::ExecToolContext::new("."));
    let toolset = nuo_protocol::ToolSet::from_tools(vec![
        std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
        std::sync::Arc::new(crate::tools::ExecuteCommandTool::new(exec_ctx)),
        std::sync::Arc::new(crate::tools::WriteFileTool::new(sys_ctx.clone())),
        std::sync::Arc::new(crate::tools::EditTextTool::new(sys_ctx)),
        std::sync::Arc::new(crate::tools::AskUserTool),
        delegate_debug_arc.clone() as std::sync::Arc<dyn Tool>,
    ]);

    // SubAgentProfile::DEBUG admits execute_command and the read tools; it
    // strictly excludes write_file, edit_text, ask_user, and the subagent dispatch tool itself (recursion).
    let model = nuo_protocol::resolve_model(&CannedProvider.model());
    let model_sel = nuo_protocol::ToolSelection::unrestricted();
    let selected = nuo_protocol::SubAgentProfile::DEBUG.resolve_tools(&toolset, &model, &model_sel);
    let names: std::collections::HashSet<&str> = selected.iter().map(|t| t.name()).collect();
    assert!(names.contains("read"));
    assert!(names.contains("execute_command"));
    assert!(!names.contains("write_file"));
    assert!(!names.contains("edit_text"));
    assert!(!names.contains("ask_user"));
    assert!(
        !names.contains("delegate_debug"),
        "recursion must be excluded"
    );

    // The tool surfaces under its own name.
    assert_eq!(delegate_debug_arc.name(), "delegate_debug");
}

/// Two dispatch tools sharing one registry is the load-bearing property
/// that lets the harness route a reply to the correct child regardless of
/// which tool spawned it. A child registered under one tool's call id is
/// reachable via the shared registry, and neither tool's `registry()` is
/// a distinct allocation.
#[test]
fn named_with_registry_shares_the_registry_across_tools() {
    let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
    let explore = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );
    let shared = explore.registry();
    let debug = SubagentTool::named_with_registry(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &nuo_protocol::SubAgentProfile::DEBUG,
        "delegate_debug",
        "debugging subagent",
        shared.clone(),
    );
    // Same Arc<SubagentRegistry> — the driver hands one to the harness, and
    // children of either tool land in the same table.
    assert!(
        std::sync::Arc::ptr_eq(&explore.registry(), &debug.registry()),
        "named_with_registry must share the registry, not clone-allocate"
    );
    // The two tools are distinct capabilities (different names) so they
    // coexist in a parent toolset without one shadowing the other.
    assert_ne!(explore.name(), debug.name());
    assert_eq!(explore.name(), "spawn_agent");
    assert!(!explore.matches_name("delegate_debug"));
}

#[tokio::test]
async fn debug_subagent_role_execution() {
    let provider = std::sync::Arc::new(CannedProvider);
    let tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let summary = tool
        .call(r#"{"description":"debug crash","prompt":"investigate segfault in parser","role":"debug"}"#)
        .await
        .expect("debug subagent dispatch succeeds");

    assert_eq!(summary, "found 3 relevant files");
}

#[tokio::test]
async fn skill_subagent_role_execution() {
    let provider = std::sync::Arc::new(CannedProvider);
    let tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let summary = tool
        .call(r#"{"description":"find rust skill","prompt":"locate rust guidelines in .nuo/skills","role":"skill"}"#)
        .await
        .expect("skill subagent dispatch succeeds");

    assert_eq!(summary, "found 3 relevant files");
}

/// The `role` enum in the schema is the enforced contract, not a hint: an
/// unadvertised value is refused with the dispatchable set named, instead
/// of being silently downgraded to the bound default (a caller that asked
/// for a write-capable child must not receive a read-only one without
/// being told).
#[tokio::test]
async fn unknown_role_is_rejected_with_the_dispatchable_set() {
    let provider = std::sync::Arc::new(CannedProvider);
    let tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let error = tool
        .call(r#"{"description":"typo","prompt":"do something","role":"explort"}"#)
        .await
        .expect_err("an unadvertised role must not silently run");
    assert!(error.contains("Unknown 'role' 'explort'"), "got: {error}");
    assert!(
        error.contains("explore"),
        "must name dispatchable roles: {error}"
    );
}

/// `title` lives in the preset pool but is harness-internal (session
/// titling drives it directly). It must not be spawnable through the
/// model-facing dispatch tool just because the pool contains it.
#[tokio::test]
async fn internal_title_role_is_not_dispatchable() {
    let provider = std::sync::Arc::new(CannedProvider);
    let tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let error = tool
        .call(r#"{"description":"title","prompt":"name this session","role":"title"}"#)
        .await
        .expect_err("the internal titling role must not be model-dispatchable");
    assert!(error.contains("Unknown 'role' 'title'"), "got: {error}");
}

/// The advertised enum and the enforced check come from one constant, so a
/// role can never be offered by the schema and refused at runtime (or
/// accepted at runtime and hidden from the schema).
#[test]
fn schema_role_enum_matches_the_enforced_dispatch_roles() {
    let tool = SubagentTool::new(
        std::sync::Arc::new(CannedProvider),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    let advertised: Vec<String> = tool.parameters()["properties"]["role"]["enum"]
        .as_array()
        .expect("role enum is an array")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("role entries are strings")
                .to_string()
        })
        .collect();
    assert_eq!(
        advertised,
        DISPATCH_ROLES
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
    );
    // Every advertised role must resolve to a real preset.
    for role in DISPATCH_ROLES {
        assert!(
            nuo_protocol::SubAgentProfile::find(role).is_some(),
            "advertised role '{role}' has no preset"
        );
    }
}

#[tokio::test]
async fn execution_policy_rejects_subagent_spawning_when_depth_capped() {
    let provider = std::sync::Arc::new(CannedProvider);
    let tool = SubagentTool::new(
        provider.clone(),
        nuo_protocol::ToolSet::default(),
        &SubAgentProfile::EXPLORE,
    );

    // Max depth is 1, current depth is already 1 (child trying to spawn grandchild)
    let capped_policy = nuo_protocol::ExecutionPolicy {
        depth: 1,
        max_depth: 1,
        allow_human_interaction: false,
        lifecycle: nuo_protocol::ContextLifecycle::EphemeralScratchpad,
        tool_policy: None,
        max_children_budget: 0,
    };
    tool.bind_execution_policy(capped_policy);

    let result = tool
        .call(
            r#"{"description":"nested subagent","prompt":"spawn more subagents","role":"explore"}"#,
        )
        .await;

    assert!(
        result.is_err(),
        "must be rejected when depth limit is reached"
    );
    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("Sub-agent spawning is rejected by ExecutionPolicy"),
        "error should mention ExecutionPolicy: {err_msg}"
    );
}
