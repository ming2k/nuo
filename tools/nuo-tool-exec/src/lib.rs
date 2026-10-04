//! Sandboxed command execution tools for Nuo cognitive agents.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nuo_tool_fs::SystemToolContext;
use nuo_tool::{
    RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolScope, ToolSchema,
};
use serde::Deserialize;
use serde_json::Value;

/// Typed parameters for [`ExecuteCommandTool`].
#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ExecuteCommandArgs {
    #[tool(desc = "The shell command to execute. Commands MUST be finite and self-terminating.")]
    pub command: String,
    #[tool(
        desc = "Overall timeout in seconds (default 1800 = 30 minutes). Enforced by StreamGuard (ADR-0257/0263)."
    )]
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
        "execute_command"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["run_command"]
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
        let args: ExecuteCommandArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::execution(self.name(), format!("invalid arguments: {err}")))?;

        let command = &args.command;
        let timeout_secs = args
            .timeout
            .map(Duration::from_secs)
            .unwrap_or(self.ctx.default_timeout);

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

        let output_future = cmd.output();
        let output = match tokio::time::timeout(timeout_secs, output_future).await {
            Ok(Ok(out)) => out,
            Ok(Err(err)) => {
                return Err(ToolError::execution(
                    self.name(),
                    format!("failed to spawn process: {err}"),
                ));
            }
            Err(_) => {
                return Err(ToolError::execution(
                    self.name(),
                    format!("command timed out after {}s", timeout_secs.as_secs()),
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        let mut summary = Vec::new();
        if !stdout.is_empty() {
            summary.push(format!("STDOUT:\n{}", stdout.trim_end()));
        }
        if !stderr.is_empty() {
            summary.push(format!("STDERR:\n{}", stderr.trim_end()));
        }
        if summary.is_empty() {
            summary.push("(No output produced)".to_string());
        }
        summary.push(format!("(exit code: {})", exit_code));

        if output.status.success() {
            Ok(ToolOutput::success(summary.join("\n\n")))
        } else {
            Ok(ToolOutput::error(summary.join("\n\n")))
        }
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

        let res = tool.execute(&t_ctx, json!({"command": "echo test"})).await.unwrap();
        assert!(res.content().contains("test"));
        assert!(tool.matches_name("run_command"));
        assert!(tool.matches_name("execute_command"));
    }
}
