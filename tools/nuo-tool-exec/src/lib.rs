//! Sandboxed command execution tools for Nuo cognitive agents.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nuo_tool::stream::{InputContract, ToolStream};
use nuo_tool::{
    BuiltinTool, RiskProfile, ShellLine, ShellStream, ShellTermination, Tool, ToolContext,
    ToolError, ToolInvocation, ToolOutput, ToolSchema, ToolScope,
};
use serde::Deserialize;
use serde_json::Value;

/// Maximum lines a foreground command may produce before StreamGuard terminates it early (ADR-0057, ADR-0099).
/// Serves strictly as a secondary ceiling against micro-allocation heap fragmentation.
pub const SHELL_STREAM_FLOOD_LINES: usize = 20_000;
/// Maximum bytes a foreground command may produce before StreamGuard terminates it early (ADR-0057, ADR-0099).
/// Primary physical memory and runaway flood watchdog (2 MB).
pub const SHELL_STREAM_FLOOD_BYTES: usize = 2 * 1024 * 1024;
/// Relaxed bounds when raw mode is explicitly requested.
pub const RAW_STREAM_FLOOD_LINES: usize = 100_000;
pub const RAW_STREAM_FLOOD_BYTES: usize = 10 * 1024 * 1024;
/// Maximum chunk size before chunking unbounded single-line streams (ADR-0099 [INV-STREAM-02]).
pub const MAX_LINE_CHUNK_BYTES: usize = 64 * 1024;

/// Default overall wall-clock timeout for shell commands: 1800s (30 minutes) (ADR-0099).
pub const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 1800;

/// Default idle timeout duration for shell commands when not explicitly configured: None (disabled).
pub const DEFAULT_IDLE_TIMEOUT_SECS: Option<u64> = None;

/// Context holding the active workspace root and process execution settings for [`ExecuteCommandTool`].
#[derive(Debug, Clone)]
pub struct ExecToolContext {
    pub workspace_root: PathBuf,
    pub default_timeout: Duration,
    pub default_idle_timeout: Option<Duration>,
}

impl ExecToolContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            default_timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
            default_idle_timeout: None,
        }
    }

    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub fn with_default_idle_timeout(mut self, idle_timeout: impl Into<Option<Duration>>) -> Self {
        self.default_idle_timeout = idle_timeout.into();
        self
    }
}

/// Typed parameters for [`ExecuteCommandTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ExecuteCommandArgs {
    #[tool(desc = "The shell command to execute. Commands MUST be finite and self-terminating.")]
    pub command: String,
    #[tool(
        desc = "Overall wall-clock timeout in seconds (default 1800 = 30 minutes). Enforces a hard physical execution ceiling."
    )]
    pub timeout: Option<u64>,
    #[tool(
        desc = "Optional idle timeout in seconds. Execution terminates early as IdleBlocked if no stdout/stderr output is observed for this duration."
    )]
    pub idle_timeout: Option<u64>,
    #[tool(
        desc = "Set to true to bypass semantic folding and output raw unabridged command stream (default false)."
    )]
    pub raw: Option<bool>,
}

/// Executes a shell command inside the workspace environment.
pub struct ExecuteCommandTool {
    ctx: Arc<ExecToolContext>,
}

impl ExecuteCommandTool {
    pub fn new(ctx: Arc<ExecToolContext>) -> Self {
        Self { ctx }
    }

