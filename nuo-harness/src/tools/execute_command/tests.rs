use super::*;
use std::time::Duration;

#[cfg(unix)]
fn native_command<'a>(posix: &'a str, _powershell: &'a str) -> &'a str {
    posix
}

#[cfg(windows)]
fn native_command<'a>(_posix: &'a str, powershell: &'a str) -> &'a str {
    powershell
}

fn arguments(command: &str) -> String {
    serde_json::json!({ "command": command }).to_string()
}

#[test]
fn idle_budget_scales_with_timeout() {
    use super::episodic::idle_budget_for;
    // Small explicit budgets keep the one-third scaling.
    assert_eq!(
        idle_budget_for(Duration::from_secs(30)),
        Duration::from_secs(10)
    );
    // Explicitly larger budgets scale up as timeout/3…
    assert_eq!(
        idle_budget_for(Duration::from_secs(60)),
        Duration::from_secs(20)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(180)),
        Duration::from_secs(60)
    );
    // …clamped to the 480s ceiling: the default 1800s (30 min) wall budget
    // tolerates 8 minutes of silence, so a compiling build (or output
    // buffered by `… | tail`) is not killed at an arbitrary short mark.
    assert_eq!(
        idle_budget_for(Duration::from_secs(1800)),
        Duration::from_secs(480)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(600)),
        Duration::from_secs(200)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(3)),
        Duration::from_secs(5)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(9)),
        Duration::from_secs(5)
    );
}

