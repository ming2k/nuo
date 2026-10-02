//! Integration test: panicking on assertion failure is the desired
//! behaviour here, so the workspace `unwrap_used`/`expect_used` lints
//! are relaxed for this file. (Lib/bin code stays linted.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Full-duplex substrate tests (ADR-0029).
//!
//! These run as a standalone integration binary (`cargo test --test duplex`)
//! so they compile against the crate's public API only and do not depend on
//! the in-`lib` unit-test module. They prove the two directions of the
//! parent↔subagent channel at the agent layer:
//!
//! 1. **Down (steering):** an `AgentOp::InjectUserMessage` submitted through a
//!    `SubagentHandle` lands in the live transcript before the next model
//!    round.
//! 2. **Down (reply) + Up (request):** a write tool's permission broker
//!    surfaces `AgentEvent::PermissionRequest` up through
//!    `run_streaming_with_events`, and a `reply_permission` submitted through
//!    the handle resolves the parked oneshot so the tool actually runs.
//!
//! The end-to-end path through `SubagentTool` (registry lookup keyed by
//! `parent_call_id`, nested `SubagentEvent::PermissionRequest` rendered in the
//! TUI) is the harness↔TUI integration step that follows; these tests cover
//! the substrate it will be built on.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use nuo_harness::{
    Agent, AgentEvent, AgentOp, Message, Provider, ProviderStreamEvent, Role, SubagentEvent,
    SubagentTool,
};
use nuo_contracts::{PermissionDecision, SubAgentProfile, Tool, ToolOutput, ToolPolicy};

/// `stream_chat` emits "done" with no tool calls (the default
/// `stream_chat_events` wraps it into one `TextDelta`). Used by the inject
/// test, where only the transcript mutation matters.
struct IdleProvider;

