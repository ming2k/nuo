//! Sandboxed command execution tools for Nuo cognitive agents.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nuo_tool::{
    BuiltinTool, RiskProfile, ShellLine, ShellStream, ShellTermination, Tool, ToolContext,
    ToolError, ToolOutput, ToolSchema, ToolScope,
};
use nuo_tool_fs::SystemToolContext;
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

/// Typed parameters for [`ExecuteCommandTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ExecuteCommandArgs {
    #[tool(desc = "The shell command to execute. Commands MUST be finite and self-terminating.")]
    pub command: String,
    #[tool(desc = "Overall wall-clock timeout in seconds (default 1800 = 30 minutes).")]
    pub timeout: Option<u64>,
    #[tool(
        desc = "Set to true to bypass semantic folding and output raw unabridged command stream (default false)."
    )]
    pub raw: Option<bool>,
}

/// Executes a shell command inside the workspace environment.
pub struct ExecuteCommandTool {
    ctx: Arc<SystemToolContext>,
}

impl ExecuteCommandTool {
    pub fn new(ctx: Arc<SystemToolContext>) -> Self {
        Self { ctx }
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

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: ExecuteCommandArgs = serde_json::from_value(arguments).map_err(|err| {
            ToolError::execution(self.name(), format!("invalid arguments: {err}"))
        })?;

        let command = &args.command;
        let timeout_secs = args
            .timeout
            .map(Duration::from_secs)
            .unwrap_or(self.ctx.default_timeout);
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

        cmd.current_dir(&self.ctx.workspace_root);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|err| {
            ToolError::execution(self.name(), format!("failed to spawn process: {err}"))
        })?;

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

        let run_future = async {
            let mut flood_halt = false;
            while let Some((stream, line)) = rx.recv().await {
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
            if flood_halt {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return (None, ShellTermination::StreamGuard);
            }
            let status = child.wait().await.ok();
            (status.and_then(|s| s.code()), ShellTermination::Exited)
        };

        let (exit_code, termination) = match tokio::time::timeout(timeout_secs, run_future).await {
            Ok((code, term)) => (code, term),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (None, ShellTermination::Timeout)
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
pub fn create_exec_tools(ctx: Arc<SystemToolContext>) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(ExecuteCommandTool::new(ctx))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_exec_tool() {
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
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
    async fn test_exec_tool_truncation_stub() {
        let dir = tempdir().unwrap();
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
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
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
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
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
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
        let ctx = Arc::new(SystemToolContext::new(dir.path()));
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