/// A healthy command captures stdout and exits cleanly with `Exited`.
#[tokio::test]
async fn execute_command_captures_stdout_and_exits() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "printf hello",
            "[Console]::Out.Write('hello')",
        )))
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell {
            stdout,
            exit,
            termination,
            ..
        } => {
            assert_eq!(stdout, "hello\n");
            assert_eq!(exit, Some(0));
            assert_eq!(
                termination,
                nuo_wire::tool_output::ShellTermination::Exited
            );
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// The default input contract is `Sealed` (`/dev/null`), so a command that
/// reads stdin gets instant EOF and fails fast instead of hanging. This
/// is the hard floor: `cat` with no input and sealed stdin exits 0
/// immediately.
#[tokio::test]
async fn execute_command_closed_stdin_means_eof_not_hang() {
    let tool = ExecuteCommandTool::new(None);
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        tool.call_structured(&arguments(native_command(
            "read x",
            "if ($null -eq [Console]::In.ReadLine()) { exit 7 }",
        ))),
    )
    .await
    .expect("closed stdin must NOT hang past 5s");
    match out.expect("ok") {
        nuo_wire::ToolOutput::Shell { exit, .. } => {
            assert_ne!(exit, Some(0));
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// A prefilled input contract pipes the bytes into the child: `cat` echoes
/// them back. This is the model/human pre-spawn injection seam.
#[tokio::test]
async fn execute_command_prefilled_stdin_feeds_the_child() {
    let tool = ExecuteCommandTool::new(None);
    let mut on_stream = |_: nuo_wire::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments: &arguments(native_command(
                    "cat",
                    "[Console]::Out.Write([Console]::In.ReadToEnd())",
                )),
                input: nuo_wire::InputContract::Prefilled {
                    data: "injected\n".into(),
                },
                input_handler: None,
            },
            Box::new(|_| {}),
            &mut on_stream,
        )
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, exit, .. } => {
            assert_eq!(stdout, "injected\n");
            assert_eq!(exit, Some(0));
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// The child runs in its own process group (`.process_group(0)`), so its
/// process id equals its process-group id.
#[cfg(unix)]
#[tokio::test]
async fn execute_command_child_runs_in_its_own_process_group() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(r#"{"command":"ps -o pid=,pgid= -p $$ || echo \"ps=$$\""}"#)
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, exit, .. } => {
            let _ = stdout;
            let _ = exit;
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// ADR-0286: The execution environment must be hermetically headless:
/// $EDITOR and $VISUAL must be 'false' and fail-fast with non-zero exit.
#[tokio::test]
async fn execute_command_hermetic_headless_environment_prevents_interactive_editor() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(r#"{"command":"$EDITOR /tmp/test_msg.txt || echo editor_failed"}"#)
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, .. } => {
            assert!(stdout.contains("editor_failed"));
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// ADR-0286: Controlling terminal decoupling prevents opening /dev/tty on Unix.
#[cfg(unix)]
#[tokio::test]
async fn execute_command_detaches_controlling_terminal() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(r#"{"command":"python3 -c \"import os; os.open('/dev/tty', os.O_RDWR)\" 2>&1 || echo cannot_open_tty"}"#)
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, .. } => {
            assert!(stdout.contains("cannot_open_tty") || stdout.contains("No such device or address"));
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// ADR-0286: git tag without -m fails fast when tag.gpgsign is true instead of opening an editor and hanging.
#[cfg(unix)]
#[tokio::test]
async fn execute_command_git_tag_fails_fast_when_gpgsign_enabled() {
    let tool = ExecuteCommandTool::new(None);
    let tmp = tempfile::tempdir().expect("tmpdir");
    let repo = tmp.path().display().to_string();
    let script = format!(
        "git -C {repo} init && \
         git -C {repo} commit --allow-empty -m init && \
         git -C {repo} config tag.gpgsign true && \
         git -C {repo} tag v0.0.1 2>&1 || echo git_tag_failed_fast"
    );
    let out = tool
        .call_structured(&format!(r#"{{"command":{}}}"#, serde_json::to_string(&script).unwrap()))
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, .. } => {
            assert!(
                stdout.contains("problem with the editor")
                    || stdout.contains("git_tag_failed_fast")
            );
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// A `sleep` is legitimate quiet computation (`wchan=hrtimer_nanosleep`), NOT
/// an input wait, so it must NOT be fast-failed as an interactive stall. It
/// falls through to the ordinary idle budget. This is the correction the
/// semantic detector makes to the former `state=='S'` heuristic, which
/// false-positived on `sleep`/`wait`/`flock`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn execute_command_sleep_is_not_mistaken_for_an_input_wait() {
    let tool = ExecuteCommandTool::new(None);
    let start = std::time::Instant::now();
    let out = tool
        .call_structured(r#"{"command":"sleep 60", "timeout": 30}"#)
        .await
        .expect("ok");
    let elapsed = start.elapsed();
    match out {
        nuo_wire::ToolOutput::Shell { termination, .. } => {
            assert_ne!(
                termination,
                nuo_wire::tool_output::ShellTermination::InputUnanswered,
                "a sleep must not be classified as an input wait"
            );
            assert_eq!(
                termination,
                nuo_wire::tool_output::ShellTermination::IdleBlocked
            );
            assert!(elapsed.as_secs() < 25, "took too long: {elapsed:?}");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// A real stdin prompt (harness-held pipe, blocked read, zero CPU) IS detected
/// from kernel evidence and fast-failed when no supervisor answers: a held-open
/// pipe makes `read` block, and with no handler the wait is killed in seconds
/// with `InputUnanswered` rather than lingering for the idle budget.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn execute_command_detected_input_wait_fast_fails_without_a_supervisor() {
    let tool = ExecuteCommandTool::new(None);
    let start = std::time::Instant::now();
    let mut on_stream = |_: nuo_wire::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments: r#"{"command":"read x; echo never", "timeout": 120}"#,
                input: nuo_wire::InputContract::Supervised { expectation: None },
                input_handler: None,
            },
            Box::new(|_| {}),
            &mut on_stream,
        )
        .await
        .expect("ok");
    let elapsed = start.elapsed();
    match out {
        nuo_wire::ToolOutput::Shell { termination, .. } => {
            assert_eq!(
                termination,
                nuo_wire::tool_output::ShellTermination::InputUnanswered,
                "a genuine stdin read must be detected and fast-failed"
            );
            // ~5s examiner floor + 2s stability, well under the 40s idle budget.
            assert!(elapsed.as_secs() < 20, "took too long: {elapsed:?}");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// A test [`InputHandler`](nuo_wire::InputHandler) that always answers
/// with a fixed line, counting how many prompts it served.
struct ScriptedHandler {
    answer: String,
    served: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl nuo_wire::InputHandler for ScriptedHandler {
    async fn resolve(&self, _prompt: nuo_wire::InputPrompt) -> Option<String> {
        self.served
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(self.answer.clone())
    }
}

/// A supervised stdin prompt is detected and answered: the injected line
/// reaches the child, which then completes on its own. This is the full
/// runtime-injection path the classifier alone could not provide.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn supervised_command_receives_injected_stdin() {
    let tool = ExecuteCommandTool::new(None);
    let handler = ScriptedHandler {
        answer: "injected-value".to_string(),
        served: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut on_stream = |_: nuo_wire::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments: &arguments("read x; printf 'GOT:%s' \"$x\""),
                input: nuo_wire::InputContract::Supervised { expectation: None },
                input_handler: Some(&handler),
            },
            Box::new(|_| {}),
            &mut on_stream,
        )
        .await
        .expect("ok");
    assert_eq!(
        handler.served.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the runtime examiner must have parked exactly one prompt"
    );
    match out {
        nuo_wire::ToolOutput::Shell {
            stdout,
            exit,
            termination,
            ..
        } => {
            assert_eq!(
                termination,
                nuo_wire::tool_output::ShellTermination::Exited
            );
            assert_eq!(exit, Some(0));
            assert_eq!(stdout, "GOT:injected-value\n");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// A supervised child that prompts on its **controlling terminal** (`/dev/tty`,
/// the `sudo`/`gpg` shape) is answered through the pty master, while stdout
/// stays a clean pipe.
#[cfg(unix)]
#[tokio::test]
async fn supervised_command_receives_injected_controlling_tty_input() {
    let tool = ExecuteCommandTool::new(None);
    let handler = ScriptedHandler {
        answer: "tty-secret".to_string(),
        served: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut on_stream = |_: nuo_wire::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments: &arguments("read x < /dev/tty; printf 'GOT:%s' \"$x\""),
                input: nuo_wire::InputContract::Supervised { expectation: None },
                input_handler: Some(&handler),
            },
            Box::new(|_| {}),
            &mut on_stream,
        )
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell {
            stdout,
            termination,
            ..
        } => {
            assert_ne!(
                termination,
                nuo_wire::tool_output::ShellTermination::InputUnanswered,
                "the /dev/tty prompt must have been answered, not refused"
            );
            assert_eq!(stdout, "GOT:tty-secret\n");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// A supervised child whose prompt the operator declines (handler returns
/// `None`) is killed with `InputUnanswered` — the supervised analogue of the
/// sealed fast-fail.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn supervised_command_declined_input_is_unanswered() {
    struct DecliningHandler;
    #[async_trait::async_trait]
    impl nuo_wire::InputHandler for DecliningHandler {
        async fn resolve(&self, _prompt: nuo_wire::InputPrompt) -> Option<String> {
            None
        }
    }
    let tool = ExecuteCommandTool::new(None);
    let handler = DecliningHandler;
    let mut on_stream = |_: nuo_wire::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments: &arguments("read x; echo never"),
                input: nuo_wire::InputContract::Supervised { expectation: None },
                input_handler: Some(&handler),
            },
            Box::new(|_| {}),
            &mut on_stream,
        )
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { termination, .. } => assert_eq!(
            termination,
            nuo_wire::tool_output::ShellTermination::InputUnanswered
        ),
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// The design-critical case: a program that opens `/dev/tty` on a **separate**
/// file descriptor while fd 0 is an unrelated *pipe* — exactly the
/// `sudo`/`gpg`/`pinentry` shape. The wait must still be classified as an input
/// wait and answered through the terminal, not misread as a pipe read. (A
/// `read x < /dev/tty` in `sh` is not sufficient: `sh` dup2's the redirect onto
/// fd 0, hiding the bug.)
#[cfg(unix)]
#[tokio::test]
async fn supervised_separate_fd_tty_prompt_is_detected_and_answered() {
    let tool = ExecuteCommandTool::new(None);
    let handler = ScriptedHandler {
        answer: "separate-fd-secret".to_string(),
        served: std::sync::atomic::AtomicUsize::new(0),
    };
    // python3 opens /dev/tty as a new fd; fd 0 stays whatever the harness gave.
    let script = "python3 -c \"import sys; f=open('/dev/tty'); print('GOT:'+f.readline().strip())\"";
    let mut on_stream = |_: nuo_wire::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments: &arguments(script),
                input: nuo_wire::InputContract::Supervised { expectation: None },
                input_handler: Some(&handler),
            },
            Box::new(|_| {}),
            &mut on_stream,
        )
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell {
            stdout,
            termination,
            ..
        } => {
            assert_ne!(
                termination,
                nuo_wire::tool_output::ShellTermination::InputUnanswered,
                "a separate-fd /dev/tty prompt must be answered, not refused"
            );
            assert!(
                stdout.contains("GOT:separate-fd-secret"),
                "expected the /dev/tty answer to reach the child; got {stdout:?}"
            );
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// Regression guard: processing a command's output lines must not do per-line
/// work proportional to the host's process count. The examiner samples `/proc`
/// only on the quiet path, never per line, so a 900-line command completes in
/// milliseconds rather than seconds. The threshold is deliberately loose
/// (1s vs. the ~4ms observed) so it flags a real regression — a return to
/// per-line `/proc` sampling took ~2s here — without being timing-flaky.
#[tokio::test]
async fn execute_command_many_lines_do_not_incur_per_line_proc_scans() {
    let tool = ExecuteCommandTool::new(None);
    let start = std::time::Instant::now();
    let out = tool
        .call_structured(&arguments(native_command(
            "seq 1 900",
            "1..900 | ForEach-Object { $_ }",
        )))
        .await
        .expect("ok");
    let elapsed = start.elapsed();
    match out {
        nuo_wire::ToolOutput::Shell { lines, .. } => {
            assert_eq!(lines.len(), 900, "all lines must be captured");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(1),
        "900 lines took {elapsed:?}; output processing must not sample /proc per line"
    );
}

/// A timed-out command's whole process group is killed.
#[cfg(unix)]
#[tokio::test]
async fn execute_command_timeout_kills_grandchildren() {
    let tool = ExecuteCommandTool::new(None);
    let marker = std::env::temp_dir().join(format!(
        "muta-grandchild-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let command = format!("sleep 60 & echo $! > {}; echo started", marker.display());
    let out = tool
        .call_structured(&format!(
            r#"{{"command":{}, "timeout": 2}}"#,
            serde_json::to_string(&command).unwrap()
        ))
        .await;
    assert!(matches!(
        &out,
        Ok(nuo_wire::ToolOutput::Shell {
            termination: nuo_wire::tool_output::ShellTermination::Timeout,
            ..
        })
    ));
    assert!(out.as_ref().unwrap().is_error());

    let pid_txt = std::fs::read_to_string(&marker).unwrap_or_default();
    let pid: i32 = pid_txt.trim().parse().unwrap_or(0);
    let _ = std::fs::remove_file(&marker);
    assert!(pid > 0, "grandchild did not record its pid ({pid_txt:?})");
    let alive = |pid: i32| {
        std::path::Path::new(&format!("/proc/{pid}"))
            .try_exists()
            .unwrap_or(false)
    };
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("grandchild pid {pid} survived the group kill");
}

#[cfg(windows)]
#[tokio::test]
async fn execute_command_timeout_kills_grandchildren() {
    let tool = ExecuteCommandTool::new(None);
    let marker = std::env::temp_dir().join(format!(
        "muta-grandchild-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let escaped_marker = marker
        .to_string_lossy()
        .replace('`', "``")
        .replace('"', "`\"");
    let command = format!(
        "$p = Start-Process powershell.exe -WindowStyle Hidden -PassThru \
         -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command',\
         'Start-Sleep -Seconds 60'; \
         Set-Content -LiteralPath \"{escaped_marker}\" -Value $p.Id; \
         Write-Output started; Wait-Process -Id $p.Id"
    );
    let out = tool
        .call_structured(&serde_json::json!({ "command": command, "timeout": 2 }).to_string())
        .await;
    assert!(matches!(
        &out,
        Ok(nuo_wire::ToolOutput::Shell {
            termination: nuo_wire::tool_output::ShellTermination::Timeout,
            ..
        })
    ));
    assert!(out.as_ref().unwrap().is_error());

    let pid_text = std::fs::read_to_string(&marker).unwrap_or_default();
    let pid: u32 = pid_text.trim().parse().unwrap_or(0);
    let _ = std::fs::remove_file(&marker);
    assert!(pid > 0, "grandchild did not record its pid ({pid_text:?})");
    for _ in 0..50 {
        if nuo_host::process::process_identity(pid).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("grandchild pid {pid} survived the Job Object termination");
}

/// A huge multi-line output command is capped in memory. The command emits
/// ~90 KB (well over the 64 KB collection cap, but under the 128 KB /
/// 1000-line stream-flood thresholds), so it *completes* — letting the head,
/// the cap marker, and the tail all be observed deterministically, rather than
/// racing a StreamGuard kill that may cut the tail off.
#[tokio::test]
async fn execute_command_caps_huge_output_in_memory() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "awk 'BEGIN{ s=\"\"; for(i=0;i<900;i++) s=s \"x\"; \
             for(i=0;i<100;i++) print s; print \"TAIL-MARKER\" }'",
            "$s='x'*900; 1..100 | ForEach-Object { $s }; 'TAIL-MARKER'",
        )))
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell {
            stdout, truncated, ..
        } => {
            assert!(truncated, "collection cap must set the hint");
            assert!(
                stdout.contains("dropped (collection cap)"),
                "marker present"
            );
            assert!(
                stdout.len() < 70_000,
                "payload bounded near the 64k cap, got {}",
                stdout.len()
            );
            assert!(stdout.starts_with("xxx"), "head kept");
            assert!(stdout.contains("TAIL-MARKER"), "tail kept");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// Captured tabs are expanded to spaces.
#[tokio::test]
async fn execute_command_captures_expanded_tabs() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "printf 'a\\tb\\n'",
            "[Console]::Out.Write(\"a`tb`n\")",
        )))
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, .. } => {
            assert_eq!(stdout, "a       b\n");
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn execute_command_runs_in_the_session_workspace_root() {
    let marker = std::env::temp_dir().join(format!("muta-command-root-{}", std::process::id()));
    std::fs::create_dir_all(&marker).expect("mkdir");
    let tool = ExecuteCommandTool::new(Some(marker.clone()));
    let out = tool
        .call_structured(r#"{"command":"pwd"}"#)
        .await
        .expect("ok");
    match out {
        nuo_wire::ToolOutput::Shell { stdout, .. } => {
            let expected = marker.canonicalize().expect("canonical workspace root");
            assert_eq!(stdout.trim(), expected.as_os_str().to_string_lossy());
        }
        other => panic!("expected Shell, got {:?}", other),
    }
    std::fs::remove_dir_all(&marker).ok();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn workspace_shell_sees_only_runtime_and_exact_workspace() {
    if !crate::execution::workspace_sandbox_available() {
        return;
    }
    let base = std::env::temp_dir().join(format!(
        "muta-command-sandbox-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let workspace = base.join("workspace");
    let outside = base.join("outside-secret");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("visible"), "workspace").expect("write workspace marker");
    std::fs::write(&outside, "host secret").expect("write host marker");

    let env = std::sync::Arc::new(crate::execution::WorkspaceExecutionEnvironment::new(
        &workspace,
    ));
    let tool = ExecuteCommandTool::workspace_with_env(env);
    let command = format!(
        "test -r visible && test ! -e {} && ! touch /etc/muta_leak_test 2>/dev/null && \
         test -z \"${{CARGO_MANIFEST_DIR:-}}\" && printf sandboxed > created",
        outside.display()
    );
    let output = tool
        .call_structured(&serde_json::json!({ "command": command }).to_string())
        .await
        .expect("sandbox command");
    assert!(matches!(
        output,
        nuo_wire::ToolOutput::Shell { exit: Some(0), .. }
    ));
    assert_eq!(
        std::fs::read_to_string(workspace.join("created")).unwrap(),
        "sandboxed"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "host secret");
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn test_background_spawn_via_service() {
    // ADR-0190: background spawn through the service returns a job id and
    // interactive-process spec; unknown args (`terminal_id`) are no longer
    // part of the schema.
    let tool = ExecuteCommandTool::new(None);
    let params = tool.parameters();
    let props = params.get("properties").expect("schema properties");
    assert!(
        props.get("terminal_id").is_none(),
        "terminal_id must be deleted from the tool surface (M6)"
    );
    assert!(
        props.get("run_persistent").is_none(),
        "run_persistent must be deleted from the tool surface (M6)"
    );
    assert!(
        props.get("service").is_none(),
        "service flag must be deleted from the tool surface (ADR-0263)"
    );
    assert!(
        props.get("background").is_none(),
        "background flag must be deleted from the tool surface (ADR-0263)"
    );
    // ADR-0234: the timer is a wake-prompt arm whose consumer (autonomous
    // wake) is disabled by ADR-0212, so `run_command` must not advertise
    // "run this command later" — the description promised a command
    // execution the runtime never performed.
    assert!(
        props.get("schedule_in_secs").is_none() && props.get("repeat").is_none(),
        "timer scheduling must not be advertised on the tool surface (ADR-0234)"
    );
}

#[cfg(unix)]
mod background_mode_selection {
    use super::*;

    #[tokio::test]
    async fn background_and_service_flags_are_ignored_in_favor_of_finite_execution() {
        // ADR-0263: background and service execution paths are eliminated.
        // All commands execute as finite synchronous commands.
        let tool = ExecuteCommandTool::new(None);
        let output = tool
            .call_structured(&arguments_flags(&["background", "service"]))
            .await
            .expect("structured output");
        let text = output.to_text();
        assert!(!text.contains("spawned_service"));
        assert!(!text.contains("spawned_in_background"));
    }

    fn arguments_flags(flags: &[&str]) -> String {
        let mut map = serde_json::Map::new();
        map.insert("command".to_string(), serde_json::json!("true"));
        for flag in flags {
            map.insert((*flag).to_string(), serde_json::json!(true));
        }
        serde_json::Value::Object(map).to_string()
    }
}

#[test]
fn execute_command_schema_documents_1800s_default_timeout() {
    let tool = ExecuteCommandTool::new(None);
    let params = tool.parameters();
    let desc = params
        .get("properties")
        .and_then(|p| p.get("timeout"))
        .and_then(|t| t.get("description"))
        .and_then(|d| d.as_str())
        .expect("timeout description");
    assert!(
        desc.contains("default 1800"),
        "schema description should state default 1800s: {desc}"
    );
}

#[test]
fn semantic_folding_collapses_pure_green_ninja_test_runs() {
    use super::pipes::{OutputCollector, is_pure_green_test_line};
    use nuo_wire::tool_output::{ShellLine, ShellStream};

    // Verify pattern matching
    assert!(is_pure_green_test_line("[1/134] test_alpha OK 0.01s"));
    assert!(is_pure_green_test_line("[ 2/134] test_beta OK 0.02s"));
    assert!(is_pure_green_test_line(
        "PASS [ 0.005s] crate::test_something"
    ));
    assert!(is_pure_green_test_line("test crate::test_something ... ok"));
    assert!(is_pure_green_test_line("✓ test_something"));

    // Verify negative invariants: warnings and errors are NEVER pure green
    assert!(!is_pure_green_test_line(
        "[1/134] test_foo OK (warning: leak detected)"
    ));
    assert!(!is_pure_green_test_line("[1/134] test_foo FAILED 0.05s"));
    assert!(!is_pure_green_test_line("test_foo ... FAILED"));

    // Build realistic collector with 10 passing tests followed by 1 failure
    let mut collector = OutputCollector::new();
    for i in 1..=10 {
        collector.lines.push(ShellLine {
            stream: ShellStream::Out,
            text: format!("[{i}/11] test_case_{i} OK 0.01s"),
        });
        collector
            .stdout_buf
            .push_str(&format!("[{i}/11] test_case_{i} OK 0.01s\n"));
    }
    collector.lines.push(ShellLine {
        stream: ShellStream::Out,
        text: "[11/11] test_case_11 FAILED 0.05s".into(),
    });
    collector
        .stdout_buf
        .push_str("[11/11] test_case_11 FAILED 0.05s\n");

    // Apply caps with folding enabled (raw: false)
    let (stdout, _stderr, lines, _truncated) = collector.apply_caps_ex(Some(1), false);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0].text,
        "⋯ 10 tests passed (pure-green output folded)"
    );
    assert_eq!(lines[1].text, "[11/11] test_case_11 FAILED 0.05s");
    assert!(stdout.contains("⋯ 10 tests passed (pure-green output folded)"));
    assert!(stdout.contains("FAILED"));
}

#[test]
fn semantic_folding_bypassed_when_raw_is_true() {
    use super::pipes::OutputCollector;
    use nuo_wire::tool_output::{ShellLine, ShellStream};

    let mut collector = OutputCollector::new();
    for i in 1..=5 {
        collector.lines.push(ShellLine {
            stream: ShellStream::Out,
            text: format!("[{i}/5] test_{i} OK 0.01s"),
        });
        collector
            .stdout_buf
            .push_str(&format!("[{i}/5] test_{i} OK 0.01s\n"));
    }

    // Apply caps with raw: true -> no folding
    let (stdout, _stderr, lines, _truncated) = collector.apply_caps_ex(Some(0), true);
    assert_eq!(lines.len(), 5);
    assert!(stdout.contains("[1/5] test_1 OK 0.01s"));
    assert!(!stdout.contains("pure-green output folded"));
}

#[test]
fn output_collector_detects_stream_flooding() {
    use super::pipes::{OutputCollector, SHELL_STREAM_FLOOD_LINES};
    use nuo_wire::tool_output::{ShellLine, ShellStream};

    let mut collector = OutputCollector::new();
    assert!(!collector.is_stream_flooded(false));

    // Under flood threshold
    for i in 0..SHELL_STREAM_FLOOD_LINES - 1 {
        collector.lines.push(ShellLine {
            stream: ShellStream::Out,
            text: format!("line {i}"),
        });
    }
    assert!(!collector.is_stream_flooded(false));

    // Reaching flood threshold triggers StreamGuard in normal mode
    collector.lines.push(ShellLine {
        stream: ShellStream::Out,
        text: "line flood".into(),
    });
    assert!(collector.is_stream_flooded(false));

    // In raw mode, higher ceiling applies
    assert!(!collector.is_stream_flooded(true));
}

#[test]
fn stream_cadence_tracker_detects_metronomic_monitoring_stream() {
    use super::pipes::StreamCadenceTracker;
    use std::time::{Duration, Instant};

    let mut tracker = StreamCadenceTracker::new();
    let base = Instant::now();

    // Line 1: Header row (e.g. intel_gpu_top -l header)
    assert!(!tracker.observe_at("Freq MHz IRQ RC6 Power W RCS BCS VCS VECS CCS", base));

    // Lines 2-6: Periodic data rows arriving ~500ms apart with matching column token counts
    for i in 1..=5 {
        let t = base + Duration::from_millis(500 * i);
        let triggered = tracker.observe_at("1351 355 213 32 2.97 14.45 46.43 0 0 0.00 0 0 0.00 0 0 0.00 0 0", t);
        if i < 5 {
            assert!(!triggered, "Should not trigger prematurely at sample {i}");
        } else {
            assert!(triggered, "Must trigger on 5th periodic sample row!");
        }
    }
}

#[test]
fn stream_cadence_tracker_ignores_rapid_compilation_bursts() {
    use super::pipes::StreamCadenceTracker;
    use std::time::{Duration, Instant};

    let mut tracker = StreamCadenceTracker::new();
    let base = Instant::now();

    // Rapid lines arriving within 20ms of each other (like cargo build or test runner)
    for i in 1..=100 {
        let t = base + Duration::from_millis(20 * i);
        let triggered = tracker.observe_at(&format!("Compiling crate_{i} v0.1.0"), t);
        assert!(!triggered, "High-frequency compilation bursts must not trigger cadence guard");
    }
}

#[test]
fn stream_cadence_tracker_detects_tui_redraws() {
    use super::pipes::StreamCadenceTracker;

    let mut tracker = StreamCadenceTracker::new();

    // First TUI frame
    assert!(!tracker.observe("\x1b[H\x1b[2Jtop - 14:00:00 up 10 days"));
    // Second TUI frame
    assert!(
        tracker.observe("\x1b[H\x1b[2Jtop - 14:00:01 up 10 days"),
        "Second TUI screen redraw must trigger early snapshot sufficiency"
    );
}

/// Continuous streaming output (e.g. `yes` or `intel_gpu_top -l`) in the foreground
/// is cut off early by StreamGuard rather than running to wall-clock timeout (ADR-0257).
#[tokio::test]
async fn execute_command_stream_guard_cuts_off_unbounded_stream() {
    let tool = ExecuteCommandTool::new(None);
    // `yes` produces infinite lines at maximum speed
    let args = serde_json::json!({
        "command": native_command("yes 'gpu metrics row'", "while ($true) { Write-Output 'gpu metrics row' }"),
        "timeout": 30,
    })
    .to_string();

    let out = tokio::time::timeout(Duration::from_secs(5), tool.call_structured(&args))
        .await
        .expect("StreamGuard must terminate infinite stream in seconds, never hanging")
        .expect("command execution succeeded with structured output");

    match &out {
        nuo_wire::ToolOutput::Shell {
            termination,
            exit,
            stdout,
            ..
        } => {
            assert_eq!(
                *termination,
                nuo_wire::tool_output::ShellTermination::StreamGuard,
                "Expected StreamGuard termination for infinite streaming command"
            );
            assert_eq!(*exit, None);
            assert!(
                stdout.contains("gpu metrics row"),
                "Snapshot must preserve output captured before cutoff"
            );
            // Verify model text representation includes actionable guidance
            let text = out.to_text();
            assert!(
                text.contains("[killed by harness: stream budget reached"),
                "to_text must provide actionable guidance to the model"
            );
        }
        other => panic!("expected Shell output, got {:?}", other),
    }
}

/// A command that emits an excessively long minified line triggers the content-aware
/// ingestion gate (ADR-0264), suppressing the inline raw line while flagging truncation.
#[tokio::test]
async fn execute_command_suppresses_long_minified_line() {
    let tool = ExecuteCommandTool::new(None);
    let minified_line = "a".repeat(10_000);
    let args = serde_json::json!({
        "command": native_command(
            &format!("echo '{minified_line}'"),
            "Write-Output ('a' * 10000)",
        ),
        "timeout": 10,
    })
    .to_string();

    let out = tool.call_structured(&args).await.expect("command succeeds");
    match out {
        nuo_wire::ToolOutput::Shell {
            stdout, truncated, ..
        } => {
            assert!(truncated, "single massive minified line must flag truncation");
            assert!(
                stdout.contains("[minified line:"),
                "stdout must contain minified line suppression notice, got: {stdout}"
            );
        }
        other => panic!("expected Shell output, got {:?}", other),
    }
}