#[async_trait]
impl Provider for IdleProvider {
    async fn chat(
        &self,
        _request: nuo_contracts::ModelRequest,
    ) -> Result<nuo_contracts::ProviderCompletion, nuo_contracts::ProviderError> {
        Ok(nuo_contracts::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }
    async fn stream_chat(
        &self,
        _request: nuo_contracts::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_contracts::ProviderError>>,
        nuo_contracts::ProviderError,
    > {
        Ok(Box::pin(stream::once(async { Ok("done".to_string()) })))
    }
}

#[tokio::test]
async fn inject_user_message_lands_in_transcript() {
    // An agent with an installed inbox is steerable: submitting an
    // `InjectUserMessage` before the turn starts causes the driver's
    // round-top drain to append it to the live transcript, so the model sees
    // it on turn 0. A non-steerable agent (no `install_inbox`) would have no
    // inbox receiver and the op would be dropped — covered by the `submit`
    // returning `false` when no inbox exists.
    let agent = Arc::new(Agent::new(
        Arc::new(IdleProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let handle = agent.install_inbox();

    assert!(handle.submit(AgentOp::Steer("STEER-PAYLOAD-9f3a".to_string())));

    let mut messages = vec![Message::new(Role::User, "begin")];
    let _ = agent
        .run_streaming_with_events(&mut messages, &CancellationToken::new(), |_| {})
        .await
        .expect("round completes");

    assert!(
        messages
            .iter()
            .any(|m| m.content.contains("STEER-PAYLOAD-9f3a")),
        "injected steering message must reach the live transcript"
    );
}

/// A Write-tier tool whose execution is gated by the permission broker. Its
/// `call` records that it actually ran by flipping a shared flag, proving the
/// reply reached the parked oneshot (rather than the tool being denied or the
/// turn hanging).
struct BrokerGatedTool(Arc<AtomicUsize>);

#[async_trait]
impl Tool for BrokerGatedTool {
    fn name(&self) -> &str {
        "gated_write"
    }
    fn description(&self) -> &str {
        "test write tool gated by the permission broker"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn scope_target(&self, _arguments: &str) -> nuo_contracts::ScopeTarget {
        nuo_contracts::ScopeTarget::Path(std::path::PathBuf::from("/tmp/gated"))
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("wrote".to_string())
    }
    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        self.call(arguments).await.map(ToolOutput::text)
    }
}

#[tokio::test]
async fn handle_reply_permission_unblocks_parked_write_tool() {
    // The full request/reply round-trip:
    //   child broker ──PermissionRequest──▶ on_event ──▶ test harness
    //   test harness ──reply_permission──▶ handle ──▶ child's parked oneshot
    // The tool only runs (flag flips) if the reply resolves the oneshot; if the
    // handle were a no-op or the reply mis-routed, the task would hang and the
    // timeout below would fire.
    let ran = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(StreamWriteCallProvider(AtomicUsize::new(0))),
        vec![Arc::new(BrokerGatedTool(Arc::clone(&ran)))],
        nuo_harness::AgentIdentity::default(),
    ));
    let handle = agent.install_inbox();

    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<nuo_contracts::PermissionRequest>();
    let run_agent = Arc::clone(&agent);
    let task = tokio::spawn(async move {
        let mut messages = vec![Message::new(Role::User, "run the write tool")];
        run_agent
            .run_streaming_with_events(&mut messages, &CancellationToken::new(), move |event| {
                if let AgentEvent::PermissionRequest(req) = event {
                    let _ = req_tx.send(req);
                }
            })
            .await
    });

    let request = tokio::time::timeout(std::time::Duration::from_secs(10), req_rx.recv())
        .await
        .expect("permission request must surface up via on_event")
        .expect("channel not closed before a request arrived");
    // The subagent is parked on the broker oneshot at this point.
    assert!(!task.is_finished(), "child must be parked awaiting reply");
    assert_eq!(request.tool, "gated_write");

    assert!(
        handle.reply_permission(&request.id, PermissionDecision::Once),
        "reply must resolve the parked oneshot while the child is alive"
    );

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("child must complete after the reply")
        .expect("task join")
        .expect("turn must succeed");
    assert_eq!(outcome.message.content, "done");
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the gated tool must have run"
    );
}

#[tokio::test]
async fn handle_reply_is_noop_after_agent_dropped() {
    // When the child's dispatcher has ended and dropped its `Arc`, every handle
    // method degrades to a no-op rather than erroring — so a late UI reply
    // after the subagent already finished can never panic or wedgelock state.
    let agent = Arc::new(Agent::new(
        Arc::new(IdleProvider),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let handle = agent.install_inbox();
    drop(agent);
    assert!(!handle.is_alive());
    assert!(
        !handle.reply_permission("any", PermissionDecision::Once),
        "reply on a dropped agent must be a no-op"
    );
    assert!(
        !handle.submit(AgentOp::Interrupt),
        "submit on a dropped agent must be a no-op"
    );
}

/// Streaming provider: turn 0 emits a tool-call for `gated_write`; turn 1
/// emits plain text "done". Drives both the direct
/// `run_streaming_with_events` loop test and the SubagentTool end-to-end path
/// (which runs the child via `run_streaming_with_events`).
struct StreamWriteCallProvider(AtomicUsize);

#[async_trait]
impl Provider for StreamWriteCallProvider {
    async fn chat(
        &self,
        _: nuo_contracts::ModelRequest,
    ) -> Result<nuo_contracts::ProviderCompletion, nuo_contracts::ProviderError> {
        Err(nuo_contracts::ProviderError::new(
            "mock",
            nuo_contracts::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }
    async fn stream_chat(
        &self,
        _: nuo_contracts::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_contracts::ProviderError>>,
        nuo_contracts::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }
    async fn stream_chat_events(
        &self,
        _: nuo_contracts::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_contracts::ProviderError>>,
        nuo_contracts::ProviderError,
    > {
        let round = self.0.fetch_add(1, Ordering::SeqCst);
        let events = if round == 0 {
            vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("child_call".to_string()),
                    name: Some("gated_write".to_string()),
                    arguments: "{}".to_string(),
                }),
                Ok(ProviderStreamEvent::Completed(
                    nuo_contracts::ProviderCompletionMeta::default(),
                )),
            ]
        } else {
            vec![
                Ok(ProviderStreamEvent::TextDelta("done".to_string())),
                Ok(ProviderStreamEvent::Completed(
                    nuo_contracts::ProviderCompletionMeta::default(),
                )),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

/// A test-only profile that admits write tools *and* leaves the permission
/// broker on (`delegated: false`), so the child's write call surfaces a
/// `PermissionRequest` — the shape needed to exercise the full up→down
/// round-trip through `SubagentTool` + the registry. Declared `const` because
/// `SubagentTool::new` borrows the profile for `'static`.
const INTERACTIVE: SubAgentProfile = SubAgentProfile {
    name: "test_interactive",
    system_prompt: "test",
    tool_policy: ToolPolicy {
        allowed_tools: None,
        allow_user_interaction: false,
    },
    variant_pins: &[],
    unattended: false,
    allow_model_stdin: false,
};

#[tokio::test]
async fn streaming_loop_fires_permission_broker_direct() {
    // Isolation: does run_streaming_with_events itself surface a permission
    // request for a write tool when unattended is false? Decouples the
    // streaming driver from the SubagentTool wrapping.
    let ran = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(StreamWriteCallProvider(AtomicUsize::new(0))),
        vec![Arc::new(BrokerGatedTool(Arc::clone(&ran))) as Arc<dyn Tool>],
        nuo_harness::AgentIdentity::default(),
    ));
    agent.set_unattended(false);

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let a = Arc::clone(&agent);
    let task = tokio::spawn(async move {
        let mut msgs = vec![Message::new(Role::User, "run the write tool")];
        a.run_streaming_with_events(&mut msgs, &CancellationToken::new(), |ev| {
            let _ = evt_tx.send(ev);
        })
        .await
    });

    let mut got = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while got.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Some(AgentEvent::PermissionRequest(r))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), evt_rx.recv()).await
        {
            got = Some(r);
        }
    }
    let req = got.expect("streaming loop must surface PermissionRequest");
    assert!(agent.reply_permission(&req.id, PermissionDecision::Once));
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("completes");
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn subagent_tool_registry_routes_reply_into_live_subagent() {
    // End-to-end through SubagentTool with an interactive profile
    // (`delegated: false`): the child's execute-tier tool surfaces a
    // permission request UP as `SubagentEvent::PermissionRequest`, the tool
    // registers the child's handle by the parent `call_id`, and a reply pulled
    // from the registry resolves the parked oneshot so the tool runs. This is
    // the agent-layer contract the session driver and TUI rely on.
    let ran = Arc::new(AtomicUsize::new(0));
    let subagent_tool = Arc::new(SubagentTool::new(
        Arc::new(StreamWriteCallProvider(AtomicUsize::new(0))),
        nuo_contracts::ToolSet::from_tools([
            Arc::new(BrokerGatedTool(Arc::clone(&ran))) as Arc<dyn Tool>
        ]),
        &INTERACTIVE,
    ));
    let registry = subagent_tool.registry();

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<SubagentEvent>();
    let tool = Arc::clone(&subagent_tool);
    let task = tokio::spawn(async move {
        let mut on_stream = |_: nuo_harness::ToolStream| ();
        tool.call_structured_with_events(
            nuo_contracts::ToolInvocation {
                call_id: "parent_call_7",
                arguments: r#"{"description":"d","prompt":"run the write tool"}"#,
                input: nuo_contracts::InputContract::default(),
                input_handler: None,
            },
            Box::new(move |e| {
                let _ = evt_tx.send(e);
            }),
            &mut on_stream,
        )
        .await
    });

    // Drain SubagentEvents until the permission request surfaces.
    let mut request = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while request.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Some(SubagentEvent::PermissionRequest(r))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), evt_rx.recv()).await
        {
            request = Some(r);
        }
    }
    let request = request.expect("subagent permission request must surface up via SubagentEvent");
    assert_eq!(request.tool, "gated_write");
    assert!(!task.is_finished(), "child parked awaiting reply");

    // The registry must hold the live child's handle keyed by the call_id.
    let handle = registry
        .get("parent_call_7")
        .expect("child handle registered by call_id");
    assert!(handle.is_alive());
    assert!(
        handle.reply_permission(&request.id, PermissionDecision::Once),
        "registry reply must resolve the parked oneshot"
    );

    let output = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("child completes after reply")
        .expect("join")
        .expect("tool call ok");
    assert!(
        output.to_text().contains("done"),
        "final summary should carry the child's answer"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1, "gated tool must have run");
    assert!(
        registry.get("parent_call_7").is_none(),
        "registry entry cleared after the child finished"
    );
}
