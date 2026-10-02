//! The command-handler hook implementation and registry builder (ADR-0025).
//!
//! Each `[hooks]` entry becomes one [`CommandHook`] that spawns a shell
//! process: the [`HookContext`] is serialized to JSON on stdin, and the
//! process replies via exit code and stdout JSON. This is the only handler
//! type v1 ships; the [`Hook`] trait (in `nuo_contracts`) is shaped so `http`
//! and `mcp_tool` handlers can be added later without touching the loop.

use std::path::Path;
use std::time::Duration;

use nuo_harness::async_trait;
use nuo_harness::{Hook, HookContext, HookEvent, HookEventKind, HookOutcome};
use nuo_persistence::config::HookSpec;
use serde_json::json;

/// Default per-hook timeout. A hook that does not finish in this window is
/// killed and treated as `Pass` (a non-blocking error), so a hung script never
/// wedges the agent loop. Generous enough for a linter or CI shard.
const HOOK_TIMEOUT: Duration = Duration::from_secs(60);

/// A lifecycle hook that runs a shell command (ADR-0025). Built from a
/// [`HookSpec`]; the command runs with the project root as cwd and receives
/// the hook context as JSON on stdin.
#[derive(Debug)]
pub struct CommandHook {
    kind: HookEventKind,
    matcher: Option<String>,
    command: String,
    sandbox_root: Option<std::path::PathBuf>,
}

impl CommandHook {
    pub fn from_spec(spec: &HookSpec) -> Self {
        Self {
            kind: spec.event,
            matcher: spec.matcher.clone(),
            command: spec.command.clone(),
            sandbox_root: spec.sandbox_root.clone(),
        }
    }
}

#[async_trait]
impl Hook for CommandHook {
    fn kind(&self) -> HookEventKind {
        self.kind
    }

    fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    fn permission_submission(
        &self,
        ctx: &HookContext,
    ) -> Option<nuo_contracts::ToolPermissionSubmission> {
        let first_word = self.command.split_whitespace().next().unwrap_or("sh");
        Some(nuo_contracts::ToolPermissionSubmission {
            hazard_level: nuo_contracts::HazardLevel::CommandExecution,
            label: format!("Execute lifecycle hook: `{}`", self.command),
            description: "Runs a configured lifecycle hook command.".to_string(),
            scope: self.command.clone(),
            payload: nuo_contracts::ToolPermissionPayload::Command {
                command: self.command.clone(),
                cwd: ctx.cwd.as_ref().map(|path| path.display().to_string()),
                kill_spec: nuo_contracts::ProcessKillSpec {
                    command: first_word.to_string(),
                    process_group_killable: true,
                    pkill_target: format!("pkill -f '{first_word}'"),
                    cwd: ctx.cwd.as_ref().map(|path| path.display().to_string()),
                },
            },
        })
    }

    async fn fire(&self, ctx: &HookContext) -> HookOutcome {
        let stdin_json = context_to_json(ctx);
        let cwd = ctx.cwd.as_deref().unwrap_or_else(|| Path::new("."));

        let mut command = if let Some(root) = &self.sandbox_root {
            let snapshot =
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load().snapshot(root);
            if !snapshot.hooks.is_trusted() {
                tracing::warn!(command = %self.command, workspace = %root.display(), "project hook quarantined after hook-domain attestation changed");
                return HookOutcome::Pass;
            }
            // Project hooks are executable extensions. They may inspect the
            // exact workspace but cannot mutate it, reach the network, read
            // unrelated host files, or inherit ambient credentials.
            match nuo_host::workspace_sandbox::shell(
                &self.command,
                root,
                nuo_host::workspace_sandbox::WorkspaceAccess::ReadOnly,
                nuo_host::workspace_sandbox::NetworkAccess::Disabled,
            ) {
                Ok(command) => command,
                Err(error) => {
                    tracing::warn!(command = %self.command, %error, "project hook sandbox unavailable");
                    return HookOutcome::Pass;
                }
            }
        } else {
            nuo_host::shell::native_shell(&self.command)
        };
        command
            .current_dir(self.sandbox_root.as_deref().unwrap_or(cwd))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Global user hooks intentionally retain the ambient environment.
        // Project hooks were built with a cleared deterministic environment.
        let (child, process_tree) = match nuo_host::process::spawn_owned(&mut command) {
            Ok(owned) => owned,
            Err(error) => {
                tracing::warn!(command = %self.command, ?error, "hook spawn failed");
                return HookOutcome::Pass;
            }
        };

        let result = match write_stdin_and_collect(child, process_tree, stdin_json.as_bytes()).await
        {
            Ok(r) => r,
            Err(error) => {
                tracing::warn!(command = %self.command, ?error, "hook io failed");
                return HookOutcome::Pass;
            }
        };

        interpret_output(result)
    }
}

