use super::*;
use nuo_tool::ToolStreamSink;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

struct TestSink {
    count: AtomicUsize,
}

impl ToolStreamSink for TestSink {
    fn emit(&self, _stream: ToolStream) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn test_exec_tool() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    let res = tool
        .execute(&t_ctx, json!({"command": "echo test"}))
        .await
        .unwrap();
    assert!(res.content().contains("test"));
    assert!(tool.matches_name("execute_command"));
    assert!(!tool.matches_name("run_command"));
}

#[tokio::test]
async fn test_exec_tool_wall_clock_timeout() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    #[cfg(not(windows))]
    let cmd = "sleep 10";
    #[cfg(windows)]
    let cmd = "powershell -Command \"Start-Sleep -Seconds 10\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "timeout": 1}))
        .await
        .unwrap();

    if let ToolOutput::Shell {
        exit, termination, ..
    } = res
    {
        assert_eq!(exit, None);
        assert_eq!(termination, ShellTermination::Timeout);
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_idle_timeout() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    #[cfg(not(windows))]
    let cmd = "sleep 10";
    #[cfg(windows)]
    let cmd = "powershell -Command \"Start-Sleep -Seconds 10\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "idle_timeout": 1}))
        .await
        .unwrap();

    if let ToolOutput::Shell {
        exit, termination, ..
    } = res
    {
        assert_eq!(exit, None);
        assert_eq!(termination, ShellTermination::IdleBlocked);
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_idle_timeout_resets_on_output() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // 3 outputs separated by 200ms pauses: total duration ~600ms, idle timeout 1s.
    // Idle timeout should reset on each output chunk and not fire.
    #[cfg(not(windows))]
    let cmd = "echo one; sleep 0.2; echo two; sleep 0.2; echo three";
    #[cfg(windows)]
    let cmd = "powershell -Command \"Write-Output 'one'; Start-Sleep -Milliseconds 200; Write-Output 'two'; Start-Sleep -Milliseconds 200; Write-Output 'three'\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "idle_timeout": 1}))
        .await
        .unwrap();

    if let ToolOutput::Shell {
        exit, termination, ..
    } = res
    {
        assert_eq!(exit, Some(0));
        assert_eq!(termination, ShellTermination::Exited);
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_idle_timeout_on_closed_pipes_hang() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // Process closes stdout and stderr immediately, then sleeps.
    // It must still be caught by the idle timeout rather than hanging.
    #[cfg(not(windows))]
    let cmd = "exec 1>&- 2>&-; sleep 10";
    #[cfg(windows)]
    let cmd = "cmd /C \"powershell -Command Start-Sleep -Seconds 10 > $null 2>&1\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "idle_timeout": 1}))
        .await
        .unwrap();

    if let ToolOutput::Shell {
        exit, termination, ..
    } = res
    {
        assert_eq!(exit, None);
        assert_eq!(termination, ShellTermination::IdleBlocked);
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_cancellation() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    let cancel_token = t_ctx.cancel_token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_token.cancel();
    });

    #[cfg(not(windows))]
    let cmd = "sleep 10";
    #[cfg(windows)]
    let cmd = "powershell -Command \"Start-Sleep -Seconds 10\"";

    let res = tool.execute(&t_ctx, json!({"command": cmd})).await.unwrap();

    if let ToolOutput::Shell {
        exit, termination, ..
    } = res
    {
        assert_eq!(exit, None);
        assert_eq!(termination, ShellTermination::Cancelled);
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_stream_sink() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);

    let sink = Arc::new(TestSink {
        count: AtomicUsize::new(0),
    });
    let t_ctx = ToolContext::default().with_stream_sink(sink.clone());

    let res = tool
        .execute(&t_ctx, json!({"command": "echo line1; echo line2"}))
        .await
        .unwrap();
    assert!(res.content().contains("line1"));
    assert!(sink.count.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn test_exec_tool_stdin_sealed() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // `cat` without arguments reads from stdin; because stdin is Sealed (/dev/null),
    // it must read EOF and exit immediately with code 0 rather than hanging.
    #[cfg(not(windows))]
    let cmd = "cat";
    #[cfg(windows)]
    let cmd = "cmd /C exit 0";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "timeout": 2}))
        .await
        .unwrap();

    if let ToolOutput::Shell {
        exit, termination, ..
    } = res
    {
        assert_eq!(exit, Some(0));
        assert_eq!(termination, ShellTermination::Exited);
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_prefilled_stdin() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    #[cfg(not(windows))]
    let cmd = "cat";
    #[cfg(windows)]
    let cmd = "powershell -Command \"$input\"";

    let input_contract = InputContract::Prefilled {
        data: "prefilled-test-payload\n".to_string(),
    };

    let res = tool
        .execute_with_options(&t_ctx, json!({"command": cmd}), None, Some(&input_contract))
        .await
        .unwrap();

    if let ToolOutput::Shell {
        stdout,
        exit,
        termination,
        ..
    } = res
    {
        assert_eq!(exit, Some(0));
        assert_eq!(termination, ShellTermination::Exited);
        assert!(stdout.contains("prefilled-test-payload"));
    } else {
        panic!("expected Shell output");
    }
}

