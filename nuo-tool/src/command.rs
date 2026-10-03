use crate::Tool;
use crate::context::ToolContext;
use crate::error::{Result, ToolError};
use crate::output::ToolOutput;
use crate::risk::RiskProfile;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;

/// Specifies which shell interpreter to use when executing commands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ShellKind {
    /// Automatic detection based on host OS and system environment.
    ///
    /// - On Windows: Prefers modern PowerShell Core (`pwsh`), falls back to Windows PowerShell
    ///   (`powershell.exe`), and as an ultimate fallback uses legacy `cmd.exe`.
    /// - On Unix/Linux/macOS: Uses `$SHELL` environment variable if available and non-empty,
    ///   otherwise falls back to `sh`.
    #[default]
    Auto,

    /// Modern PowerShell Core or Windows PowerShell.
    ///
    /// Invoked with `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command <command>`.
    /// Sets UTF-8 encoding for clean multi-lingual output.
    PowerShell,

    /// POSIX compatible shell (`sh`). Invoked with `-c <command>`.
    Sh,

    /// Bourne-Again SHell (`bash`). Invoked with `-c <command>`.
    Bash,

    /// Windows Command Prompt (`cmd.exe`). Invoked with `/C <command>`.
    Cmd,

    /// Custom shell program and argument prefix.
    /// Example: `ShellKind::Custom("zsh".into(), vec!["-c".into()])`.
    Custom(String, Vec<String>),
}

impl ShellKind {
    /// Parses a string representation into a known `ShellKind`.
    pub fn parse_loose(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "auto" | "default" => Some(Self::Auto),
            "powershell" | "pwsh" | "ps" => Some(Self::PowerShell),
            "sh" => Some(Self::Sh),
            "bash" => Some(Self::Bash),
            "cmd" | "cmd.exe" => Some(Self::Cmd),
            _ => None,
        }
    }
}

/// Production-grade asynchronous shell command execution tool.
///
/// Features:
/// - First-class modern shell integration: prioritizes modern PowerShell on Windows
///   (with `-NoProfile -NonInteractive -ExecutionPolicy Bypass`) and `$SHELL`/`sh` on Unix.
/// - Cross-platform UTF-8 console output consistency.
/// - Comprehensive heuristic detection for destructive commands across Unix, Windows, and PowerShell.
/// - Bounded execution timeouts with `kill_on_drop` process termination.
/// - Working directory configuration and cooperative cancellation via `ToolContext`.
/// - Explicit `ArbitraryExecution` risk profiling.
#[derive(Clone)]
pub struct CommandTool {
    default_timeout: Duration,
    default_working_dir: Option<PathBuf>,
    default_shell: ShellKind,
    require_all_approval: bool,
}

impl Default for CommandTool {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandTool {
    pub fn new() -> Self {
        Self {
            default_timeout: DEFAULT_COMMAND_TIMEOUT,
            default_working_dir: None,
            default_shell: ShellKind::Auto,
            require_all_approval: false,
        }
    }

    /// Sets the default execution timeout.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Sets the default working directory for executed commands.
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.default_working_dir = Some(dir.into());
        self
    }

    /// Sets the default shell interpreter kind.
    pub fn with_shell(mut self, shell: ShellKind) -> Self {
        self.default_shell = shell;
        self
    }

    /// Gets the current default shell configuration.
    pub fn shell(&self) -> &ShellKind {
        &self.default_shell
    }