/// The captured result of running a hook command.
struct CommandResult {
    stdout: String,
    stderr: String,
    exit: Option<i32>,
}

/// Write `stdin_bytes` to the child's stdin, then await exit, collecting
/// stdout/stderr. Bounded by [`HOOK_TIMEOUT`].
async fn write_stdin_and_collect(
    mut child: tokio::process::Child,
    process_tree: nuo_host::process::OwnedProcessTree,
    stdin_bytes: &[u8],
) -> std::io::Result<CommandResult> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Detach the stdio handles up front so the wait below does not deadlock
    // waiting on a pipe the child holds open while it waits on our stdin.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes).await;
        let _ = stdin.flush().await;
        // Drop to signal EOF.
        drop(stdin);
    }

    // Read both streams to EOF off the wait path, so a child that writes a
    // large result then exits does not block on a full pipe.
    // Hook output cap: a hook is a small JSON-in/JSON-out protocol
    // participant, but nothing stopped a chatty one from buffering unbounded
    // output for its whole 60s budget. Interpreters look at the first bytes
    // (a JSON decision), so a head cap preserves the protocol and bounds the
    // memory. `take` stops reading at the cap, letting the pipe drain (the
    // child is never blocked on a full pipe).
    const HOOK_OUTPUT_CAP: usize = 1 << 20; // 1 MiB
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut stream) = stdout {
            let _ = tokio::io::AsyncReadExt::take(&mut stream, HOOK_OUTPUT_CAP as u64)
                .read_to_end(&mut buf)
                .await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut stream) = stderr {
            let _ = tokio::io::AsyncReadExt::take(&mut stream, HOOK_OUTPUT_CAP as u64)
                .read_to_end(&mut buf)
                .await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    match tokio::time::timeout(HOOK_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            let stdout = stdout_task.await.unwrap_or_default();
            let stderr = stderr_task.await.unwrap_or_default();
            Ok(CommandResult {
                stdout,
                stderr,
                exit: status.code(),
            })
        }
        Ok(Err(error)) => Err(error),
        Err(_) => {
            // Timed out; best-effort native tree kill. `child.wait()` borrows,
            // so the child is still ours to kill here. This also reaches hook
            // children the shell backgrounded; a lingering hook child must not
            // silently outlive its budget.
            let _ = process_tree.terminate();
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "hook timed out",
            ))
        }
    }
}