#[tokio::test]
async fn test_exec_tool_truncation_stub() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // Output ~10,000 characters to cross SHELL_MAX_OUTPUT_CHARS (8,000)
    #[cfg(not(windows))]
    let cmd = "python3 -c \"print('A' * 10000)\" 2>/dev/null || seq 1 2500";
    #[cfg(windows)]
    let cmd = "powershell -Command \"Write-Output ('A' * 10000)\"";

    let res = tool.execute(&t_ctx, json!({"command": cmd})).await.unwrap();
    if let ToolOutput::Shell {
        truncated, stdout, ..
    } = &res
    {
        if stdout.len() > nuo_tool::tool_output::SHELL_MAX_OUTPUT_CHARS {
            assert!(
                truncated,
                "Oversized output must be marked as truncated stub"
            );
            assert!(res.content().contains("Output truncated"));
        }
    }
}

#[tokio::test]
async fn test_exec_tool_streamguard_flood_protection() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // Emit more than SHELL_STREAM_FLOOD_BYTES (2 MB) to trigger StreamGuard physical kill
    #[cfg(not(windows))]
    let cmd = "python3 -c \"import sys; sys.stdout.write('A' * (3 * 1024 * 1024))\" 2>/dev/null || yes 'streamguard_flood_payload_for_testing' | head -n 60000";
    #[cfg(windows)]
    let cmd = "powershell -Command \"Write-Output ('A' * (3 * 1024 * 1024))\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "raw": false}))
        .await
        .unwrap();
    if let ToolOutput::Shell {
        termination,
        truncated,
        ..
    } = res
    {
        assert_eq!(termination, ShellTermination::StreamGuard);
        assert!(truncated);
    }
}

#[tokio::test]
async fn test_exec_tool_streamguard_unbounded_single_line_flood() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // Emit an unbounded single line (>2 MB without newlines) to verify chunked ingestion
    #[cfg(not(windows))]
    let cmd = "python3 -c \"import sys; sys.stdout.write('B' * (3 * 1024 * 1024))\" 2>/dev/null || head -c 3145728 /dev/zero | tr '\\0' 'B'";
    #[cfg(windows)]
    let cmd = "powershell -Command \"[Console]::Out.Write('B' * (3 * 1024 * 1024))\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "raw": false}))
        .await
        .unwrap();
    if let ToolOutput::Shell {
        termination,
        truncated,
        ..
    } = res
    {
        assert_eq!(termination, ShellTermination::StreamGuard);
        assert!(truncated);
    }
}

#[tokio::test]
async fn test_exec_tool_large_finite_output_not_killed_by_streamguard() {
    let dir = tempdir().unwrap();
    let ctx = Arc::new(ExecToolContext::new(dir.path()));
    let tool = ExecuteCommandTool::new(ctx);
    let t_ctx = ToolContext::default();

    // Emit ~2,000 lines (>8 KB, typical unit test / clippy suite) without raw mode
    #[cfg(not(windows))]
    let cmd = "seq 1 2000";
    #[cfg(windows)]
    let cmd = "powershell -Command \"1..2000\"";

    let res = tool
        .execute(&t_ctx, json!({"command": cmd, "raw": false}))
        .await
        .unwrap();
    if let ToolOutput::Shell {
        termination,
        exit,
        stdout,
        truncated,
        ..
    } = res
    {
        assert_eq!(
            termination,
            ShellTermination::Exited,
            "Normal finite multi-thousand line command must exit cleanly, not be murdered by StreamGuard"
        );
        assert_eq!(exit, Some(0));
        assert!(
            stdout.len() > nuo_tool::tool_output::SHELL_MAX_OUTPUT_CHARS,
            "Output should cross cognitive threshold (8 KB)"
        );
        assert!(
            truncated,
            "Downstream should mark it truncated for cognitive context protection"
        );
    }
}

#[test]
fn scope_target_and_accesses_resolve_command() {
    let ctx = Arc::new(ExecToolContext::new("."));
    let tool = ExecuteCommandTool::new(ctx);
    assert_eq!(
        tool.scope_target(r#"{"command": "cargo test"}"#),
        ScopeTarget::Command("cargo test".to_string())
    );
    assert_eq!(
        tool.accesses(r#"{"command": "cargo test"}"#),
        nuo_tool::ToolAccesses::all()
    );
    assert_eq!(tool.scope_target(r#"{}"#), ScopeTarget::Unspecified);
}
