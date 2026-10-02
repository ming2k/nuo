//! Explicit native shell selection, quoting, and sentinel protocol shims.

use tokio::process::Command;

/// Dialects of shell execution supported across platforms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellDialect {
    /// POSIX compatible shell (sh, bash, zsh, dash).
    Posix,
    /// Modern PowerShell Core (`pwsh`) or Windows PowerShell (`powershell.exe`).
    PowerShell,
}

impl ShellDialect {
    /// Short identifier for the dialect (e.g. "posix" or "powershell").
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::PowerShell => "powershell",
        }
    }

    /// Human-readable description of the shell.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Posix => "POSIX sh / bash",
            Self::PowerShell => "PowerShell (pwsh / powershell.exe)",
        }
    }

    /// Preferred binary executable name for this dialect.
    #[must_use]
    pub fn executable_name(&self) -> &'static str {
        match self {
            Self::Posix => "sh",
            Self::PowerShell => {
                #[cfg(windows)]
                {
                    "powershell.exe"
                }
                #[cfg(not(windows))]
                {
                    "pwsh"
                }
            }
        }
    }

    /// Build a non-interactive episodic shell command invocation for user script text.
    pub fn build_episodic_command(&self, script: &str) -> Command {
        let mut command = match self {
            Self::Posix => {
                let mut command = Command::new(self.executable_name());
                command.arg("-c").arg(script);
                command
            }
            Self::PowerShell => {
                let mut command = Command::new(self.executable_name());
                command
                    .arg("-NoLogo")
                    .arg("-NoProfile")
                    .arg("-NonInteractive")
                    .arg("-ExecutionPolicy")
                    .arg("Bypass")
                    .arg("-Command")
                    .arg(format!(
                        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {}",
                        script
                    ));
                command
            }
        };
        configure_headless_env(&mut command);
        command
    }

    /// Build a persistent interactive-like shell command reading from stdin.
    pub fn build_persistent_command(&self) -> Command {
        let mut command = match self {
            Self::Posix => {
                let mut command = Command::new(self.executable_name());
                command.arg("-s");
                command
            }
            Self::PowerShell => {
                let mut command = Command::new(self.executable_name());
                command
                    .arg("-NoLogo")
                    .arg("-NoProfile")
                    .arg("-NonInteractive")
                    .arg("-ExecutionPolicy")
                    .arg("Bypass")
                    .arg("-Command")
                    .arg("-");
                command
            }
        };
        configure_headless_env(&mut command);
        command
    }

    /// Format a sentinel payload string written into stdin to execute `command`
    /// and reliably print the exit status together with `sentinel_id`.
    #[must_use]
    pub fn format_sentinel_command(&self, command: &str, sentinel_id: &str) -> String {
        match self {
            Self::Posix => {
                format!("{}\nprintf '\\n{}:%d\\n' $?\n", command, sentinel_id)
            }
            Self::PowerShell => {
                format!(
                    "{}\r\n$__muta_ec = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}; Write-Output \"`r`n{}:$__muta_ec`r`n\"\r\n",
                    command, sentinel_id
                )
            }
        }
    }

    /// Quote and escape an argument for safe insertion into a command line of this dialect.
    #[must_use]
    pub fn quote_arg(&self, arg: &str) -> String {
        match self {
            Self::Posix => {
                if arg.is_empty() {
                    return "''".to_string();
                }
                if arg.chars().all(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=')
                }) {
                    return arg.to_string();
                }
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
            Self::PowerShell => {
                if arg.is_empty() {
                    return "''".to_string();
                }
                if arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':'))
                {
                    return arg.to_string();
                }
                format!("'{}'", arg.replace('\'', "''"))
            }
        }
    }
}

/// Configure standard hermetic headless environment invariants for non-interactive execution.
///
/// Implements ADR-0286:
/// - Rejects interactive editor prompts fail-fast (`EDITOR=false`, `VISUAL=false`).
/// - Suppresses credential and pager blocking (`GIT_TERMINAL_PROMPT=0`, `PAGER=cat`).
/// - Sets ecosystem non-interactive consensus (`CI=1`, `DEBIAN_FRONTEND=noninteractive`, `TERM=dumb`).
/// - Strips host terminal and display environment bindings (`GPG_TTY`, `DISPLAY`, `WAYLAND_DISPLAY`).
pub fn configure_headless_env(command: &mut Command) {
    command
        .env("EDITOR", "false")
        .env("VISUAL", "false")
        .env("GIT_EDITOR", "false")
        .env("GIT_SEQUENCE_EDITOR", "false")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("CI", "1")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("TERM", "dumb")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GPG_TTY")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
}

/// Synchronous counterpart of [`configure_headless_env`] for standard process commands.
pub fn configure_headless_env_std(command: &mut std::process::Command) {
    command
        .env("EDITOR", "false")
        .env("VISUAL", "false")
        .env("GIT_EDITOR", "false")
        .env("GIT_SEQUENCE_EDITOR", "false")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("CI", "1")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("TERM", "dumb")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GPG_TTY")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
}

/// Detect the default native shell dialect of the host platform.
#[must_use]
pub const fn native_shell_dialect() -> ShellDialect {
    #[cfg(unix)]
    return ShellDialect::Posix;
    #[cfg(windows)]
    return ShellDialect::PowerShell;
}

/// Build a non-interactive native-shell invocation for user-authored script
/// text. Internal commands should still use argv directly.
pub fn native_shell(script: &str) -> Command {
    native_shell_dialect().build_episodic_command(script)
}

/// Build a long-running persistent native shell command that reads commands from stdin.
pub fn persistent_shell_command() -> Command {
    native_shell_dialect().build_persistent_command()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_sentinel_format() {
        let formatted = ShellDialect::Posix.format_sentinel_command("echo test", "TOKEN123");
        assert!(formatted.contains("printf '\\nTOKEN123:%d\\n' $?"));
    }

    #[test]
    fn powershell_sentinel_format() {
        let formatted =
            ShellDialect::PowerShell.format_sentinel_command("Write-Output test", "TOKEN123");
        assert!(formatted.contains("TOKEN123:$__muta_ec"));
        assert!(formatted.contains("Write-Output"));
    }

    #[test]
    fn posix_quoting_rules() {
        assert_eq!(ShellDialect::Posix.quote_arg("hello"), "hello");
        assert_eq!(
            ShellDialect::Posix.quote_arg("hello world"),
            "'hello world'"
        );
        assert_eq!(ShellDialect::Posix.quote_arg("don't"), "'don'\\''t'");
    }

    #[test]
    fn powershell_quoting_rules() {
        assert_eq!(ShellDialect::PowerShell.quote_arg("hello"), "hello");
        assert_eq!(
            ShellDialect::PowerShell.quote_arg("hello world"),
            "'hello world'"
        );
        assert_eq!(ShellDialect::PowerShell.quote_arg("don't"), "'don''t'");
    }
}