    /// Gets the configured default working directory.
    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.default_working_dir.as_ref()
    }

    /// Gets the default timeout duration.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// Enforces that EVERY command invocation requires approval, regardless of heuristic risk.
    pub fn require_approval_for_all(mut self, require: bool) -> Self {
        self.require_all_approval = require;
        self
    }

    /// Comprehensive evaluation for potentially destructive or high-risk commands
    /// across Unix, Windows cmd, and modern PowerShell environments.
    pub fn is_high_risk_command(cmd: &str) -> bool {
        let lower = cmd.to_lowercase();
        let tokens: Vec<&str> = lower.split_whitespace().collect();

        // 1. Unix destructive file deletion & format
        if lower.contains("rm -rf")
            || lower.contains("rm -fr")
            || lower.contains("rm -r -f")
            || lower.contains("rm -f -r")
            || lower.contains("rm --recursive --force")
            || lower.contains("mkfs")
            || lower.contains("dd if=")
            || lower.contains("chmod -r 777")
            || lower.contains("chmod -r 000")
            || lower.contains("chown -r")
            || lower.contains(":(){ :|:& };:")
        {
            return true;
        }

        // Direct device overwrites (> /dev/sd*, > /dev/nvme*, > /dev/hd*, > /dev/vd*)
        if lower.contains("> /dev/sd")
            || lower.contains(">/dev/sd")
            || lower.contains("> /dev/nvme")
            || lower.contains(">/dev/nvme")
            || lower.contains("> /dev/hd")
            || lower.contains(">/dev/hd")
            || lower.contains("> /dev/vd")
            || lower.contains(">/dev/vd")
        {
            return true;
        }

        // Unix root / home wiping attempts
        if tokens.contains(&"rm")
            && (lower.contains("/*")
                || lower.contains(" /")
                || lower.contains("~")
                || lower.contains("/root")
                || lower.contains("/etc")
                || lower.contains("/usr")
                || lower.contains("/var"))
        {
            return true;
        }

        // 2. Windows / Cmd destructive file & volume operations
        if lower.contains("rmdir /s")
            || lower.contains("rmdir /q /s")
            || lower.contains("rd /s")
            || lower.contains("rd /q /s")
            || lower.contains("del /f /s")
            || lower.contains("del /s /f")
            || lower.contains("erase /f /s")
            || lower.contains("erase /s /f")
        {
            return true;
        }

        // Disk formatting and partitioning
        if lower.contains("format c:")
            || lower.contains("format d:")
            || lower.contains("clear-disk")
            || lower.contains("initialize-disk")
            || lower.contains("format-volume")
            || lower.contains("clear-partition")
            || lower.contains("remove-partition")
            || lower.contains("diskpart")
        {
            return true;
        }

        // 3. PowerShell destructive cmdlets & recursive removal
        if (lower.contains("remove-item") || tokens.contains(&"ri"))
            && (lower.contains("-recurse") || lower.contains("-r"))
            && (lower.contains("-force") || lower.contains("-confirm:$false"))
        {
            return true;
        }

        // Shadow copies deletion (ransomware pattern)
        if lower.contains("vssadmin delete shadows") || lower.contains("wmic shadowcopy delete") {
            return true;
        }

        // Registry destructive deletions
        if lower.contains("reg delete") && (lower.contains("hklm") || lower.contains("hkcu")) {
            return true;
        }

        // 4. System power state & execution policy tampering
        if lower.contains("shutdown")
            || lower.contains("reboot")
            || lower.contains("poweroff")
            || lower.contains("init 0")
            || lower.contains("init 6")
            || lower.contains("stop-computer")
            || lower.contains("restart-computer")
        {
            return true;
        }

        // Database wipe
        if lower.contains("drop database")
            || lower.contains("drop table")
            || lower.contains("truncate table")
        {
            return true;
        }

        false
    }

    /// Resolves the concrete shell program and arguments for executing the given command string.
    pub fn build_process_command(
        &self,
        command_str: &str,
        shell_override: Option<&str>,
    ) -> tokio::process::Command {
        let requested_shell = shell_override
            .and_then(ShellKind::parse_loose)
            .unwrap_or_else(|| self.default_shell.clone());

        let mut cmd = match requested_shell {
            ShellKind::Auto => {
                #[cfg(windows)]
                {
                    build_windows_auto_command(command_str)
                }
                #[cfg(not(windows))]
                {
                    build_unix_auto_command(command_str)
                }
            }
            ShellKind::PowerShell => build_powershell_command(command_str),
            ShellKind::Sh => {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(command_str);
                c
            }
            ShellKind::Bash => {
                let mut c = tokio::process::Command::new("bash");
                c.arg("-c").arg(command_str);
                c
            }
            ShellKind::Cmd => {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C").arg(command_str);
                c
            }
            ShellKind::Custom(ref program, ref prefix_args) => {
                let mut c = tokio::process::Command::new(program);
                for arg in prefix_args {
                    c.arg(arg);
                }
                c.arg(command_str);
                c
            }
        };

        // Ensure UTF-8 consistency across cross-platform environments
        cmd.env("PYTHONIOENCODING", "utf-8");
        if std::env::var_os("LANG").is_none() {
            cmd.env("LANG", "en_US.UTF-8");
        }

        cmd.kill_on_drop(true);
        cmd
    }
}

