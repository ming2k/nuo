mod episodic;
pub mod pipes;

#[cfg(test)]
mod tests;

use async_trait::async_trait;
use nuo_wire::Tool;
use nuo_tool::ToolSchema;
use serde::Deserialize;
use tokio::time::Duration;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, json_string, workspace_base,
};

#[derive(ToolSchema, Deserialize)]
struct ExecuteCommandArgs {
    #[tool(desc = "The shell command to execute. Commands MUST be finite and self-terminating.")]
    command: String,
    #[tool(
        desc = "Overall timeout in seconds (default 1800 = 30 minutes). Enforced by StreamGuard (ADR-0257/0263)."
    )]
    timeout: Option<u64>,
    #[tool(
        desc = "Set to true to bypass semantic folding and output raw unabridged command stream (default false)."
    )]
    raw: Option<bool>,
}

#[allow(dead_code)] // tool-schema: dynamic JSON schema generation
#[derive(ToolSchema, Deserialize)]
struct WorkspaceExecuteCommandArgs {
    #[tool(
        desc = "The shell command to execute inside the workspace sandbox. Foreground commands must be finite and self-terminating."
    )]
    command: String,
    #[tool(
        desc = "Overall timeout in seconds (default 1800 = 30 minutes). A command producing no output for timeout/3 (min 5s, max 480s) is killed early as a blocked-command guard. Continuous unbounded streaming is terminated early by StreamGuard (ADR-0257)."
    )]
    timeout: Option<u64>,
    #[tool(
        desc = "Set to true to bypass semantic folding and output raw unabridged command stream (default false)."
    )]
    raw: Option<bool>,
}

/// Execute a command in a non-interactive shell.
///
/// # Security & Threat Model
///
/// ⚠️ **HIGH-PRIVILEGE TOOL (DANGEROUS)**:
/// When executing under [`ShellIsolation::Host`](nuo_wire::ShellIsolation::Host) (default without an active
/// workspace sandbox container/namespace), commands run directly on the host system with the full privileges of the
/// running process. This **bypasses all workspace boundaries and jail constraints** that are strictly enforced on
/// filesystem tools (`read_text`, `write_file`, etc.).
///
/// ### Crucial Security Guidelines for Agent & Tool Integration:
/// 1. **Principle of Least Privilege (PoLP)**:
///    - **NEVER** expose `run_command` to read-only, analysis, exploratory, or untrusted sub-agents (e.g. `explore`
///      sub-agents must exclude this tool).
///    - **NEVER** expose this tool in environments where agent inputs come from untrusted external sources (such as
///      raw web scrapers, webhook listeners, or untrusted prompt contexts) without strict human-in-the-loop approval
///      or full virtualization/sandboxing.
/// 2. **Prefer Atomic Tools**:
///    - Model prompts and policies MUST actively discourage using shell commands (such as `cat`, `sed`, `echo >`,
///      `grep`, `find`) for filesystem inspection or editing, and direct the model to dedicated workspace-bound tools.
/// 3. **Sandbox Recommended**:
///    - For untrusted or autonomous multi-turn loops, configure [`ShellIsolation::Workspace`](nuo_wire::ShellIsolation::Workspace)
///      so commands run within an isolated Linux namespace/container where external filesystem access and network are restricted.
///
/// Commands run in the session's workspace root (captured at factory time),
/// not the daemon process's cwd — under the unified daemon (ADR-0096) those
/// differ whenever the daemon was first spawned from another project.
pub struct ExecuteCommandTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>>,
    workspace_sandbox: bool,
}

impl ExecuteCommandTool {
    /// Build the default host-command variant against a workspace root.
    /// Runtime uses this for the `!`-prefix shell path, which bypasses the
    /// factory-based toolset assembly but must still run in the session's
    /// project (not the daemon's process cwd, ADR-0096).
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self {
            root,
            env: None,
            workspace_sandbox: false,
        }
    }

    /// Build the shell tool backed by a custom execution environment.
    pub fn with_env(env: std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
            workspace_sandbox: false,
        }
    }

    /// Build the workspace-contained variant. It shares the same
    /// model-facing capability name; agent presets select it by variant id.
    pub fn workspace_with_env(
        env: std::sync::Arc<dyn nuo_wire::ExecutionEnvironment>,
    ) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
            workspace_sandbox: true,
        }
    }

    fn shell_isolation(&self) -> nuo_wire::ShellIsolation {
        if self.workspace_sandbox {
            nuo_wire::ShellIsolation::Workspace
        } else {
            self.env
                .as_ref()
                .map(|env| env.shell_isolation())
                .unwrap_or(nuo_wire::ShellIsolation::Host)
        }
    }
}