    /// Internal execution method supporting progressive stream callbacks and input contracts.
    pub async fn execute_with_options(
        &self,
        ctx: &ToolContext,
        arguments: Value,
        mut on_stream: Option<&mut (dyn FnMut(ToolStream) + Send)>,
        input_contract: Option<&InputContract>,
    ) -> Result<ToolOutput, ToolError> {
        let args: ExecuteCommandArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

        let command = &args.command;
        let wall_clock_timeout = args
            .timeout
            .map(Duration::from_secs)
            .unwrap_or(self.ctx.default_timeout);
        let idle_timeout = args
            .idle_timeout
            .map(Duration::from_secs)
            .or(self.ctx.default_idle_timeout);
        let raw = args.raw.unwrap_or(false);

        let (max_lines, max_bytes) = if raw {
            (RAW_STREAM_FLOOD_LINES, RAW_STREAM_FLOOD_BYTES)
        } else {
            (SHELL_STREAM_FLOOD_LINES, SHELL_STREAM_FLOOD_BYTES)
        };

        #[cfg(windows)]
        let mut cmd = tokio::process::Command::new("cmd");
        #[cfg(windows)]
        cmd.args(["/C", command]);

        #[cfg(not(windows))]
        let mut cmd = tokio::process::Command::new("sh");
        #[cfg(not(windows))]
        cmd.args(["-c", command]);

        let cwd = ctx
            .workspace_roots
            .as_ref()
            .and_then(|roots| roots.first())
            .unwrap_or(&self.ctx.workspace_root);
        cmd.current_dir(cwd);

        // Input contract enforcement: prefilled stdin pipe or sealed /dev/null
        let prefilled_data = match input_contract {
            Some(InputContract::Prefilled { data }) => {
                cmd.stdin(std::process::Stdio::piped());
                Some(data.clone())
            }
            _ => {
                cmd.stdin(std::process::Stdio::null());
                None
            }
        };

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|err| {
            ToolError::execution(self.name(), format!("failed to spawn process: {err}"))
        })?;

        // If prefilled data provided, write it out and close stdin immediately
        if let Some(data) = prefilled_data {
            if let Some(mut stdin) = child.stdin.take() {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let _ = stdin.write_all(data.as_bytes()).await;
                    let _ = stdin.flush().await;
                    drop(stdin);
                });
            }
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(ShellStream, String)>();

        let stdout_handle =
            stdout.map(|out| tokio::spawn(read_stream_lines(out, tx.clone(), ShellStream::Out)));

        let stderr_handle =
            stderr.map(|err| tokio::spawn(read_stream_lines(err, tx.clone(), ShellStream::Err)));
        drop(tx);

        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();
        let mut lines = Vec::new();

        let cancel_token = ctx.cancel_token.clone();
        let wall_clock_deadline = tokio::time::Instant::now() + wall_clock_timeout;
        let mut idle_deadline = idle_timeout.map(|d| tokio::time::Instant::now() + d);

        let mut flood_halt = false;
        let mut idle_halt = false;
        let mut cancel_halt = false;
        let mut wall_clock_halt = false;

        loop {
            let sleep_wall_clock = tokio::time::sleep_until(wall_clock_deadline);
            let sleep_idle = async {
                match idle_deadline {
                    Some(dl) => tokio::time::sleep_until(dl).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;

                _ = cancel_token.cancelled() => {
                    cancel_halt = true;
                    break;
                }

                _ = sleep_wall_clock => {
                    wall_clock_halt = true;
                    break;
                }

                _ = sleep_idle => {
                    idle_halt = true;
                    break;
                }

                msg = rx.recv() => {
                    match msg {
                        Some((stream, line)) => {
                            if let Some(dur) = idle_timeout {
                                idle_deadline = Some(tokio::time::Instant::now() + dur);
                            }

                            if let Some(ref mut sink) = on_stream {
                                match stream {
                                    ShellStream::Out => sink(ToolStream::Stdout(line.clone())),
                                    ShellStream::Err => sink(ToolStream::Stderr(line.clone())),
                                }
                            }

                            if let Some(sink) = &ctx.stream_sink {
                                match stream {
                                    ShellStream::Out => sink.emit(ToolStream::Stdout(line.clone())),
                                    ShellStream::Err => sink.emit(ToolStream::Stderr(line.clone())),
                                }
                            }

                            match stream {
                                ShellStream::Out => {
                                    stdout_buf.push_str(&line);
                                    stdout_buf.push('\n');
                                }
                                ShellStream::Err => {
                                    stderr_buf.push_str(&line);
                                    stderr_buf.push('\n');
                                }
                            }
                            lines.push(ShellLine { stream, text: line });

                            if lines.len() >= max_lines || (stdout_buf.len() + stderr_buf.len()) >= max_bytes {
                                flood_halt = true;
                                break;
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }
            }
        }

        let (exit_code, termination) = if cancel_halt {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, ShellTermination::Cancelled)
        } else if wall_clock_halt {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, ShellTermination::Timeout)
        } else if idle_halt {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, ShellTermination::IdleBlocked)
        } else if flood_halt {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, ShellTermination::StreamGuard)
        } else {
            let sleep_idle = async {
                match idle_deadline {
                    Some(dl) => tokio::time::sleep_until(dl).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;

                _ = cancel_token.cancelled() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (None, ShellTermination::Cancelled)
                }

                _ = tokio::time::sleep_until(wall_clock_deadline) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (None, ShellTermination::Timeout)
                }

                _ = sleep_idle => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (None, ShellTermination::IdleBlocked)
                }

                status = child.wait() => {
                    match status {
                        Ok(st) => (st.code(), ShellTermination::Exited),
                        Err(_) => (None, ShellTermination::Exited),
                    }
                }
            }
        };

        if let Some(h) = stdout_handle {
            h.abort();
        }
        if let Some(h) = stderr_handle {
            h.abort();
        }

        let total_chars = stdout_buf.len() + stderr_buf.len();
        let truncated = total_chars > nuo_tool::tool_output::SHELL_MAX_OUTPUT_CHARS
            || termination == ShellTermination::StreamGuard;
        let truncated_note = truncated.then(|| {
            format!("Output truncated: {total_chars} chars total; full raw log spilled to disk")
        });

        Ok(ToolOutput::Shell {
            command: command.clone(),
            stdout: stdout_buf,
            stderr: stderr_buf,
            lines,
            exit: exit_code,
            truncated,
            truncated_note,
            termination,
            detached_job_id: None,
        })
    }
}