/// Translate a command's exit code + stdout into a [`HookOutcome`].
///
/// - exit `2`: blocking deny; the stderr (or a default) is the reason fed back
///   to the model. Honoured on `PreToolUse` / `Stop`.
/// - stdout parses as a JSON object: `{"decision":"deny","reason":"…""}`
///   denies; `{"context":"…"}` injects; `{"decision":"approve"}` passes.
/// - anything else (exit 0 with no JSON, non-2 exit, parse failure): `Pass`.
///   A non-blocking error never aborts the loop — enforce hard rules with the
///   permission system, not a flaky script.
fn interpret_output(result: CommandResult) -> HookOutcome {
    if result.exit == Some(2) {
        let reason = result.stderr.trim();
        let reason = if reason.is_empty() {
            "blocked by hook".to_string()
        } else {
            reason.to_string()
        };
        return HookOutcome::Deny { reason };
    }

    let trimmed = result.stdout.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        if !result.stderr.is_empty() && !matches!(result.exit, Some(0) | None) {
            tracing::info!(
                exit = ?result.exit,
                stderr = %result.stderr.trim(),
                "hook exited non-zero (non-blocking)"
            );
        }
        return HookOutcome::Pass;
    }

    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => {
            let decision = value.get("decision").and_then(|v| v.as_str());
            match decision {
                Some("deny") => {
                    let reason = value
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("blocked by hook")
                        .to_string();
                    HookOutcome::Deny { reason }
                }
                Some("approve") => HookOutcome::Pass,
                _ => {
                    if let Some(context) = value.get("context").and_then(|v| v.as_str()) {
                        HookOutcome::Inject {
                            context: context.to_string(),
                        }
                    } else {
                        HookOutcome::Pass
                    }
                }
            }
        }
        Err(_) => HookOutcome::Pass,
    }
}

/// Serialize a [`HookContext`] to a flat JSON object convenient for shell
/// scripts (one level, `jq`-friendly) rather than a nested enum.
fn context_to_json(ctx: &HookContext) -> String {
    let mut value = json!({
        "session_id": ctx.session_id,
        "event": event_name(&ctx.event),
    });
    if let Some(cwd) = &ctx.cwd {
        value["cwd"] = json!(cwd.display().to_string());
    }
    match &ctx.event {
        HookEvent::SessionStart { source } => {
            value["source"] = json!(match source {
                nuo_harness::SessionSource::Startup => "startup",
                nuo_harness::SessionSource::Resume => "resume",
            });
        }
        HookEvent::SessionEnd => {}
        HookEvent::UserPromptSubmit { prompt } => {
            value["prompt"] = json!(prompt);
        }
        HookEvent::PreToolUse {
            tool_name,
            tool_input,
        } => {
            value["tool_name"] = json!(tool_name);
            value["tool_input"] = tool_input.clone();
        }
        HookEvent::PostToolUse {
            tool_name,
            tool_output,
            duration_ms,
        } => {
            value["tool_name"] = json!(tool_name);
            value["tool_output"] = json!(tool_output);
            value["duration_ms"] = json!(duration_ms);
        }
        HookEvent::PostToolUseFailure { tool_name, error } => {
            value["tool_name"] = json!(tool_name);
            value["error"] = json!(error);
        }
        HookEvent::Stop { last_message } => {
            value["last_message"] = json!(last_message);
        }
        HookEvent::Turn {
            round,
            turn,
            consecutive_readonly,
        } => {
            value["round"] = json!(round);
            value["turn"] = json!(turn);
            value["consecutive_readonly"] = json!(consecutive_readonly);
        }
        HookEvent::TurnStart {
            round,
            turn,
            consecutive_readonly,
        } => {
            value["round"] = json!(round);
            value["turn"] = json!(turn);
            value["consecutive_readonly"] = json!(consecutive_readonly);
        }
        HookEvent::PermissionRequest { request } => {
            value["tool"] = json!(request.tool);
            value["label"] = json!(request.label);
            value["description"] = json!(request.description);
            value["scope"] = json!(request.scope);
            value["arguments"] = json!(request.arguments);
        }
        HookEvent::UserQuestion { request } => {
            // Render the question(s) as plain text so a notification script can
            // show them without parsing the nested options structure.
            let summary = request
                .questions
                .iter()
                .map(|q| {
                    let header = q
                        .header
                        .as_deref()
                        .map(|h| format!("{h}: "))
                        .unwrap_or_default();
                    format!("{header}{}", q.question)
                })
                .collect::<Vec<_>>()
                .join("\n");
            value["questions"] = json!(summary);
        }
        HookEvent::PreCompact | HookEvent::PostCompact => {}
    }
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

fn event_name(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart { .. } => "SessionStart",
        HookEvent::SessionEnd => "SessionEnd",
        HookEvent::UserPromptSubmit { .. } => "UserPromptSubmit",
        HookEvent::PreToolUse { .. } => "PreToolUse",
        HookEvent::PostToolUse { .. } => "PostToolUse",
        HookEvent::PostToolUseFailure { .. } => "PostToolUseFailure",
        HookEvent::Stop { .. } => "Stop",
        HookEvent::PreCompact => "PreCompact",
        HookEvent::PostCompact => "PostCompact",
        HookEvent::Turn { .. } => "Turn",
        HookEvent::TurnStart { .. } => "TurnStart",
        HookEvent::PermissionRequest { .. } => "PermissionRequest",
        HookEvent::UserQuestion { .. } => "UserQuestion",
    }
}

