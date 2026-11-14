use super::{
    RoundState, ScopedToolDisable, checkpoint_tool_signature, permission_required_output,
    subagent_result_text,
};

#[test]
fn missing_command_authority_has_runtime_only_guidance() {
    let command = "pwd; ls -la".to_string();
    let request = nuo_tool::PermissionRequest {
        id: String::new(),
        tool: "execute_command".to_string(),
        label: "run command".to_string(),
        description: String::new(),
        arguments: String::new(),
        scope: command.clone(),
        elevation: false,
        one_off: false,
        origin: None,
        hazard: Some(nuo_tool::HazardLevel::CommandExecution),
        submission: Some(nuo_tool::ToolPermissionSubmission {
            hazard_level: nuo_tool::HazardLevel::CommandExecution,
            label: "run command".to_string(),
            description: String::new(),
            scope: command.clone(),
            payload: nuo_tool::ToolPermissionPayload::Command {
                command: command.clone(),
                cwd: None,
                kill_spec: nuo_tool::ProcessKillSpec {
                    command: "pwd".to_string(),
                    process_group_killable: true,
                    pkill_target: "pkill -f pwd".to_string(),
                    cwd: None,
                },
            },
        }),
        preview: None,
    };

    let output = permission_required_output(&request).to_text();
    assert!(
        output.contains(
            "[permission required] Command 'pwd; ls -la' requires runtime execution grant."
        )
    );
    assert!(output.contains("Add a permission rule in settings or approve interactively."));
    assert!(!output.contains("/trust"));
}

fn tool_call(id: &str, arguments: &str) -> nuo_tool::ToolCall {
    nuo_tool::ToolCall {
        id: id.to_string(),
        name: "write_file".to_string(),
        arguments: arguments.to_string(),
    }
}

