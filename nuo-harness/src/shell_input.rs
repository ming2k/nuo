//! Agent policy for shell commands that require operator input.
//!
//! This advisory classifier belongs beside tool dispatch: it decides whether
//! the agent should open its input-injection path and whether that input must
//! be masked. The shell result DTO and formatting remain in `nuo-contracts`.

/// The kind of operator input a shell command is expected to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellInputKind {
    Plain,
    Secret,
}

impl ShellInputKind {
    pub(crate) fn is_secret(self) -> bool {
        self == Self::Secret
    }
}

/// Build the advisory [`InputExpectation`] for a command the classifier
/// recognized. Used to seed the operator input panel's prompt text and masking
/// before the runtime examiner confirms the wait.
pub(crate) fn expectation(command: &str, kind: ShellInputKind) -> nuo_contracts::InputExpectation {
    nuo_contracts::InputExpectation {
        prompt: if kind.is_secret() {
            "Enter the secret this command is waiting for:".to_string()
        } else {
            format!("This command needs input ({command}):")
        },
        secret: kind.is_secret(),
    }
}

/// Classify a shell command that is likely to block waiting for input.
///
/// Every program token is scanned, including commands behind pipelines and
/// shell separators. A secret-requiring program takes precedence over an
/// ordinary interactive program anywhere in the command. This remains an
/// advisory layer: closed stdin and the idle watchdog are the correctness
/// backstops for commands the classifier does not recognize.
pub(crate) fn classify(command: &str) -> Option<ShellInputKind> {
    let gpg_noninteractive = command.contains("--batch") || command.contains("--passphrase");
    let mut interactive = false;

    for token in
        command.split(|c: char| c.is_whitespace() || matches!(c, '|' | '&' | ';' | '(' | ')'))
    {
        let program = token.rsplit('/').next().unwrap_or(token);
        if program.is_empty() {
            continue;
        }

        // Skip leading/inline environment assignments (e.g. `FOO=bar sudo ...`).
        if let Some((var_name, _)) = program.split_once('=')
            && !var_name.is_empty()
            && var_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }

        if program.eq_ignore_ascii_case("gpg") {
            if !gpg_noninteractive {
                return Some(ShellInputKind::Secret);
            }
            continue;
        }

        if matches!(program, "sudo" | "su" | "passwd" | "visudo") || program.starts_with("pinentry")
        {
            return Some(ShellInputKind::Secret);
        }

        interactive |= matches!(
            program,
            "chpasswd"
                | "adduser"
                | "useradd"
                | "whiptail"
                | "dialog"
                | "vim"
                | "vi"
                | "nano"
                | "emacs"
                | "less"
                | "more"
                | "man"
                | "top"
                | "htop"
                | "watch"
        );
    }

    interactive.then_some(ShellInputKind::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_secret_commands_and_nested_invocations() {
        for command in [
            "sudo apt update",
            "/usr/bin/sudo ls",
            "passwd user",
            "pinentry-curses",
            "gpg --decrypt f",
            "echo secret | sudo -S apt update",
            "cd a && gpg --sign f",
        ] {
            assert_eq!(classify(command), Some(ShellInputKind::Secret), "{command}");
        }
    }

    #[test]
    fn classifies_plain_interactive_commands_and_nested_invocations() {
        for command in [
            "vim file.txt",
            "less README.md",
            "man grep",
            "top",
            "dialog --menu pick 10 40 3",
            "prepare.sh && whiptail --gauge work 10 40 0",
        ] {
            assert_eq!(classify(command), Some(ShellInputKind::Plain), "{command}");
        }
    }

    #[test]
    fn leaves_noninteractive_commands_alone() {
        for command in [
            "git status",
            "cargo build",
            "ls -la",
            "echo hello",
            "",
            "gpg --batch --sign file",
            "gpg --passphrase-file /tmp/pw --decrypt f",
        ] {
            assert_eq!(classify(command), None, "{command}");
        }
    }

    #[test]
    fn secret_input_wins_over_plain_input() {
        assert_eq!(
            classify("dialog --msgbox hi 10 40 && sudo true"),
            Some(ShellInputKind::Secret)
        );
    }
}