#[async_trait]
impl Tool for ExecuteCommandTool {
    fn name(&self) -> &str {
        BuiltinTool::ExecuteCommand.as_str()
    }

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::ExecuteCommand.aliases()
    }

    fn description(&self) -> &str {
        "Execute a shell command in a non-interactive environment. Commands must be finite and self-terminating."
    }

    fn parameters_schema(&self) -> Value {
        ExecuteCommandArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ArbitraryExecution
    }

    fn scopes(&self) -> Vec<ToolScope> {
        vec![ToolScope::Workspace]
    }

    async fn execute(&self, ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        self.execute_with_options(ctx, arguments, None, None).await
    }

    async fn call_structured_with_events<'a>(
        &self,
        invocation: ToolInvocation<'a>,
        _on_event: Box<dyn FnMut(nuo_tool::events::SubagentEvent) + Send + 'a>,
        on_stream: &mut (dyn FnMut(ToolStream) + Send + 'a),
    ) -> std::result::Result<ToolOutput, String> {
        let value: Value = serde_json::from_str(invocation.arguments).map_err(|e| e.to_string())?;
        let ctx = ToolContext::default();
        self.execute_with_options(&ctx, value, Some(on_stream), Some(&invocation.input))
            .await
            .map_err(|e| e.to_string())
    }
}

/// Asynchronously reads an incoming stream, emitting lines or bounded 64 KB chunks to prevent
/// memory bloat on unbounded single-line outputs (ADR-0099 [INV-STREAM-02]).
async fn read_stream_lines(
    stream: impl tokio::io::AsyncRead + Unpin,
    tx: tokio::sync::mpsc::UnboundedSender<(ShellStream, String)>,
    stream_type: ShellStream,
) {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        let available = match reader.fill_buf().await {
            Ok([]) => break,
            Ok(n) => n,
            Err(_) => break,
        };

        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let consume_len = pos + 1;
            buf.extend_from_slice(&available[..consume_len]);
            reader.consume(consume_len);
            let line = String::from_utf8_lossy(&buf)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            buf.clear();
            if tx.send((stream_type, line)).is_err() {
                break;
            }
        } else {
            let chunk_len = available.len();
            buf.extend_from_slice(available);
            reader.consume(chunk_len);
            if buf.len() >= MAX_LINE_CHUNK_BYTES {
                let line = String::from_utf8_lossy(&buf).to_string();
                buf.clear();
                if tx.send((stream_type, line)).is_err() {
                    break;
                }
            }
        }
    }
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let _ = tx.send((stream_type, line));
    }
}

/// Creates command execution tools for an agent.
pub fn create_exec_tools(ctx: Arc<ExecToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(ExecuteCommandTool::new(ctx))]
}

#[cfg(test)]
mod tests {
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
}