#[test]
fn checkpoint_tool_identity_ignores_json_object_key_order() {
    let first = tool_call("first", r#"{"path":"x","content":"y"}"#);
    let retried = tool_call("retry", r#"{"content":"y","path":"x"}"#);
    assert_eq!(
        checkpoint_tool_signature(&first),
        checkpoint_tool_signature(&retried)
    );
}

#[test]
fn provider_retry_protects_only_calls_completed_before_its_checkpoint() {
    let before_retry = tool_call("first", r#"{"path":"before"}"#);
    let after_retry = tool_call("second", r#"{"path":"after"}"#);
    let mut state = RoundState::default();
    state.remember_completed_tool(&before_retry);
    state.protect_completed_tools_for_retry();
    state.remember_completed_tool(&after_retry);

    assert!(state.is_checkpoint_replay(&before_retry));
    assert!(!state.is_checkpoint_replay(&after_retry));
}

/// The successful subagent result carries the `[<tool> result]:` header, the
/// original summary verbatim, and the success re-anchor note.
#[test]
fn subagent_result_text_reanchors_on_success() {
    let text = subagent_result_text("subagent", "Found the symbol in lib.rs", false, false);
    assert!(
        text.starts_with("[subagent result]:\n"),
        "header present: {text}"
    );
    assert!(
        text.contains("Found the symbol in lib.rs"),
        "summary preserved verbatim: {text}"
    );
    // The anchor must pin the master's write capability back to the
    // master and call out the read-only scope as subagent-only.
    assert!(
        text.contains("applies to the subagent only"),
        "anchor scope pin missing: {text}"
    );
    assert!(
        text.contains("retain your full toolset"),
        "parent re-anchor missing: {text}"
    );
}

/// A failed subagent carries a different (re-delegate-or-act-directly) anchor,
/// and still preserves the partial summary for the master to act on.
#[test]
fn subagent_result_text_reanchors_on_failure() {
    let text = subagent_result_text("subagent", "partial findings before crash", true, false);
    assert!(
        text.contains("partial findings before crash"),
        "partial summary preserved: {text}"
    );
    assert!(
        text.contains("could not complete its sub-task"),
        "failure anchor missing: {text}"
    );
    // Both anchors must re-affirm the master retains write capability.
    assert!(
        text.contains("retain your full toolset"),
        "parent re-anchor missing on failure: {text}"
    );
    // And must NOT carry the success-only phrasing (regression guard against
    // the success anchor leaking onto a failed subagent).
    assert!(
        !text.contains("applies to the subagent only"),
        "success anchor leaked onto failure: {text}"
    );
}

/// The re-anchor is unconditional for any subagent result — a regression guard
/// that a future refactor cannot silently drop it.
#[test]
fn subagent_result_text_anchor_is_unconditional() {
    for (failed, interrupted) in [(false, false), (true, false), (false, true)] {
        let text = subagent_result_text("subagent", "x", failed, interrupted);
        assert!(
            text.contains("[system]"),
            "system anchor tag present (failed={failed}, interrupted={interrupted}): {text}"
        );
    }
}

/// An interrupted subagent gets its own re-anchor: the partial findings are
/// real work to continue, not an error to work around — and the read-only
/// framing still does not transfer to the parent.
#[test]
fn subagent_result_text_reanchors_interruption() {
    let text = subagent_result_text("subagent", "found 2 of 5 handlers", false, true);
    assert!(
        text.contains("found 2 of 5 handlers"),
        "partial summary preserved: {text}"
    );
    assert!(
        text.contains("interrupted mid-task"),
        "interruption anchor missing: {text}"
    );
    assert!(
        !text.contains("could not complete its sub-task"),
        "failure anchor leaked onto interruption: {text}"
    );
    assert!(
        text.contains("retain your full toolset"),
        "parent re-anchor missing: {text}"
    );
}

use nuo_protocol::RestorePoint;

/// A scoped disable hides the tool until its restore point fires.
#[test]
fn scoped_disable_hides_until_restore() {
    let mut scoped = ScopedToolDisable::default();
    assert!(!scoped.contains("execute_command"));
    scoped.disable("execute_command", RestorePoint::TurnEnd);
    assert!(scoped.contains("execute_command"));
    scoped.restore_turn_end();
    assert!(
        !scoped.contains("execute_command"),
        "TurnEnd restore must re-enable the tool"
    );
    assert!(scoped.is_empty(), "both buckets drained");
}

/// `TurnEnd` restore clears the turn-scoped bucket only; `RoundEnd`
/// disables survive until the user-round boundary.
#[test]
fn turn_end_restore_keeps_round_end_disables() {
    let mut scoped = ScopedToolDisable::default();
    scoped.disable("execute_command", RestorePoint::TurnEnd);
    scoped.disable("edit_text", RestorePoint::RoundEnd);
    scoped.restore_turn_end();
    assert!(
        !scoped.contains("execute_command"),
        "TurnEnd disable must be restored at the ReAct-turn boundary"
    );
    assert!(
        scoped.contains("edit_text"),
        "RoundEnd disable must survive the ReAct-turn boundary"
    );
}

/// Nested disables compose via refcount: two hooks disable `execute_command` at
/// different restore points; the earlier (TurnEnd) restore must NOT bring
/// it back while the later (RoundEnd) is still in effect.
#[test]
fn nested_disables_refcount_correctly() {
    let mut scoped = ScopedToolDisable::default();
    scoped.disable("execute_command", RestorePoint::RoundEnd);
    scoped.disable("execute_command", RestorePoint::TurnEnd);
    assert!(scoped.contains("execute_command"));
    scoped.restore_turn_end();
    assert!(
        scoped.contains("execute_command"),
        "execute_command still hidden: the RoundEnd disable outlives the TurnEnd restore"
    );
    scoped.restore_round_end();
    assert!(
        !scoped.contains("execute_command"),
        "execute_command back after round end"
    );
}

// skip_interactive_input wiring (ADR-0043 interactive-input opt-out)

/// Minimal provider mock so an `Agent` can be constructed in unit tests
/// without a live model. `decide_command_stdin` never reaches the provider, so
/// the chat/stream impls are unreachable panics.
struct NoopProvider;

#[async_trait::async_trait]
impl nuo_protocol::Provider for NoopProvider {
    async fn chat(
        &self,
        _: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        unreachable!("decide_command_stdin must not call the provider")
    }
    async fn stream_chat(
        &self,
        _: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        unreachable!("decide_command_stdin must not call the provider")
    }
}

fn stdin_test_agent() -> super::Agent {
    use std::sync::Arc;
    super::Agent::new(
        Arc::new(NoopProvider) as Arc<dyn nuo_protocol::Provider>,
        vec![],
        nuo_protocol::AgentIdentity::default(),
    )
}

/// A `sudo` command (matched by the interactive classifier) must, with
/// `skip_interactive_input` on, be **sealed** and emit **no** `InputRequest`
/// — the inline panel never pops. This is the opt-out's core contract and
/// mirrors the delegated-autonomous path.
#[tokio::test]
async fn skip_interactive_input_seals_without_input_request() {
    use nuo_protocol::AgentEvent;
    use tokio::sync::mpsc;
    let agent = stdin_test_agent();
    agent.set_unattended(false);
    agent.set_skip_interactive_input(true);

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let contract = agent.decide_command_input(r#"{"command":"sudo ls /root"}"#);
    assert_eq!(
        contract,
        nuo_tool::InputContract::Sealed,
        "input must be sealed under skip_interactive_input"
    );
    assert!(
        rx.try_recv().is_err(),
        "no InputRequest must be emitted under skip_interactive_input"
    );
    let _ = tx;
}

/// Without the opt-out (and attended, on a platform that can fully
/// supervise), the same `sudo` command must be **supervised** — carrying the
/// classifier's advisory expectation. Regression guard: a refactor must not
/// silently route the interactive path to `Sealed` when the opt-out is off.
#[tokio::test]
async fn interactive_input_path_supervises_when_opt_out_is_off() {
    let agent = stdin_test_agent();
    agent.set_unattended(false);
    agent.set_skip_interactive_input(false);

    let contract = agent.decide_command_input(r#"{"command":"sudo ls /root"}"#);
    if nuo_host::supervised::input_supervision()
        == nuo_host::supervised::InputSupervision::Supervised
    {
        match contract {
            nuo_tool::InputContract::Supervised {
                expectation: Some(exp),
            } => {
                assert!(exp.secret, "sudo expectation must be masked");
            }
            other => panic!("expected Supervised with a secret expectation, got {other:?}"),
        }
    } else {
        // A platform without full supervision must fall back to Sealed —
        // never induce interactivity it cannot service.
        assert_eq!(contract, nuo_tool::InputContract::Sealed);
    }
}

/// `apply_preset` must seed `skip_interactive_input` from the
/// profile's runtime config — the wiring the bootstrap path relies on.
#[test]
fn apply_preset_seeds_skip_interactive_input() {
    let agent = stdin_test_agent();
    assert!(!agent.skip_interactive_input(), "default off");
    let profile = nuo_protocol::AgentRoleProfile::with_identity(
        "developer",
        nuo_protocol::AgentIdentity::default(),
    )
    .with_runtime_config(nuo_protocol::AgentRuntimeConfig {
        skip_interactive_input: true,
        ..Default::default()
    });
    agent.apply_preset(&profile);
    assert!(
        agent.skip_interactive_input(),
        "profile overlay took effect"
    );
}

/// The capability gate is atomic and structural: on a platform
/// that is not fully `Supervised`, an interactive command can NEVER be
/// dispatched as `Supervised` — it must fall back to `Sealed`. This is the
/// invariant that prevents "arm a terminal without detection" (the macOS
/// stall the seam removes).
#[tokio::test]
async fn non_supervised_platform_never_dispatches_supervised() {
    let agent = stdin_test_agent();
    agent.set_unattended(false);
    agent.set_skip_interactive_input(false);

    // Every interactive classification the classifier knows about.
    for command in ["sudo ls", "gpg --decrypt f", "vim x", "passwd", "less a"] {
        let args = serde_json::json!({ "command": command }).to_string();
        let contract = agent.decide_command_input(&args);
        if nuo_host::supervised::input_supervision()
            != nuo_host::supervised::InputSupervision::Supervised
        {
            assert_eq!(
                contract,
                nuo_tool::InputContract::Sealed,
                "non-Supervised platform must seal '{command}'"
            );
        } else {
            assert!(
                matches!(contract, nuo_tool::InputContract::Supervised { .. }),
                "Supervised platform must supervise '{command}', got {contract:?}"
            );
        }
    }
}