/// Build the hook registry from the `[hooks]` config. Unknown/invalid specs
/// are skipped with a warning rather than aborting startup.
pub fn build_hook_registry(
    specs: &[HookSpec],
    agent: &std::sync::Arc<nuo_harness::Agent>,
) -> nuo_harness::HookRegistry {
    let hooks: Vec<std::sync::Arc<dyn Hook>> = specs
        .iter()
        .map(|spec| {
            let hook: std::sync::Arc<dyn Hook> = std::sync::Arc::new(CommandHook::from_spec(spec));
            tracing::info!(
                event = ?spec.event,
                matcher = ?spec.matcher,
                command = %spec.command,
                "registered hook"
            );
            hook
        })
        .collect();
    let weak_agent = std::sync::Arc::downgrade(agent);
    let authorizer: nuo_harness::hooks::HookAuthorizer = std::sync::Arc::new(move |submission| {
        weak_agent
            .upgrade()
            .is_some_and(|agent| agent.is_permission_allowed("hook", &submission.scope))
    });
    nuo_harness::HookRegistry::with_authorizer(hooks, authorizer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(stdout: &str, stderr: &str, exit: Option<i32>) -> CommandResult {
        CommandResult {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit,
        }
    }

    #[test]
    fn exit_2_denies_with_stderr_reason() {
        assert_eq!(
            interpret_output(result("", "nope", Some(2))),
            HookOutcome::Deny {
                reason: "nope".into()
            }
        );
    }

    #[test]
    fn exit_0_no_json_passes() {
        assert_eq!(interpret_output(result("", "", Some(0))), HookOutcome::Pass);
    }

    #[test]
    fn json_deny_wins_over_exit_code() {
        assert_eq!(
            interpret_output(result(r#"{"decision":"deny","reason":"bad"}"#, "", Some(0))),
            HookOutcome::Deny {
                reason: "bad".into()
            }
        );
    }

    #[test]
    fn json_context_injects() {
        assert_eq!(
            interpret_output(result(r#"{"context":"remember X"}"#, "", Some(0))),
            HookOutcome::Inject {
                context: "remember X".into()
            }
        );
    }

    #[test]
    fn invalid_json_passes() {
        assert_eq!(
            interpret_output(result("{not json", "", Some(0))),
            HookOutcome::Pass
        );
    }

    #[test]
    fn turn_hook_json_carries_the_nested_round_and_turn_position() {
        let ctx = HookContext {
            session_id: "session-1".to_string(),
            cwd: None,
            event: HookEvent::TurnStart {
                round: 7,
                turn: 2,
                consecutive_readonly: 1,
            },
        };
        let value: serde_json::Value = serde_json::from_str(&context_to_json(&ctx)).unwrap();
        assert_eq!(value["event"], "TurnStart");
        assert_eq!(value["round"], 7);
        assert_eq!(value["turn"], 2);
        assert_eq!(value["consecutive_readonly"], 1);
    }
}