#[async_trait]
impl Tool for ExecuteCommandTool {
    fn name(&self) -> &str {
        "run_command"
    }
    fn variant(&self) -> &str {
        if self.workspace_sandbox {
            "workspace"
        } else {
            "default"
        }
    }
    fn is_available(&self) -> bool {
        self.shell_isolation() != nuo_wire::ShellIsolation::Workspace
            || nuo_host::workspace_sandbox::available()
    }
    /// The command tool's primary purpose is execution, not workspace
    /// mutation — so it sits in the `Execute` tier between pure reads and
    /// file-writing tools. The broker still gates it (`Execute > Read`). See
    /// ADR-0012.
    fn description(&self) -> &str {
        if self.workspace_sandbox {
            "Execute a shell command inside the isolated workspace. Foreground commands must be finite and self-terminating; continuous unbounded streaming is terminated early by StreamGuard. Host files outside admitted roots and network access are unavailable."
        } else {
            "Execute a shell command in a headless non-interactive environment. Foreground commands MUST be finite and self-terminating. Never execute unbounded continuous monitoring or streaming tools (e.g. top, intel_gpu_top, tail -f, ping) without bounds (e.g. `timeout 2s`, `| head`, or one-shot flags). Continuous streaming in foreground is terminated early by StreamGuard (ADR-0257/0263)."
        }
    }
    fn parameters(&self) -> serde_json::Value {
        if self.workspace_sandbox {
            WorkspaceExecuteCommandArgs::parameters_schema()
        } else {
            ExecuteCommandArgs::parameters_schema()
        }
    }
    fn scope_target(&self, arguments: &str) -> nuo_wire::ScopeTarget {
        nuo_wire::ScopeTarget::Command(json_string(arguments, "command"))
    }
    fn hazard_level(&self) -> nuo_wire::HazardLevel {
        nuo_wire::HazardLevel::CommandExecution
    }
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<nuo_wire::ToolPermissionSubmission> {
        let command = json_string(arguments, "command");
        let first_word = command.split_whitespace().next().unwrap_or("sh");
        let sandboxed = self.shell_isolation() == nuo_wire::ShellIsolation::Workspace;
        Some(nuo_wire::ToolPermissionSubmission {
            hazard_level: nuo_wire::HazardLevel::CommandExecution,
            label: format!(
                "Execute{}: `{}`",
                if sandboxed {
                    " in workspace"
                } else {
                    " command"
                },
                if command.len() > 50 {
                    format!("{}...", &command[..47])
                } else {
                    command.clone()
                }
            ),
            description: if sandboxed {
                format!(
                    "Runs command `{command}` inside the isolated workspace with network access disabled."
                )
            } else {
                format!(
                    "Runs host shell command `{command}`. May modify system state or execute arbitrary binaries."
                )
            },
            scope: command.clone(),
            payload: nuo_wire::ToolPermissionPayload::Command {
                command: command.clone(),
                cwd: None,
                kill_spec: nuo_wire::ProcessKillSpec {
                    command: first_word.to_string(),
                    process_group_killable: true,
                    pkill_target: format!("pkill -f '{first_word}'"),
                    cwd: None,
                },
            },
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<nuo_wire::ToolOutput, String> {
        self.call_structured_with_events(
            nuo_wire::ToolInvocation {
                call_id: "",
                arguments,
                input: nuo_wire::InputContract::default(),
                input_handler: None,
            },
            Box::new(|_| {}),
            &mut |_| {},
        )
        .await
    }

    async fn call_structured_with_events<'a>(
        &self,
        invocation: nuo_wire::ToolInvocation<'a>,
        _on_event: Box<dyn FnMut(nuo_wire::SubagentEvent) + Send + 'a>,
        on_stream: &mut (dyn FnMut(nuo_wire::ToolStream) + Send + 'a),
    ) -> Result<nuo_wire::ToolOutput, String> {
        let args: ExecuteCommandArgs = serde_json::from_str(invocation.arguments)
            .map_err(|e| format!("Invalid JSON: {}", e))?;
        let timeout_secs = args.timeout.unwrap_or(1800);
        let timeout_duration = Duration::from_secs(timeout_secs);

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        episodic::run_episodic_command(
            &args.command,
            self.shell_isolation(),
            env,
            invocation.input,
            episodic::RunPolicy {
                timeout: timeout_duration,
                raw: args.raw.unwrap_or(false),
                handler: invocation.input_handler,
            },
            on_stream,
        )
        .await
    }
}

nuo_wire::register_tool!(ExecuteCommandFactory => |ctx| {
    let env = Some(execution_environment(ctx));
    ExecuteCommandTool {
        root: workspace_base(ctx),
        env,
        workspace_sandbox: false,
    }
});

nuo_wire::register_tool!(WorkspaceExecuteCommandFactory => |ctx| {
    let env = execution_environment(ctx);
    ExecuteCommandTool::workspace_with_env(env)
});