/// Builds a modern PowerShell command invocation configured for headless, non-interactive execution.
fn build_powershell_command(command_str: &str) -> tokio::process::Command {
    let pwsh_bin = if is_executable_available("pwsh") {
        "pwsh"
    } else {
        "powershell"
    };

    let mut c = tokio::process::Command::new(pwsh_bin);
    c.arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command");

    // Guarantee UTF-8 console output encoding and propagate $LASTEXITCODE
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $OutputEncoding = [System.Text.Encoding]::UTF8; {command_str}; if ($LASTEXITCODE -ne $null -and $LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}"
    );
    c.arg(script);
    c
}

/// Windows auto-detection: pwsh -> powershell -> cmd
#[cfg(windows)]
fn build_windows_auto_command(command_str: &str) -> tokio::process::Command {
    if is_executable_available("pwsh") || is_executable_available("powershell") || is_windows_powershell_installed() {
        build_powershell_command(command_str)
    } else {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command_str);
        c
    }
}

/// Unix auto-detection: $SHELL -> sh
#[cfg(not(windows))]
fn build_unix_auto_command(command_str: &str) -> tokio::process::Command {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    let mut c = tokio::process::Command::new(shell);
    c.arg("-c").arg(command_str);
    c
}

/// Checks whether an executable exists in any of the PATH directories.
fn is_executable_available(name: &str) -> bool {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            #[cfg(windows)]
            {
                let candidates = [
                    dir.join(name),
                    dir.join(format!("{name}.exe")),
                    dir.join(format!("{name}.cmd")),
                    dir.join(format!("{name}.bat")),
                ];
                for candidate in &candidates {
                    if candidate.is_file() {
                        return true;
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(windows)]
fn is_windows_powershell_installed() -> bool {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let default_path = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    default_path.is_file()
}

#[async_trait]
impl Tool for CommandTool {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Executes a system shell command safely with bounded execution time, working directory support, and cooperative cancellation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (e.g. 'cargo build', 'git status', 'Get-ChildItem')"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional working directory path (defaults to current directory)"
                },
                "shell": {
                    "type": "string",
                    "enum": ["auto", "powershell", "sh", "bash", "cmd"],
                    "description": "Optional shell interpreter override (default: auto, choosing modern PowerShell on Windows and default shell on Unix)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional timeout in seconds (default: 60, max: 600)"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ArbitraryExecution
    }

    fn requires_approval(&self, _ctx: &ToolContext, arguments: &serde_json::Value) -> bool {
        if self.require_all_approval {
            return true;
        }

        if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
            Self::is_high_risk_command(cmd)
        } else {
            false
        }
    }

    async fn execute(&self, ctx: &ToolContext, arguments: serde_json::Value) -> Result<ToolOutput> {
        ctx.check_cancelled(self.name())?;

        let command_str = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::invalid_args(self.name(), "missing required `command` argument")
            })?;

        let working_dir = arguments
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .or_else(|| self.default_working_dir.clone());

        let shell_override = arguments.get("shell").and_then(|v| v.as_str());

        let timeout_duration = arguments
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .map(|s| Duration::from_secs(s.min(MAX_COMMAND_TIMEOUT_SECS)))
            .unwrap_or(self.default_timeout);

        let mut cmd = self.build_process_command(command_str, shell_override);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let child_future = cmd.output();

        let output = tokio::select! {
            _ = ctx.cancel_token.cancelled() => {
                return Err(ToolError::cancelled(self.name()));
            }
            res = tokio::time::timeout(timeout_duration, child_future) => {
                match res {
                    Ok(Ok(output)) => output,
                    Ok(Err(err)) => {
                        return Err(ToolError::execution(
                            self.name(),
                            format!("failed executing command process `{command_str}`: {err}"),
                        ));
                    }
                    Err(_) => {
                        return Err(ToolError::timeout(format!("{}: `{command_str}`", self.name())));
                    }
                }
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status_code = output.status.code().unwrap_or(-1);

        let mut result = format!("[Exit status: {status_code}]\n");
        if !stdout.is_empty() {
            result.push_str("--- stdout ---\n");
            result.push_str(&stdout);
            if !stdout.ends_with('\n') {
                result.push('\n');
            }
        }
        if !stderr.is_empty() {
            result.push_str("--- stderr ---\n");
            result.push_str(&stderr);
            if !stderr.ends_with('\n') {
                result.push('\n');
            }
        }

        if output.status.success() {
            Ok(ToolOutput::success(result))
        } else {
            Ok(ToolOutput::error(result))
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_kind_parse() {
        assert_eq!(ShellKind::parse_loose("auto"), Some(ShellKind::Auto));
        assert_eq!(ShellKind::parse_loose("POWERSHELL"), Some(ShellKind::PowerShell));
        assert_eq!(ShellKind::parse_loose("pwsh"), Some(ShellKind::PowerShell));
        assert_eq!(ShellKind::parse_loose("sh"), Some(ShellKind::Sh));
        assert_eq!(ShellKind::parse_loose("bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::parse_loose("cmd"), Some(ShellKind::Cmd));
        assert_eq!(ShellKind::parse_loose("unknown_shell"), None);
    }

    #[test]
    fn test_high_risk_command_heuristics_unix() {
        assert!(CommandTool::is_high_risk_command("rm -rf /"));
        assert!(CommandTool::is_high_risk_command("rm -fr /home/user"));
        assert!(CommandTool::is_high_risk_command("rm -r -f test"));
        assert!(CommandTool::is_high_risk_command("rm --recursive --force ."));
        assert!(CommandTool::is_high_risk_command("mkfs.ext4 /dev/sdb"));
        assert!(CommandTool::is_high_risk_command("dd if=/dev/zero of=/dev/sda"));
        assert!(CommandTool::is_high_risk_command("chmod -R 777 /"));
        assert!(CommandTool::is_high_risk_command("echo hello > /dev/sda"));
        assert!(CommandTool::is_high_risk_command("cat malicious > /dev/nvme0n1"));
        assert!(CommandTool::is_high_risk_command("shutdown -h now"));
        assert!(CommandTool::is_high_risk_command("reboot"));
        assert!(CommandTool::is_high_risk_command(":(){ :|:& };:"));
        assert!(CommandTool::is_high_risk_command("DROP DATABASE production;"));

        // Safe commands must not trigger high risk
        assert!(!CommandTool::is_high_risk_command("echo 'hello world' > /dev/null"));
        assert!(!CommandTool::is_high_risk_command("cargo test --all"));
        assert!(!CommandTool::is_high_risk_command("git status"));
        assert!(!CommandTool::is_high_risk_command("ls -la"));
    }

    #[test]
    fn test_high_risk_command_heuristics_windows() {
        // Cmd destructive commands
        assert!(CommandTool::is_high_risk_command("rmdir /s /q C:\\Users"));
        assert!(CommandTool::is_high_risk_command("rd /s /q D:\\Data"));
        assert!(CommandTool::is_high_risk_command("del /f /s /q *.dll"));
        assert!(CommandTool::is_high_risk_command("erase /s /f C:\\"));
        assert!(CommandTool::is_high_risk_command("format C: /fs:ntfs"));
        assert!(CommandTool::is_high_risk_command("vssadmin delete shadows /all"));
        assert!(CommandTool::is_high_risk_command("reg delete HKLM\\Software\\Test"));

        // PowerShell destructive cmdlets
        assert!(CommandTool::is_high_risk_command("Remove-Item -Path C:\\Temp -Recurse -Force"));
        assert!(CommandTool::is_high_risk_command("ri -Recurse -Force C:\\Logs"));
        assert!(CommandTool::is_high_risk_command("Clear-Disk -Number 1 -RemoveData"));
        assert!(CommandTool::is_high_risk_command("Initialize-Disk -Number 2"));
        assert!(CommandTool::is_high_risk_command("Stop-Computer -Force"));
        assert!(CommandTool::is_high_risk_command("Restart-Computer"));

        // Safe Windows/PowerShell commands
        assert!(!CommandTool::is_high_risk_command("Get-ChildItem -Path ."));
        assert!(!CommandTool::is_high_risk_command("dir"));
        assert!(!CommandTool::is_high_risk_command("Get-Process | Where-Object WorkingSet -gt 100MB"));
    }

    #[tokio::test]
    async fn test_command_execution_success() {
        let tool = CommandTool::new();
        let ctx = ToolContext::default();

        let output = tool
            .execute(&ctx, json!({"command": "echo 'Testing Nous Command'"}))
            .await
            .expect("execution must succeed");

        assert!(!output.is_error());
        assert!(output.content().contains("Testing Nous Command"));
    }

    #[tokio::test]
    async fn test_command_execution_failure_exit_status() {
        let tool = CommandTool::new();
        let ctx = ToolContext::default();

        let output = tool
            .execute(&ctx, json!({"command": "non_existent_command_12345"}))
            .await
            .expect("should return tool output with error indicator");

        assert!(output.is_error());
        assert!(output.content().contains("[Exit status:"));
    }
}
